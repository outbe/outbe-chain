//! AEAD, HKDF, and offer decryption shared by every confidential domain.

use alloy_primitives::{Address, B256, U256};
use outbe_tee::resident::OfferKeyHandle;
use ring::{
    aead::{self, BoundKey},
    hkdf::{self, KeyType},
};
use zeroize::Zeroizing;

use super::errors::TeeError;
use super::offer_encrypt::{self, OFFER_HKDF_INFO, OFFER_HKDF_SALT};

struct OneNonce([u8; 12]);

impl OneNonce {
    fn new(nonce: [u8; 12]) -> Self {
        Self(nonce)
    }
}

impl aead::NonceSequence for OneNonce {
    fn advance(&mut self) -> Result<aead::Nonce, ring::error::Unspecified> {
        Ok(aead::Nonce::assume_unique_for_key(self.0))
    }
}

struct Len32;

impl KeyType for Len32 {
    fn len(&self) -> usize {
        32
    }
}

/// HKDF-SHA256 extract and expand to 32 bytes.
pub fn hkdf_sha256(salt: &[u8], ikm: &[u8], info: &[u8]) -> Result<[u8; 32], TeeError> {
    let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, salt);
    let prk = salt.extract(ikm);
    let info_parts = [info];
    let okm = prk
        .expand(&info_parts, Len32)
        .map_err(TeeError::HkdfFailed)?;
    let mut out = [0u8; 32];
    okm.fill(&mut out).map_err(TeeError::HkdfFailed)?;
    Ok(out)
}

/// ChaCha20-Poly1305 decrypt with empty additional data.
pub fn chacha20poly1305_decrypt(
    key: &[u8; 32],
    nonce: &[u8; 12],
    ciphertext: &[u8],
) -> Result<Vec<u8>, TeeError> {
    let unbound = aead::UnboundKey::new(&aead::CHACHA20_POLY1305, key)
        .map_err(|err| TeeError::DecryptFailed(Some(err)))?;
    let mut opening = aead::OpeningKey::new(unbound, OneNonce::new(*nonce));
    let mut in_out = ciphertext.to_vec();
    let plaintext = opening
        .open_in_place(aead::Aad::empty(), &mut in_out)
        .map_err(|err| TeeError::DecryptFailed(Some(err)))?;
    Ok(plaintext.to_vec())
}

/// ChaCha20-Poly1305 encrypt with empty additional data.
pub fn chacha20poly1305_encrypt(
    key: &[u8; 32],
    nonce: &[u8; 12],
    plaintext: &[u8],
) -> Result<Vec<u8>, TeeError> {
    let unbound = aead::UnboundKey::new(&aead::CHACHA20_POLY1305, key)
        .map_err(|err| TeeError::EncryptFailed(Some(err)))?;
    let mut sealing = aead::SealingKey::new(unbound, OneNonce::new(*nonce));
    let mut in_out = plaintext.to_vec();
    sealing
        .seal_in_place_append_tag(aead::Aad::empty(), &mut in_out)
        .map_err(|err| TeeError::EncryptFailed(Some(err)))?;
    Ok(in_out)
}

/// Inverse of [`super::offer_encrypt::encrypt_tribute_offer`].
pub fn ecdhe_tribute_offer_decrypt(
    tribute_offer_private_key: &OfferKeyHandle,
    salt: &[u8; 32],
    ephemeral_pubkey: &[u8; 32],
    nonce: &[u8],
    ciphertext: &[u8],
) -> Result<Vec<u8>, TeeError> {
    ecdhe_offer_decrypt(
        tribute_offer_private_key,
        (salt, OFFER_HKDF_INFO),
        ephemeral_pubkey,
        nonce,
        ciphertext,
    )
}

/// Decrypt a 92-byte `ephemeral_pub || nonce || ciphertext` amount envelope.
pub fn decrypt_tee_amount(
    tribute_offer_private_key: &OfferKeyHandle,
    encrypted_amount: &[u8],
    chain_id: B256,
    account: Address,
) -> Result<U256, TeeError> {
    if encrypted_amount.len() != offer_encrypt::ENCRYPTED_TEE_AMOUNT_LEN {
        return Err(TeeError::DecryptFailed(None));
    }
    let mut ephemeral = [0u8; 32];
    ephemeral.copy_from_slice(&encrypted_amount[..32]);
    let info = offer_encrypt::promis_mine_key_info(chain_id, account);
    let plaintext = Zeroizing::new(ecdhe_offer_decrypt(
        tribute_offer_private_key,
        (&OFFER_HKDF_SALT, &info),
        &ephemeral,
        &encrypted_amount[32..44],
        &encrypted_amount[44..],
    )?);
    let bytes: [u8; 32] = plaintext
        .as_slice()
        .try_into()
        .map_err(|_| TeeError::DecryptFailed(None))?;
    Ok(U256::from_be_bytes(bytes))
}

fn ecdhe_offer_decrypt(
    tribute_offer_private_key: &OfferKeyHandle,
    (salt, info): (&[u8; 32], &[u8]),
    ephemeral_pubkey: &[u8; 32],
    nonce: &[u8],
    ciphertext: &[u8],
) -> Result<Vec<u8>, TeeError> {
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

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use super::*;

    #[test]
    fn tampered_ciphertext_downcasts_to_ring_unspecified() {
        let key = [0x11; 32];
        let nonce = [0x22; 12];
        let mut ciphertext = chacha20poly1305_encrypt(&key, &nonce, b"amount").unwrap();
        let last = ciphertext.len() - 1;
        ciphertext[last] ^= 0xff;
        let error = chacha20poly1305_decrypt(&key, &nonce, &ciphertext).unwrap_err();
        assert_eq!(
            error.to_string(),
            "AEAD decryption failed (bad key, nonce, or tampered ciphertext)"
        );
        let source = error.source().expect("ring source");
        assert!(source.downcast_ref::<ring::error::Unspecified>().is_some());
        assert!(TeeError::DecryptFailed(None).source().is_none());
        let round = chacha20poly1305_encrypt(&key, &nonce, b"decode me").unwrap();
        assert_eq!(round.len(), b"decode me".len() + 16);
        assert_eq!(
            chacha20poly1305_decrypt(&key, &nonce, &round).unwrap(),
            b"decode me"
        );
        assert!(chacha20poly1305_decrypt(&key, &[0; 12], &round).is_err());
        assert!(chacha20poly1305_decrypt(&[0; 32], &nonce, &round).is_err());
        let short = ecdhe_tribute_offer_decrypt(
            &OfferKeyHandle::for_test([0; 32]),
            &[0; 32],
            &[0; 32],
            &[0; 11],
            &[],
        )
        .unwrap_err();
        assert!(matches!(short, TeeError::InvalidNonce(11)));
    }
}
