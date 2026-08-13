//! NodFactory runtime: issuance, PoW-gated mining, event emission.
//!
//! All persistent Nod state lives in the entity store at
//! [`outbe_primitives::addresses::NOD_ADDRESS`]. NodFactory mutates that
//! state exclusively through [`outbe_nod::api`] and emits its own events at
//! [`NOD_FACTORY_ADDRESS`].

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{SolCall, SolEvent};
use outbe_primitives::addresses::{NOD_FACTORY_ADDRESS, VAULT_ROUTER_ADDRESS};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use outbe_common::pow;
use outbe_compressed_entities::{EntityId36, ExecutionScope, ParentBodySource};
use outbe_nod::api as nod_api;
use outbe_nod::api::{LoadedNodBucket, LoadedNodItem};
use outbe_nod::schema::{NodContract, NodIssueParams, NodItemState};

use crate::errors::NodFactoryError;
use crate::precompile::INodFactory;
use crate::sol_ext::IERC20;

/// Issues a Nod through the block-scoped compressed-body lifecycle.
pub fn issue_nod(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    params: &NodIssueParams,
) -> Result<EntityId36> {
    if params.owner.is_zero() {
        return Err(NodFactoryError::InvalidOwner.into());
    }

    let nod_id = NodContract::generate_nod_id(params.owner, params.worldwide_day)?;
    if nod_api::get_item(storage, scope, parent, nod_id)?.is_some() {
        return Err(NodFactoryError::NodAlreadyExists.into());
    }

    issue_nod_inner(storage, params, |item| {
        nod_api::add_nod(storage, scope, parent, item, params.entry_price_minor)
    })
}

fn issue_nod_inner(
    storage: &StorageHandle<'_>,
    params: &NodIssueParams,
    add: impl FnOnce(&NodItemState) -> Result<()>,
) -> Result<EntityId36> {
    let nod_id = NodContract::generate_nod_id(params.owner, params.worldwide_day)?;

    let bucket_key = NodContract::bucket_key(
        params.worldwide_day,
        params.floor_price_minor,
        params.reference_currency,
    );

    let issued_at = storage.timestamp()?.to::<u64>();

    let item = NodItemState {
        nod_id,
        owner: params.owner,
        gratis_load_minor: params.gratis_load_minor,
        worldwide_day: params.worldwide_day,
        league_id: params.league_id,
        floor_price_minor: params.floor_price_minor,
        bucket_key,
        cost_amount_minor: params.cost_amount_minor,
        issuance_currency: params.issuance_currency,
        reference_currency: params.reference_currency,
        issued_at,
        is_settled: false,
    };
    add(&item)?;

    emit_event(
        storage,
        INodFactory::NodIssued {
            owner: params.owner,
            nodId: Bytes::copy_from_slice(nod_id.as_bytes()),
            worldwideDay: U256::from(u32::from(params.worldwide_day)),
            leagueId: U256::from(params.league_id),
            floorPriceMinor: params.floor_price_minor,
            gratisLoadMinor: params.gratis_load_minor,
            entryPriceMinor: params.entry_price_minor,
            costAmountMinor: params.cost_amount_minor,
        },
    )?;

    Ok(nod_id)
}

/// Atomic settle path: pay `cost_amount_minor` into the reserve vault and mark
/// the Nod settled, emitting `NodSettled`. Returns the amount paid.
///
/// Settlement is unrestricted: any address may settle any Nod at any point in
/// its life, including before its bucket qualifies. Ownership, PoW, and
/// qualification are `mine_gratis` concerns only.
///
/// The payment asset is not caller-selected: it is resolved from the vault
/// router's registry of assets denominating the Nod's recorded
/// `reference_currency`, taking the first registered entry. Settling a
/// nonzero-cost Nod whose reference currency has no registered asset fails
/// with `NoSettlementAsset`.
///
/// Payment pulls `cost_amount_minor` of that asset from `payer` into the
/// precompile address via `IERC20.transferFrom`, approves the reserve
/// `VAULT_ROUTER_ADDRESS` for the same amount, and calls `IVaultRouter.deposit`
/// declaring the `StablesSource::NodCostAmount` classifier. The payer MUST
/// grant the NodFactory precompile an ERC20 allowance of at least
/// `cost_amount_minor` beforehand.
///
/// When `cost_amount_minor == 0` the payment sequence is skipped entirely and
/// no asset is resolved.
pub fn settle_nod(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    payer: Address,
    nod_id: EntityId36,
) -> Result<U256> {
    let item =
        nod_api::load_item(storage, scope, parent, nod_id)?.ok_or(NodFactoryError::NodNotFound)?;
    if item.body().is_settled {
        return Err(NodFactoryError::NodAlreadySettled.into());
    }
    storage
        .clone()
        .with_checkpoint(|| settle_nod_inner(storage, scope, payer, nod_id, item))
}

fn settle_nod_inner(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    payer: Address,
    nod_id: EntityId36,
    item: LoadedNodItem,
) -> Result<U256> {
    let owner = item.body().owner;
    let cost = item.body().cost_amount_minor;
    let reference_currency = item.body().reference_currency;

    // Effects before interactions: a reentrant asset sees the settled flag
    // and reverts instead of paying twice.
    nod_api::settle_nod(storage, scope, item)?;

    let mut asset = Address::ZERO;
    if !cost.is_zero() {
        // The cost amount is denominated in the Nod's reference currency, so the
        // asset comes from the router's registry for that currency rather than
        // from the caller.
        asset = settlement_asset(storage, reference_currency)?;

        // 1) Pull stablecoin from the payer into the nodfactory precompile address.
        let transfer = IERC20::transferFromCall {
            from: payer,
            to: NOD_FACTORY_ADDRESS,
            amount: cost,
        }
        .abi_encode();
        storage.call(asset, U256::ZERO, transfer.into())?;

        // 2) Approve the reserve vault to spend that exact amount. The precompile
        //    owns the intermediate balance and resets to `cost` each call, so
        //    there is no leftover allowance to clear.
        let approve = IERC20::approveCall {
            spender: VAULT_ROUTER_ADDRESS,
            amount: cost,
        }
        .abi_encode();
        storage.call(asset, U256::ZERO, approve.into())?;

        // 3) Vault pulls and deposits into the reserve vault via its Solidity ABI.
        outbe_vaultrouter::api::deposit(storage, asset, cost)?;
    }

    emit_event(
        storage,
        INodFactory::NodSettled {
            owner,
            payer,
            nodId: Bytes::copy_from_slice(nod_id.as_bytes()),
            asset,
            amountPaid: cost,
        },
    )?;

    Ok(cost)
}

/// Atomic mine-gratis path: validate ownership + PoW + bucket qualification +
/// settlement, burn the Nod (emitting `NodBurned`), then delegate the matching
/// gratis mint to `gratisfactory` (which mints to the owner and records the
/// Fidelity cohort; the `GratisMinted` event is emitted by the Gratis token).
/// Returns the minted amount.
///
/// The Nod's cost amount must already have been paid through [`settle_nod`];
/// this path moves no value of its own.
pub fn mine_gratis(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    caller: Address,
    nod_id: EntityId36,
    nonce: U256,
    auth: outbe_gratisfactory::api::ModifyAuth,
) -> Result<U256> {
    let item =
        nod_api::load_item(storage, scope, parent, nod_id)?.ok_or(NodFactoryError::NodNotFound)?;
    let bucket_id = EntityId36::new(item.body().worldwide_day, item.body().bucket_key.0);
    let bucket = nod_api::load_bucket(storage, scope, parent, bucket_id)?
        .ok_or(NodFactoryError::NodNotQualified)?;
    storage.clone().with_checkpoint(|| {
        mine_gratis_inner(
            storage,
            MineGratisInput {
                caller,
                nod_id,
                nonce,
                item,
                bucket,
                auth,
            },
            scope,
        )
    })
}

struct MineGratisInput {
    caller: Address,
    nod_id: EntityId36,
    nonce: U256,
    item: LoadedNodItem,
    bucket: LoadedNodBucket,
    auth: outbe_gratisfactory::api::ModifyAuth,
}

fn mine_gratis_inner(
    storage: &StorageHandle<'_>,
    input: MineGratisInput,
    scope: &ExecutionScope,
) -> Result<U256> {
    let MineGratisInput {
        caller,
        nod_id,
        nonce,
        item,
        bucket,
        auth,
    } = input;
    if caller != item.body().owner {
        return Err(NodFactoryError::NotOwner.into());
    }

    validate_pow(nod_id, nonce)?;

    if !bucket.body().is_qualified {
        return Err(NodFactoryError::NodNotQualified.into());
    }

    if !item.body().is_settled {
        return Err(NodFactoryError::NodNotSettled.into());
    }

    let owner = item.body().owner;
    let gratis_load_minor = item.body().gratis_load_minor;
    nod_api::remove_nod(storage, scope, item, bucket)?;

    emit_event(
        storage,
        INodFactory::NodBurned {
            owner: caller,
            nodId: Bytes::copy_from_slice(nod_id.as_bytes()),
            gratisLoadMinor: gratis_load_minor,
        },
    )?;

    outbe_gratisfactory::api::mint(storage.clone(), owner, gratis_load_minor, auth)?;

    Ok(gratis_load_minor)
}

/// Resolves the settlement asset for a reference currency from the vault
/// router's registry. Registry order carries no meaning, so the first entry is
/// as good as any; an empty registry is a configuration error, not a payer one.
fn settlement_asset(storage: &StorageHandle<'_>, reference_currency: u16) -> Result<Address> {
    outbe_vaultrouter::api::reference_currency_assets(storage, reference_currency)?
        .into_iter()
        .next()
        .ok_or_else(|| NodFactoryError::NoSettlementAsset { reference_currency }.into())
}

/// PoW gate for `mine_gratis`, delegating to the shared [`outbe_common::pow`]
/// scheme and mapping failures onto [`NodFactoryError`].
pub fn validate_pow(nod_id: EntityId36, nonce: U256) -> Result<()> {
    pow::validate_pow_bytes(nod_id.as_bytes(), nonce).map_err(|e| NodFactoryError::from(e).into())
}

/// Shared PoW hash over `ascii(hex(nod_id)) || nonce.to_be_bytes::<8>()`.
pub fn compute_pow_hash(nod_id: EntityId36, nonce: U256) -> Result<[u8; 32]> {
    pow::compute_pow_hash_bytes(nod_id.as_bytes(), nonce)
        .map_err(|e| NodFactoryError::from(e).into())
}

fn emit_event<E: SolEvent>(storage: &StorageHandle<'_>, event: E) -> Result<()> {
    storage.emit_event(NOD_FACTORY_ADDRESS, event.encode_log_data())
}
