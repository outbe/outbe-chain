//! Grumpkin, whose affine coordinates are native BN254 field elements.

use ark_ec::{AffineRepr, PrimeGroup};
use ark_ff::{BigInteger, PrimeField};

use crate::{Error, Fr};

pub use ark_grumpkin::{Affine, Fr as Scalar, Projective as Group};

/// Affine `(x, y)` as native field elements, rejecting the identity.
pub fn coords(p: &Affine) -> Result<(Fr, Fr), Error> {
    let (x, y) = p.xy().ok_or(Error::Identity("curve point coords"))?;
    Ok((x, y))
}

/// Reinterpret a base-field element as a scalar (canonical for the
/// BN254/Grumpkin cycle: `r < q`, so the reduction is a no-op and the
/// map is injective). Used to turn a Poseidon challenge into a scalar.
pub fn base_to_scalar(x: &Fr) -> Scalar {
    Scalar::from_le_bytes_mod_order(&x.into_bigint().to_bytes_le())
}

/// The standard Grumpkin generator.
pub fn generator() -> Group {
    Group::generator()
}

/// `[scalar]·G`.
pub fn mul_generator(scalar: &Scalar) -> Group {
    generator().mul_bigint(scalar.into_bigint())
}
