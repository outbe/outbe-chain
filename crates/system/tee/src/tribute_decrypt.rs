//! Creator-local decryption. No enclave secret is used or exported here.

use outbe_primitives::tribute_encryption::{
    EncryptedTributeV2, TributeAmountsV2, TRIBUTE_AMOUNT_NONCE_INFO,
};
use ring::aead;
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroizing;

use crate::offer_encrypt::hkdf_sha256;

/// Open a public Tribute record with the creator's own X25519 private key.
pub fn decrypt_tribute_for_creator(
    creator_secret: &[u8; 32],
    enclave_public: &[u8; 32],
    tribute: &EncryptedTributeV2,
) -> Result<TributeAmountsV2, String> {
    if !tribute.has_valid_encoding() {
        return Err("invalid encrypted Tribute encoding".into());
    }
    let secret = StaticSecret::from(*creator_secret);
    let creator_public = PublicKey::from(&secret).to_bytes();
    let shared = secret.diffie_hellman(&PublicKey::from(*enclave_public));
    if !shared.was_contributory() {
        return Err("invalid network X25519 public key".into());
    }
    let context = &tribute.context;
    let info = context.amount_key_info(
        enclave_public,
        &creator_public,
        &tribute.encrypted_creator_public_key,
    );
    let key = Zeroizing::new(hkdf_sha256(
        context.digest().as_slice(),
        shared.as_bytes(),
        &info,
    )?);
    let mut nonce_input = context.crypto_slot().as_slice().to_vec();
    nonce_input.push(0);
    nonce_input.extend_from_slice(&1u64.to_be_bytes());
    let nonce_bytes = hkdf_sha256(&*key, &nonce_input, TRIBUTE_AMOUNT_NONCE_INFO)?;
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&nonce_bytes[..12]);
    let unbound = aead::UnboundKey::new(&aead::CHACHA20_POLY1305, &*key)
        .map_err(|_| "invalid Tribute amount key")?;
    let opening = aead::LessSafeKey::new(unbound);
    let mut bytes = Zeroizing::new(tribute.encrypted_amounts[8..].to_vec());
    let plaintext = opening
        .open_in_place(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::empty(),
            &mut bytes,
        )
        .map_err(|_| "Tribute amount decryption failed")?;
    let amounts = (&*plaintext)
        .try_into()
        .map_err(|_| "invalid Tribute amount length")?;
    Ok(TributeAmountsV2::from_be_bytes(amounts))
}
