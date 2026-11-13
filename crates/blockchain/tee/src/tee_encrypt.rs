//! Domain-neutral client-side encryption to an enclave's public X25519 key.
//!
//! Recipe:
//! 1. Ephemeral X25519 keypair.
//! 2. ECDHE shared secret with `recipient_x25519_pub`.
//! 3. `HKDF-SHA256(salt = hkdf_salt, ikm = shared_secret, info = domain_info)`.
//! 4. ChaCha20Poly1305 with empty AAD.
//!
//! This module contains zero enclave-resident secrets and zero domain knowledge.

use crate::errors::TeeCryptoError;

/// Sealed envelope produced by [`tee_encrypt`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeeSealedEnvelope {
    /// 32-byte X25519 ephemeral public key (`ephemeral_secret * G`).
    pub ephemeral_pub: [u8; 32],
    /// 12-byte ChaCha20Poly1305 nonce.
    pub nonce: [u8; 12],
    /// Ciphertext with trailing 16-byte Poly1305 authentication tag.
    pub ciphertext: Vec<u8>,
}

impl TeeSealedEnvelope {
    /// Encode as `ephemeral_pub(32) || nonce(12) || ciphertext_with_tag`.
    #[must_use]
    pub fn to_wire_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(32 + 12 + self.ciphertext.len());
        out.extend_from_slice(&self.ephemeral_pub);
        out.extend_from_slice(&self.nonce);
        out.extend_from_slice(&self.ciphertext);
        out
    }
}

/// Deterministic encryption material and HKDF domain binding for [`tee_encrypt_with`].
#[derive(Clone, Copy, Debug)]
pub struct TeeEncryptMaterial<'a> {
    /// Explicit 32-byte X25519 ephemeral secret scalar.
    pub ephemeral_secret: [u8; 32],
    /// Explicit 12-byte ChaCha20Poly1305 nonce.
    pub nonce: [u8; 12],
    /// HKDF-SHA256 salt.
    pub hkdf_salt: &'a [u8],
    /// HKDF-SHA256 domain separation info string.
    pub domain_info: &'a [u8],
}

/// Seal `plaintext` to `recipient_x25519_pub` using fresh OS randomness.
pub fn tee_encrypt(
    recipient_x25519_pub: &[u8; 32],
    hkdf_salt: &[u8],
    domain_info: &[u8],
    plaintext: &[u8],
) -> Result<TeeSealedEnvelope, TeeCryptoError> {
    let (ephemeral_secret, nonce) = random_ephemeral_and_nonce()?;
    let ephemeral_pub =
        x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(ephemeral_secret))
            .to_bytes();
    let ciphertext = tee_encrypt_with(
        recipient_x25519_pub,
        TeeEncryptMaterial {
            ephemeral_secret,
            nonce,
            hkdf_salt,
            domain_info,
        },
        plaintext,
    )?;
    Ok(TeeSealedEnvelope {
        ephemeral_pub,
        nonce,
        ciphertext,
    })
}

/// Deterministic variant of [`tee_encrypt`] with an explicit ephemeral secret and nonce,
/// returning the raw ChaCha20Poly1305 `ciphertext || tag` bytes.
pub fn tee_encrypt_with(
    recipient_x25519_pub: &[u8; 32],
    material: TeeEncryptMaterial<'_>,
    plaintext: &[u8],
) -> Result<Vec<u8>, TeeCryptoError> {
    let eph_secret = x25519_dalek::StaticSecret::from(material.ephemeral_secret);
    let shared = eph_secret.diffie_hellman(&x25519_dalek::PublicKey::from(*recipient_x25519_pub));
    let key = hkdf_sha256(material.hkdf_salt, shared.as_bytes(), material.domain_info)?;
    chacha20poly1305_encrypt(&key, &material.nonce, plaintext)
}

/// Draw a fresh 32-byte X25519 ephemeral secret and 12-byte AEAD nonce from the OS CSPRNG.
pub fn random_ephemeral_and_nonce() -> Result<([u8; 32], [u8; 12]), TeeCryptoError> {
    use ring::rand::SecureRandom as _;

    let rng = ring::rand::SystemRandom::new();
    let mut eph_sk = [0u8; 32];
    rng.fill(&mut eph_sk).map_err(TeeCryptoError::rng_failure)?;
    let mut nonce = [0u8; 12];
    rng.fill(&mut nonce).map_err(TeeCryptoError::rng_failure)?;
    Ok((eph_sk, nonce))
}

/// HKDF-SHA256 extract + expand to 32 bytes.
pub fn hkdf_sha256(salt: &[u8], ikm: &[u8], info: &[u8]) -> Result<[u8; 32], TeeCryptoError> {
    use ring::hkdf;

    struct Len32;
    impl hkdf::KeyType for Len32 {
        fn len(&self) -> usize {
            32
        }
    }

    let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, salt);
    let prk = salt.extract(ikm);
    let info_refs: &[&[u8]] = &[info];
    let okm = prk
        .expand(info_refs, Len32)
        .map_err(TeeCryptoError::hkdf_expand)?;
    let mut out = [0u8; 32];
    okm.fill(&mut out).map_err(TeeCryptoError::hkdf_fill)?;
    Ok(out)
}

/// ChaCha20Poly1305 AEAD encrypt with empty AAD.
fn chacha20poly1305_encrypt(
    key: &[u8; 32],
    nonce: &[u8; 12],
    plaintext: &[u8],
) -> Result<Vec<u8>, TeeCryptoError> {
    use ring::aead::{self, BoundKey as _};

    struct OneNonce([u8; 12]);
    impl aead::NonceSequence for OneNonce {
        fn advance(&mut self) -> Result<aead::Nonce, ring::error::Unspecified> {
            Ok(aead::Nonce::assume_unique_for_key(self.0))
        }
    }

    let unbound =
        aead::UnboundKey::new(&aead::CHACHA20_POLY1305, key).map_err(TeeCryptoError::aead_key)?;
    let mut sealing = aead::SealingKey::new(unbound, OneNonce(*nonce));
    let mut in_out = plaintext.to_vec();
    sealing
        .seal_in_place_append_tag(aead::Aad::empty(), &mut in_out)
        .map_err(TeeCryptoError::aead_seal)?;
    Ok(in_out)
}

#[cfg(test)]
mod tests {
    use ring::aead;

    use super::*;

    #[test]
    fn tee_encrypt_roundtrips_with_recipient_x25519_secret_and_rejects_wrong_domain() {
        let recipient_sk = x25519_dalek::StaticSecret::from([0x21; 32]);
        let recipient_pub = x25519_dalek::PublicKey::from(&recipient_sk).to_bytes();
        let salt = b"outbe/test/salt/v1";
        let info = b"outbe/test/info/v1";
        let plaintext = b"domain-neutral-envelope";
        let eph_sk_bytes = [0x33; 32];
        let nonce = [0x44; 12];
        let eph_pub =
            x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(eph_sk_bytes))
                .to_bytes();

        let ciphertext = tee_encrypt_with(
            &recipient_pub,
            TeeEncryptMaterial {
                ephemeral_secret: eph_sk_bytes,
                nonce,
                hkdf_salt: salt,
                domain_info: info,
            },
            plaintext,
        )
        .expect("deterministic encrypt");
        assert_eq!(ciphertext.len(), plaintext.len() + 16);

        let shared = recipient_sk.diffie_hellman(&x25519_dalek::PublicKey::from(eph_pub));
        let key = hkdf_sha256(salt, shared.as_bytes(), info).unwrap();
        let opening =
            aead::LessSafeKey::new(aead::UnboundKey::new(&aead::CHACHA20_POLY1305, &key).unwrap());
        let mut opened = ciphertext.clone();
        let plain = opening
            .open_in_place(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::empty(),
                &mut opened,
            )
            .expect("recipient opens sealed envelope");
        assert_eq!(plain, plaintext);

        let wrong_key = hkdf_sha256(salt, shared.as_bytes(), b"outbe/test/wrong/v1").unwrap();
        let wrong_opening = aead::LessSafeKey::new(
            aead::UnboundKey::new(&aead::CHACHA20_POLY1305, &wrong_key).unwrap(),
        );
        let mut wrong_buf = ciphertext.clone();
        assert!(wrong_opening
            .open_in_place(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::empty(),
                &mut wrong_buf,
            )
            .is_err());

        let fresh = tee_encrypt(&recipient_pub, salt, info, plaintext).expect("random encrypt");
        assert_eq!(fresh.ciphertext.len(), ciphertext.len());
        assert_eq!(fresh.to_wire_bytes().len(), 32 + 12 + plaintext.len() + 16);
        assert_ne!(fresh.ephemeral_pub, [0u8; 32]);
    }
}
