//! Domain-neutral client-side encryption to an enclave's public X25519 key.
//!
//! Recipe:
//! 1. Ephemeral X25519 keypair.
//! 2. ECDHE shared secret with `recipient_x25519_pub`.
//! 3. `HKDF-SHA256(salt = domain.salt, ikm = shared_secret, info = domain.info)`.
//! 4. ChaCha20Poly1305 with empty AAD.
//!
//! This module contains zero enclave-resident secrets and zero domain knowledge.

use crate::errors::TeeCryptoError;

/// Domain-separation parameters for [`tee_encrypt`] and [`tee_encrypt_with`].
#[derive(Clone, Copy, Debug)]
pub struct EncryptDomain<'a> {
    /// Public HKDF-SHA256 salt providing domain separation.
    pub salt: &'a [u8],
    /// Domain-binding HKDF info bytes.
    pub info: &'a [u8],
}

impl<'a> EncryptDomain<'a> {
    /// Construct a new HKDF domain descriptor.
    #[must_use]
    pub const fn new(salt: &'a [u8], info: &'a [u8]) -> Self {
        Self { salt, info }
    }
}

/// Explicit ephemeral key and AEAD nonce for deterministic sealing in tests,
/// benchmarks, and callers that manage their own CSPRNG.
#[derive(Clone, Copy, Debug)]
pub struct ExplicitNonceKey {
    /// 32-byte X25519 ephemeral secret scalar.
    pub ephemeral_secret: [u8; 32],
    /// 12-byte ChaCha20Poly1305 nonce.
    pub nonce: [u8; 12],
}

impl ExplicitNonceKey {
    /// Construct an explicit `(ephemeral_secret, nonce)` pair.
    #[must_use]
    pub const fn new(ephemeral_secret: [u8; 32], nonce: [u8; 12]) -> Self {
        Self {
            ephemeral_secret,
            nonce,
        }
    }
}

/// Sealed envelope produced by [`tee_encrypt`] and [`tee_encrypt_with`].
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

/// Seal `plaintext` to `recipient_x25519_pub` using fresh OS randomness.
pub fn tee_encrypt(
    recipient_x25519_pub: &[u8; 32],
    domain: EncryptDomain<'_>,
    plaintext: &[u8],
) -> Result<TeeSealedEnvelope, TeeCryptoError> {
    let (ephemeral_secret, nonce) = random_ephemeral_and_nonce()?;
    tee_encrypt_with(
        recipient_x25519_pub,
        domain,
        ExplicitNonceKey::new(ephemeral_secret, nonce),
        plaintext,
    )
}

/// Deterministic variant of [`tee_encrypt`] with an explicit ephemeral secret and nonce.
pub fn tee_encrypt_with(
    recipient_x25519_pub: &[u8; 32],
    domain: EncryptDomain<'_>,
    key_nonce: ExplicitNonceKey,
    plaintext: &[u8],
) -> Result<TeeSealedEnvelope, TeeCryptoError> {
    let eph_secret = x25519_dalek::StaticSecret::from(key_nonce.ephemeral_secret);
    let ephemeral_pub = x25519_dalek::PublicKey::from(&eph_secret).to_bytes();
    let shared = eph_secret.diffie_hellman(&x25519_dalek::PublicKey::from(*recipient_x25519_pub));
    let key = hkdf_sha256(domain.salt, shared.as_bytes(), domain.info)?;
    let ciphertext = chacha20poly1305_encrypt(&key, &key_nonce.nonce, plaintext)?;
    Ok(TeeSealedEnvelope {
        ephemeral_pub,
        nonce: key_nonce.nonce,
        ciphertext,
    })
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
        let domain = EncryptDomain::new(b"outbe/test/salt/v1", b"outbe/test/info/v1");
        let plaintext = b"domain-neutral-envelope";

        let sealed = tee_encrypt_with(
            &recipient_pub,
            domain,
            ExplicitNonceKey::new([0x33; 32], [0x44; 12]),
            plaintext,
        )
        .expect("deterministic encrypt");
        assert_eq!(sealed.ciphertext.len(), plaintext.len() + 16);
        assert_eq!(sealed.to_wire_bytes().len(), 32 + 12 + plaintext.len() + 16);

        let shared =
            recipient_sk.diffie_hellman(&x25519_dalek::PublicKey::from(sealed.ephemeral_pub));
        let key = hkdf_sha256(domain.salt, shared.as_bytes(), domain.info).unwrap();
        let opening =
            aead::LessSafeKey::new(aead::UnboundKey::new(&aead::CHACHA20_POLY1305, &key).unwrap());
        let mut opened = sealed.ciphertext.clone();
        let plain = opening
            .open_in_place(
                aead::Nonce::assume_unique_for_key(sealed.nonce),
                aead::Aad::empty(),
                &mut opened,
            )
            .expect("recipient opens sealed envelope");
        assert_eq!(plain, plaintext);

        let wrong_key =
            hkdf_sha256(domain.salt, shared.as_bytes(), b"outbe/test/wrong/v1").unwrap();
        let wrong_opening = aead::LessSafeKey::new(
            aead::UnboundKey::new(&aead::CHACHA20_POLY1305, &wrong_key).unwrap(),
        );
        let mut wrong_buf = sealed.ciphertext.clone();
        assert!(wrong_opening
            .open_in_place(
                aead::Nonce::assume_unique_for_key(sealed.nonce),
                aead::Aad::empty(),
                &mut wrong_buf,
            )
            .is_err());

        let fresh = tee_encrypt(&recipient_pub, domain, plaintext).expect("random encrypt");
        assert_eq!(fresh.ciphertext.len(), sealed.ciphertext.len());
        assert_ne!(fresh.ephemeral_pub, [0u8; 32]);
    }
}
