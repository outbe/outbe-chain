//! Cross-module API for the Gratisfactory module.

use alloy_primitives::{Address, U256};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

/// Re-exported so cross-module callers (e.g. `outbe_nodfactory`) can build the
/// caller's Gratis write authorization without depending on `outbe_gratis`.
pub use outbe_gratis::api::ModifyAuth;

/// Mint `amount` gratis to `account` (authorized by the account owner's modify
/// key), record the Fidelity acquisition cohort, and emit `GratisMined`.
/// See [`crate::runtime::mint`].
pub fn mint(
    storage: StorageHandle<'_>,
    account: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<()> {
    crate::runtime::mint(storage, account, amount, auth)
}

/// Mint `amount` gratis to `account` without recording a Fidelity acquisition
/// cohort (authorized by the account owner's modify key). The `GratisMinted`
/// event is emitted by the Gratis token. See [`crate::runtime::mint_from_promis`].
pub fn mint_from_promis(
    storage: StorageHandle<'_>,
    account: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<()> {
    crate::runtime::mint_from_promis(storage, account, amount, auth)
}
