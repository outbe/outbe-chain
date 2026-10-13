//! BN254 Poseidon2 hashes and the protocol's field-valued formulas.

use ark_ff::PrimeField;

use crate::codec::field_from_be_bytes;
use crate::primitive::curve::{coords, Affine};
use crate::{Error, Fr};

/// Protocol domain separator folded into [`binding`].
pub const BINDING_DOMAIN: u64 = 1;

/// BN254 Poseidon2 via `outbe-poseidon`, bit-identical to Noir's in-circuit
/// sponge over the barretenberg `poseidon2_permutation`.
pub fn poseidon2(inputs: &[Fr]) -> Result<Fr, Error> {
    use outbe_poseidon::PoseidonHasher as _;
    outbe_poseidon::Poseidon2::new()
        .hash(inputs)
        .map_err(|error| Error::Hash(error.to_string()))
}

/// Iterated 2-to-1 hash seeded at `seed`; an empty body returns `seed`.
pub fn iterate(seed: Fr, body: &[Fr]) -> Result<Fr, Error> {
    let mut state = seed;
    for x in body {
        state = poseidon2(&[state, *x])?;
    }
    Ok(state)
}

/// `derivedOwner = Poseidon2([pk.x, pk.y, nonce])`.
pub fn derive_owner(pk: &Affine, nonce: Fr) -> Result<Fr, Error> {
    let (x, y) = coords(pk)?;
    poseidon2(&[x, y, nonce])
}

/// Entity hash: iterated hash seeded at `id` over `body`.
pub fn nft_hash(id: Fr, body: &[Fr]) -> Result<Fr, Error> {
    iterate(id, body)
}

/// The Schnorr message: `Poseidon2([nft_hash, nonce, binding])`.
pub fn signing_payload(nft_hash: Fr, nonce: Fr, binding: Fr) -> Result<Fr, Error> {
    poseidon2(&[nft_hash, nonce, binding])
}

/// Submission binding:
/// `Poseidon2([DOMAIN, sender, cid_lo128, cid_hi128, host_chain_id, l2_chain_id])`.
///
/// The commitment ID uses its established two-limb Tribute encoding, independent
/// of the three-limb encoding used for U256 amounts. Both chain IDs are bound so
/// identical circuits on different L2s cannot share a submission context.
pub fn binding(
    sender: &[u8; 20],
    commitment_id: &[u8; 32],
    host_chain_id: u64,
    l2_chain_id: u64,
) -> Result<Fr, Error> {
    poseidon2(&[
        Fr::from(BINDING_DOMAIN),
        field_from_be_bytes(sender),
        field_from_be_bytes(&commitment_id[16..]),
        field_from_be_bytes(&commitment_id[..16]),
        Fr::from(host_chain_id),
        Fr::from(l2_chain_id),
    ])
}

/// Interpret a string's bytes as a big-endian integer, reduced mod the field.
pub fn ascii_field(value: &str) -> Fr {
    Fr::from_be_bytes_mod_order(value.as_bytes())
}
