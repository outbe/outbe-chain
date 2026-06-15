
// ---------------------------------------------------------------------------
// Field-element conversion
// ---------------------------------------------------------------------------

use alloy_primitives::{Address, U256};
use ark_bn254::Fr;
use ark_ff::{BigInt, PrimeField};
use outbe_poseidon::{Poseidon, PoseidonHasher};
use crate::GratisPoolError;

/// `U256 → Fr` via 4-limb conversion mod-reduced to the BN254 scalar field.
///
/// Returns `None` if the input is not in a canonical form.
pub(crate) fn u256_to_fr(x: U256) -> Option<Fr> {
    let limbs = BigInt::new(x.into_limbs());
    Fr::from_bigint(limbs)
}

/// `Fr → U256` via limbs.
pub(crate) fn fr_to_u256(x: Fr) -> U256 {
    let limbs = x.into_bigint().0;
    U256::from_limbs(limbs)
}

/// `u64 → Fr` for tag and action constants.
pub(crate) fn u64_to_fr(x: u64) -> Fr {
    Fr::from(x)
}

/// `Address → Fr` — the 20-byte address is padded to 32 BE bytes and
/// mod-reduced into the scalar field.
pub(crate) fn address_to_fr(addr: Address) -> Fr {
    let mut buf = [0u8; 32];
    buf[12..32].copy_from_slice(addr.as_slice());
    Fr::from_be_bytes_mod_order(&buf)
}


/// Variadic Poseidon helper. Constructs a fresh `Poseidon` with `inputs.len()`
/// arity and returns the hash as a `U256` (so callers can store / compare it
/// in storage directly).
pub(crate) fn poseidon(inputs: &[Fr]) -> outbe_primitives::error::Result<U256> {
    let mut hasher = Poseidon::<Fr>::new_circom(inputs.len())
        .map_err(|e| GratisPoolError::PoseidonFailed(e.to_string()))?;
    let h = hasher
        .hash(inputs)
        .map_err(|e| GratisPoolError::PoseidonFailed(e.to_string()))?;
    Ok(fr_to_u256(h))
}
