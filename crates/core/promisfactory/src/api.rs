//! Cross-module API for the Promisfactory module.

use alloy_primitives::{Address, U256};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

/// Mint `amount` promis to `account` and record the Fidelity acquisition cohort.
/// The `PromisMinted` event is emitted by the Promis token. See
/// [`crate::runtime::mint`].
pub fn mint(storage: StorageHandle<'_>, account: Address, amount: U256) -> Result<()> {
    crate::runtime::mint(storage, account, amount)
}
