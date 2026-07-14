//! Cross-module API for the Gratisfactory module.

use alloy_primitives::{Address, U256};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

/// Mint `amount` gratis to `account`, record the Fidelity acquisition cohort,
/// and emit `GratisMined`. See [`crate::runtime::mine`].
pub fn mine(storage: StorageHandle<'_>, account: Address, amount: U256) -> Result<()> {
    crate::runtime::mine(storage, account, amount)
}

/// Mint `amount` gratis to `account` and emit `GratisMined`, without recording a
/// Fidelity acquisition cohort. See [`crate::runtime::mine_from_promis`].
pub fn mine_from_promis(storage: StorageHandle<'_>, account: Address, amount: U256) -> Result<()> {
    crate::runtime::mine_from_promis(storage, account, amount)
}
