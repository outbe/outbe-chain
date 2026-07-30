//! Cross-module API for the Fidelity module.
//!
//! Cohort mutations and league lookups route through the enclave (see
//! [`crate::enclave_client`]); callers keep their existing signatures. RCFI/league
//! are never computed on-chain.

use alloy_primitives::{Address, U256};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::schema::FidelityContract;

/// ACQUISITION hook: record a new active gratis cohort for `account` at block
/// time `timestamp` (seconds). See [`FidelityContract::cohort_in`].
pub fn cohort_in(
    storage: StorageHandle<'_>,
    account: Address,
    amount: U256,
    timestamp: u64,
) -> Result<()> {
    FidelityContract::new(storage).cohort_in(account, amount, timestamp)
}

/// SALE hook: destroy `account`'s active cohorts LIFO at block time `timestamp`
/// (seconds), logging the sold slices. See [`FidelityContract::cohort_out`].
pub fn cohort_out(
    storage: StorageHandle<'_>,
    account: Address,
    amount: U256,
    timestamp: u64,
) -> Result<()> {
    FidelityContract::new(storage).cohort_out(account, amount, timestamp)
}

/// Fidelity league for `account` at the current block time, a tier in
/// `[MIN_LEAGUE, MAX_LEAGUE]`.
pub fn league(storage: StorageHandle<'_>, account: Address) -> Result<u16> {
    FidelityContract::new(storage).league(account)
}

/// Fidelity league for `account` at an explicit block time `timestamp` (seconds).
/// See [`FidelityContract::league_at`].
pub fn league_at(storage: StorageHandle<'_>, account: Address, timestamp: u64) -> Result<u16> {
    FidelityContract::new(storage).league_at(account, timestamp)
}

/// Batch league snapshot for `owners` at `timestamp` (one enclave round-trip),
/// returned in `owners` order. Used by the OCOMP prepare phase to snapshot the
/// day's leagues. See [`FidelityContract::snapshot_leagues`].
pub fn snapshot_leagues(
    storage: StorageHandle<'_>,
    timestamp: u64,
    owners: &[Address],
) -> Result<Vec<(Address, u16)>> {
    FidelityContract::new(storage).snapshot_leagues(timestamp, owners)
}
