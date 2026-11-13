//! PancakeSwap Liquidity Book `PriceHelper` port.
//!
//! Verified against `pancakeswap/infinity-core@main` (2026-05-12):
//! - `src/pool-bin/libraries/PriceHelper.sol`
//!
//! Function names mirror the Solidity verbatim. Note that
//! `get_id_from_price` **saturates** instead of reverting on out-of-range
//! cast. See the inline comment for the rationale.

use alloy_primitives::{I256, U256};

use crate::errors::PrimitivesMathError;
use crate::math::constants::{
    BASIS_POINT_MAX, MAX_BIN_ID, PRECISION, REAL_ID_SHIFT, SCALE_1_128_128, SCALE_OFFSET,
};
use crate::math::uint128x128_math::{log2, pow};
use crate::math::uint256x256_math::{mul_shift_round_down, shift_div_round_down};
use outbe_primitives::error::Result;

/// Mirrors `PriceHelper.getBase(uint16)` (PriceHelper.sol L46-50).
/// Returns `1 + binStep / BASIS_POINT_MAX` in 128.128 fixed-point.
///
/// The integer division `(bin_step_u256 << SCALE_OFFSET as usize) /
/// U256::from(BASIS_POINT_MAX)` truncates (rounds down toward zero). That
/// matches `Uint256x256Math.shiftDivRoundDown` and upstream Solidity
/// `PriceHelper.getBase`.
pub fn get_base(bin_step: u16) -> U256 {
    let bin_step_u256 = U256::from(bin_step);
    SCALE_1_128_128 + (bin_step_u256 << SCALE_OFFSET as usize) / U256::from(BASIS_POINT_MAX)
}

/// Mirrors `PriceHelper.getExponent(uint24)` (PriceHelper.sol L54-58).
/// `exponent = id - REAL_ID_SHIFT`. Rejects `id > MAX_BIN_ID`.
pub fn get_exponent(id: u32) -> Result<i32> {
    if id > MAX_BIN_ID {
        return Err(PrimitivesMathError::LbMathBinIdOutOfRange { id }.into());
    }
    Ok((id as i32) - REAL_ID_SHIFT)
}

/// Mirrors `PriceHelper.getPriceFromId(uint24, uint16)`
/// (PriceHelper.sol L21-26). Returns the 128.128 fixed-point price for
/// the given bin id. May error at extreme bin ids where LB's pow exponent
/// exceeds `2^20`. For typical bin ladders, these ids correspond to
/// astronomical or sub-atomic prices that the caller treats as diagnostic.
pub fn get_price_from_id(id: u32, bin_step: u16) -> Result<U256> {
    let base = get_base(bin_step);
    let exponent = get_exponent(id)?;
    pow(base, exponent)
}

/// Mirrors `PriceHelper.getIdFromPrice(uint256, uint16)`
/// (PriceHelper.sol L29-37), but **saturates** instead of reverting on
/// out-of-range cast (LB uses `safe24`). Saturation rationale: reverting
/// would brick downstream callers that need a deterministic id for any
/// finite 128.128 price.
pub fn get_id_from_price(price_128x128: U256, bin_step: u16) -> Result<u32> {
    let base = get_base(bin_step);
    let log2_price = log2(price_128x128)?;
    let log2_base = log2(base)?;
    if log2_base.is_zero() {
        return Err(PrimitivesMathError::LbMathLog2BaseZero.into());
    }
    // Solidity int256 / int256 truncates toward zero (matches I256 / impl).
    let real_id = log2_price / log2_base;

    let shift =
        I256::try_from(REAL_ID_SHIFT).map_err(|_| PrimitivesMathError::LbMathRealIdShiftWiden)?;
    let id_signed = real_id.saturating_add(shift);

    // Saturate to [0, MAX_BIN_ID].
    if id_signed.is_negative() {
        return Ok(0);
    }
    let id_unsigned = U256::try_from(id_signed).map_err(|_| PrimitivesMathError::LbMathIdWiden)?;
    let max = U256::from(MAX_BIN_ID);
    let bounded = if id_unsigned > max { max } else { id_unsigned };
    Ok(bounded.to::<u32>())
}

/// Mirrors `PriceHelper.convertDecimalPriceTo128x128(uint256)`
/// (PriceHelper.sol L62-64): `(price << 128) / 1e18`.
pub fn convert_decimal_price_to_128x128(price_18dec: U256) -> Result<U256> {
    shift_div_round_down(price_18dec, SCALE_OFFSET as u8, PRECISION)
}

/// Mirrors `PriceHelper.convert128x128PriceToDecimal(uint256)`
/// (PriceHelper.sol L68-70): `(price128x128 * 1e18) >> 128`.
pub fn convert_128x128_price_to_decimal(price_128x128: U256) -> Result<U256> {
    mul_shift_round_down(price_128x128, PRECISION, SCALE_OFFSET as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BIN_STEP_BP: u16 = 25;

    #[test]
    fn get_base_25bp() {
        // base = (1 << 128) + ((25 << 128) / 10_000)
        let expected = (U256::ONE << 128) + (U256::from(25u64) << 128) / U256::from(10_000u64);
        assert_eq!(get_base(BIN_STEP_BP), expected);
    }

    #[test]
    fn get_exponent_centered_at_real_id_shift() {
        assert_eq!(get_exponent(REAL_ID_SHIFT as u32).unwrap(), 0);
        assert_eq!(get_exponent(0).unwrap(), -REAL_ID_SHIFT);
        assert_eq!(get_exponent(REAL_ID_SHIFT as u32 + 100).unwrap(), 100);
    }

    #[test]
    fn convert_decimal_round_trip() {
        // 1.0 in 18-decimal == 1.0 in 128.128 == SCALE.
        assert_eq!(
            convert_decimal_price_to_128x128(PRECISION).unwrap(),
            SCALE_1_128_128
        );
        assert_eq!(
            convert_128x128_price_to_decimal(SCALE_1_128_128).unwrap(),
            PRECISION
        );
    }

    #[test]
    fn get_id_from_price_one_is_real_id_shift() {
        // Price = 1.0 -> bin_id = REAL_ID_SHIFT.
        let id = get_id_from_price(SCALE_1_128_128, BIN_STEP_BP).unwrap();
        assert_eq!(id, REAL_ID_SHIFT as u32);
    }

    #[test]
    fn get_id_from_price_does_not_underflow() {
        // `log2(U256::ONE)` is the 128.128 value `-128 << 128`. Dividing by
        // `log2(base)` for `BIN_STEP_BP = 25` yields `REAL_ID_SHIFT - 35_533`.
        let id = get_id_from_price(U256::ONE, BIN_STEP_BP).unwrap();
        assert_eq!(id, 8_353_075);
        assert!(id < get_id_from_price(U256::from(2u64), BIN_STEP_BP).unwrap());
    }

    #[test]
    fn price_helper_rejects_out_of_domain() {
        // `(2^24 - 1) - 2^23 = 2^23 - 1`.
        assert_eq!(get_exponent(MAX_BIN_ID).unwrap(), REAL_ID_SHIFT - 1);

        let displayed = |id: u32| {
            outbe_primitives::error::PrecompileError::from(
                PrimitivesMathError::LbMathBinIdOutOfRange { id },
            )
            .to_string()
        };
        let above = MAX_BIN_ID + 1;
        assert_eq!(
            get_exponent(above).unwrap_err().to_string(),
            displayed(above)
        );
        assert_eq!(
            get_exponent(u32::MAX).unwrap_err().to_string(),
            displayed(u32::MAX)
        );
        assert_eq!(
            get_price_from_id(above, BIN_STEP_BP)
                .unwrap_err()
                .to_string(),
            displayed(above)
        );

        // `(3 << 128) / 10_000` is not exact, so round-down stays one below the ceiling.
        let scaled = U256::from(3u64) << 128;
        let denom = U256::from(10_000u64);
        assert_ne!(scaled % denom, U256::ZERO);
        let floor = scaled / denom;
        assert_eq!(get_base(3), SCALE_1_128_128 + floor);
        assert_eq!(SCALE_1_128_128 + floor + U256::ONE - get_base(3), U256::ONE);
    }

    #[test]
    fn get_id_from_price_does_not_overflow() {
        // The full 128.128 dynamic range (up to U256::MAX) gives log2 in
        // [-128, +128]. After division by log2(base) ~= 1.225e36 (in 128.128),
        // the quotient stays in roughly +/-35_000 around REAL_ID_SHIFT. So
        // U256::MAX maps to ~REAL_ID_SHIFT + 35_533, well below MAX_BIN_ID.
        // Saturation branches exist defensively for future BIN_STEP_BP
        // values where the bin range could overflow.
        let id = get_id_from_price(U256::MAX, BIN_STEP_BP).unwrap();
        // Stays within the realistic narrow band; confirm < MAX_BIN_ID.
        assert!(
            id > REAL_ID_SHIFT as u32 && id < MAX_BIN_ID,
            "id = {id}; expected REAL_ID_SHIFT < id < MAX_BIN_ID"
        );
    }

    #[test]
    fn price_to_bin_monotonic_small_sample() {
        // For a hand-picked ascending price ladder, bin ids must be
        // non-decreasing.
        let prices = [
            U256::from(1u64),
            U256::from(1_000u64),
            U256::from(1_000_000u64),
            PRECISION / U256::from(1000u64),
            PRECISION,
            PRECISION * U256::from(1000u64),
        ];
        let mut last_bin = 0u32;
        for p in prices {
            let p128 = convert_decimal_price_to_128x128(p).unwrap();
            let id = get_id_from_price(p128, BIN_STEP_BP).unwrap();
            assert!(id >= last_bin, "monotonicity broken: {last_bin} -> {id}");
            last_bin = id;
        }
    }
}
