//! Cross-module API for the Promisfactory module.

use alloy_primitives::{Address, U256};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

pub use outbe_promis::api::ModifyAuth;

/// Mint `amount` promis to `account` from a settled Gem position (`gem_id`),
/// authorized by the account owner's Promis modify key.
pub fn mint_from_gem(
    storage: StorageHandle<'_>,
    gem_id: U256,
    account: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<()> {
    crate::runtime::mint_from_gem(storage, gem_id, account, amount, auth)
}

/// Mint `amount` promis to `account` from a settled Intex position (`intex_id`),
/// authorized by the account owner's Promis modify key.
pub fn mint_from_intex(
    storage: StorageHandle<'_>,
    intex_id: U256,
    account: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<()> {
    crate::runtime::mint_from_intex(storage, intex_id, account, amount, auth)
}
