//! Module-local protocol constants for the CCA program.
//!
//! Values trace to the Credis product paper §8 and §10. Those still marked TBD
//! there carry a placeholder here plus a `ponytail:` note naming what has to be
//! decided before launch.

use alloy_primitives::U256;

/// `m = 1`, the unpenalized multiplier. Also the ceiling: recovery can restore a
/// multiplier to 1 but never above it.
pub const MULTIPLIER_ONE: U256 = U256::from_limbs([1_000_000_000_000_000_000u64, 0, 0, 0]);

/// Below this multiplier a CCA cannot originate (§8.4 quarantine). Its existing
/// book can still settle, which is its only road back.
pub const ORIGINATION_FLOOR: U256 = U256::from_limbs([500_000_000_000_000_000u64, 0, 0, 0]);

/// The full penalty step: a completely unpaid void costs 10% of the multiplier.
/// A partial void costs this scaled by the unpaid share (§8.4).
pub const VOID_PENALTY_STEP: U256 = U256::from_limbs([100_000_000_000_000_000u64, 0, 0, 0]);

/// Multiplier restored per [`RECOVERY_VALUE_UNIT`] of settled principal.
// ponytail: §10 lists the recovery rate as TBD. 1% per unit means a CCA works a
// full void (10%) off with ten units of repayment; the pair of constants is the
// knob, and a governance parameter is the upgrade path.
pub const RECOVERY_STEP: U256 = U256::from_limbs([10_000_000_000_000_000u64, 0, 0, 0]);

/// Settled principal that earns one [`RECOVERY_STEP`], in stablecoin minor units
/// (6 decimals) — $1,000.
// ponytail: §10 TBD, paired with RECOVERY_STEP above.
pub const RECOVERY_VALUE_UNIT: U256 = U256::from_limbs([1_000_000_000u64, 0, 0, 0]);

/// Minimum principal for an origination to earn a reward unit, in stablecoin
/// minor units (6 decimals) — $100. The §8.3 dust guard.
// ponytail: §10 TBD. Too low and units are farmable with trivial positions; too
// high and legitimate small credits stop paying the agent that sourced them.
pub const MIN_PRINCIPAL_FOR_UNIT: U256 = U256::from_limbs([100_000_000u64, 0, 0, 0]);

/// Cap on the §8.3 repeat-owner halvings. The nth position a CCA opens for the
/// same owner earns `1 / 2^(n-1)` units; past this many halvings the unit stops
/// shrinking, which keeps it from decaying to zero and losing the distinction
/// between "heavily decayed" and "no origination at all".
pub const MAX_DECAY_HALVINGS: u32 = 20;
