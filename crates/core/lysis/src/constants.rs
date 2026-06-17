use crate::algorithm::{u256_from_u128, SCALE_U128};
use alloy_primitives::U256;

pub const FLOOR_RATE_PERCENT: u64 = 8;

pub fn calc_floor_price(price: U256) -> U256 {
    price * U256::from(100 + FLOOR_RATE_PERCENT) / U256::from(100u64)
}

// corresponds to the symbolic rate
const F_FP_DEFAULT_U128: u128 = SCALE_U128 * 32 / 100;
pub const F_FP_DEFAULT: U256 = u256_from_u128(F_FP_DEFAULT_U128);
pub const F_MAX_FP: U256 = u256_from_u128(F_FP_DEFAULT_U128 * 2);
