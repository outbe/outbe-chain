use num_bigint::{BigInt, BigUint};
use num_traits::{One, Zero};

use crate::failure::ReferenceFailure;

const SCALE_DECIMAL: &str = "1000000";

pub(crate) fn scale() -> BigUint {
    BigUint::parse_bytes(SCALE_DECIMAL.as_bytes(), 10).expect("constant scale is valid")
}

pub(crate) fn u256_modulus() -> BigUint {
    BigUint::one() << 256_u16
}

fn i256_max() -> BigUint {
    (BigUint::one() << 255_u16) - BigUint::one()
}

pub(crate) fn wrap_u256(value: BigUint) -> BigUint {
    value % u256_modulus()
}

pub(crate) fn checked_add_u256(
    left: &BigUint,
    right: &BigUint,
    failure: ReferenceFailure,
) -> Result<BigUint, ReferenceFailure> {
    let result = left + right;
    if result >= u256_modulus() {
        Err(failure)
    } else {
        Ok(result)
    }
}

pub(crate) fn decimal(value: &str) -> Result<BigUint, ReferenceFailure> {
    BigUint::parse_bytes(value.as_bytes(), 10).ok_or_else(|| ReferenceFailure::new("ARITHMETIC"))
}

pub(crate) fn to_i256(value: &BigUint) -> Result<BigInt, ReferenceFailure> {
    if value > &i256_max() {
        Err(ReferenceFailure::new("ARITHMETIC"))
    } else {
        Ok(BigInt::from(value.clone()))
    }
}

pub(crate) fn trunc_signed_div(numerator: &BigInt, denominator: &BigInt) -> BigInt {
    assert!(!denominator.is_zero(), "reference denominator is non-zero");
    numerator / denominator
}

pub(crate) fn fp_root(
    value: &BigUint,
    exponent_denominator: u32,
) -> Result<BigUint, ReferenceFailure> {
    let unit = scale();
    if value.is_zero() {
        return Ok(BigUint::zero());
    }
    if value == &unit {
        return Ok(unit);
    }
    let target = value * unit.pow(exponent_denominator - 1);
    let mut low = BigUint::one();
    let mut high = value.max(&unit).clone();
    while low < high {
        let middle = &low + ((&high - &low + BigUint::one()) / 2_u8);
        if middle.pow(exponent_denominator) > target {
            high = middle - BigUint::one();
        } else {
            low = middle;
        }
    }
    if low >= u256_modulus() {
        Err(ReferenceFailure::new("ARITHMETIC"))
    } else {
        Ok(low)
    }
}
