//! The legacy DKG envelope and KDF are unchanged; working plaintext zeroizes.
use super::super::{InProcessError, Result};
use crate::resident::crypto_ops::{
    derive, encrypt_share as seal_share, open, shared_legacy, x25519_public,
};
use crate::tee_encrypt::TeeSealedEnvelope;
use zeroize::Zeroizing;
const DKG_SHARE_INFO: &[u8] = b"outbe/tee/dkg-share/v1";
pub type EncryptedShare = TeeSealedEnvelope;
pub(super) fn encrypt_share(recipient: &[u8; 32], plaintext: &[u8]) -> Result<EncryptedShare> {
    Ok(seal_share(recipient, DKG_SHARE_INFO, plaintext)?)
}
pub(super) fn decrypt_share(
    secret: &[u8; 32],
    blob: &EncryptedShare,
) -> Result<Zeroizing<Vec<u8>>> {
    let shared = shared_legacy(secret, &blob.ephemeral_pub);
    let key = derive(&x25519_public(secret), shared.as_ref(), DKG_SHARE_INFO)?;
    Ok(open(&key, &blob.nonce, &blob.ciphertext)?)
}
pub(super) fn decode(bytes: &[u8]) -> Result<EncryptedShare> {
    if bytes.len() < 44 {
        return Err(InProcessError::InvalidEnvelope);
    }
    let mut ephemeral_pub = [0; 32];
    ephemeral_pub.copy_from_slice(&bytes[..32]);
    let mut nonce = [0; 12];
    nonce.copy_from_slice(&bytes[32..44]);
    Ok(EncryptedShare {
        ephemeral_pub,
        nonce,
        ciphertext: bytes[44..].to_vec(),
    })
}
