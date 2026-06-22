//! Cross-module API for the Fidelity module.

use alloy_primitives::{Address, U256};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::schema::FidelityContract;

/// ACQUISITION hook: record a new active gratis cohort for `account` at block
/// time `timestamp` (seconds).
/// See [`FidelityContract::cohort_in`].
pub fn cohort_in(
    storage: StorageHandle<'_>,
    account: Address,
    amount: U256,
    timestamp: u64,
) -> Result<()> {
    FidelityContract::new(storage).cohort_in(account, amount, timestamp)
}

/// SALE hook: destroy `account`'s active cohorts LIFO at block time `timestamp`
/// (seconds), logging the sold slices.
pub fn cohort_out(
    storage: StorageHandle<'_>,
    account: Address,
    amount: U256,
    timestamp: u64,
) -> Result<()> {
    FidelityContract::new(storage).cohort_out(account, amount, timestamp)
}

/// RCFI for `account` at the current block time, in decayed days (0..L). See
/// [`FidelityContract::get_rcfi`].
pub fn get_rcfi(storage: StorageHandle<'_>, account: Address) -> Result<u64> {
    FidelityContract::new(storage).get_rcfi(account)
}

/// RCFI for `account` at an explicit block time `now` (seconds), in decayed
/// days (0..L). See [`FidelityContract::compute_rcfi`].
pub fn compute_rcfi(storage: StorageHandle<'_>, account: Address, timestamp: u64) -> Result<u64> {
    FidelityContract::new(storage).compute_rcfi(account, timestamp)
}

/// Fidelity league for `account` at the current block time, a tier in
/// `[MIN_LEAGUE, MAX_LEAGUE]`. See [`FidelityContract::league`].
pub fn league(storage: StorageHandle<'_>, account: Address) -> Result<u16> {
    FidelityContract::new(storage).league(account)
}
