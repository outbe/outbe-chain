//! Schnorr signatures on Grumpkin, matching the Outbe ownership circuits.
//!
//! A signature is `s ‖ e` (each a 32-byte big-endian value), where
//! `e = blake2s(pedersen_hash([R.x, pk.x, pk.y]).to_be_bytes() ‖ message)`
//! and `s = k − e·sk`. Verification recomputes `R' = s·G + e·pk`.
//!
//! The nonce is derived deterministically from `(sk, message)`, with caller RNG
//! output used only as auxiliary entropy. A broken or constant RNG therefore
//! still yields a per-message-unique nonce. The counter handles zero `k`/`e`/`s`.

use ark_ec::{AffineRepr, CurveGroup};
use ark_ff::{PrimeField, Zero};
use ark_std::rand::Rng;
use ark_std::UniformRand;
use blake2::{Blake2s256, Digest};

use crate::primitive::curve::{base_to_scalar, coords, mul_generator, Affine, Scalar};
use crate::{Error, Fr};

/// Domain tag for the deterministic nonce hash, kept distinct from the
/// challenge hash so the two blake2s invocations can never collide.
const NONCE_DOMAIN: &[u8] = b"outbe/schnorr/nonce/v1";

/// 32-byte big-endian encoding of any field element (no `Vec`).
fn field_to_be32<F: PrimeField>(f: &F) -> [u8; 32] {
    let mut b = [0u8; 32];
    for (i, limb) in f.into_bigint().as_ref().iter().rev().enumerate() {
        b[i * 8..i * 8 + 8].copy_from_slice(&limb.to_be_bytes());
    }
    b
}

/// `pedersen_hash([a, b, c]) = (G0·a + G1·b + G2·c + L·3).x`, scalars taken
/// as the integer values of the inputs (`base_to_scalar`).
pub fn pedersen_hash3(a: Fr, b: Fr, c: Fr) -> Result<Fr, Error> {
    let [g0, g1, g2, l] = pedersen_generators();
    let acc = g0.mul_bigint(base_to_scalar(&a).into_bigint())
        + g1.mul_bigint(base_to_scalar(&b).into_bigint())
        + g2.mul_bigint(base_to_scalar(&c).into_bigint())
        + l.mul_bigint(Scalar::from(3u64).into_bigint());
    let (x, _y) = coords(&acc.into_affine())?;
    Ok(x)
}

/// Challenge bytes: `blake2s(pedersen_hash([R.x, pk.x, pk.y]) ‖ message)`.
fn challenge_bytes(rx: Fr, pk: &Affine, message: &[u8]) -> Result<[u8; 32], Error> {
    let (px, py) = coords(pk)?;
    let pde = field_to_be32(&pedersen_hash3(rx, px, py)?);
    let mut hasher = Blake2s256::new();
    hasher.update(pde);
    hasher.update(message);
    Ok(hasher.finalize().into())
}

/// Verify a 64-byte `s ‖ e` signature over an arbitrary-length `message`,
/// exactly as the in-circuit verifier does.
pub fn verify_bytes(pk: &Affine, signature: &[u8; 64], message: &[u8]) -> bool {
    // Reject zero s / e (the circuit's null-signature guard).
    if signature[0..32].iter().all(|&b| b == 0) || signature[32..64].iter().all(|&b| b == 0) {
        return false;
    }
    let s = Scalar::from_be_bytes_mod_order(&signature[0..32]);
    let e = Scalar::from_be_bytes_mod_order(&signature[32..64]);

    // R' = s·G + e·pk.
    let r = (mul_generator(&s) + pk.mul_bigint(e.into_bigint())).into_affine();
    let Ok((rx, _)) = coords(&r) else {
        return false; // r is the identity
    };
    let Ok(challenge) = challenge_bytes(rx, pk, message) else {
        return false;
    };
    challenge == signature[32..64]
}

/// Derive the per-signature nonce `k` deterministically from `(sk, message)`,
/// hedged with `aux` and disambiguated by `counter`.
/// Reduces a 32-byte blake2s digest into the scalar field.
fn derive_nonce(sk: &Scalar, message: &[u8], aux: &[u8; 32], counter: u32) -> Scalar {
    let mut hasher = Blake2s256::new();
    hasher.update(NONCE_DOMAIN);
    hasher.update(field_to_be32(sk));
    hasher.update(aux);
    hasher.update(counter.to_be_bytes());
    hasher.update(message);
    Scalar::from_be_bytes_mod_order(&hasher.finalize())
}

/// Sign an arbitrary-length `message`, producing the 64-byte `s ‖ e` form.
pub fn sign_bytes<R: Rng>(rng: &mut R, sk: &Scalar, message: &[u8]) -> Result<[u8; 64], Error> {
    let pk = mul_generator(sk).into_affine();
    // Auxiliary entropy hedge — folded into the nonce hash, never relied on.
    let mut aux = [0u8; 32];
    rng.fill_bytes(&mut aux);
    let mut counter = 0u32;
    loop {
        let k = derive_nonce(sk, message, &aux, counter);
        counter = counter.wrapping_add(1);
        if k.is_zero() {
            continue;
        }
        let r = mul_generator(&k).into_affine();
        let Ok((rx, _)) = coords(&r) else {
            continue;
        };

        let e_bytes = challenge_bytes(rx, &pk, message)?;
        let e = Scalar::from_be_bytes_mod_order(&e_bytes);
        if e.is_zero() {
            continue;
        }
        // Verify recomputes R' = s·G + e·pk = (s + e·sk)·G and needs R' = R,
        // so s = k − e·sk.
        let s = k - e * *sk;
        if s.is_zero() {
            continue;
        }
        let mut sig = [0u8; 64];
        sig[0..32].copy_from_slice(&field_to_be32(&s));
        sig[32..64].copy_from_slice(&e_bytes);
        return Ok(sig);
    }
}

/// Sample a signing keypair.
pub fn keypair<R: Rng>(rng: &mut R) -> (Scalar, Affine) {
    let sk = Scalar::rand(rng);
    (sk, mul_generator(&sk).into_affine())
}

/// Public-key affine coordinates as native field elements.
pub fn public_coords(pk: &Affine) -> Result<(Fr, Fr), Error> {
    coords(pk)
}

/// Sign the circuit's 32-byte big-endian form of a field payload.
pub fn sign<R: Rng>(rng: &mut R, sk: &Scalar, message: Fr) -> Result<[u8; 64], Error> {
    sign_bytes(rng, sk, &field_to_be32(&message))
}

/// Verify a signature over a field payload.
pub fn verify(pk: &Affine, message: Fr, sig: &[u8; 64]) -> bool {
    verify_bytes(pk, sig, &field_to_be32(&message))
}

/// `pk = [sk]·G`.
pub fn public_key(sk: &Scalar) -> Result<Affine, Error> {
    Ok(mul_generator(sk).into_affine())
}

// ---- Grumpkin's Pedersen generators (the constants the noir circuits use) ----

/// Decode a 64-char big-endian hex string into a Grumpkin coordinate.
fn grumpkin_fq_from_hex(hex: &str) -> ark_grumpkin::Fq {
    let hex = hex.strip_prefix("0x").unwrap_or(hex);
    assert_eq!(hex.len(), 64, "expected 32-byte hex");
    let mut bytes = [0u8; 32];
    let h = hex.as_bytes();
    for i in 0..32 {
        let hi = (h[2 * i] as char).to_digit(16).expect("hex digit") as u8;
        let lo = (h[2 * i + 1] as char).to_digit(16).expect("hex digit") as u8;
        bytes[i] = (hi << 4) | lo;
    }
    ark_grumpkin::Fq::from_be_bytes_mod_order(&bytes)
}

fn pedersen_generators() -> [Affine; 4] {
    let pt = |x: &str, y: &str| {
        ark_grumpkin::Affine::new_unchecked(grumpkin_fq_from_hex(x), grumpkin_fq_from_hex(y))
    };
    [
        pt(
            "0x083e7911d835097629f0067531fc15cafd79a89beecb39903f69572c636f4a5a",
            "0x1a7f5efaad7f315c25a918f30cc8d7333fccab7ad7c90f14de81bcc528f9935d",
        ),
        pt(
            "0x054aa86a73cb8a34525e5bbed6e43ba1198e860f5f3950268f71df4591bde402",
            "0x209dcfbf2cfb57f9f6046f44d71ac6faf87254afc7407c04eb621a6287cac126",
        ),
        pt(
            "0x1c44f2a5207c81c28a8321a5815ce8b1311024bbed131819bbdaf5a2ada84748",
            "0x03aaee36e6422a1d0191632ac6599ae9eba5ac2c17a8c920aa3caf8b89c5f8a8",
        ),
        pt(
            "0x2df8b940e5890e4e1377e05373fae69a1d754f6935e6a780b666947431f2cdcd",
            "0x2ecd88d15967bc53b885912e0d16866154acb6aac2d3f85e27ca7eefb2c19083",
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The canonical Schnorr known-answer vector the Outbe circuits' verifier is
    /// specified against. If our pure-Rust verify reproduces `true` here, the
    /// whole stack (Pedersen generators + blake2s + Grumpkin EC + byte
    /// conventions) is bit-identical to the in-circuit verifier.
    #[test]
    fn canonical_smoke_test_vector() {
        let pk = ark_grumpkin::Affine::new_unchecked(
            grumpkin_fq_from_hex(
                "0x04b260954662e97f00cab9adb773a259097f7a274b83b113532bce27fa3fb96a",
            ),
            grumpkin_fq_from_hex(
                "0x2fd51571db6c08666b0edfbfbc57d432068bccd0110a39b166ab243da0037197",
            ),
        );
        let signature: [u8; 64] = [
            1, 13, 119, 112, 212, 39, 233, 41, 84, 235, 255, 93, 245, 172, 186, 83, 157, 253, 76,
            77, 33, 128, 178, 15, 214, 67, 105, 107, 177, 234, 77, 48, 27, 237, 155, 84, 39, 84,
            247, 27, 22, 8, 176, 230, 24, 115, 145, 220, 254, 122, 135, 179, 171, 4, 214, 202, 64,
            199, 19, 84, 239, 138, 124, 12,
        ];
        let message: [u8; 10] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9];
        assert!(
            verify_bytes(&pk, &signature, &message),
            "canonical KAT must verify"
        );
    }

    /// `pedersen_hash([1, 2, 3])` must match the circuits' value.
    #[test]
    fn pedersen_matches_circuit() {
        let h = pedersen_hash3(
            ark_grumpkin::Fq::from(1u64),
            ark_grumpkin::Fq::from(2u64),
            ark_grumpkin::Fq::from(3u64),
        )
        .unwrap();
        let expected = grumpkin_fq_from_hex(
            "0x0c21b8e26f60b476d9568df4807131ff70d8b7fffb03fa07960aa1cac9be7c46",
        );
        assert_eq!(h, expected);
    }

    /// Our own sign → verify round-trip (same challenge the circuit checks).
    #[test]
    fn sign_verify_round_trip() {
        let mut rng = ark_std::test_rng();
        let (sk, pk) = keypair(&mut rng);
        let message = b"outbe ownership payload bytes----.";
        let sig = sign_bytes(&mut rng, &sk, message).unwrap();
        assert!(verify_bytes(&pk, &sig, message));
        let mut bad = sig;
        bad[40] ^= 1;
        assert!(!verify_bytes(&pk, &bad, message));
    }
}
