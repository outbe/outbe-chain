//! CCA bonding parameters.
use alloy_primitives::{uint, U256};
use outbe_primitives::{time::SECONDS_PER_DAY, units::ONE_COEN};

/// One billion COENs.
pub const BOND_REQUIREMENT: U256 = ONE_COEN.strict_mul(uint!(1_000_000_000_U256));

pub const UNBOND_COOLDOWN_SECONDS: u64 = 128 * SECONDS_PER_DAY;

/// Cleanup has its own budget, separate from reward accrual.
pub const DAY_CLEANUP_ENTRIES_PER_BLOCK: u32 = 256;
pub const DAY_CLEANUP_HEADERS_PER_BLOCK: u32 = 4;
