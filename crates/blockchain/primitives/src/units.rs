//! Fixed-point scaling helpers.
//!
//! The default economic type is `U256`

use alloy_primitives::U256;

/// Native token symbol.
pub const NATIVE_TOKEN_SYMBOL: &str = "COEN";

/// Base denomination.
pub const BASE_DENOM: &str = "unit";

/// Independent 18-decimal fixed-point scale.
///
/// This is not a token denomination. Dimensionless protocols that explicitly
/// own an FP18 contract may keep using it after the native-token cutover.
pub const SCALE_1E18: U256 = U256::from_limbs([1_000_000_000_000_000_000, 0, 0, 0]);
pub const SCALE_1E18_U128: u128 = 1_000_000_000_000_000_000;

/// Raw units in one whole COEN.
pub const UNITS_PER_COEN: U256 = U256::from_limbs([1_000_000, 0, 0, 0]);
/// Raw units in one whole GRATIS.
pub const UNITS_PER_GRATIS: U256 = U256::from_limbs([1_000_000, 0, 0, 0]);
/// Raw units in one whole PROMIS.
pub const UNITS_PER_PROMIS: U256 = U256::from_limbs([1_000_000, 0, 0, 0]);
/// Raw units in one whole WCOEN.
pub const UNITS_PER_WCOEN: U256 = U256::from_limbs([1_000_000, 0, 0, 0]);
/// Integer scale of a COEN/840 Oracle price.
pub const COEN840_PRICE_SCALE: U256 = U256::from_limbs([1_000_000, 0, 0, 0]);

/// One whole coen, expressed in the on-chain `U256` representation.
pub const ONE_COEN: U256 = UNITS_PER_COEN;

/// The smallest representable on-chain amount (1 unit).
pub const ONE_UNIT: U256 = U256::ONE;

/// Number of decimal places.
pub const NATIVE_TOKEN_DECIMALS: u8 = 6;

/// Conversion from a whole-COEN count into native on-chain `unit`.
///
/// Implementations multiply the supplied whole-COEN value by
/// [`UNITS_PER_COEN`]. The trait is generic over the input type so callers can
/// pass any of the natural integer types without an explicit cast.
pub trait Units<T>: Sized {
    /// Returns `value * 1_000_000` as a `U256`.
    fn in_units(value: T) -> U256;
}

impl Units<U256> for U256 {
    fn in_units(value: U256) -> U256 {
        value * UNITS_PER_COEN
    }
}

impl Units<u128> for U256 {
    fn in_units(value: u128) -> U256 {
        U256::from(value) * UNITS_PER_COEN
    }
}

impl Units<i32> for U256 {
    fn in_units(value: i32) -> U256 {
        U256::from(value) * UNITS_PER_COEN
    }
}

impl Units<u64> for U256 {
    fn in_units(value: u64) -> U256 {
        U256::from(value) * UNITS_PER_COEN
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_coen_uses_six_decimal_units() {
        let one_coen = U256::from(1_000_000u64);

        assert_eq!(ONE_UNIT, U256::ONE);
        assert_eq!(ONE_COEN, one_coen);
        assert_eq!(NATIVE_TOKEN_DECIMALS, 6);
        assert_eq!(
            U256::in_units(U256::from(2u64)),
            one_coen * U256::from(2u64)
        );
        assert_eq!(U256::in_units(2u128), one_coen * U256::from(2u64));
        assert_eq!(U256::in_units(2i32), one_coen * U256::from(2u64));
        assert_eq!(U256::in_units(2u64), one_coen * U256::from(2u64));
    }
}
