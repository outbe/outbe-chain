//! Closed-form daily emission cap.
//!
//! Computes `INITIAL_DAILY × exp(-K_SOFT × day_number)` clamped to
//! `FLOOR_DAILY` past `FLOOR_DAY_THRESHOLD`. Uses fixed-point Taylor
//! expansion (SCALE = 10^18, alternating pos/neg sums in unsigned
//! U256, saturating subtraction, 32 terms). The legacy per-block
//! formula and its precompile view RPC were removed alongside this
//! daily-only entry point.
//!
//! No floating point in production OR tests. All comparisons against
//! reference values are integer-pinned, computed offline once with the
//! same fixed-point algorithm and committed as literals. Any change to
//! `K_NUM`/`K_DEN`/`TAYLOR_TERMS`/`SCALE` requires recomputing and
//! re-pinning the test expectations.

use alloy_primitives::{uint, U256};

/// Fixed-point scale factor: 10^18 (matches token decimals).
const SCALE: U256 = uint!(1_000_000_000_000_000_000_U256);

/// Initial daily reward in base units: 2^30 tokens × 10^18 wei/token.
/// 2^30 = 1_073_741_824. Product = 1_073_741_824 × 10^18.
pub const INITIAL_DAILY: U256 = uint!(1_073_741_824_000_000_000_000_000_000_U256);

/// Floor daily reward in base units: 2^26 tokens × 10^18 wei/token.
/// 2^26 = 67_108_864. Product = 67_108_864 × 10^18.
pub const FLOOR_DAILY: U256 = uint!(67_108_864_000_000_000_000_000_000_U256);

/// Decay coefficient k_soft = ln(2^4) / 2920 ≈ 9.4952e-4 per day.
/// Encoded as integer ratio K_NUM / K_DEN. Chosen so that
/// `K_NUM × max_day × SCALE / K_DEN` fits comfortably in U256.
const K_NUM: U256 = uint!(9_4952_U256);
const K_DEN: U256 = uint!(100_000_000_U256);

/// Past this many days since genesis, the formula floor-clamps.
pub const FLOOR_DAY_THRESHOLD: u32 = 2920;

/// Number of Taylor terms; matches `crate::emission` for shape/precision parity.
const TAYLOR_TERMS: usize = 32;

/// Returns the daily emission cap for `day_number` days since the chain's
/// genesis UTC day. Closed-form, no storage I/O, pure function.
/// Monotonically non-increasing in `day_number`; clamped to `FLOOR_DAILY`
/// for `day_number >= FLOOR_DAY_THRESHOLD`.
///
/// Boundary: `INITIAL_DAILY × exp_fp / SCALE` cannot overflow U256 —
/// `INITIAL_DAILY ≈ 1.07e27`, `exp_fp ≤ SCALE = 10^18`, product ≤ 1.07e45,
/// well below `2^256 ≈ 1.16e77`.
pub fn day_emission_limit(day_number: u32) -> U256 {
    if day_number >= FLOOR_DAY_THRESHOLD {
        return FLOOR_DAILY;
    }

    // x_fp = (K_NUM × day) / K_DEN, scaled into fixed-point by × SCALE.
    let n = U256::from(day_number);
    let x_fp = K_NUM * n * SCALE / K_DEN;

    // Taylor series for exp(-x): 1 - x + x^2/2! - x^3/3! + ...
    // Track positive and negative sums separately (U256 is unsigned).
    let mut pos_sum = SCALE; // term 0 = 1.0
    let mut neg_sum = U256::ZERO;
    let mut term = SCALE;
    for k in 1..TAYLOR_TERMS {
        term = term * x_fp / (U256::from(k as u64) * SCALE);
        if term.is_zero() {
            break;
        }
        if k % 2 == 1 {
            neg_sum += term;
        } else {
            pos_sum += term;
        }
    }
    let exp_fp = pos_sum.saturating_sub(neg_sum);

    let reward = INITIAL_DAILY * exp_fp / SCALE;
    if reward < FLOOR_DAILY {
        FLOOR_DAILY
    } else {
        reward
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pinned sample values, computed once by the implementation itself
    /// (run `cargo test -p outbe-emissionlimit daily_emission::tests::print_pins -- --ignored --nocapture`
    /// to regenerate after any K_NUM/K_DEN/TAYLOR_TERMS/SCALE change).
    /// Asserted byte-equal to detect any regression in the fixed-point
    /// math, including incidental rounding shifts.
    const PIN_DAY_0: U256 = INITIAL_DAILY;
    const PIN_DAY_365: U256 = uint!(759_249_206_514_486_004_915_634_176_U256);
    const PIN_DAY_730: U256 = uint!(536_869_613_074_582_646_657_384_448_U256);
    const PIN_DAY_1460: U256 = uint!(268_434_157_076_153_980_843_720_704_U256);
    const PIN_DAY_2190: U256 = uint!(134_216_753_808_293_984_930_365_440_U256);
    const PIN_DAY_2919: U256 = uint!(67_171_965_393_083_432_817_393_664_U256);

    #[test]
    fn day_zero_equals_initial_daily_exactly() {
        assert_eq!(day_emission_limit(0), INITIAL_DAILY);
    }

    #[test]
    fn floor_clamp_at_and_beyond_threshold() {
        assert_eq!(day_emission_limit(FLOOR_DAY_THRESHOLD), FLOOR_DAILY);
        assert_eq!(day_emission_limit(FLOOR_DAY_THRESHOLD + 1), FLOOR_DAILY);
        assert_eq!(day_emission_limit(u32::MAX), FLOOR_DAILY);
    }

    #[test]
    fn last_unclamped_day_is_close_to_floor() {
        // 2919 is the last day before the threshold clamp kicks in.
        let v = day_emission_limit(FLOOR_DAY_THRESHOLD - 1);
        // Sanity: greater than or equal to floor (the clamp would
        // otherwise have returned FLOOR_DAILY) and at most ~10% above.
        assert!(v >= FLOOR_DAILY);
        let upper_bound = FLOOR_DAILY * U256::from(110u64) / U256::from(100u64);
        assert!(v <= upper_bound, "v={v} > 110% of floor={upper_bound}");
    }

    #[test]
    fn monotonic_non_increasing_first_year() {
        // Sweep day 0..=365 inclusive; reward must never increase.
        let mut prev = day_emission_limit(0);
        for d in 1..=365u32 {
            let cur = day_emission_limit(d);
            assert!(
                cur <= prev,
                "non-monotonic at d={d}: prev={prev}, cur={cur}"
            );
            prev = cur;
        }
    }

    #[test]
    fn monotonic_non_increasing_at_threshold_boundary() {
        // Around the floor clamp, monotonicity must still hold across
        // the unclamped → clamped transition.
        for d in (FLOOR_DAY_THRESHOLD - 5)..=(FLOOR_DAY_THRESHOLD + 5) {
            let prev = day_emission_limit(d);
            let next = day_emission_limit(d + 1);
            assert!(next <= prev, "non-monotonic at d={d}");
        }
    }

    #[test]
    fn pinned_sample_values_match_byte_equal() {
        assert_eq!(day_emission_limit(0), PIN_DAY_0);
        assert_eq!(day_emission_limit(365), PIN_DAY_365);
        assert_eq!(day_emission_limit(730), PIN_DAY_730);
        assert_eq!(day_emission_limit(1460), PIN_DAY_1460);
        assert_eq!(day_emission_limit(2190), PIN_DAY_2190);
        assert_eq!(day_emission_limit(2919), PIN_DAY_2919);
    }

    /// Helper: prints the sample values so the `PIN_DAY_*` constants
    /// above can be regenerated after a parameter change. Ignored by
    /// default; run via `cargo test ... -- --include-ignored --nocapture
    /// print_pins` to capture output.
    #[test]
    #[ignore]
    fn print_pins() {
        for d in [0u32, 365, 730, 1460, 2190, 2919] {
            eprintln!("PIN_DAY_{d} = {}", day_emission_limit(d));
        }
    }
}
