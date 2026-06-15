use crate::algorithm::SCALE;
use alloy_primitives::U256;

pub const FLOOR_RATE_PERCENT: u64 = 8;

pub fn calc_floor_price(price: U256) -> U256 {
    price * U256::from(100 + FLOOR_RATE_PERCENT) / U256::from(100u64)
}

// corresponds to the symbolic rate
pub const F_FP_DEFAULT: u128 = SCALE * 32 / 100;
pub const F_MAX_FP: u128 = F_FP_DEFAULT * 2;
