//! CCA bonding parameters.
use alloy_primitives::{uint, U256};
use outbe_primitives::{time::SECONDS_PER_DAY, units::ONE_COEN};

/// One billion COENs.
pub const BOND_REQUIREMENT: U256 = ONE_COEN.strict_mul(uint!(1_000_000_000_U256));

pub const UNBOND_COOLDOWN_SECONDS: u64 = 128 * SECONDS_PER_DAY;

/// Active CCA population scanned once when a UTC day settles.
///
/// A full positive-weight distribution of this set stays within 5_000_000 gas
/// under production storage metering, inside the 30_000_000 steady block gas limit.
pub const MAX_ACTIVE_CCAS: u32 = 128;
