//! NodFactory runtime: issuance, PoW-gated mining, event emission.
//!
//! All persistent Nod state lives in the entity store at
//! [`outbe_primitives::addresses::NOD_ADDRESS`]. NodFactory mutates that
//! state exclusively through [`outbe_nod::api`] and emits its own events at
//! [`NOD_FACTORY_ADDRESS`].

use alloy_primitives::{Address, U256};
use alloy_sol_types::{SolCall, SolEvent};
use outbe_primitives::addresses::{NOD_FACTORY_ADDRESS, VAULT_PROVIDER_ADDRESS};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use outbe_common::pow;
use outbe_nod::api as nod_api;
use outbe_nod::constants::UNLOCK_PERIOD_SECONDS;
use outbe_nod::schema::{NodContract, NodIssueParams, NodItemState};

use crate::errors::NodFactoryError;
use crate::precompile::INodFactory;
use crate::sol_ext::IERC20;

/// Issue a new Nod at the originating owner's (worldwide_day) bucket.
///
/// Privileged: the only production caller is Lysis via the cross-module
/// [`crate::api::issue_nod`]. Not exposed on the NodFactory ABI.
pub fn issue_nod(storage: &StorageHandle<'_>, params: &NodIssueParams) -> Result<U256> {
    if params.owner.is_zero() {
        return Err(NodFactoryError::InvalidOwner.into());
    }

    let nod_id = NodContract::generate_nod_id(params.owner, params.worldwide_day);
    if nod_api::get_item(storage, nod_id)?.is_some() {
        return Err(NodFactoryError::NodAlreadyExists.into());
    }

    let bucket_key = NodContract::bucket_key(params.worldwide_day, params.floor_price_minor);

    let issued_at = storage.timestamp()?.to::<u64>();
    let unlocks_at = issued_at
        .checked_add(UNLOCK_PERIOD_SECONDS)
        .ok_or(NodFactoryError::NodLocked)?;

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
        unlocks_at,
        reference_currency: params.reference_currency,
        issued_at,
    };
    nod_api::add_nod(storage, &item, params.entry_price_minor)?;

    emit_event(
        storage,
        INodFactory::NodIssued {
            owner: params.owner,
            nodId: nod_id,
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

/// Atomic mine-gratis path: validate ownership + unlock + PoW + bucket
/// qualification, pull `cost_amount_minor` from the caller as a vault
/// deposit (when non-zero), burn the Nod (emitting `NodBurned`), then
/// delegate the matching gratis mint to `gratisfactory` (which mints to the
/// owner, records the Fidelity cohort, and emits `GratisMined`). Returns the
/// minted amount.
///
/// Cost-amount payment: when `item.cost_amount_minor > 0` the runtime pulls
/// that amount of `asset` from the caller into the precompile address via
/// `IERC20.transferFrom`, approves the reserve `VAULT_PROVIDER_ADDRESS` for the
/// same amount, and calls `IVaultProvider.depositLiquidity` declaring the
/// `LiquiditySource::NodCostPrice` classifier. The caller MUST grant the
/// NodFactory precompile an ERC20 allowance of at least `cost_amount_minor`
/// before invoking `mineGratis`.
///
/// When `cost_amount_minor == 0` the payment sequence is skipped entirely
/// and `asset` is not validated, so callers mining zero-cost Nods can pass
/// `Address::ZERO`.
pub fn mine_gratis(
    storage: &StorageHandle<'_>,
    caller: Address,
    nod_id: U256,
    nonce: U256,
    asset: Address,
) -> Result<U256> {
    let item = nod_api::get_item(storage, nod_id)?.ok_or(NodFactoryError::NodNotFound)?;
    if caller != item.owner {
        return Err(NodFactoryError::NotOwner.into());
    }

    let now = storage.timestamp()?.to::<u64>();
    if now < item.unlocks_at {
        return Err(NodFactoryError::NodLocked.into());
    }

    validate_pow(nod_id, nonce)?;

    let bucket =
        nod_api::get_bucket(storage, item.bucket_key)?.ok_or(NodFactoryError::NodNotQualified)?;
    if !bucket.is_qualified {
        return Err(NodFactoryError::NodNotQualified.into());
    }

    let cost = item.cost_amount_minor;
    if !cost.is_zero() {
        // TODO check that asset aligns with reference_currency
        if asset.is_zero() {
            return Err(NodFactoryError::InvalidAsset.into());
        }

        // 1) Pull stablecoin from caller into the nodfactory precompile address.
        let transfer = IERC20::transferFromCall {
            from: caller,
            to: NOD_FACTORY_ADDRESS,
            amount: cost,
        }
        .abi_encode();
        storage.call(asset, U256::ZERO, transfer.into())?;

        // 2) Approve the reserve vault to spend that exact amount. The precompile
        //    owns the intermediate balance and resets to `cost` each call, so
        //    there is no leftover allowance to clear.
        let approve = IERC20::approveCall {
            spender: VAULT_PROVIDER_ADDRESS,
            amount: cost,
        }
        .abi_encode();
        storage.call(asset, U256::ZERO, approve.into())?;

        // 3) Vault pulls and deposits into the reserve vault via its Solidity ABI.
        outbe_vaultprovider::api::deposit_liquidity(storage, asset, cost)?;
    }

    nod_api::remove_nod(storage, &item)?;

    emit_event(
        storage,
        INodFactory::NodBurned {
            owner: caller,
            nodId: nod_id,
            gratisLoadMinor: item.gratis_load_minor,
        },
    )?;

    outbe_gratisfactory::api::mine(storage.clone(), caller, item.gratis_load_minor)?;

    Ok(item.gratis_load_minor)
}

/// PoW gate for `mine_gratis`, delegating to the shared [`outbe_common::pow`]
/// scheme and mapping failures onto [`NodFactoryError`].
pub fn validate_pow(nod_id: U256, nonce: U256) -> Result<()> {
    pow::validate_pow(nod_id, nonce).map_err(|e| NodFactoryError::from(e).into())
}

/// Shared PoW hash over `ascii(hex(nod_id)) || nonce.to_be_bytes::<8>()`.
pub fn compute_pow_hash(nod_id: U256, nonce: U256) -> Result<[u8; 32]> {
    pow::compute_pow_hash(nod_id, nonce).map_err(|e| NodFactoryError::from(e).into())
}

fn emit_event<E: SolEvent>(storage: &StorageHandle<'_>, event: E) -> Result<()> {
    storage.emit_event(NOD_FACTORY_ADDRESS, event.encode_log_data())
}
