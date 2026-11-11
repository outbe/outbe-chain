//! Client-side tribute-offer encryption. This is the single shared implementation.
//! `outbe-cli`, the enclave throughput bench and the node's canary probe use it.
//!
//! The recipe is byte-compatible with the enclave decrypt path
//! `outbe_tee_enclave::crypto::ecdhe_tribute_offer_decrypt`:
//! 1. Ephemeral X25519.
//! 2. ECDHE with the offer public key.
//! 3. `HKDF-SHA256(salt = OFFER_HKDF_SALT, ikm = shared, info =
//!    b"tribute-factory-encryption")`.
//! 4. ChaCha20Poly1305 with empty AAD.
//!
//! This is client-side crypto over an ephemeral secret and the PUBLIC offer key.
//! It uses no enclave-resident secret material. Thus the crate rule holds:
//! secret-bearing cryptography lives only in `bin/outbe-tee-enclave`.

use alloy_primitives::{Address, B256, U256};

use crate::{errors::LocalCryptoError, OFFER_HKDF_SALT};

/// HKDF info string binding the derived key to the tribute-offer domain.
pub const OFFER_HKDF_INFO: &[u8] = b"tribute-factory-encryption";

/// HKDF info binding a `mineGratis` amount to the Promis conversion domain.
pub const PROMIS_MINE_HKDF_INFO: &[u8] = b"outbe/promis/mine-gratis/v1";

/// Builds the HKDF info binding `encryptedPromisMinor` to `(domain, chain_id, account)`.
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
///
/// The key is `HKDF-SHA256(salt = OFFER_HKDF_SALT, ikm = ECDHE, info = promis_mine_key_info(chain_id, account))`
/// and ChaCha20Poly1305 with empty AAD. Returns `ephemeral_pub(32) || nonce(12) || ciphertext(48)`.
pub fn encrypt_tee_amount(
    tee_pub: &[u8; 32],
    chain_id: B256,
    account: Address,
    amount: U256,
) -> Result<[u8; ENCRYPTED_TEE_AMOUNT_LEN], LocalCryptoError> {
    let (ephemeral_secret, nonce) = random_ephemeral_and_nonce()?;
    encrypt_tee_amount_with(
        tee_pub,
        (chain_id, account),
        amount,
        ephemeral_secret,
        nonce,
    )
}

/// Deterministic variant of [`encrypt_tee_amount`] with an explicit ephemeral secret and nonce.
pub fn encrypt_tee_amount_with(
    tee_pub: &[u8; 32],
    (chain_id, account): (B256, Address),
    amount: U256,
    ephemeral_secret: [u8; 32],
    nonce: [u8; 12],
) -> Result<[u8; ENCRYPTED_TEE_AMOUNT_LEN], LocalCryptoError> {
    let eph_pub =
        x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(ephemeral_secret));
    let info = promis_mine_key_info(chain_id, account);
    let cipher_text = ecdhe_offer_encrypt(
        tee_pub,
        ephemeral_secret,
        nonce,
        &info,
        &amount.to_be_bytes::<32>(),
    )?;
    let mut out = [0u8; ENCRYPTED_TEE_AMOUNT_LEN];
    out[..32].copy_from_slice(eph_pub.as_bytes());
    out[32..44].copy_from_slice(&nonce);
    out[44..].copy_from_slice(&cipher_text);
    Ok(out)
}

/// Encrypt `plaintext` to `offer_pub` with a fresh ephemeral key and nonce.
/// Returns `(cipher_text, nonce, ephemeral_pub)`.
pub fn encrypt_tribute_offer(
    offer_pub: &[u8; 32],
    plaintext: &[u8],
) -> Result<EncryptedOfferParts, LocalCryptoError> {
    let (eph_sk, nonce) = random_ephemeral_and_nonce()?;
    let eph_pub = x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(eph_sk));
    let cipher_text = encrypt_tribute_offer_with(offer_pub, eph_sk, nonce, plaintext)?;
    Ok((cipher_text, nonce, eph_pub.to_bytes()))
}

/// Deterministic variant with an explicit ephemeral secret and nonce. Use it for
/// the bench, tests, and any caller that manages its own randomness.
pub fn encrypt_tribute_offer_with(
    offer_pub: &[u8; 32],
    eph_sk: [u8; 32],
    nonce: [u8; 12],
    plaintext: &[u8],
) -> Result<Vec<u8>, LocalCryptoError> {
    ecdhe_offer_encrypt(offer_pub, eph_sk, nonce, OFFER_HKDF_INFO, plaintext)
}

fn random_ephemeral_and_nonce() -> Result<([u8; 32], [u8; 12]), LocalCryptoError> {
    use ring::rand::SecureRandom as _;
    let rng = ring::rand::SystemRandom::new();
    // Cryptographic randomness for an offer-encryption ephemeral + nonce. This is
    // never consensus randomness: the ciphertext is request DATA, and every
    // validator's enclave decrypts it deterministically.
    let mut eph_sk = [0u8; 32];
    rng.fill(&mut eph_sk)
        .map_err(LocalCryptoError::offer_encryption_rng_failure)?;
    let mut nonce = [0u8; 12];
    rng.fill(&mut nonce)
        .map_err(LocalCryptoError::offer_encryption_rng_failure)?;
    Ok((eph_sk, nonce))
}

fn ecdhe_offer_encrypt(
    offer_pub: &[u8; 32],
    eph_sk: [u8; 32],
    nonce: [u8; 12],
    info: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>, LocalCryptoError> {
    let eph_secret = x25519_dalek::StaticSecret::from(eph_sk);
    let shared = eph_secret.diffie_hellman(&x25519_dalek::PublicKey::from(*offer_pub));
    let key = hkdf_sha256(&OFFER_HKDF_SALT, shared.as_bytes(), info)?;
    chacha20poly1305_encrypt(&key, &nonce, plaintext)
}

/// The known-plaintext canary/bench offer payload: 1 SU hash and ISO 4217 code
/// 999 (the standard no-currency code). One shared recipe so the canary asserts
/// exactly what the bench measures.
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

/// HKDF-SHA256 extract+expand to 32 bytes (matches the enclave).
pub(crate) fn hkdf_sha256(
    salt: &[u8],
    ikm: &[u8],
    info: &[u8],
) -> Result<[u8; 32], LocalCryptoError> {
    use ring::hkdf;
    struct Len32;
    impl hkdf::KeyType for Len32 {
        fn len(&self) -> usize {
            32
        }
    }
    let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, salt);
    let prk = salt.extract(ikm);
    let info_refs: &[&[u8]] = &[info];
    let okm = prk
        .expand(info_refs, Len32)
        .map_err(LocalCryptoError::hkdf_expand)?;
    let mut out = [0u8; 32];
    okm.fill(&mut out).map_err(LocalCryptoError::hkdf_fill)?;
    Ok(out)
}

/// ChaCha20Poly1305 AEAD encrypt with empty AAD (matches the enclave offer path).
fn chacha20poly1305_encrypt(
    key: &[u8; 32],
    nonce: &[u8; 12],
    plaintext: &[u8],
) -> Result<Vec<u8>, LocalCryptoError> {
    use ring::aead::{self, BoundKey as _};
    struct OneNonce([u8; 12]);
    impl aead::NonceSequence for OneNonce {
        fn advance(&mut self) -> Result<aead::Nonce, ring::error::Unspecified> {
            Ok(aead::Nonce::assume_unique_for_key(self.0))
        }
    }
    let unbound =
        aead::UnboundKey::new(&aead::CHACHA20_POLY1305, key).map_err(LocalCryptoError::aead_key)?;
    let mut sealing = aead::SealingKey::new(unbound, OneNonce(*nonce));
    let mut in_out = plaintext.to_vec();
    sealing
        .seal_in_place_append_tag(aead::Aad::empty(), &mut in_out)
        .map_err(LocalCryptoError::aead_seal)?;
    Ok(in_out)
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
        // ChaCha20Poly1305 appends a 16-byte tag.
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
