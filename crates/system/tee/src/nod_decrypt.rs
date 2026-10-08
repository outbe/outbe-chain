//! Owner-local NOD decryption using the owner's X25519 private key.
use crate::offer_encrypt::hkdf_sha256;
use alloy_primitives::U256;
use outbe_primitives::nod_encryption::{EncryptedNodV2, NOD_AMOUNT_NONCE_INFO};
use ring::aead;
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroizing;
pub fn decrypt_nod_for_owner(
    secret: &[u8; 32],
    enclave_public: &[u8; 32],
    nod: &EncryptedNodV2,
) -> Result<U256, String> {
    if !nod.has_valid_encoding() {
        return Err("invalid encrypted NOD encoding".into());
    }
    let secret = StaticSecret::from(*secret);
    let public = PublicKey::from(&secret).to_bytes();
    let shared = secret.diffie_hellman(&PublicKey::from(*enclave_public));
    if !shared.was_contributory() {
        return Err("invalid network X25519 public key".into());
    }
    let key = Zeroizing::new(hkdf_sha256(
        nod.context_digest().as_slice(),
        shared.as_bytes(),
        &nod.amount_key_info(enclave_public, &public),
    )?);
    let mut input = nod.crypto_slot().as_slice().to_vec();
    input.push(0);
    input.extend_from_slice(&1u64.to_be_bytes());
    let nonce_bytes = hkdf_sha256(&*key, &input, NOD_AMOUNT_NONCE_INFO)?;
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&nonce_bytes[..12]);
    let opening = aead::LessSafeKey::new(
        aead::UnboundKey::new(&aead::CHACHA20_POLY1305, &*key)
            .map_err(|_| "invalid NOD amount key")?,
    );
    let mut bytes = Zeroizing::new(nod.encrypted_gratis_amount[8..].to_vec());
    let plaintext = opening
        .open_in_place(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::empty(),
            &mut bytes,
        )
        .map_err(|_| "NOD amount decryption failed")?;
    let amount: &[u8; 32] = (&*plaintext)
        .try_into()
        .map_err(|_| "invalid NOD amount length")?;
    Ok(U256::from_be_bytes(*amount))
}
