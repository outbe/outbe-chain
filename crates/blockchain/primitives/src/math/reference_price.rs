//! Outbe-owned COEN/840 price adapter for the existing Liquidity Book bins.
//!
//! COEN/840 prices are six-decimal integers. The underlying PancakeSwap port
//! remains unchanged and continues to consume and return 128.128 prices.

use alloy_primitives::U256;

use crate::error::Result;
use crate::math::price_helper;
use crate::math::uint256x256_math::{mul_shift_round_down, shift_div_round_down};
use crate::units::COEN840_PRICE_SCALE;

const PRICE_BINARY_OFFSET: u8 = 128;

/// Converts a six-decimal COEN/840 price to the existing 128.128 price domain.
pub fn coen840_price_to_128x128(price: U256) -> Result<U256> {
    shift_div_round_down(price, PRICE_BINARY_OFFSET, COEN840_PRICE_SCALE)
}

/// Converts an existing 128.128 price to a six-decimal COEN/840 price.
pub fn price_128x128_to_coen840(price: U256) -> Result<U256> {
    mul_shift_round_down(price, COEN840_PRICE_SCALE, PRICE_BINARY_OFFSET)
}

/// Maps a six-decimal COEN/840 price to a Liquidity Book bin id.
pub fn coen840_price_to_bin_id(price: U256, bin_step: u16) -> Result<u32> {
    price_helper::get_id_from_price(coen840_price_to_128x128(price)?, bin_step)
}

/// Maps a Liquidity Book bin id back to a six-decimal COEN/840 price.
pub fn bin_id_to_coen840_price(bin_id: u32, bin_step: u16) -> Result<U256> {
    let price = price_helper::get_price_from_id(bin_id, bin_step)?;
    price_128x128_to_coen840(price)
}
