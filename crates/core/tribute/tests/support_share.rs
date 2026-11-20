//! Deterministic share seal for frozen golden vectors.
use outbe_core_common::confidential_tee::{
    chacha20poly1305_encrypt, hkdf_sha256, EncryptedShare, TeeError,
};
use x25519_dalek::{PublicKey, StaticSecret};

const DKG_SHARE_INFO: &[u8] = b"outbe/tee/dkg-share/v1";
const REGISTRY_NONCE_INFO: &[u8] = b"outbe/tee/registry-seal-nonce/v1";

pub fn encrypt_share_deterministic(
    sender_static_secret: &[u8; 32],
    recipient_pub: &[u8; 32],
    plaintext: &[u8],
) -> Result<EncryptedShare, TeeError> {
    let sender = StaticSecret::from(*sender_static_secret);
    let sender_pub = PublicKey::from(&sender).to_bytes();
    let shared = sender.diffie_hellman(&PublicKey::from(*recipient_pub));
    let key = hkdf_sha256(recipient_pub, shared.as_bytes(), DKG_SHARE_INFO)?;
    let nonce_okm = hkdf_sha256(recipient_pub, shared.as_bytes(), REGISTRY_NONCE_INFO)?;
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&nonce_okm[..12]);
    let ciphertext = chacha20poly1305_encrypt(&key, &nonce, plaintext)?;
    Ok(EncryptedShare {
        ephemeral_pub: sender_pub,
        nonce,
        ciphertext,
    })
}
