//! Cross-module API for the Gratisfactory module.

use alloy_primitives::{Address, B256, U256};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::schema::GratisFactoryContract;

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

/// Unix timestamp the pledge quote behind `pledge_handle` was struck at, or 0
/// when no live quote is recorded — an unknown handle, or one already spent.
///
/// The quote fixes the entry price, so the module that spends a ticket
/// (`credisfactory`) decides how stale a quote it will accept; the TTL is a
/// Credis product parameter and does not belong here.
pub fn pledge_quoted_at(storage: StorageHandle<'_>, pledge_handle: B256) -> Result<u64> {
    GratisFactoryContract::new(storage)
        .pledge_quoted_at
        .read(&pledge_handle)
}

/// Drops the recorded quote timestamp once its ticket has been spent, so the
/// map does not accumulate entries for handles that can never be presented
/// again. Idempotent.
pub fn clear_pledge_quote(storage: StorageHandle<'_>, pledge_handle: B256) -> Result<()> {
    GratisFactoryContract::new(storage)
        .pledge_quoted_at
        .clear(&pledge_handle)
}
