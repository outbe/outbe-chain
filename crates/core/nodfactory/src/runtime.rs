//! NodFactory runtime: issuance, settlement, PoW-gated exercise, event emission.
//!
//! All persistent Nod state lives in the entity store at
//! [`outbe_primitives::addresses::NOD_ADDRESS`]. NodFactory mutates that
//! state exclusively through [`outbe_nod::api`] and emits its own events at
//! [`NOD_FACTORY_ADDRESS`].

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::{SolCall, SolEvent};
use outbe_primitives::addresses::{NOD_FACTORY_ADDRESS, VAULT_ROUTER_ADDRESS};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;

use outbe_common::pow;
use outbe_common::settlement::floor_to_asset_units;
use outbe_compressed_entities::{ExecutionScope, ParentBodySource, WwdEntityId};
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
) -> Result<WwdEntityId> {
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
) -> Result<WwdEntityId> {
    let nod_id = NodContract::generate_nod_id(params.owner, params.worldwide_day)?;

    let bucket_key = NodContract::bucket_key(
        params.worldwide_day,
        params.floor_price_minor,
        params.reference_currency,
    );

    let issued_at = storage.timestamp()?.to::<u64>();

    let item = NodItemState {
        is_settled: false,
        nod_id,
        owner: params.owner,
        gratis_load_minor: params.gratis_load_minor,
        worldwide_day: params.worldwide_day,
        league_id: params.league_id,
        floor_price_minor: params.floor_price_minor,
        bucket_key,
        issuance_currency: params.issuance_currency,
        reference_currency: params.reference_currency,
        issued_at,
    };
    add(&item)?;

    emit_event(
        storage,
        INodFactory::NodIssued {
            owner: params.owner,
            nodId: nod_id.to_u256(),
            worldwideDay: U256::from(u32::from(params.worldwide_day)),
            leagueId: U256::from(params.league_id),
            floorPriceMinor: params.floor_price_minor,
            gratisLoadMinor: params.gratis_load_minor,
            entryPriceMinor: params.entry_price_minor,
            costAmountMinor: nod_api::cost_amount_minor(
                params.entry_price_minor,
                params.gratis_load_minor,
            )?,
        },
    )?;

    Ok(nod_id)
}

/// Owner-authorized exercise of one paid Nod.
pub struct MineGratisRequest {
    pub caller: Address,
    pub nod_id: WwdEntityId,
    pub nonce: u64,
    pub auth: outbe_gratisfactory::api::ModifyAuth,
}

/// Pays a qualified Nod's known cost directly in ERC20 base units.
pub fn settle_nod(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    caller: Address,
    nod_id: WwdEntityId,
    asset: Address,
) -> Result<()> {
    settle(
        storage,
        scope,
        parent,
        caller,
        nod_id,
        |reference_currency, gratis_load, entry_price| {
            check_settlement_asset(storage, reference_currency, asset)?;
            let cost = nod_api::cost_amount_minor(entry_price, gratis_load)?;
            if !cost.is_zero() {
                let before = token_balance(storage, asset)?;
                checked_token_call(
                    storage,
                    asset,
                    IERC20::transferFromCall {
                        from: caller,
                        to: NOD_FACTORY_ADDRESS,
                        amount: cost,
                    },
                )?;
                if token_balance(storage, asset)?.checked_sub(before) != Some(cost) {
                    return Err(NodFactoryError::SettlementAmountMismatch.into());
                }
                checked_token_call(
                    storage,
                    asset,
                    IERC20::approveCall {
                        spender: VAULT_ROUTER_ADDRESS,
                        amount: cost,
                    },
                )?;
                outbe_vaultrouter::api::deposit(storage, asset, cost)?;
                if token_balance(storage, asset)? != before {
                    return Err(NodFactoryError::SettlementAmountMismatch.into());
                }
            }
            Ok(PaidCost {
                asset,
                nullifier: B256::ZERO,
                spend_amount: cost,
            })
        },
    )
}

/// Pays a qualified Nod's exact cost by spending a PayNote.
pub fn settle_nod_with_paynote(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    caller: Address,
    nod_id: WwdEntityId,
    paynote_proof: &[u8],
) -> Result<()> {
    settle(
        storage,
        scope,
        parent,
        caller,
        nod_id,
        |reference_currency, gratis_load, entry_price| {
            discharge_cost(
                storage,
                reference_currency,
                gratis_load,
                entry_price,
                caller,
                paynote_proof,
            )
        },
    )
}

fn settle(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    caller: Address,
    nod_id: WwdEntityId,
    pay: impl FnOnce(u16, U256, U256) -> Result<PaidCost>,
) -> Result<()> {
    let (item, bucket) = load_owned_nod(storage, scope, parent, caller, nod_id)?;
    if item.body().is_settled {
        return Err(NodFactoryError::NodAlreadySettled.into());
    }
    if !bucket.body().is_qualified {
        return Err(NodFactoryError::NodNotQualified.into());
    }
    let deadline = nod_api::settlement_deadline(storage, item.body().bucket_key)?;
    if deadline != 0 && storage.timestamp()?.to::<u64>() > deadline {
        return Err(NodFactoryError::CallDeadlineExpired.into());
    }
    storage.clone().with_checkpoint(|| {
        let reference_currency = item.body().reference_currency;
        let gratis_load = item.body().gratis_load_minor;
        let entry_price = bucket.body().entry_price_minor;
        // Publish the transition before external payment calls so callbacks cannot
        // settle the same Nod twice. A failed payment rolls the transition back.
        nod_api::settle_nod(storage, scope, item, bucket)?;
        let paid = pay(reference_currency, gratis_load, entry_price)?;
        emit_event(
            storage,
            INodFactory::NodPaid {
                owner: caller,
                nodId: nod_id.to_u256(),
                asset: paid.asset,
                nullifier: paid.nullifier,
                amountCovered: paid.spend_amount,
            },
        )
    })
}

fn checked_token_call(
    storage: &StorageHandle<'_>,
    asset: Address,
    call: impl SolCall,
) -> Result<()> {
    let ret = storage.call(asset, U256::ZERO, call.abi_encode().into())?;
    if !ret.is_empty() && ret.as_ref() != U256::ONE.to_be_bytes::<32>() {
        return Err(NodFactoryError::TokenOperationFailed.into());
    }
    Ok(())
}

fn token_balance(storage: &StorageHandle<'_>, asset: Address) -> Result<U256> {
    let ret = storage.staticcall(
        asset,
        IERC20::balanceOfCall {
            account: NOD_FACTORY_ADDRESS,
        }
        .abi_encode()
        .into(),
    )?;
    IERC20::balanceOfCall::abi_decode_returns(&ret)
        .map_err(|_| NodFactoryError::TokenOperationFailed.into())
}

/// Exercises a paid entitlement without payment or a deadline. Mint failure
/// rolls back the removal, preserving the paid Nod for retry.
pub fn mine_gratis(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    request: MineGratisRequest,
) -> Result<U256> {
    let MineGratisRequest {
        caller,
        nod_id,
        nonce,
        auth,
    } = request;
    let (item, bucket) = load_owned_nod(storage, scope, parent, caller, nod_id)?;
    if !item.body().is_settled {
        return Err(NodFactoryError::NodNotSettled.into());
    }
    validate_pow(nod_id, nonce)?;
    let gratis_load_minor = item.body().gratis_load_minor;
    storage.clone().with_checkpoint(|| {
        nod_api::remove_nod(storage, scope, item, bucket)?;
        emit_event(
            storage,
            INodFactory::NodExercised {
                owner: caller,
                nodId: nod_id.to_u256(),
                gratisLoadMinor: gratis_load_minor,
            },
        )?;
        emit_event(
            storage,
            INodFactory::NodBurned {
                owner: caller,
                nodId: nod_id.to_u256(),
                gratisLoadMinor: gratis_load_minor,
            },
        )?;
        outbe_gratisfactory::api::mint(storage.clone(), caller, gratis_load_minor, auth)?;
        Ok(gratis_load_minor)
    })
}

fn load_owned_nod(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    caller: Address,
    nod_id: WwdEntityId,
) -> Result<(LoadedNodItem, LoadedNodBucket)> {
    let item =
        nod_api::load_item(storage, scope, parent, nod_id)?.ok_or(NodFactoryError::NodNotFound)?;
    if caller != item.body().owner {
        return Err(NodFactoryError::NotOwner.into());
    }
    if NodContract::new(storage.clone())
        .ocomp_certified_generation(item.body().worldwide_day)?
        .is_some_and(|generation| generation.next_nod_ordinal < generation.nod_count)
    {
        return Err(NodFactoryError::NodGenerationNotMaterialized.into());
    }
    let bucket_id =
        WwdEntityId::from_day_and_digest(item.body().worldwide_day, item.body().bucket_key.0);
    let bucket = nod_api::load_bucket(storage, scope, parent, bucket_id)?
        .ok_or(NodFactoryError::NodNotQualified)?;
    Ok((item, bucket))
}

/// One discharged Nod cost, as it is reported by `NodPaid`.
struct PaidCost {
    asset: Address,
    nullifier: B256,
    spend_amount: U256,
}

/// Discharges a Nod's cost by spending one PayNote.
///
/// The proof is the payment. `consume` books its nullifier before returning, so
/// the note cannot be spent twice; running inside the caller's checkpoint means
/// a later failure un-books it. It is called last, after the cheap
/// owner/qualification/deadline guards, so rejected settlement never pays for
/// verification.
fn discharge_cost(
    storage: &StorageHandle<'_>,
    reference_currency: u16,
    gratis_load_minor: U256,
    entry_price_minor: U256,
    caller: Address,
    paynote_proof: &[u8],
) -> Result<PaidCost> {
    let claim = outbe_paynote::api::consume(storage, paynote_proof)?;

    // PayNote notes are bearer instruments: the proof names its own owner and
    // anyone can relay it. Binding that owner to the caller is what stops an
    // observer from lifting a broadcast proof to pay for their own Nod.
    if claim.owner != caller {
        return Err(NodFactoryError::PayNoteOwnerMismatch {
            expected: caller,
            actual: claim.owner,
        }
        .into());
    }
    check_settlement_asset(storage, reference_currency, claim.asset)?;
    let cost = settlement_units(
        entry_price_minor,
        gratis_load_minor,
        read_decimals(storage, claim.asset)?,
    )?;
    if claim.spend_amount != cost {
        return Err(NodFactoryError::PayNoteCostMismatch {
            covered: claim.spend_amount,
            required: cost,
        }
        .into());
    }

    Ok(PaidCost {
        asset: claim.asset,
        nullifier: claim.nullifier,
        spend_amount: claim.spend_amount,
    })
}

/// The Nod's cost in the settlement asset's minor units, floored once.
pub(crate) fn settlement_units(
    entry_price_minor: U256,
    gratis_load_minor: U256,
    asset_decimals: u8,
) -> Result<U256> {
    const OBLIGATION_DECIMALS: u32 = 12;
    let obligation = entry_price_minor
        .checked_mul(gratis_load_minor)
        .ok_or_else(|| PrecompileError::Revert("nod cost overflow".into()))?;
    floor_to_asset_units(obligation, U256::ONE, OBLIGATION_DECIMALS, asset_decimals)
        .map_err(|e| NodFactoryError::from(e).into())
}

/// Reads the settlement asset's `decimals()` via a static sub-call.
fn read_decimals(storage: &StorageHandle<'_>, asset: Address) -> Result<u8> {
    let ret = storage.staticcall(asset, IERC20::decimalsCall {}.abi_encode().into())?;
    IERC20::decimalsCall::abi_decode_returns(&ret)
        .map_err(|_| PrecompileError::Revert("settlement asset decimals undecodable".into()))
}

/// Rejects a payment whose asset the vault router does not register under
/// `reference_currency`.
///
/// The cost is denominated in the Nod's own reference currency, so any asset
/// registered under it settles the Nod: the registry lists interchangeable
/// alternatives, not a preference, and the payer selects the asset. An empty
/// registry is a configuration error, not a payer one.
fn check_settlement_asset(
    storage: &StorageHandle<'_>,
    reference_currency: u16,
    asset: Address,
) -> Result<()> {
    let registered =
        outbe_vaultrouter::api::reference_currency_assets(storage, reference_currency)?;
    if !registered.contains(&asset) {
        return Err(NodFactoryError::SettlementAssetMismatch {
            asset,
            reference_currency,
        }
        .into());
    }
    Ok(())
}

/// PoW gate for `mine_gratis`, delegating to the shared [`outbe_common::pow`]
/// scheme and mapping failures onto [`NodFactoryError`].
pub fn validate_pow(nod_id: WwdEntityId, nonce: u64) -> Result<()> {
    pow::validate_pow(nod_id.to_u256(), nonce).map_err(|e| NodFactoryError::from(e).into())
}

/// Shared PoW hash over `nod_id.to_be_bytes::<32>() || nonce.to_be_bytes()`.
pub fn compute_pow_hash(nod_id: WwdEntityId, nonce: u64) -> [u8; 32] {
    pow::compute_pow_hash(nod_id.to_u256(), nonce)
}

fn emit_event<E: SolEvent>(storage: &StorageHandle<'_>, event: E) -> Result<()> {
    storage.emit_event(NOD_FACTORY_ADDRESS, event.encode_log_data())
}
