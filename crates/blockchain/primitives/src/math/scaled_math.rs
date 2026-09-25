//! Checked integer scaling shared by monetary protocol modules.
//!
//! Generic multiply/divide helpers reject a `U256` product overflow. Pledge
//! quotes use bounded `U512` intermediates and check their final `U256` outputs.

use alloy_primitives::{U256, U512};

use crate::error::{PrecompileError, Result};
use crate::units::{SCALE_1E18, SCALE_1E6_U256};

/// Quote six-decimal asset atomic principal and an FP18
/// currency-per-COEN valuation. Floor each equation only after full scaling.
/// Assets support 0–18 decimals. Zero inputs/results and output overflow fail.
pub fn checked_quote(
    principal: U256,
    decimals: u8,
    valuation: U256,
) -> Result<(U256, U256)> {
    if decimals > 18 {
        return Err(PrecompileError::Revert(
            "unsupported asset decimals".into(),
        ));
    }
    if principal.is_zero() || valuation.is_zero() {
        return Err(PrecompileError::Revert(
            "principal and valuation must be positive".into(),
        ));
    }
    let asset_scale = U512::from(10u64).pow(U512::from(decimals));
    let principal = U512::from(principal);
    let six = U512::from(SCALE_1E6_U256);
    // Bounds: the largest numerator is 256 + 60 + 20 bits; denominators
    // are at most 60 + 256 bits. Every intermediate therefore fits U512.
    let quoted = principal * U512::from(SCALE_1E18) * six / (asset_scale * U512::from(valuation));
    let quoted = convert_to_u256(quoted)?;
    let entry = principal * six * six / (asset_scale * U512::from(quoted));
    Ok((quoted, convert_to_u256(entry)?))
}

fn convert_to_u256(value: U512) -> Result<U256> {
    // A quote is stored as U256: explicitly reject values outside that bound
    // instead of truncating the high limbs (covered by quote boundary tests).
    let value = U256::checked_from_limbs_slice(value.as_limbs())
        .ok_or_else(|| PrecompileError::Revert("arithmetic overflow".into()))?;
    if value.is_zero() {
        return Err(PrecompileError::Revert("value rounds to zero".into(), ));
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

    #[test]
    fn pledge_scales_asset_units_and_keeps_full_oracle_precision() {
        for decimals in [0, 6, 8, 18] {
            let principal = U256::from(2) * U256::from(10).pow(U256::from(decimals));
            assert_eq!(
                checked_quote(principal, decimals, U256::from(2) * SCALE_1E18).unwrap(),
                (SCALE_1E6_U256, U256::from(2_000_000)),
            );
        }
        // Truncating this price to six decimals would incorrectly debit 1e6.
        assert_eq!(
            checked_quote(
                U256::from(2_000_000),
                6,
                U256::from(2) * SCALE_1E18 + U256::ONE
            )
            .unwrap(),
            (U256::from(999_999), U256::from(2_000_002)),
        );
        // Fractional Gratis is floored first; entry is principal / accepted Gratis,
        // not the Oracle's normalized price (which is 2e6 here).
        assert_eq!(
            checked_quote(U256::from(3), 6, U256::from(2) * SCALE_1E18).unwrap(),
            (U256::ONE, U256::from(3_000_000)),
        );
    }

    #[test]
    fn pledge_rejects_invalid_scales_zeros_and_output_overflow() {
        for (principal, decimals, price) in [
            (U256::ONE, 19, SCALE_1E18),
            (U256::ZERO, 6, SCALE_1E18),
            (U256::ONE, 6, U256::ZERO),
            (U256::ONE, 6, U256::from(2) * SCALE_1E18), // zero Gratis
            (U256::ONE, 0, U256::ONE),                  // zero entry
            (U256::MAX, 0, SCALE_1E18),                 // Gratis exceeds U256
        ] {
            assert!(checked_quote(principal, decimals, price).is_err());
        }
        // A U256 intermediate would overflow, although both outputs fit.
        assert_eq!(
            checked_quote(U256::MAX, 6, SCALE_1E18).unwrap(),
            (U256::MAX, SCALE_1E6_U256)
        );
        // Exact upper conversion boundary; the next integer is rejected.
        assert_eq!(convert_to_u256(U512::from(U256::MAX)).unwrap(), U256::MAX);
        assert!(convert_to_u256(U512::from(U256::MAX) + U512::ONE).is_err());
    }
}
