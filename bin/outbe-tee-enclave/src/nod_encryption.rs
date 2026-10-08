//! Stateless NOD encryption with repeated ECDH on every read.
use crate::{
    confidential::SlotCipherDomain,
    crypto::hkdf_sha256,
    errors::{Result, TeeError},
    tribute_encryption::read_creator_public_key,
};
use alloy_primitives::{B256, U256};
use outbe_primitives::{
    nod_encryption::{EncryptedNodV2, NodTermsV2, NOD_AMOUNT_NONCE_INFO},
    tribute_encryption::EncryptedTributeV2,
};
use ring::hmac;
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroizing;
const CREATOR_KEY: SlotCipherDomain = SlotCipherDomain {
    nonce_info: b"outbe/nod/creator-key/nonce/v2",
};
const AMOUNT: SlotCipherDomain = SlotCipherDomain {
    nonce_info: NOD_AMOUNT_NONCE_INFO,
};
/// The caller authenticates the certified action and source membership first.
pub fn encrypt_nod_for_tribute(
    network_secret: &[u8; 32],
    tribute: &EncryptedTributeV2,
    terms: NodTermsV2,
    amount: U256,
) -> Result<EncryptedNodV2> {
    let source = &tribute.context;
    let source_terms = (
        source.chain_id,
        source.owner,
        source.worldwide_day,
        source.issuance_currency,
        source.reference_currency,
    );
    let issued_terms = (
        terms.chain_id,
        terms.owner,
        terms.worldwide_day,
        terms.issuance_currency,
        terms.reference_currency,
    );
    if issued_terms != source_terms || terms.nod_id.worldwide_day() != terms.worldwide_day {
        return Err(TeeError::TributeOfferReject(
            "NOD source terms mismatch".into(),
        ));
    }
    let creator = read_creator_public_key(network_secret, tribute)?;
    encrypt_nod(network_secret, &creator, terms, amount)
}

/// Encrypt already authenticated immutable terms; production issuance supplies a verified source.
pub fn encrypt_nod(
    network_secret: &[u8; 32],
    creator: &[u8; 32],
    terms: NodTermsV2,
    amount: U256,
) -> Result<EncryptedNodV2> {
    let mut nod = EncryptedNodV2 {
        encryption_binding: binding(network_secret, &terms, creator, amount),
        terms,
        encrypted_creator_public_key: Vec::new(),
        encrypted_gratis_amount: Vec::new(),
    };
    nod.encrypted_creator_public_key = CREATOR_KEY
        .slot(network_secret, nod.crypto_slot(), 0)
        .write_blob(0, creator)?;
    let key = amount_key(network_secret, creator, &nod)?;
    let plaintext = Zeroizing::new(amount.to_be_bytes::<32>());
    nod.encrypted_gratis_amount = AMOUNT
        .slot(&key, nod.crypto_slot(), 0)
        .write_blob(0, plaintext.as_ref())?;
    Ok(nod)
}
pub fn decrypt_nod(network_secret: &[u8; 32], nod: &EncryptedNodV2) -> Result<U256> {
    if !nod.has_valid_encoding() {
        return Err(TeeError::DecryptFailed);
    }
    let (_, creator) = CREATOR_KEY
        .slot(network_secret, nod.crypto_slot(), 0)
        .read_blob(&nod.encrypted_creator_public_key)?;
    let creator: &[u8; 32] = creator
        .as_slice()
        .try_into()
        .map_err(|_| TeeError::DecryptFailed)?;
    let key = amount_key(network_secret, creator, nod)?;
    let (_, plaintext) = AMOUNT
        .slot(&key, nod.crypto_slot(), 0)
        .read_blob(&nod.encrypted_gratis_amount)?;
    let plaintext = Zeroizing::new(plaintext);
    let amount: &[u8; 32] = plaintext
        .as_slice()
        .try_into()
        .map_err(|_| TeeError::DecryptFailed)?;
    let amount = U256::from_be_bytes(*amount);
    if binding(network_secret, &nod.terms, creator, amount) != nod.encryption_binding {
        return Err(TeeError::DecryptFailed);
    }
    Ok(amount)
}
fn binding(secret: &[u8; 32], terms: &NodTermsV2, creator: &[u8; 32], amount: U256) -> B256 {
    let mut input = b"outbe/nod/issuance-binding/v2".to_vec();
    input.extend_from_slice(terms.digest().as_slice());
    input.extend_from_slice(creator);
    input.extend_from_slice(&amount.to_be_bytes::<32>());
    let input = Zeroizing::new(input);
    let tag = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, secret), &input);
    B256::from_slice(tag.as_ref())
}
fn amount_key(
    secret: &[u8; 32],
    creator: &[u8; 32],
    nod: &EncryptedNodV2,
) -> Result<Zeroizing<[u8; 32]>> {
    let secret = StaticSecret::from(*secret);
    let shared = secret.diffie_hellman(&PublicKey::from(*creator));
    if !shared.was_contributory() {
        return Err(TeeError::InvalidKeyLen);
    }
    let public = PublicKey::from(&secret).to_bytes();
    Ok(Zeroizing::new(hkdf_sha256(
        nod.context_digest().as_slice(),
        shared.as_bytes(),
        &nod.amount_key_info(&public, creator),
    )?))
}
