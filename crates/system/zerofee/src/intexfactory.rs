//! Sizing caps for a contributor batch.
//!
//! The zero-fee hook that waived `payContributorBatch` is gone. Callers that
//! still bound a batch keep these caps.

use alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE;

/// Exact ABI envelope cap for a full batch: selector, four head words, then a
/// 256-leaf array of four-word structs and a 24-hash proof.
pub const MAX_ZERO_FEE_CONTRIBUTOR_BATCH_CALLDATA_BYTES: usize =
    4 + 4 * 32 + (32 + 256 * 4 * 32) + (32 + 24 * 32);

/// Only explicit storage ops are metered inside precompiles (reads 100,
/// writes 5000). Transfers and events are journal ops outside metering, so a
/// full batch executes in tens of thousands of gas.
pub const MAX_ZERO_FEE_CONTRIBUTOR_BATCH_GAS_LIMIT: u64 =
    21_000 + 16 * MAX_ZERO_FEE_CONTRIBUTOR_BATCH_CALLDATA_BYTES as u64 + 500_000;

/// Public txpool compatibility floor previously required by the contributor-batch waiver.
pub const MIN_ZERO_FEE_CONTRIBUTOR_BATCH_MAX_FEE_PER_GAS: u128 = MIN_PROTOCOL_BASE_FEE as u128;
