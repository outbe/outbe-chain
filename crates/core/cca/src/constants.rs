//! CCA bonding parameters.
use alloy_primitives::{uint, U256};

/// One billion whole COEN, in 18-decimal native atomic units.
pub const BOND_REQUIREMENT: U256 = uint!(1_000_000_000_000_000_000_000_000_000_U256);
pub const UNBOND_COOLDOWN_SECONDS: u64 = 128 * 86_400;
