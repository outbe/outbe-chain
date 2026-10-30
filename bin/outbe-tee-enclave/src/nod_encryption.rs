//! Stateless NOD encryption under the `NOD` confidential domain.
use crate::{
    confidential::{SlotCipherDomain, NOD},
    crypto::hkdf_sha256,
    errors::{Result, TeeError},
    tribute_encryption::decrypt_tribute,
};
use alloy_primitives::{Address, B256, U256};
use outbe_primitives::{
    nod_encryption::{EncryptedNodV1, NodTermsV1, NOD_AMOUNT_NONCE_INFO},
    tribute_encryption::EncryptedTributeV2,
};
use ring::hmac;
use zeroize::Zeroizing;

const AMOUNT: SlotCipherDomain = SlotCipherDomain {
    nonce_info: NOD_AMOUNT_NONCE_INFO,
};

/// Derive the resident NOD state key from the DKG group signature.
pub fn derive_nod_state_key(group_sig: &[u8], chain_id: B256, epoch: u64) -> Result<[u8; 32]> {
    NOD.derive_state_key(group_sig, chain_id, epoch)
}

/// Derive the per-owner NOD view key from `nod_state_key`.
pub fn derive_view_key(state_key: &[u8; 32], owner: Address) -> Result<[u8; 32]> {
    NOD.account_keys.derive_view_key(state_key, owner)
}

/// The caller authenticates the certified action and source membership first.
pub fn encrypt_nod_for_tribute(
    tribute_state_key: &[u8; 32],
    nod_state_key: &[u8; 32],
    tribute: &EncryptedTributeV2,
    terms: NodTermsV1,
    amount: U256,
) -> Result<EncryptedNodV1> {
    let source = &tribute.context;
    let source_terms = (
        source.chain_id,
        source.owner,
        source.issuance_currency,
        source.reference_currency,
    );
    let issued_terms = (
        terms.chain_id,
        terms.owner,
        terms.issuance_currency,
        terms.reference_currency,
    );
    let canonical_id = terms
        .series_id()
        .map(|series_id| outbe_primitives::nod_rules::derive_nod_id(series_id, terms.owner));
    if issued_terms != source_terms || canonical_id != Ok(terms.nod_id) {
        return Err(TeeError::TributeOfferReject(
            "NOD source terms mismatch".into(),
        ));
    }
    let _ = decrypt_tribute(tribute_state_key, tribute)?;
    encrypt_nod(nod_state_key, terms, amount)
}

/// Encrypt already authenticated immutable terms; production issuance supplies a verified source.
pub fn encrypt_nod(
    nod_state_key: &[u8; 32],
    terms: NodTermsV1,
    amount: U256,
) -> Result<EncryptedNodV1> {
    let mut nod = EncryptedNodV1 {
        encryption_binding: binding(nod_state_key, &terms, amount),
        terms,
        encrypted_gratis_amount: Vec::new(),
    };
    let key = amount_key(nod_state_key, &nod)?;
    let plaintext = Zeroizing::new(amount.to_be_bytes::<32>());
    nod.encrypted_gratis_amount = AMOUNT
        .slot(&key, nod.crypto_slot(), 0)
        .write_blob(0, plaintext.as_ref())?;
    Ok(nod)
}

pub fn decrypt_nod(nod_state_key: &[u8; 32], nod: &EncryptedNodV1) -> Result<U256> {
    if !nod.has_valid_encoding() {
        return Err(TeeError::DecryptFailed);
    }
    let key = amount_key(nod_state_key, nod)?;
    let (_, plaintext) = AMOUNT
        .slot(&key, nod.crypto_slot(), 0)
        .read_blob(&nod.encrypted_gratis_amount)?;
    let plaintext = Zeroizing::new(plaintext);
    let amount: &[u8; 32] = plaintext
        .as_slice()
        .try_into()
        .map_err(|_| TeeError::DecryptFailed)?;
    let amount = U256::from_be_bytes(*amount);
    if binding(nod_state_key, &nod.terms, amount) != nod.encryption_binding {
        return Err(TeeError::DecryptFailed);
    }
    Ok(amount)
}

fn binding(nod_state_key: &[u8; 32], terms: &NodTermsV1, amount: U256) -> B256 {
    let mut input = b"outbe/nod/issuance-binding/v2".to_vec();
    input.extend_from_slice(terms.digest().as_slice());
    input.extend_from_slice(&amount.to_be_bytes::<32>());
    let input = Zeroizing::new(input);
    let tag = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, nod_state_key), &input);
    B256::from_slice(tag.as_ref())
}

fn amount_key(nod_state_key: &[u8; 32], nod: &EncryptedNodV1) -> Result<Zeroizing<[u8; 32]>> {
    let view_key = Zeroizing::new(derive_view_key(nod_state_key, nod.terms.owner)?);
    Ok(Zeroizing::new(hkdf_sha256(
        nod.context_digest().as_slice(),
        &*view_key,
        &nod.amount_key_info(),
    )?))
}
