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

/// Fidelity league for `account` at the current block time, a tier in
/// `[MIN_LEAGUE, MAX_LEAGUE]`. See [`FidelityContract::league`].
pub fn league(storage: StorageHandle<'_>, account: Address) -> Result<u16> {
    FidelityContract::new(storage).league(account)
}
