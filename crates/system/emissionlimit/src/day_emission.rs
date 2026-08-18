//! Closed-form day emission cap.
//!
//! Computes `INITIAL_DAY_EMISSION × exp(-K_SOFT × day_number)` clamped to
//! `FLOOR_DAY_EMISSION` past `FLOOR_DAY_THRESHOLD`. Uses fixed-point Taylor
//! expansion (SCALE = 10^18, alternating pos/neg sums in unsigned
//! U256, saturating subtraction, 32 terms). The legacy per-block
//! formula and its precompile view RPC were removed alongside this
//! day-only entry point.
//!
//! No floating point in production OR tests. All comparisons against
//! reference values are integer-pinned, computed offline once with the
//! same fixed-point algorithm and committed as literals. Any change to
//! `K_NUM`/`K_DEN`/`TAYLOR_TERMS`/`SCALE` requires recomputing and
//! re-pinning the test expectations.

use alloy_primitives::{uint, U256};

/// Fixed-point scale factor: 10^18 (matches token decimals).
const SCALE: U256 = uint!(1_000_000_000_000_000_000_U256);

/// Initial day emission in base units: 2^30 tokens × 10^18 wei/token.
/// 2^30 = 1_073_741_824. Product = 1_073_741_824 × 10^18.
pub const INITIAL_DAY_EMISSION: U256 = uint!(1_073_741_824_000_000_000_000_000_000_U256);

/// Floor day emission in base units: 2^26 tokens × 10^18 wei/token.
/// 2^26 = 67_108_864. Product = 67_108_864 × 10^18.
pub const FLOOR_DAY_EMISSION: U256 = uint!(67_108_864_000_000_000_000_000_000_U256);

/// Decay coefficient k_soft = ln(2^4) / 2920 ≈ 9.4952e-4 per day.
/// Encoded as integer ratio K_NUM / K_DEN. Chosen so that
/// `K_NUM × max_day × SCALE / K_DEN` fits comfortably in U256.
const K_NUM: U256 = uint!(9_4952_U256);
const K_DEN: U256 = uint!(100_000_000_U256);

/// Past this many days since genesis, the formula floor-clamps.
pub const FLOOR_DAY_THRESHOLD: u32 = 2920;

/// Number of Taylor terms; matches `crate::emission` for shape/precision parity.
const TAYLOR_TERMS: usize = 32;

/// Returns the day emission cap for `day_number` days since the chain's
/// genesis UTC day. Closed-form, no storage I/O, pure function.
/// Monotonically non-increasing in `day_number`; clamped to `FLOOR_DAY_EMISSION`
/// for `day_number >= FLOOR_DAY_THRESHOLD`.
///
/// Boundary: `INITIAL_DAY_EMISSION × exp_fp / SCALE` cannot overflow U256 —
/// `INITIAL_DAY_EMISSION ≈ 1.07e27`, `exp_fp ≤ SCALE = 10^18`, product ≤ 1.07e45,
/// well below `2^256 ≈ 1.16e77`.
pub fn day_emission_limit(day_number: u32) -> U256 {
    if day_number >= FLOOR_DAY_THRESHOLD {
        return FLOOR_DAY_EMISSION;
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

    let reward = INITIAL_DAY_EMISSION * exp_fp / SCALE;
    if reward < FLOOR_DAY_EMISSION {
        FLOOR_DAY_EMISSION
    } else {
        reward
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REFERENCE_VECTORS: &str =
        include_str!("../../../../testing/emission-reference/vectors.json");
    const PIN_DAY_0: U256 = uint!(1_073_741_824_000_000_U256);
    const PIN_DAY_1: U256 = uint!(1_072_722_768_546_607_U256);
    const PIN_DAY_365: U256 = uint!(759_249_206_514_485_U256);
    const PIN_DAY_730: U256 = uint!(536_869_613_074_582_U256);
    const PIN_DAY_1460: U256 = uint!(268_434_157_076_154_U256);
    const PIN_DAY_2190: U256 = uint!(134_216_753_808_294_U256);
    const PIN_DAY_2919: U256 = uint!(67_171_965_393_083_U256);
    const PIN_DAY_2920: U256 = uint!(67_108_864_000_000_U256);

    fn reference_days() -> Vec<(u32, U256)> {
        let value: serde_json::Value =
            serde_json::from_str(REFERENCE_VECTORS).expect("independent emission vectors parse");
        value["days"]
            .as_array()
            .expect("days is an array")
            .iter()
            .map(|row| {
                let day =
                    u32::try_from(row["day"].as_u64().expect("day is u64")).expect("day fits u32");
                let emission = U256::from_str_radix(
                    row["emission_units"]
                        .as_str()
                        .expect("emission_units is a decimal string"),
                    10,
                )
                .expect("emission_units fits U256");
                (day, emission)
            })
            .collect()
    }

    #[test]
    fn pinned_days_match_independent_amount_domain_reference() {
        for (day, expected) in [
            (0, PIN_DAY_0),
            (1, PIN_DAY_1),
            (365, PIN_DAY_365),
            (730, PIN_DAY_730),
            (1460, PIN_DAY_1460),
            (2190, PIN_DAY_2190),
            (2919, PIN_DAY_2919),
            (2920, PIN_DAY_2920),
        ] {
            assert_eq!(day_emission_limit(day), expected, "day {day}");
        }
    }

    #[test]
    fn floor_clamp_at_and_beyond_threshold() {
        assert_eq!(day_emission_limit(FLOOR_DAY_THRESHOLD), PIN_DAY_2920);
        assert_eq!(day_emission_limit(FLOOR_DAY_THRESHOLD + 1), PIN_DAY_2920);
        assert_eq!(day_emission_limit(u32::MAX), PIN_DAY_2920);
    }

    #[test]
    fn independent_reference_is_contiguous_monotonic_and_floor_clamped() {
        let days = reference_days();
        assert_eq!(days.len(), FLOOR_DAY_THRESHOLD as usize + 1);
        for (index, (day, value)) in days.iter().copied().enumerate() {
            assert_eq!(day as usize, index, "reference day sequence");
            if let Some((_, previous)) = index.checked_sub(1).map(|i| days[i]) {
                assert!(value <= previous, "reference increases at day {day}");
            }
        }
        assert_eq!(
            days.last().copied(),
            Some((FLOOR_DAY_THRESHOLD, PIN_DAY_2920))
        );
    }

    #[test]
    fn production_matches_independent_reference_for_full_range() {
        for (day, expected) in reference_days() {
            assert_eq!(day_emission_limit(day), expected, "day {day}");
        }
    }

    #[test]
    fn production_is_monotonic_for_the_full_emission_range() {
        let mut previous = day_emission_limit(0);
        for day in 1..=FLOOR_DAY_THRESHOLD {
            let current = day_emission_limit(day);
            assert!(current <= previous, "production increases at day {day}");
            previous = current;
        }
    }
}
