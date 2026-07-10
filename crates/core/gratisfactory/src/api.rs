//! Cross-module API for the Gratisfactory module.

use alloy_primitives::{Address, U256};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

/// Re-exported so cross-module callers (e.g. `outbe_nodfactory`) can build the
/// caller's Gratis write authorization without depending on `outbe_gratis`.
pub use outbe_gratis::api::ModifyAuth;

/// Mint `amount` gratis to `account` (authorized by the account owner's modify
/// key), record the Fidelity acquisition cohort, and emit `GratisMined`.
/// See [`crate::runtime::mine`].
pub fn mine(
    storage: StorageHandle<'_>,
    account: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<()> {
    crate::runtime::mine(storage, account, amount, auth)
}
