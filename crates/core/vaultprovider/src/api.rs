//! In-process API for the vaultprovider precompile.
//!
//! These wrappers let other precompiles drive reserve liquidity **without an
//! ABI sub-call** — same-runtime-context calls that forward directly to
//! [`crate::runtime`]. The Solidity ABI dispatch in [`crate::precompile`] and
//! this module are two thin front doors over the single source of truth in
//! `runtime`; this module adds no logic (mirrors `outbe_gratispool::api`).
//!
//! Callers MUST pass their own precompile address as `caller`: the liquidity
//! source/target authorization gates key off it exactly as they would off the
//! sub-call `msg.sender` on the Solidity path. See `outbe_primitives::addresses`
//! for the factory address constants (e.g. `CREDIS_FACTORY_ADDRESS`).

use alloy_primitives::{Address, U256};

use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::runtime;

/// `depositLiquidity` (source-gated). See [`runtime::deposit_liquidity`].
///
/// `caller` must be a registered liquidity source. Pulls `amount` of `asset`
/// from `caller` and deposits it into the asset's vault, returning the minted
/// shares.
pub fn deposit_liquidity(
    storage: StorageHandle<'_>,
    caller: Address,
    asset: Address,
    amount: U256,
) -> Result<U256> {
    runtime::deposit_liquidity(storage, caller, asset, amount)
}

/// `withdrawLiquidity` (target-gated). See [`runtime::withdraw_liquidity`].
///
/// `caller` must be a registered liquidity target. Redeems `amount` of `asset`
/// from the vault and tops it up into `receiver`, returning the burned shares.
pub fn withdraw_liquidity(
    storage: StorageHandle<'_>,
    caller: Address,
    asset: Address,
    amount: U256,
    receiver: Address,
) -> Result<U256> {
    runtime::withdraw_liquidity(storage, caller, asset, amount, receiver)
}

/// `assetAt`: the registered reserve asset at `index`. See [`runtime::asset_at`].
pub fn asset_at(storage: StorageHandle<'_>, index: U256) -> Result<Address> {
    runtime::asset_at(storage, index)
}
