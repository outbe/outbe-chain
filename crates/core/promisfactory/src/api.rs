//! Cross-module API for the Promisfactory module.

use alloy_primitives::{Address, U256};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

/// Mint `amount` promis to `account`, record the Fidelity acquisition cohort,
/// and emit `PromisMined`. See [`crate::runtime::mine`].
pub fn mine(storage: StorageHandle<'_>, account: Address, amount: U256) -> Result<()> {
    crate::runtime::mine(storage, account, amount)
}
