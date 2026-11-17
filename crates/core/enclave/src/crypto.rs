//! Pure enclave cryptography shared by the sidecar and domain consumers.
//!
//! Server offer operations borrow opaque resident handles. Generic AEAD/HKDF
//! helpers remain for owner-local interoperability and fixed golden fixtures.

use ring::{
    aead,
    hkdf::{self, KeyType},
};
use zeroize::Zeroizing;

use alloy_primitives::{Address, B256};
use outbe_tee::resident::OfferKeyHandle;

use crate::errors::{Result, TeeError};

/// HKDF info label for the offer encryption key. MUST match
/// `crate::offer_encrypt::OFFER_HKDF_INFO` for byte-identical decryption.
const OFFER_AEAD_INFO: &[u8] = b"tribute-factory-encryption";
/// Single-use nonce provider for ring's `BoundKey` API. Replicated locally
/// (mirrors `outbe_primitives::crypto::OneNonce`) to keep enclave dependencies
/// minimal for reproducible `MRENCLAVE`.
pub(crate) struct OneNonce([u8; 12]);

impl OneNonce {
    pub(crate) fn new(nonce: [u8; 12]) -> Self {
        Self(nonce)
    }
}

impl aead::NonceSequence for OneNonce {
    fn advance(&mut self) -> core::result::Result<aead::Nonce, ring::error::Unspecified> {
        Ok(aead::Nonce::assume_unique_for_key(self.0))
    }
}

/// `KeyType` for 32-byte HKDF output.
struct Len32;
impl KeyType for Len32 {
    fn len(&self) -> usize {
        32
    }
}

/// HKDF-SHA256 extract + expand to 32 bytes.
pub fn hkdf_sha256(salt: &[u8], ikm: &[u8], info: &[u8]) -> Result<[u8; 32]> {
    let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, salt);
    let prk = salt.extract(ikm);
    let info_refs: &[&[u8]] = &[info];
    let okm = prk
        .expand(info_refs, Len32)
        .map_err(|_| TeeError::HkdfFailed)?;
    let mut out = [0u8; 32];
    okm.fill(&mut out).map_err(|_| TeeError::HkdfFailed)?;
    Ok(out)
}

/// ChaCha20Poly1305 AEAD decrypt (empty AAD), matching host semantics.
pub fn chacha20poly1305_decrypt(
    key: &[u8; 32],
    nonce: &[u8; 12],
    ciphertext: &[u8],
) -> Result<Vec<u8>> {
    use aead::BoundKey;
    let unbound = aead::UnboundKey::new(&aead::CHACHA20_POLY1305, key)
        .map_err(|_| TeeError::DecryptFailed)?;
    let mut opening = aead::OpeningKey::new(unbound, OneNonce::new(*nonce));
    let mut in_out = ciphertext.to_vec();
    let plaintext = opening
        .open_in_place(aead::Aad::empty(), &mut in_out)
        .map_err(|_| TeeError::DecryptFailed)?;
    Ok(plaintext.to_vec())
}

/// ChaCha20Poly1305 AEAD encrypt (empty AAD). The client/host and tests use it
/// to produce ciphertext. The enclave itself only decrypts offers.
pub fn chacha20poly1305_encrypt(
    key: &[u8; 32],
    nonce: &[u8; 12],
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    use aead::BoundKey;
    let unbound = aead::UnboundKey::new(&aead::CHACHA20_POLY1305, key)
        .map_err(|_| TeeError::EncryptFailed)?;
    let mut sealing = aead::SealingKey::new(unbound, OneNonce::new(*nonce));
    let mut in_out = plaintext.to_vec();
    sealing
        .seal_in_place_append_tag(aead::Aad::empty(), &mut in_out)
        .map_err(|_| TeeError::EncryptFailed)?;
    Ok(in_out)
}

/// Offer decryption primitive: ECDHE(static_secret, ephemeral_pubkey) ->
/// HKDF-SHA256(salt, shared, info) -> ChaCha20Poly1305 decrypt.
///
/// This is the inverse of `crate::offer_encrypt::encrypt_tribute_offer`.
/// This function returns raw plaintext. Payload parsing is a later slice.
pub fn ecdhe_tribute_offer_decrypt(
    tribute_offer_private_key: &OfferKeyHandle,
    salt: &[u8; 32],
    ephemeral_pubkey: &[u8; 32],
    nonce: &[u8],
    ciphertext: &[u8],
) -> Result<Vec<u8>> {
    ecdhe_offer_decrypt(
        tribute_offer_private_key,
        (salt, OFFER_AEAD_INFO),
        ephemeral_pubkey,
        nonce,
        ciphertext,
    )
}

/// Decrypt a 92-byte `ephemeral_pub(32) || nonce(12) || ciphertext(48)` amount
/// envelope sealed via [`crate::offer_encrypt::encrypt_tee_amount`].
///
/// The key uses [`crate::offer_encrypt::promis_mine_key_info`] and
/// ChaCha20Poly1305 with empty AAD.
pub fn decrypt_tee_amount(
    tribute_offer_private_key: &OfferKeyHandle,
    encrypted_amount: &[u8],
    chain_id: B256,
    account: Address,
) -> Result<alloy_primitives::U256> {
    if encrypted_amount.len() != crate::offer_encrypt::ENCRYPTED_TEE_AMOUNT_LEN {
        return Err(TeeError::DecryptFailed);
    }
    let mut eph_pub = [0u8; 32];
    eph_pub.copy_from_slice(&encrypted_amount[..32]);
    let info = crate::offer_encrypt::promis_mine_key_info(chain_id, account);
    let plaintext = Zeroizing::new(ecdhe_offer_decrypt(
        tribute_offer_private_key,
        (&crate::OFFER_HKDF_SALT, &info),
        &eph_pub,
        &encrypted_amount[32..44],
        &encrypted_amount[44..],
    )?);
    let bytes: [u8; 32] = plaintext
        .as_slice()
        .try_into()
        .map_err(|_| TeeError::DecryptFailed)?;
    Ok(alloy_primitives::U256::from_be_bytes(bytes))
}

fn ecdhe_offer_decrypt(
    tribute_offer_private_key: &OfferKeyHandle,
    (salt, info): (&[u8; 32], &[u8]),
    ephemeral_pubkey: &[u8; 32],
    nonce: &[u8],
    ciphertext: &[u8],
) -> Result<Vec<u8>> {
    if nonce.len() != 12 {
        return Err(TeeError::InvalidNonce(nonce.len()));
    }
    let nonce_arr: &[u8; 12] = nonce
        .try_into()
        .map_err(|_| TeeError::InvalidNonce(nonce.len()))?;
    Ok(tribute_offer_private_key
        .ecdhe_decrypt(salt, info, ephemeral_pubkey, nonce_arr, ciphertext)?
        .to_vec())
}
