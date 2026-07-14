//! Cross-module API for the Gratisfactory module.

use alloy_primitives::{Address, U256};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

/// Mint `amount` gratis to `account` and record the Fidelity acquisition cohort.
/// The `GratisMinted` event is emitted by the Gratis token. See
/// [`crate::runtime::mint`].
pub fn mint(storage: StorageHandle<'_>, account: Address, amount: U256) -> Result<()> {
    crate::runtime::mint(storage, account, amount)
}

/// Mint `amount` gratis to `account` without recording a Fidelity acquisition
/// cohort. The `GratisMinted` event is emitted by the Gratis token. See
/// [`crate::runtime::mint_from_promis`].
pub fn mint_from_promis(storage: StorageHandle<'_>, account: Address, amount: U256) -> Result<()> {
    crate::runtime::mint_from_promis(storage, account, amount)
}
