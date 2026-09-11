//! Rounding rule shared by Nod, Intex and Gem settlement: the obligation is
//! computed at full precision, any FX leg included, then floored once into the
//! settlement asset's minor units.

use alloy_primitives::U256;
use core::fmt;

/// Widest asset the factories settle in.
pub const MAX_ASSET_DECIMALS: u8 = 18;

/// Ways the floor can refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundingError {
    UnsupportedDecimals(u8),
    ZeroRate,
    Overflow,
    /// A positive obligation that floors to nothing would sell the right for free.
    RoundsToZero,
}

impl fmt::Display for RoundingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedDecimals(decimals) => {
                write!(f, "settlement asset has unsupported decimals {decimals}")
            }
            Self::ZeroRate => f.write_str("settlement rate denominator is zero"),
            Self::Overflow => f.write_str("settlement conversion overflow"),
            Self::RoundsToZero => f.write_str("settlement cost rounds to zero"),
        }
    }
}

/// `floor(numerator x 10^asset_decimals / (denominator x 10^obligation_decimals))`.
///
/// `numerator / denominator` is the obligation carrying `obligation_decimals`; a
/// rate leg belongs in that fraction so it is floored with the unit scaling.
pub fn floor_to_asset_units(
    numerator: U256,
    denominator: U256,
    obligation_decimals: u32,
    asset_decimals: u8,
) -> Result<U256, RoundingError> {
    if asset_decimals > MAX_ASSET_DECIMALS {
        return Err(RoundingError::UnsupportedDecimals(asset_decimals));
    }
    if denominator.is_zero() {
        return Err(RoundingError::ZeroRate);
    }
    let asset_decimals = u32::from(asset_decimals);
    let (numerator, denominator) = if asset_decimals >= obligation_decimals {
        let scaled = numerator
            .checked_mul(pow10(asset_decimals - obligation_decimals))
            .ok_or(RoundingError::Overflow)?;
        (scaled, denominator)
    } else {
        let scaled = denominator
            .checked_mul(pow10(obligation_decimals - asset_decimals))
            .ok_or(RoundingError::Overflow)?;
        (numerator, scaled)
    };
    let units = numerator / denominator;
    if units.is_zero() && !numerator.is_zero() {
        return Err(RoundingError::RoundsToZero);
    }
    Ok(units)
}

fn pow10(exponent: u32) -> U256 {
    U256::from(10u64).pow(U256::from(exponent))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(value: u128) -> U256 {
        U256::from(value)
    }

    #[test]
    fn same_scale_passes_through() {
        assert_eq!(
            floor_to_asset_units(u(1_234_567), U256::ONE, 6, 6),
            Ok(u(1_234_567))
        );
    }

    #[test]
    fn a_wider_asset_scales_up_exactly() {
        assert_eq!(
            floor_to_asset_units(u(7), U256::ONE, 6, 18),
            Ok(u(7_000_000_000_000))
        );
    }

    #[test]
    fn a_narrower_asset_floors_the_dust_away() {
        // 1.999999 units of a six-decimal obligation into a zero-decimal asset.
        assert_eq!(
            floor_to_asset_units(u(1_999_999), U256::ONE, 6, 0),
            Ok(U256::ONE)
        );
    }

    #[test]
    fn the_rate_leg_is_floored_together_with_the_scaling() {
        // 7.5 at rate 2/3 = 5.0 exactly; flooring the rate leg on its own first
        // (7 x 2 / 3 = 4.66) would have lost a unit.
        assert_eq!(
            floor_to_asset_units(u(7_500_000) * u(2), u(3), 6, 6),
            Ok(u(5_000_000))
        );
    }

    #[test]
    fn a_positive_obligation_never_floors_to_nothing() {
        assert_eq!(
            floor_to_asset_units(U256::ONE, U256::ONE, 12, 0),
            Err(RoundingError::RoundsToZero)
        );
        assert_eq!(
            floor_to_asset_units(U256::ZERO, U256::ONE, 12, 0),
            Ok(U256::ZERO)
        );
    }

    #[test]
    fn refuses_what_it_cannot_express() {
        assert_eq!(
            floor_to_asset_units(u(1), U256::ONE, 6, 19),
            Err(RoundingError::UnsupportedDecimals(19))
        );
        assert_eq!(
            floor_to_asset_units(u(1), U256::ZERO, 6, 6),
            Err(RoundingError::ZeroRate)
        );
        assert_eq!(
            floor_to_asset_units(U256::MAX, U256::ONE, 6, 18),
            Err(RoundingError::Overflow)
        );
    }
}
