//! Owner-local opening of context-bound ciphertexts. Domain adapters select
//! the record context, key inputs, nonce domain, envelope and result type.

use crate::offer_encrypt::hkdf_sha256;
use ring::aead;
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroizing;

pub(crate) struct LocalOpen<'a> {
    pub key: &'a [u8; 32],
    pub nonce_context: &'a [u8],
    pub nonce_info: &'a [u8],
    pub ciphertext: &'a [u8],
    pub invalid_key_error: &'static str,
    pub decryption_error: &'static str,
}

pub(crate) struct OpenedLocalBytes {
    bytes: Zeroizing<Vec<u8>>,
    plaintext_len: usize,
}

impl OpenedLocalBytes {
    pub(crate) fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.plaintext_len]
    }
}

pub(crate) fn open_local_ciphertext(input: LocalOpen<'_>) -> Result<OpenedLocalBytes, String> {
    let nonce_material = Zeroizing::new(hkdf_sha256(
        input.key,
        input.nonce_context,
        input.nonce_info,
    )?);
    let mut nonce = [0_u8; 12];
    nonce.copy_from_slice(&nonce_material[..12]);
    let opening = aead::LessSafeKey::new(
        aead::UnboundKey::new(&aead::CHACHA20_POLY1305, input.key)
            .map_err(|_| input.invalid_key_error)?,
    );
    let mut bytes = Zeroizing::new(input.ciphertext.to_vec());
    let plaintext_len = opening
        .open_in_place(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::empty(),
            &mut bytes,
        )
        .map_err(|_| input.decryption_error)?
        .len();
    Ok(OpenedLocalBytes {
        bytes,
        plaintext_len,
    })
}

// Owner amount records use the same slot, field tag and first-counter nonce input.
pub(crate) fn owner_amount_nonce_context(crypto_slot: &[u8]) -> Vec<u8> {
    let mut input = crypto_slot.to_vec();
    input.push(0);
    input.extend_from_slice(&1u64.to_be_bytes());
    input
}

pub(crate) fn owner_x25519_shared(
    owner_secret: &[u8; 32],
    enclave_public: &[u8; 32],
) -> Result<([u8; 32], Zeroizing<[u8; 32]>), String> {
    let secret = StaticSecret::from(*owner_secret);
    let owner_public = PublicKey::from(&secret).to_bytes();
    let shared = secret.diffie_hellman(&PublicKey::from(*enclave_public));
    if !shared.was_contributory() {
        return Err("invalid network X25519 public key".into());
    }
    Ok((owner_public, Zeroizing::new(*shared.as_bytes())))
}

/// Domain inputs for an immutable owner amount with its first-counter envelope.
/// Check the record encoding before this input is passed to the opener.
pub(crate) struct OwnerAmountBlob<'a> {
    pub context_digest: &'a [u8],
    pub key_info: &'a [u8],
    pub crypto_slot: &'a [u8],
    pub nonce_info: &'a [u8],
    pub blob: &'a [u8],
    pub invalid_key_error: &'static str,
    pub decryption_error: &'static str,
}

/// Derive the immutable amount key and open its first-counter envelope.
pub(crate) fn open_owner_amount_blob(
    shared: &[u8; 32],
    input: OwnerAmountBlob<'_>,
) -> Result<OpenedLocalBytes, String> {
    let key = Zeroizing::new(hkdf_sha256(input.context_digest, shared, input.key_info)?);
    let nonce_context = owner_amount_nonce_context(input.crypto_slot);
    open_local_ciphertext(LocalOpen {
        key: &key,
        nonce_context: &nonce_context,
        nonce_info: input.nonce_info,
        ciphertext: &input.blob[8..],
        invalid_key_error: input.invalid_key_error,
        decryption_error: input.decryption_error,
    })
}
