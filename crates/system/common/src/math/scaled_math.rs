//! Checked integer scaling shared by monetary protocol modules.
//!
//! Pledge quotes use bounded `U512` intermediates and check their final `U256` output.

use alloy_primitives::{U256, U512};

use crate::errors::PrimitivesMathError;
use crate::units::MAX_ASSET_DECIMALS;
use outbe_primitives::error::Result;

/// Gratis quantity of an asset-atomic `amount` at a canonical six-decimal
/// COEN/currency VWAP:
///
/// ```text
/// G = floor(P * 10^12 / (10^d * V))
/// ```
///
/// `vwap_minor` is scale `1e6`. For a six-decimal asset this is
/// `floor(P * 10^6 / V)`. Assets support 0–18 decimals. Zero inputs, a zero
/// floor, and an output that does not fit `U256` fail.
pub fn checked_quote(amount: U256, decimals: u8, vwap_minor: U256) -> Result<U256> {
    if decimals > MAX_ASSET_DECIMALS {
        return Err(PrimitivesMathError::UnsupportedAssetDecimals.into());
    }
    if amount.is_zero() || vwap_minor.is_zero() {
        return Err(PrimitivesMathError::AmountAndValuationMustBePositive.into());
    }
    let asset_scale = U512::from(10u64).pow(U512::from(decimals));
    // 10^12 puts the six-decimal price onto asset atomic units.
    // The numerator is at most 256 + 40 bits and the denominator at most
    // 60 + 256 bits, so both fit U512.
    let minor_shift = U512::from(10u64).pow(U512::from(12u64));
    convert_to_u256(U512::from(amount) * minor_shift / (asset_scale * U512::from(vwap_minor)))
}

fn convert_to_u256(value: U512) -> Result<U256> {
    // A quote is stored as U256: explicitly reject values outside that bound
    // instead of truncating the high limbs (covered by quote boundary tests).
    let value = U256::checked_from_limbs_slice(value.as_limbs())
        .ok_or(PrimitivesMathError::ArithmeticOverflow)?;
    if value.is_zero() {
        return Err(PrimitivesMathError::ValueRoundsToZero.into());
    }
    Ok(value)
}

#[cfg(test)]
mod pledge_tests {
    use super::*;
    use crate::units::SCALE_1E6_U256;

    #[test]
    fn pledge_quote_uses_canonical_minor6() {
        assert_eq!(
            checked_quote(U256::from(1_000_000u64), 6, U256::from(3_000_000u64)).unwrap(),
            U256::from(333_333u64),
        );
        // V = 2.000000 is one canonical price. The next minor unit is a
        // different price. There is no sub-minor residue to floor against.
        assert_eq!(
            checked_quote(U256::from(2_000_000u64), 6, U256::from(2_000_000u64)).unwrap(),
            U256::from(1_000_000u64),
        );
        for decimals in [0, 6, 8, 18] {
            let principal = U256::from(2) * U256::from(10).pow(U256::from(decimals));
            assert_eq!(
                checked_quote(principal, decimals, U256::from(2_000_000u64)).unwrap(),
                SCALE_1E6_U256,
                "decimals {decimals}"
            );
        }
        // The quote floors fractional Gratis first. Entry uses the accepted Gratis.
        assert_eq!(
            checked_quote(U256::from(3u64), 6, U256::from(2_000_000u64)).unwrap(),
            U256::ONE,
        );
    }

    #[test]
    fn pledge_rejects_invalid_scales_zeros_and_output_overflow() {
        for (principal, decimals, price) in [
            (U256::ONE, 19, U256::from(1_000_000u64)),
            (U256::ZERO, 6, U256::from(1_000_000u64)),
            (U256::ONE, 6, U256::ZERO),
            (U256::ONE, 6, U256::from(2_000_000u64)), // zero Gratis
            (U256::MAX, 0, U256::from(1_000_000u64)), // Gratis exceeds U256
        ] {
            assert!(checked_quote(principal, decimals, price).is_err());
        }
        // A U256 intermediate would overflow, although the output fits.
        assert_eq!(
            checked_quote(U256::MAX, 6, U256::from(1_000_000u64)).unwrap(),
            U256::MAX
        );
        // Exact upper conversion boundary. The conversion rejects the next integer.
        assert_eq!(convert_to_u256(U512::from(U256::MAX)).unwrap(), U256::MAX);
        assert!(convert_to_u256(U512::from(U256::MAX) + U512::ONE).is_err());
    }
}
