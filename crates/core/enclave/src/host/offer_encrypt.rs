//! Client-side tribute-offer and Promis `mineGratis` encryption built over
//! [`outbe_tee::tee_encrypt`].

use alloy_primitives::{Address, B256, U256};
use outbe_tee::tee_encrypt::{tee_encrypt, tee_encrypt_with, EncryptDomain, ExplicitNonceKey};

use super::{errors::LocalCryptoError, OFFER_HKDF_SALT};

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

/// Fixed wire size of a `U256` amount encrypted to the enclave's X25519 offer key:
/// `ephemeral_pub(32) || nonce(12) || ciphertext_with_tag(48)`.
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
        EncryptDomain::new(&OFFER_HKDF_SALT, &info),
        &amount.to_be_bytes::<32>(),
    )
    .map_err(LocalCryptoError::from_tee_encrypt)?;
    sealed
        .to_wire_bytes()
        .try_into()
        .map_err(|_| LocalCryptoError::AeadSeal)
}

/// Deterministic variant of [`encrypt_tee_amount`] with an explicit ephemeral secret and nonce.
pub fn encrypt_tee_amount_with(
    tee_pub: &[u8; 32],
    (chain_id, account): (B256, Address),
    amount: U256,
    ephemeral_secret: [u8; 32],
    nonce: [u8; 12],
) -> Result<[u8; ENCRYPTED_TEE_AMOUNT_LEN], LocalCryptoError> {
    let info = promis_mine_key_info(chain_id, account);
    let sealed = tee_encrypt_with(
        tee_pub,
        EncryptDomain::new(&OFFER_HKDF_SALT, &info),
        ExplicitNonceKey::new(ephemeral_secret, nonce),
        &amount.to_be_bytes::<32>(),
    )
    .map_err(LocalCryptoError::from_tee_encrypt)?;
    sealed
        .to_wire_bytes()
        .try_into()
        .map_err(|_| LocalCryptoError::AeadSeal)
}

/// Encrypt `plaintext` to `offer_pub` with a fresh ephemeral key and nonce.
/// Returns `(cipher_text, nonce, ephemeral_pub)`.
pub fn encrypt_tribute_offer(
    offer_pub: &[u8; 32],
    plaintext: &[u8],
) -> Result<EncryptedOfferParts, LocalCryptoError> {
    let sealed = tee_encrypt(
        offer_pub,
        EncryptDomain::new(&OFFER_HKDF_SALT, OFFER_HKDF_INFO),
        plaintext,
    )
    .map_err(LocalCryptoError::from_tee_encrypt)?;
    Ok((sealed.ciphertext, sealed.nonce, sealed.ephemeral_pub))
}

/// Deterministic variant with an explicit ephemeral secret and nonce.
pub fn encrypt_tribute_offer_with(
    offer_pub: &[u8; 32],
    eph_sk: [u8; 32],
    nonce: [u8; 12],
    plaintext: &[u8],
) -> Result<Vec<u8>, LocalCryptoError> {
    let sealed = tee_encrypt_with(
        offer_pub,
        EncryptDomain::new(&OFFER_HKDF_SALT, OFFER_HKDF_INFO),
        ExplicitNonceKey::new(eph_sk, nonce),
        plaintext,
    )
    .map_err(LocalCryptoError::from_tee_encrypt)?;
    Ok(sealed.ciphertext)
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

    #[test]
    fn promis_mine_key_info_is_domain_chain_and_account() {
        let chain = B256::repeat_byte(0x11);
        let account = Address::repeat_byte(0x22);
        let info = promis_mine_key_info(chain, account);
        let domain_len = PROMIS_MINE_HKDF_INFO.len();
        assert_eq!(info.len(), domain_len + 32 + 20);
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
        assert_ne!(
            alice_ct,
            encrypt_tee_amount_with(
                &tee_pub,
                (B256::repeat_byte(0x44), alice),
                amount,
                secret,
                nonce
            )
            .unwrap()
        );
    }
}
