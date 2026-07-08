// ---------------------------------------------------------------------------
// Field-element conversion
// ---------------------------------------------------------------------------

use crate::constants::{DenomAmount, TAG_BINDING, TAG_COMMIT_GRATIS, TAG_NULLIFIER_GRATIS};
use crate::GratisPoolError;
use alloy_primitives::{Address, U256};
use ark_bn254::Fr;
use ark_ff::{BigInt, PrimeField};
use outbe_poseidon::{Poseidon, PoseidonHasher};
// ---------------------------------------------------------------------------
// Public helpers — formulas exposed to runtime / tests
// ---------------------------------------------------------------------------

/// `commitment = poseidon(TAG_COMMIT_GRATIS, secret, nullifier_secret, denom_id)`.
pub fn commitment_hash(
    secret: U256,
    nullifier_secret: U256,
    denom: DenomAmount,
) -> outbe_primitives::error::Result<U256> {
    let secret_fr = u256_to_fr(secret)
        .ok_or_else(|| GratisPoolError::NonCanonicalFieldInput("secret".to_string()))?;
    let nullifier_secret_fr = u256_to_fr(nullifier_secret)
        .ok_or_else(|| GratisPoolError::NonCanonicalFieldInput("nullifier_secret".to_string()))?;

    poseidon(&[
        u64_to_fr(TAG_COMMIT_GRATIS),
        secret_fr,
        nullifier_secret_fr,
        u64_to_fr(denom.id() as u64),
    ])
}

/// `nullifier_hash = poseidon(TAG_NULLIFIER_GRATIS, nullifier_secret)`.
pub fn nullifier_hash(nullifier_secret: U256) -> outbe_primitives::error::Result<U256> {
    let nullifier_secret_fr = u256_to_fr(nullifier_secret)
        .ok_or_else(|| GratisPoolError::NonCanonicalFieldInput("nullifier_secret".to_string()))?;
    poseidon(&[u64_to_fr(TAG_NULLIFIER_GRATIS), nullifier_secret_fr])
}

/// `receiver_binding = poseidon(TAG_BINDING, action_tag, target_address, chain_id, nonce)`.
pub fn receiver_binding(
    action_tag: u64,
    target: Address,
    chain_id: u64,
    nonce: U256,
) -> outbe_primitives::error::Result<U256> {
    let nonce_fr = u256_to_fr(nonce)
        .ok_or_else(|| GratisPoolError::NonCanonicalFieldInput("nonce".to_string()))?;

    poseidon(&[
        u64_to_fr(TAG_BINDING),
        u64_to_fr(action_tag),
        address_to_fr(target),
        u64_to_fr(chain_id),
        nonce_fr,
    ])
}

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

#[cfg(test)]
mod tests {
    use super::{address_to_fr, fr_to_u256, u256_to_fr, u64_to_fr};
    use alloy_primitives::{address, U256};
    use ark_bn254::Fr;
    use ark_ff::{BigInteger, PrimeField};

    /// BN254 scalar field modulus `p`, encoded as a `U256`.
    fn bn254_modulus_as_u256() -> U256 {
        let p_be = <Fr as PrimeField>::MODULUS.to_bytes_be();
        let mut buf = [0u8; 32];
        buf[32 - p_be.len()..].copy_from_slice(&p_be);
        U256::from_be_bytes(buf)
    }

    /// A canonical `U256` whose high byte is below the BN254 modulus's high
    /// byte (`0x30`), exercising the "large but still canonical" branch
    /// without depending on `U256::rem`.
    fn high_canonical_u256() -> U256 {
        let mut buf = [0xFFu8; 32];
        buf[0] = 0x07;
        U256::from_be_bytes(buf)
    }

    #[test]
    fn u256_to_fr_then_fr_to_u256_round_trips_canonical_inputs() {
        // For every x in [0, p), the conversion must be lossless: the limbs
        // are interpreted as a scalar-field element with the same numeric
        // value, and `fr_to_u256` reads back the canonical representative.
        let cases = [
            U256::ZERO,
            U256::from(1u64),
            U256::from(0xCAFE_BABE_u64),
            U256::from(u64::MAX),
            high_canonical_u256(),
            bn254_modulus_as_u256() - U256::ONE,
        ];
        for x in cases {
            let fr = u256_to_fr(x).expect("input below the modulus must convert");
            assert_eq!(
                fr_to_u256(fr),
                x,
                "round-trip lost information for input {x}",
            );
        }
    }

    #[test]
    fn u256_to_fr_rejects_non_canonical_inputs() {
        let p = bn254_modulus_as_u256();
        assert!(u256_to_fr(p).is_none(), "p itself must be rejected");
        assert!(
            u256_to_fr(p + U256::ONE).is_none(),
            "p + 1 must be rejected",
        );
        assert!(
            u256_to_fr(U256::MAX).is_none(),
            "U256::MAX is far above p and must be rejected",
        );
    }

    #[test]
    fn u64_to_fr_matches_u256_round_trip() {
        // `u64_to_fr` should agree with the `U256` path for any `u64`,
        // because every `u64` is far below the BN254 modulus.
        for x in [0u64, 1, 42, 0xDEAD_BEEF, u64::MAX] {
            let fr_via_u64 = u64_to_fr(x);
            let fr_via_u256 = u256_to_fr(U256::from(x)).expect("u64 is canonical");
            assert_eq!(fr_via_u64, fr_via_u256);
            assert_eq!(fr_to_u256(fr_via_u64), U256::from(x));
        }
    }

    #[test]
    fn address_to_fr_equals_left_padded_be_bytes() {
        // Address bytes are 160-bit, always strictly below `p`, so the
        // `mod_order` reduction is a no-op and the field element must equal
        // the U256 of the left-padded 32-byte representation.
        let addr = address!("0x1111111111111111111111111111111111111111");
        let fr = address_to_fr(addr);

        let mut buf = [0u8; 32];
        buf[12..32].copy_from_slice(addr.as_slice());
        assert_eq!(fr_to_u256(fr), U256::from_be_bytes(buf));
    }
}
