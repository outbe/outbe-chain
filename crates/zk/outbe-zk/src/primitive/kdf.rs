//! Domain-separated Poseidon2 key derivation in the native BN254 field.

use ark_ff::PrimeField;

use crate::primitive::hash::poseidon2;
use crate::{Error, Fr};

/// Domain separator folded into the derivation.
pub const DOMAIN: &str = "Outbe/kdf/poseidon/v1";

/// Derive one field element from `inputs`.
pub fn derive(inputs: &[Fr]) -> Result<Fr, Error> {
    let tag = Fr::from_le_bytes_mod_order(DOMAIN.as_bytes());
    let mut v = Vec::with_capacity(inputs.len() + 1);
    v.push(tag);
    v.extend_from_slice(inputs);
    poseidon2(&v)
}
