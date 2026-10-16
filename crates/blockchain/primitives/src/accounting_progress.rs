//! V2 Phase 1 accounting-progress reader trait.
//!
//! Cycle needs to observe one value. This value is the highest block number
//! whose Phase 1 (`CertifiedParentAccounting`) system tx has successfully
//! committed progress to the `ACCOUNTING_PROGRESS_ADDRESS` storage slot. This
//! trait is the read-only surface that Cycle consumes. The writer is
//! `outbe_cycle`'s Phase 1 runtime helper, and only the V2 executor path invokes it.
//!
//! The trait lives in `outbe-primitives` so callers can abstract over
//! the storage reader in tests.

use crate::error::Result;

/// Read-only view of the V2 Phase 1 accounting-progress slot.
///
/// Implementations resolve the EVM storage backing
/// `ACCOUNTING_PROGRESS_ADDRESS` slot 0 and return the persisted
/// `last_accounted_block_number`. A fresh chain (no Phase 1 commit yet)
/// must return `0`.
pub trait AccountingProgressView {
    /// Returns the highest block number whose Phase 1 system tx has
    /// recorded progress. Returns `0` if no Phase 1 has committed yet
    /// (genesis / first-block fresh chain).
    fn last_accounted_block_number(&self) -> Result<u64>;
}
