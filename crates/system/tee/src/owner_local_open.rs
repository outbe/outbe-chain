//! Owner-local opening of context-bound ciphertexts. Domain adapters select
//! the record context, key inputs, nonce domain, envelope and result type.

use crate::{errors::LocalCryptoError, offer_encrypt::hkdf_sha256};
use alloy_primitives::{Address, U256};
use ring::aead;
use zeroize::Zeroizing;

pub(crate) struct LocalOpen<'a> {
    pub key: &'a [u8; 32],
    pub nonce_context: &'a [u8],
    pub nonce_info: &'a [u8],
    pub ciphertext: &'a [u8],
    pub invalid_key_error: LocalCryptoError,
    pub decryption_error: LocalCryptoError,
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

pub(crate) fn open_local_ciphertext(
    input: LocalOpen<'_>,
) -> Result<OpenedLocalBytes, LocalCryptoError> {
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

/// Domain inputs for an immutable owner amount with its first-counter envelope.
/// Check the record encoding before this input is passed to the opener.
pub(crate) struct OwnerAmountBlob<'a> {
    pub context_digest: &'a [u8],
    pub key_info: &'a [u8],
    pub crypto_slot: &'a [u8],
    pub nonce_info: &'a [u8],
    pub blob: &'a [u8],
    pub invalid_key_error: LocalCryptoError,
    pub decryption_error: LocalCryptoError,
}

/// Derive the immutable amount key and open its first-counter envelope.
pub(crate) fn open_owner_amount_blob(
    view_key: &[u8; 32],
    input: OwnerAmountBlob<'_>,
) -> Result<OpenedLocalBytes, LocalCryptoError> {
    let key = Zeroizing::new(hkdf_sha256(input.context_digest, view_key, input.key_info)?);
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

/// Domain constants and typed errors for a 92-byte V2 balance envelope (`GRA2` / `PRO2`).
pub(crate) struct V2BalanceSpec<'a> {
    pub marker: &'a [u8; 4],
    pub key_info: &'a [u8],
    pub nonce_info: &'a [u8],
    pub invalid_balance_error: LocalCryptoError,
    pub invalid_key_error: LocalCryptoError,
    pub decryption_error: LocalCryptoError,
    pub invalid_length_error: LocalCryptoError,
}

pub(crate) const GRATIS_V2_SPEC: V2BalanceSpec<'static> = V2BalanceSpec {
    marker: b"GRA2",
    key_info: b"outbe/gratis/amount-key/v2",
    nonce_info: b"outbe/gratis/amount-nonce/v2",
    invalid_balance_error: LocalCryptoError::InvalidGratisBalance,
    invalid_key_error: LocalCryptoError::InvalidGratisViewKey,
    decryption_error: LocalCryptoError::GratisBalanceDecryptionFailed,
    invalid_length_error: LocalCryptoError::InvalidGratisAmountLength,
};

pub(crate) const PROMIS_V2_SPEC: V2BalanceSpec<'static> = V2BalanceSpec {
    marker: b"PRO2",
    key_info: b"outbe/promis/amount-key/v2",
    nonce_info: b"outbe/promis/amount-nonce/v2",
    invalid_balance_error: LocalCryptoError::InvalidPromisBalance,
    invalid_key_error: LocalCryptoError::InvalidPromisViewKey,
    decryption_error: LocalCryptoError::PromisBalanceDecryptionFailed,
    invalid_length_error: LocalCryptoError::InvalidPromisAmountLength,
};

/// Open a 92-byte V2 balance envelope (`version(8) || marker(4) || binding(32) || ct(48)`).
pub(crate) fn open_v2_balance_blob(
    view_key: &[u8; 32],
    account: Address,
    blob: &[u8],
    spec: V2BalanceSpec<'_>,
) -> Result<U256, LocalCryptoError> {
    if blob.is_empty() {
        return Ok(U256::ZERO);
    }
    if blob.len() != 92 || &blob[8..12] != spec.marker || blob[..8] == [0; 8] {
        return Err(spec.invalid_balance_error);
    }
    let mut context = account.as_slice().to_vec();
    context.push(0);
    context.extend_from_slice(&blob[..8]);
    context.extend_from_slice(&blob[12..44]);
    let key = Zeroizing::new(hkdf_sha256(view_key, &context, spec.key_info)?);
    let opened = open_local_ciphertext(LocalOpen {
        key: &key,
        nonce_context: &context,
        nonce_info: spec.nonce_info,
        ciphertext: &blob[44..],
        invalid_key_error: spec.invalid_key_error,
        decryption_error: spec.decryption_error,
    })?;
    let amount: &[u8; 32] = opened
        .as_slice()
        .try_into()
        .map_err(|_| spec.invalid_length_error)?;
    Ok(U256::from_be_bytes(*amount))
}
