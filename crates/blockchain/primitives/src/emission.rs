//! Closed-form two-phase day emission cap and Metadosis base share.
//!
//! The curve rises from 2^28 COEN to `P` through day 1024, then falls to
//! 2^26 COEN on day 3072. From that day onward it returns the permanent
//! `FLOOR_DAY_EMISSION`.

use crate::units::SCALE_1E6_U256;
use alloy_primitives::{uint, U256};
use std::sync::LazyLock;

/// Initial day emission: 2^28 COEN expressed in six-decimal `unit`.
pub const INITIAL_DAY_EMISSION: U256 = uint!(268_435_456_000_000_U256);

/// Floor day emission: 2^26 COEN expressed in six-decimal `unit`.
pub const FLOOR_DAY_EMISSION: U256 = uint!(67_108_864_000_000_U256);

/// The founder curve reaches 2^26 COEN on this day and stays there forever.
pub const FLOOR_DAY_THRESHOLD: u32 = 3_072;

const MATH_SCALE: U256 = uint!(1_000_000_000_000_000_000_000_000_000_000_U256);
const EXP_TAYLOR_TERMS: u32 = 64;
const K1: u32 = 128;
const K2: u32 = K1 * 3;
const PHASE_ONE_MIDPOINT_DAY: u32 = 512;
const PHASE_SPLIT_DAY: u32 = 1_024;
const PHASE_TWO_MIDPOINT_DAY: u32 = 2_048;

static TWO_26: LazyLock<U256> =
    LazyLock::new(|| U256::from(1u64 << 26) * SCALE_1E6_U256 * MATH_SCALE);
static TWO_28: LazyLock<U256> =
    LazyLock::new(|| U256::from(1u64 << 28) * SCALE_1E6_U256 * MATH_SCALE);
static P: LazyLock<U256> = LazyLock::new(|| U256::from(26) * *TWO_26 / U256::from(3));
static A: LazyLock<U256> = LazyLock::new(|| {
    fixed_div(
        *P - *TWO_28,
        tanh_positive_ratio(PHASE_ONE_MIDPOINT_DAY, 2 * K1),
    )
});
static O1: LazyLock<U256> = LazyLock::new(|| (*TWO_28 + *P) / U256::from(2) - *A / U256::from(2));
static D: LazyLock<U256> =
    LazyLock::new(|| fixed_div(*P - *TWO_26, tanh_positive_ratio(PHASE_SPLIT_DAY, 2 * K2)));
static O2: LazyLock<U256> = LazyLock::new(|| (*P + *TWO_26) / U256::from(2) + *D / U256::from(2));

fn exp_positive_ratio(numerator: u32, denominator: u32) -> U256 {
    let x = U256::from(numerator) * MATH_SCALE / U256::from(denominator);
    let mut sum = MATH_SCALE;
    let mut term = MATH_SCALE;

    for index in 1..=EXP_TAYLOR_TERMS {
        term = term * x / (MATH_SCALE * U256::from(index));
        if term.is_zero() {
            break;
        }
        sum += term;
    }

    sum
}

fn exp_signed_ratio(numerator: i64, denominator: u32) -> U256 {
    if numerator >= 0 {
        exp_positive_ratio(numerator as u32, denominator)
    } else {
        MATH_SCALE * MATH_SCALE / exp_positive_ratio(numerator.unsigned_abs() as u32, denominator)
    }
}

fn tanh_positive_ratio(numerator: u32, denominator: u32) -> U256 {
    let exp_twice_x = exp_positive_ratio(numerator * 2, denominator);
    (exp_twice_x - MATH_SCALE) * MATH_SCALE / (exp_twice_x + MATH_SCALE)
}

fn fixed_div(value: U256, divisor: U256) -> U256 {
    value * MATH_SCALE / divisor
}

/// Returns the day emission cap for `day_number` days since the chain's
/// genesis UTC day. It is a closed-form pure function with no storage I/O.
pub fn day_emission_limit(day_number: u32) -> U256 {
    if day_number >= FLOOR_DAY_THRESHOLD {
        return FLOOR_DAY_EMISSION;
    }
    if day_number == 0 {
        return INITIAL_DAY_EMISSION;
    }

    let a = *A;
    let o1 = *O1;
    let d = *D;
    let o2 = *O2;

    let emission = if day_number <= PHASE_SPLIT_DAY {
        let exponent = exp_signed_ratio(
            i64::from(PHASE_ONE_MIDPOINT_DAY) - i64::from(day_number),
            K1,
        );
        o1 + a * MATH_SCALE / (MATH_SCALE + exponent)
    } else {
        let exponent = exp_signed_ratio(
            i64::from(PHASE_TWO_MIDPOINT_DAY) - i64::from(day_number),
            K2,
        );
        o2 - d * MATH_SCALE / (MATH_SCALE + exponent)
    };
    let reward = emission / (MATH_SCALE * SCALE_1E6_U256) * SCALE_1E6_U256;
    if reward < FLOOR_DAY_EMISSION {
        FLOOR_DAY_EMISSION
    } else {
        reward
    }
}

/// Returns the terminal Metadosis share (84 % after four 4 % fixed pools)
/// of `day_emission_limit(day_number)`.
pub fn metadosis_day_emission_limit(day_number: u32) -> U256 {
    let total = day_emission_limit(day_number);
    let fixed_pool = total * U256::from(4u64) / U256::from(100u64);
    total.saturating_sub(fixed_pool * U256::from(4u64))
}
