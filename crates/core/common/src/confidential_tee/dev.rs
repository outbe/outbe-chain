//! Deterministic development keys and the historical sealed-share codec.
//!
//! These fixtures are compiled for tests and the `dev-keys` feature. They are
//! not part of a release build.

use alloy_primitives::{B256, U256};
use x25519_dalek::{PublicKey, StaticSecret};

use super::crypto::chacha20poly1305_decrypt;
#[cfg(feature = "dev-keys")]
use super::crypto::chacha20poly1305_encrypt;
use super::crypto::hkdf_sha256;
use super::errors::TeeError;

/// Shared dev group signature for the fidelity test-enclave key.
pub const FIDELITY_GROUP_SIG: &[u8] = b"outbe-dev-fidelity-group-signature-fixed-seed";
/// Shared dev chain id for the fidelity test-enclave key.
pub const FIDELITY_CHAIN_ID: u64 = outbe_primitives::chain::DEVNET_CHAIN_ID;
/// Shared dev group signature for the gratis test-enclave key.
pub const GRATIS_GROUP_SIG: &[u8] = b"outbe-dev-gratis-group-signature-fixed-seed!!";
/// Shared dev chain id for the gratis test-enclave key.
pub const GRATIS_CHAIN: B256 = B256::repeat_byte(0xC1);
/// Shared dev X25519 secret for encrypted amount envelopes in test-enclave mode.
pub const OFFER_SECRET_KEY: [u8; 32] = [0x42; 32];

const DKG_SHARE_INFO: &[u8] = b"outbe/tee/dkg-share/v1";

/// Result of a development share seal or open.
pub type CryptoResult<T> = Result<T, TeeError>;

/// A DKG share sealed to a recipient X25519 key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptedShare {
    /// Ephemeral public key that sealed the share.
    pub ephemeral_pub: [u8; 32],
    /// ChaCha20-Poly1305 nonce.
    pub nonce: [u8; 12],
    /// Ciphertext including the authentication tag.
    pub ciphertext: Vec<u8>,
}

impl EncryptedShare {
    /// Flat wire encoding `ephemeral_pub(32) || nonce(12) || ciphertext`.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(44 + self.ciphertext.len());
        out.extend_from_slice(&self.ephemeral_pub);
        out.extend_from_slice(&self.nonce);
        out.extend_from_slice(&self.ciphertext);
        out
    }

    /// Decode the flat wire encoding produced by [`Self::to_bytes`].
    pub fn from_bytes(bytes: &[u8]) -> CryptoResult<Self> {
        if bytes.len() < 44 {
            return Err(TeeError::DecryptFailed(None));
        }
        let mut ephemeral_pub = [0u8; 32];
        ephemeral_pub.copy_from_slice(&bytes[..32]);
        let mut nonce = [0u8; 12];
        nonce.copy_from_slice(&bytes[32..44]);
        Ok(Self {
            ephemeral_pub,
            nonce,
            ciphertext: bytes[44..].to_vec(),
        })
    }
}

/// The fidelity dev chain id in the runtime's `B256` encoding.
#[must_use]
pub fn fidelity_chain() -> B256 {
    B256::from(U256::from(FIDELITY_CHAIN_ID))
}

/// Public key corresponding to [`OFFER_SECRET_KEY`].
#[must_use]
pub fn offer_public_key() -> [u8; 32] {
    offer_key().public()
}

/// Resident material for deterministic development and golden-vector fixtures.
pub fn offer_material(
    group_signature: &[u8],
    chain_id: B256,
) -> outbe_tee::resident::ResidentOfferMaterial {
    outbe_tee::resident::ResidentOfferMaterial::for_test(group_signature.to_vec(), chain_id, 0, 0)
        .expect("fixed development key material")
}

/// Fixed development offer handle.
#[must_use]
pub fn offer_key() -> outbe_tee::resident::OfferKeyHandle {
    outbe_tee::resident::OfferKeyHandle::for_test(OFFER_SECRET_KEY)
}

/// Seal `plaintext` to `recipient_pub` with a fresh ephemeral X25519 key.
///
/// `OsRng` draws transport randomness only. The opened plaintext is the bytes
/// passed in. The ciphertext is not a consensus value.
#[cfg(feature = "dev-keys")]
pub fn encrypt_share(recipient_pub: &[u8; 32], plaintext: &[u8]) -> CryptoResult<EncryptedShare> {
    use rand_core::RngCore;
    use zeroize::Zeroize;
    let mut ephemeral_secret = [0u8; 32];
    rand_core::OsRng.fill_bytes(&mut ephemeral_secret);
    let mut nonce = [0u8; 12];
    rand_core::OsRng.fill_bytes(&mut nonce);

    let eph_secret = StaticSecret::from(ephemeral_secret);
    ephemeral_secret.zeroize();
    let ephemeral_pub = PublicKey::from(&eph_secret).to_bytes();
    let shared = eph_secret.diffie_hellman(&PublicKey::from(*recipient_pub));
    let key = hkdf_sha256(recipient_pub, shared.as_bytes(), DKG_SHARE_INFO)?;
    let ciphertext = chacha20poly1305_encrypt(&key, &nonce, plaintext)?;
    Ok(EncryptedShare {
        ephemeral_pub,
        nonce,
        ciphertext,
    })
}

/// Open a share sealed by [`encrypt_share`] with the recipient's X25519 secret.
pub fn decrypt_share(recipient_secret: &[u8; 32], enc: &EncryptedShare) -> CryptoResult<Vec<u8>> {
    let secret = StaticSecret::from(*recipient_secret);
    let recipient_pub = PublicKey::from(&secret).to_bytes();
    let shared = secret.diffie_hellman(&PublicKey::from(enc.ephemeral_pub));
    let key = hkdf_sha256(&recipient_pub, shared.as_bytes(), DKG_SHARE_INFO)?;
    chacha20poly1305_decrypt(&key, &enc.nonce, &enc.ciphertext)
}

#[cfg(all(test, feature = "dev-keys"))]
mod tests {
    use super::*;

    #[test]
    fn fresh_share_opens_with_the_recipient_secret() {
        let secret = [0x21; 32];
        let public = PublicKey::from(&StaticSecret::from(secret)).to_bytes();
        let sealed = encrypt_share(&public, b"share-bytes").unwrap();
        let opened = decrypt_share(&secret, &sealed).unwrap();
        assert_eq!(opened, b"share-bytes");
        assert_eq!(
            EncryptedShare::from_bytes(&sealed.to_bytes()).unwrap(),
            sealed
        );
    }
}
