//! Client-side tribute-offer and Promis `mineGratis` encryption.

use alloy_primitives::{Address, B256, U256};
use outbe_tee::tee_encrypt::{tee_encrypt, tee_encrypt_with, TeeEncryptMaterial};

use super::errors::{map_offer_encrypt, LocalCryptoError};

/// Protocol-fixed 32-byte HKDF-SHA256 salt for Tribute offer and confidential amount ECDHE.
///
/// Value: ASCII `"outbe/tribute/offer-salt/v1"`, zero-padded to 32 bytes.
pub const OFFER_HKDF_SALT: [u8; 32] = *b"outbe/tribute/offer-salt/v1\0\0\0\0\0";

/// HKDF info string binding the derived key to the tribute-offer domain.
pub const OFFER_HKDF_INFO: &[u8] = b"tribute-factory-encryption";

/// HKDF info binding a `mineGratis` amount to the Promis conversion domain.
pub const PROMIS_MINE_HKDF_INFO: &[u8] = b"outbe/promis/mine-gratis/v1";

/// Builds the HKDF info binding `encryptedPromisMinor` to `(domain, chain_id, account)`.
#[must_use]
pub fn promis_mine_key_info(chain_id: B256, account: Address) -> Vec<u8> {
    let mut info = PROMIS_MINE_HKDF_INFO.to_vec();
    info.extend_from_slice(chain_id.as_slice());
    info.extend_from_slice(account.as_slice());
    info
}

/// Wire size of a `U256` amount encrypted to the enclave X25519 offer key.
pub const ENCRYPTED_TEE_AMOUNT_LEN: usize = 32 + 12 + 32 + 16;

/// `(cipher_text, nonce, ephemeral_pub)` produced by [`encrypt_tribute_offer`].
pub type EncryptedOfferParts = (Vec<u8>, [u8; 12], [u8; 32]);

/// Encrypt a `U256` `mineGratis` amount to `tee_pub`.
pub fn encrypt_tee_amount(
    tee_pub: &[u8; 32],
    chain_id: B256,
    account: Address,
    amount: U256,
) -> Result<[u8; ENCRYPTED_TEE_AMOUNT_LEN], LocalCryptoError> {
    let info = promis_mine_key_info(chain_id, account);
    let sealed = tee_encrypt(
        tee_pub,
        &OFFER_HKDF_SALT,
        &info,
        &amount.to_be_bytes::<32>(),
    )
    .map_err(map_offer_encrypt)?;
    sealed
        .to_wire_bytes()
        .try_into()
        .map_err(|_| LocalCryptoError::AeadOutputLength)
}

/// Deterministic variant of [`encrypt_tee_amount`].
pub fn encrypt_tee_amount_with(
    tee_pub: &[u8; 32],
    (chain_id, account): (B256, Address),
    amount: U256,
    ephemeral_secret: [u8; 32],
    nonce: [u8; 12],
) -> Result<[u8; ENCRYPTED_TEE_AMOUNT_LEN], LocalCryptoError> {
    let info = promis_mine_key_info(chain_id, account);
    let ephemeral_pub =
        x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(ephemeral_secret))
            .to_bytes();
    let ciphertext = tee_encrypt_with(
        tee_pub,
        TeeEncryptMaterial {
            ephemeral_secret,
            nonce,
            hkdf_salt: &OFFER_HKDF_SALT,
            domain_info: &info,
        },
        &amount.to_be_bytes::<32>(),
    )
    .map_err(map_offer_encrypt)?;
    pack_amount(&ephemeral_pub, &nonce, &ciphertext)
}

fn pack_amount(
    ephemeral_pub: &[u8; 32],
    nonce: &[u8; 12],
    ciphertext: &[u8],
) -> Result<[u8; ENCRYPTED_TEE_AMOUNT_LEN], LocalCryptoError> {
    if ciphertext.len() != 48 {
        return Err(LocalCryptoError::AeadOutputLength);
    }
    let mut out = [0u8; ENCRYPTED_TEE_AMOUNT_LEN];
    out[..32].copy_from_slice(ephemeral_pub);
    out[32..44].copy_from_slice(nonce);
    out[44..].copy_from_slice(ciphertext);
    Ok(out)
}

/// Encrypt `plaintext` to `offer_pub`. Returns `(cipher_text, nonce, ephemeral_pub)`.
pub fn encrypt_tribute_offer(
    offer_pub: &[u8; 32],
    plaintext: &[u8],
) -> Result<EncryptedOfferParts, LocalCryptoError> {
    let sealed = tee_encrypt(offer_pub, &OFFER_HKDF_SALT, OFFER_HKDF_INFO, plaintext)
        .map_err(map_offer_encrypt)?;
    Ok((sealed.ciphertext, sealed.nonce, sealed.ephemeral_pub))
}

/// Deterministic variant of [`encrypt_tribute_offer`].
pub fn encrypt_tribute_offer_with(
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
        let ciphertext =
            encrypt_tribute_offer_with(&offer_pub, [9u8; 32], [1u8; 12], plaintext.as_bytes())
                .expect("encrypt");
        assert_eq!(ciphertext.len(), plaintext.len() + 16);
        let again =
            encrypt_tribute_offer_with(&offer_pub, [9u8; 32], [1u8; 12], plaintext.as_bytes())
                .expect("encrypt");
        assert_eq!(ciphertext, again);
        let ephemeral_pub =
            x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from([9u8; 32])).to_bytes();
        let opened = super::super::crypto::ecdhe_tribute_offer_decrypt(
            &outbe_tee::resident::OfferKeyHandle::for_test([7u8; 32]),
            &OFFER_HKDF_SALT,
            &ephemeral_pub,
            &[1u8; 12],
            &ciphertext,
        )
        .expect("offer decrypt");
        assert_eq!(opened, plaintext.as_bytes());
        let (random, _nonce, ephemeral) =
            encrypt_tribute_offer(&offer_pub, plaintext.as_bytes()).expect("encrypt random");
        assert_eq!(random.len(), ciphertext.len());
        assert_ne!(ephemeral, [0u8; 32]);
    }

    #[test]
    fn ecdhe_tribute_offer_roundtrip_with_explicit_salt() {
        use super::super::crypto::{
            chacha20poly1305_encrypt, ecdhe_tribute_offer_decrypt, hkdf_sha256,
        };
        let offer_pub =
            x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from([7u8; 32])).to_bytes();
        let custom_salt = [3u8; 32];
        assert_ne!(custom_salt, OFFER_HKDF_SALT);
        let plaintext = b"{\"hello\":\"tribute\"}";
        let shared = x25519_dalek::StaticSecret::from([9u8; 32])
            .diffie_hellman(&x25519_dalek::PublicKey::from(offer_pub));
        let key = hkdf_sha256(&custom_salt, shared.as_bytes(), OFFER_HKDF_INFO).expect("hkdf");
        let ephemeral_pub =
            x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from([9u8; 32])).to_bytes();
        let custom_ct = chacha20poly1305_encrypt(&key, &[1u8; 12], plaintext).expect("encrypt");
        let opened = ecdhe_tribute_offer_decrypt(
            &outbe_tee::resident::OfferKeyHandle::for_test([7u8; 32]),
            &custom_salt,
            &ephemeral_pub,
            &[1u8; 12],
            &custom_ct,
        )
        .expect("custom salt");
        assert_eq!(opened, plaintext);
        assert!(ecdhe_tribute_offer_decrypt(
            &outbe_tee::resident::OfferKeyHandle::for_test([7u8; 32]),
            &OFFER_HKDF_SALT,
            &ephemeral_pub,
            &[1u8; 12],
            &custom_ct,
        )
        .is_err());
    }

    #[test]
    fn promis_mine_key_info_is_domain_chain_and_account() {
        let chain = B256::repeat_byte(0x11);
        let account = Address::repeat_byte(0x22);
        let info = promis_mine_key_info(chain, account);
        let domain_len = PROMIS_MINE_HKDF_INFO.len();
        assert_eq!(&info[..domain_len], PROMIS_MINE_HKDF_INFO);
        assert_eq!(&info[domain_len..domain_len + 32], chain.as_slice());
        assert_eq!(&info[domain_len + 32..], account.as_slice());
    }

    #[test]
    fn promis_mine_ciphertext_is_bound_to_account_and_chain() {
        let tee_pub =
            x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from([7u8; 32])).to_bytes();
        let chain = B256::repeat_byte(0x11);
        let alice = Address::repeat_byte(0x22);
        let eve = Address::repeat_byte(0x33);
        let amount = U256::from(1000u64);
        let secret = [9u8; 32];
        let nonce = [1u8; 12];
        let alice_ct = encrypt_tee_amount_with(&tee_pub, (chain, alice), amount, secret, nonce)
            .expect("encrypt");
        assert_eq!(alice_ct.len(), ENCRYPTED_TEE_AMOUNT_LEN);
        assert_eq!(
            alice_ct,
            encrypt_tee_amount_with(&tee_pub, (chain, alice), amount, secret, nonce).unwrap()
        );
        assert_ne!(
            alice_ct,
            encrypt_tee_amount_with(&tee_pub, (chain, eve), amount, secret, nonce).unwrap()
        );
        let other_chain = B256::repeat_byte(0x44);
        let chain_ct =
            encrypt_tee_amount_with(&tee_pub, (other_chain, alice), amount, secret, nonce).unwrap();
        assert_ne!(alice_ct, chain_ct);
        let key = outbe_tee::resident::OfferKeyHandle::for_test([7u8; 32]);
        assert_eq!(
            super::super::crypto::decrypt_tee_amount(&key, &alice_ct, chain, alice).unwrap(),
            amount
        );
        assert!(matches!(
            super::super::crypto::decrypt_tee_amount(&key, &alice_ct, chain, eve),
            Err(super::super::errors::TeeError::Resident(_))
        ));
        assert!(matches!(
            super::super::crypto::decrypt_tee_amount(&key, &chain_ct, chain, alice),
            Err(super::super::errors::TeeError::Resident(_))
        ));
    }
}
