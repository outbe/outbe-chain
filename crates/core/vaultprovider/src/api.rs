//! In-process API for the vaultprovider precompile.

use alloy_primitives::{Address, U256};

use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::runtime;

/// Re-exported liquidity classifiers so cross-module callers can name the
/// deposit/withdraw discriminant without reaching into [`crate::precompile`].
pub use crate::precompile::IVaultProvider::{LiquiditySource, LiquidityTarget};

/// `depositLiquidity`. See [`runtime::deposit_liquidity`].
///
/// Pulls `amount` of `asset` from `caller` and deposits it into the asset's
/// vault, returning the minted shares.
pub fn deposit_liquidity(
    storage: StorageHandle<'_>,
    caller: Address,
    asset: Address,
    amount: U256,
    source: LiquiditySource,
) -> Result<U256> {
    runtime::deposit_liquidity(storage, caller, asset, amount, source)
}

/// `withdrawLiquidity`. See [`runtime::withdraw_liquidity`].
///
/// Redeems `amount` of `asset` from the vault and tops it up into `receiver`,
/// returning the burned shares. `target` classifies `caller` for event tracking
/// and must not be [`LiquidityTarget::Unknown`].
pub fn withdraw_liquidity(
    storage: StorageHandle<'_>,
    caller: Address,
    asset: Address,
    amount: U256,
    receiver: Address,
    target: LiquidityTarget,
) -> Result<U256> {
    runtime::withdraw_liquidity(storage, caller, asset, amount, receiver, target)
}

/// `assetAt`: the registered reserve asset at `index`. See [`runtime::asset_at`].
pub fn asset_at(storage: StorageHandle<'_>, index: U256) -> Result<Address> {
    runtime::asset_at(storage, index)
}
