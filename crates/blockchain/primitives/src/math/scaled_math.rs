//! Checked integer scaling shared by monetary protocol modules.
//!
//! Generic multiply/divide helpers reject a `U256` product overflow. Pledge
//! quotes use bounded `U512` intermediates and check their final `U256` outputs.

use alloy_primitives::{U256, U512};

use crate::error::{PrecompileError, Result};

/// Quote asset-atomic principal against a canonical six-decimal
/// COEN/currency VWAP (`issuanceCurrencyVwapMinor`).
///
/// ```text
/// G = floor(P * 10^12 / (10^d * V))
/// E = floor(P * 10^12 / (10^d * G))
/// ```
///
/// `vwap_minor` is scale `1e6`. For a six-decimal asset this is
/// `floor(P * 10^6 / V)` and `floor(P * 10^6 / G)`. Assets support 0–18
/// decimals. Zero inputs, a zero floor, and an output that does not fit
/// `U256` fail. `V` is that canonical six-decimal price.
pub fn checked_quote(principal: U256, decimals: u8, vwap_minor: U256) -> Result<(U256, U256)> {
    if decimals > 18 {
        return Err(PrecompileError::Revert("unsupported asset decimals".into()));
    }
    if principal.is_zero() || vwap_minor.is_zero() {
        return Err(PrecompileError::Revert(
            "principal and valuation must be positive".into(),
        ));
    }
    let asset_scale = U512::from(10u64).pow(U512::from(decimals));
    let principal = U512::from(principal);
    // 10^12 puts the six-decimal price onto asset atomic units.
    // The numerator is at most 256 + 40 bits and the denominator at most
    // 60 + 256 bits, so both fit U512.
    let minor_shift = U512::from(10u64).pow(U512::from(12u64));
    let quoted = principal * minor_shift / (asset_scale * U512::from(vwap_minor));
    let quoted = convert_to_u256(quoted)?;
    let entry = principal * minor_shift / (asset_scale * U512::from(quoted));
    Ok((quoted, convert_to_u256(entry)?))
}

fn convert_to_u256(value: U512) -> Result<U256> {
    // A quote is stored as U256: explicitly reject values outside that bound
    // instead of truncating the high limbs (covered by quote boundary tests).
    let value = U256::checked_from_limbs_slice(value.as_limbs())
        .ok_or_else(|| PrecompileError::Revert("arithmetic overflow".into()))?;
    if value.is_zero() {
        return Err(PrecompileError::Revert("value rounds to zero".into()));
    }
    Ok(value)
}

fn checked_product(numerator: U256, multiplier: U256) -> Result<U256> {
    numerator
        .checked_mul(multiplier)
        .ok_or_else(|| PrecompileError::Revert("scaled_math: multiplication overflow".into()))
}

fn require_denominator(denominator: U256) -> Result<()> {
    if denominator.is_zero() {
        return Err(PrecompileError::Revert(
            "scaled_math: division by zero".into(),
        ));
    }
    Ok(())
}

/// Returns `floor(numerator * multiplier / denominator)`.
pub fn checked_mul_div_floor(numerator: U256, multiplier: U256, denominator: U256) -> Result<U256> {
    require_denominator(denominator)?;
    Ok(checked_product(numerator, multiplier)? / denominator)
}

/// Returns `ceil(numerator * multiplier / denominator)`.
pub fn checked_mul_div_ceil(numerator: U256, multiplier: U256, denominator: U256) -> Result<U256> {
    require_denominator(denominator)?;
    let product = checked_product(numerator, multiplier)?;
    let quotient = product / denominator;
    if product % denominator == U256::ZERO {
        return Ok(quotient);
    }
    quotient
        .checked_add(U256::ONE)
        .ok_or_else(|| PrecompileError::Revert("scaled_math: ceiling overflow".into()))
}

#[cfg(test)]
mod pledge_tests {
    use super::*;
    use crate::units::SCALE_1E6_U256;

    #[test]
    fn pledge_quote_uses_canonical_minor6() {
        assert_eq!(
            checked_quote(U256::from(1_000_000u64), 6, U256::from(3_000_000u64)).unwrap(),
            (U256::from(333_333u64), U256::from(3_000_003u64)),
        );
        // V = 2.000000 is one canonical price. The next minor unit is a
        // different price; there is no sub-minor residue to floor against.
        assert_eq!(
            checked_quote(U256::from(2_000_000u64), 6, U256::from(2_000_000u64)).unwrap(),
            (U256::from(1_000_000u64), U256::from(2_000_000u64)),
        );
        for decimals in [0, 6, 8, 18] {
            let principal = U256::from(2) * U256::from(10).pow(U256::from(decimals));
            assert_eq!(
                checked_quote(principal, decimals, U256::from(2_000_000u64)).unwrap(),
                (SCALE_1E6_U256, U256::from(2_000_000u64)),
                "decimals {decimals}"
            );
        }
        // Fractional Gratis is floored first; entry uses the accepted Gratis.
        assert_eq!(
            checked_quote(U256::from(3u64), 6, U256::from(2_000_000u64)).unwrap(),
            (U256::ONE, U256::from(3_000_000u64)),
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
        // A U256 intermediate would overflow, although both outputs fit.
        assert_eq!(
            checked_quote(U256::MAX, 6, U256::from(1_000_000u64)).unwrap(),
            (U256::MAX, SCALE_1E6_U256)
        );
        // Exact upper conversion boundary; the next integer is rejected.
        assert_eq!(convert_to_u256(U512::from(U256::MAX)).unwrap(), U256::MAX);
        assert!(convert_to_u256(U512::from(U256::MAX) + U512::ONE).is_err());
    }
}
