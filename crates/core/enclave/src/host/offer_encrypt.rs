//! Client-side tribute-offer encryption built over
//! [`outbe_tee::tee_encrypt`].

use outbe_tee::{
    tee_encrypt::{tee_encrypt, tee_encrypt_with, TeeEncryptMaterial},
    TeeCryptoError,
};

use super::errors::LocalCryptoError;

/// Protocol-fixed 32-byte HKDF-SHA256 salt for Tribute offer ECDHE.
///
/// Value: ASCII `"outbe/tribute/offer-salt/v1"`, zero-padded to 32 bytes.
pub const OFFER_HKDF_SALT: [u8; 32] = *b"outbe/tribute/offer-salt/v1\0\0\0\0\0";

/// HKDF info string binding the derived key to the tribute-offer domain.
pub const OFFER_HKDF_INFO: &[u8] = b"tribute-factory-encryption";

/// `(cipher_text, nonce, ephemeral_pub)` produced by [`encrypt_tribute_offer`].
pub type EncryptedOfferParts = (Vec<u8>, [u8; 12], [u8; 32]);

const fn map_encrypt_error(error: TeeCryptoError) -> LocalCryptoError {
    match error {
        TeeCryptoError::RngFailure => LocalCryptoError::OfferEncryptionRngFailure,
        TeeCryptoError::HkdfExpand => LocalCryptoError::HkdfExpand,
        TeeCryptoError::HkdfFill => LocalCryptoError::HkdfFill,
        TeeCryptoError::AeadKey => LocalCryptoError::AeadKey,
        TeeCryptoError::AeadSeal
        | TeeCryptoError::InvalidEnvelope
        | TeeCryptoError::DecryptionFailed
        | TeeCryptoError::InvalidPlaintextLength { .. } => LocalCryptoError::AeadSeal,
    }
}

/// Encrypt `plaintext` to `offer_pub` with a fresh ephemeral key and nonce.
/// Returns `(cipher_text, nonce, ephemeral_pub)`.
pub fn encrypt_tribute_offer(
    offer_pub: &[u8; 32],
    plaintext: &[u8],
) -> Result<EncryptedOfferParts, LocalCryptoError> {
    let sealed = tee_encrypt(offer_pub, &OFFER_HKDF_SALT, OFFER_HKDF_INFO, plaintext)
        .map_err(map_encrypt_error)?;
    Ok((sealed.ciphertext, sealed.nonce, sealed.ephemeral_pub))
}

/// Deterministic variant with an explicit ephemeral secret and nonce.
pub fn encrypt_tribute_offer_with(
    offer_pub: &[u8; 32],
    eph_sk: [u8; 32],
    nonce: [u8; 12],
    plaintext: &[u8],
) -> Result<Vec<u8>, LocalCryptoError> {
    tee_encrypt_with(
        offer_pub,
        TeeEncryptMaterial {
            ephemeral_secret: eph_sk,
            nonce,
            hkdf_salt: &OFFER_HKDF_SALT,
            domain_info: OFFER_HKDF_INFO,
        },
        plaintext,
    )
    .map_err(map_encrypt_error)
}

/// The known-plaintext canary/bench offer payload: 1 SU hash and ISO 4217 code 999.
#[must_use]
pub fn canary_offer_json(worldwide_day: u32) -> String {
    format!(
        r#"{{
    "creator": "canary",
    "tribute_draft_id": "0x1111111111111111111111111111111111111111111111111111111111111111",
    "worldwide_day": {worldwide_day},
    "currency": 999,
    "amount_base": "100",
    "amount_micro": "0",
    "su_hashes": ["0x2222222222222222222222222222222222222222222222222222222222222222"]
}}"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canary_offer_json_names_no_currency() {
        let payload: serde_json::Value =
            serde_json::from_str(&canary_offer_json(20250115)).expect("valid JSON");
        assert_eq!(payload["currency"], 999);
    }

    #[test]
    fn deterministic_and_random_variants_agree() {
        let offer_pub =
            x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from([7u8; 32])).to_bytes();
        let plaintext = canary_offer_json(20250115);
        let ct = encrypt_tribute_offer_with(&offer_pub, [9u8; 32], [1u8; 12], plaintext.as_bytes())
            .expect("encrypt");
        assert_eq!(ct.len(), plaintext.len() + 16);
        let again =
            encrypt_tribute_offer_with(&offer_pub, [9u8; 32], [1u8; 12], plaintext.as_bytes())
                .expect("encrypt");
        assert_eq!(ct, again, "deterministic variant is reproducible");

        let (ct2, _nonce, eph_pub) =
            encrypt_tribute_offer(&offer_pub, plaintext.as_bytes()).expect("encrypt random");
        assert_eq!(ct2.len(), ct.len());
        assert_ne!(eph_pub, [0u8; 32]);
    }
}
