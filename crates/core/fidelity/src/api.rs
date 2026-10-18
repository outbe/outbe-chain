//! Cross-module API for the Fidelity module.
//!
//! Cohort mutations and league lookups route through the enclave (see
//! [`crate::enclave_client`]). Callers keep their existing signatures. RCFI/league
//! are never computed on-chain.

use alloy_primitives::{Address, U256};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

pub use outbe_tee::protocol::{FidelityCohortOp, FidelityOpOutcome, FidelityOpSection};

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
/// returned in `owners` order. Metadosis preparation uses it to snapshot the
/// day's leagues. See [`FidelityContract::snapshot_leagues`].
pub fn snapshot_leagues(
    storage: StorageHandle<'_>,
    timestamp: u64,
    owners: &[Address],
) -> Result<Vec<(Address, u16)>> {
    FidelityContract::new(storage).snapshot_leagues(timestamp, owners)
}

// --- Folded-op helpers (co-located cohort ops inside a gratis round-trip) ---

/// Build the fidelity cohort section to fold into a co-located gratis op
/// (reads `account`'s current cohort blob + the global anchor from storage).
/// `In` for an acquisition, `Out` for a sale, `Probe` for a read-only league.
pub fn cohort_section(
    storage: StorageHandle<'_>,
    account: Address,
    op: FidelityCohortOp,
    timestamp: u64,
) -> Result<FidelityOpSection> {
    FidelityContract::new(storage).cohort_section(account, op, timestamp)
}

/// Persist the fidelity outcome returned by a folded gratis op (writes the new
/// cohort ciphertext and, on first acquisition, the global anchor). A probe
/// outcome is a no-op.
pub fn apply_fidelity_outcome(
    storage: StorageHandle<'_>,
    account: Address,
    outcome: &FidelityOpOutcome,
) -> Result<()> {
    FidelityContract::new(storage).apply_outcome(account, outcome)
}

/// Returns the fixed `max_fidelity` for `wwd` (`WWD - 1` ceiling).
pub fn max_fidelity_for_wwd(
    storage: StorageHandle<'_>,
    wwd: outbe_primitives::time::WorldwideDay,
) -> Result<U256> {
    FidelityContract::new(storage).max_fidelity_for_wwd(wwd)
}

/// Locks and returns the fixed `max_fidelity` for `wwd`.
pub fn ensure_wwd_max_fidelity(
    storage: StorageHandle<'_>,
    wwd: outbe_primitives::time::WorldwideDay,
) -> Result<U256> {
    FidelityContract::new(storage).ensure_wwd_max_fidelity(wwd)
}

/// Computes `league` (`1..=4096`) for `user_rcfi` against the fixed `max_fidelity`
/// of `wwd`, and records `user_rcfi` into `wwd_running_max_fidelity[wwd]`.
pub fn league_from_rcfi_for_wwd(
    storage: StorageHandle<'_>,
    wwd: outbe_primitives::time::WorldwideDay,
    user_rcfi: U256,
) -> Result<u16> {
    FidelityContract::new(storage).league_from_rcfi_for_wwd(wwd, user_rcfi)
}

/// Evaluates `league` (`1..=4096`) for `account` on `wwd` at `timestamp`
/// (user fidelity by UTC day, max fidelity fixed per WWD).
pub fn league_for_wwd(
    storage: StorageHandle<'_>,
    account: Address,
    wwd: outbe_primitives::time::WorldwideDay,
    timestamp: u64,
) -> Result<u16> {
    FidelityContract::new(storage).league_for_wwd(account, wwd, timestamp)
}

/// Finalizes `wwd` at Metadosis time by rolling `wwd_running_max_fidelity[wwd]`
/// into `max_fidelity_prev` for the next WWD.
pub fn finalize_wwd_max_fidelity(
    storage: StorageHandle<'_>,
    wwd: outbe_primitives::time::WorldwideDay,
    timestamp: u64,
) -> Result<U256> {
    FidelityContract::new(storage).finalize_wwd_max_fidelity(wwd, timestamp)
}
