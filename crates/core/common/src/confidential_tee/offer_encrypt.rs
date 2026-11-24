//! Client-side offer encryption.

use outbe_tee::tee_encrypt::{tee_encrypt, tee_encrypt_with, TeeEncryptMaterial};

use super::errors::{map_offer_encrypt, LocalCryptoError};

/// Protocol-fixed 32-byte HKDF-SHA256 salt for offer ECDHE.
///
/// Value: ASCII `"outbe/tribute/offer-salt/v1"`, zero-padded to 32 bytes.
pub const OFFER_HKDF_SALT: [u8; 32] = *b"outbe/tribute/offer-salt/v1\0\0\0\0\0";

/// HKDF info string binding the derived key to the offer domain.
pub const OFFER_HKDF_INFO: &[u8] = b"tribute-factory-encryption";

/// `(cipher_text, nonce, ephemeral_pub)` produced by [`encrypt_offer`].
pub type EncryptedOfferParts = (Vec<u8>, [u8; 12], [u8; 32]);

/// Encrypt `plaintext` to `offer_pub`. Returns `(cipher_text, nonce, ephemeral_pub)`.
pub fn encrypt_offer(
    offer_pub: &[u8; 32],
    plaintext: &[u8],
) -> Result<EncryptedOfferParts, LocalCryptoError> {
    let sealed = tee_encrypt(offer_pub, &OFFER_HKDF_SALT, OFFER_HKDF_INFO, plaintext)
        .map_err(map_offer_encrypt)?;
    Ok((sealed.ciphertext, sealed.nonce, sealed.ephemeral_pub))
}

/// Deterministic variant of [`encrypt_offer`].
pub fn encrypt_offer_with(
    offer_pub: &[u8; 32],
    ephemeral_secret: [u8; 32],
    nonce: [u8; 12],
    plaintext: &[u8],
) -> Result<Vec<u8>, LocalCryptoError> {
    tee_encrypt_with(
        offer_pub,
        TeeEncryptMaterial {
            ephemeral_secret,
            nonce,
            hkdf_salt: &OFFER_HKDF_SALT,
            domain_info: OFFER_HKDF_INFO,
        },
        plaintext,
    )
    .map_err(map_offer_encrypt)
}

/// Known-plaintext canary offer payload: 1 SU hash and ISO 4217 code 999.
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
            serde_json::from_str(&canary_offer_json(20_250_115)).expect("valid JSON");
        assert_eq!(payload["currency"], 999);
    }

    #[test]
    fn deterministic_and_random_variants_agree() {
        let offer_pub =
            x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from([7u8; 32])).to_bytes();
        let plaintext = canary_offer_json(20_250_115);
        let ciphertext = encrypt_offer_with(&offer_pub, [9u8; 32], [1u8; 12], plaintext.as_bytes())
            .expect("encrypt");
        assert_eq!(ciphertext.len(), plaintext.len() + 16);
        let again = encrypt_offer_with(&offer_pub, [9u8; 32], [1u8; 12], plaintext.as_bytes())
            .expect("encrypt");
        assert_eq!(ciphertext, again);
        let ephemeral_pub =
            x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from([9u8; 32])).to_bytes();
        let opened = super::super::crypto::ecdhe_offer_decrypt(
            &outbe_tee::resident::OfferKeyHandle::for_test([7u8; 32]),
            &OFFER_HKDF_SALT,
            &ephemeral_pub,
            &[1u8; 12],
            &ciphertext,
        )
        .expect("offer decrypt");
        assert_eq!(opened, plaintext.as_bytes());
        let (random, _nonce, ephemeral) =
            encrypt_offer(&offer_pub, plaintext.as_bytes()).expect("encrypt random");
        assert_eq!(random.len(), ciphertext.len());
        assert_ne!(ephemeral, [0u8; 32]);
    }

    #[test]
    fn ecdhe_offer_roundtrip_with_explicit_salt() {
        use super::super::crypto::{chacha20poly1305_encrypt, ecdhe_offer_decrypt, hkdf_sha256};
        let offer_pub =
            x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from([7u8; 32])).to_bytes();
        let custom_salt = [3u8; 32];
        assert_ne!(custom_salt, OFFER_HKDF_SALT);
        let plaintext = b"{\"hello\":\"offer\"}";
        let shared = x25519_dalek::StaticSecret::from([9u8; 32])
            .diffie_hellman(&x25519_dalek::PublicKey::from(offer_pub));
        let key = hkdf_sha256(&custom_salt, shared.as_bytes(), OFFER_HKDF_INFO).expect("hkdf");
        let ephemeral_pub =
            x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from([9u8; 32])).to_bytes();
        let custom_ct = chacha20poly1305_encrypt(&key, &[1u8; 12], plaintext).expect("encrypt");
        let opened = ecdhe_offer_decrypt(
            &outbe_tee::resident::OfferKeyHandle::for_test([7u8; 32]),
            &custom_salt,
            &ephemeral_pub,
            &[1u8; 12],
            &custom_ct,
        )
        .expect("custom salt");
        assert_eq!(opened, plaintext);
        assert!(ecdhe_offer_decrypt(
            &outbe_tee::resident::OfferKeyHandle::for_test([7u8; 32]),
            &OFFER_HKDF_SALT,
            &ephemeral_pub,
            &[1u8; 12],
            &custom_ct,
        )
        .is_err());
    }
}
