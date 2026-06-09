//! Fixed-point scaling helpers.
//!
//! The default economic type is `U256`

use alloy_primitives::U256;

/// Native token symbol.
pub const NATIVE_TOKEN_SYMBOL: &str = "COEN";

/// Base denomination.
pub const BASE_DENOM: &str = "unit";

/// Scale factor equal to `10^18`.
pub const SCALE_1E18: U256 = U256::from_limbs([1_000_000_000_000_000_000, 0, 0, 0]);
pub const SCALE_1E18_U128: u128 = 1_000_000_000_000_000_000;

/// One whole coen, expressed in the on-chain `U256` representation.
pub const ONE_COEN: U256 = SCALE_1E18;

/// The smallest representable on-chain amount (1 unit).
pub const ONE_UNIT: U256 = U256::ONE;

/// Number of decimal places.
pub const NATIVE_TOKEN_DECIMALS: u8 = 18;

/// Conversion from a whole-unit count into the scaled on-chain `U256`.
///
/// Implementations multiply the supplied whole-unit value by
/// [`SCALE_1E18`]. The trait is generic over the input type so callers can
/// pass any of the natural integer types without an explicit cast.
pub trait Units<T>: Sized {
    /// Returns `value * 10^18` as a `U256`.
    fn in_units(value: T) -> U256;
}

impl Units<U256> for U256 {
    fn in_units(value: U256) -> U256 {
        value * SCALE_1E18
    }
}

impl Units<u128> for U256 {
    fn in_units(value: u128) -> U256 {
        U256::from(value) * SCALE_1E18
    }
}

impl Units<i32> for U256 {
    fn in_units(value: i32) -> U256 {
        U256::from(value) * SCALE_1E18
    }
}

impl Units<u64> for U256 {
    fn in_units(value: u64) -> U256 {
        U256::from(value) * SCALE_1E18
    }
}
