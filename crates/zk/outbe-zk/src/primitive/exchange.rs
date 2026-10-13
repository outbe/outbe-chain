//! Grumpkin ECDH for consent-box key derivation.

use ark_ec::{AffineRepr, CurveGroup};
use ark_ff::PrimeField;
use ark_std::rand::Rng;
use ark_std::UniformRand;

use crate::primitive::curve::{coords, mul_generator, Affine, Scalar};
use crate::{Error, Fr};

/// Sample an exchange keypair.
pub fn random_keypair<R: Rng>(rng: &mut R) -> (Scalar, Affine) {
    let sk = Scalar::rand(rng);
    let pk = mul_generator(&sk).into_affine();
    (sk, pk)
}

/// Shared secret: the x-coordinate of `sk · other`.
/// Symmetric for `A = [a]G`, `B = [b]G`: `shared(a, B) == shared(b, A)`.
pub fn shared(sk: &Scalar, other: &Affine) -> Result<Fr, Error> {
    let point = other.mul_bigint(sk.into_bigint()).into_affine();
    let (x, _y) = coords(&point)?;
    Ok(x)
}
