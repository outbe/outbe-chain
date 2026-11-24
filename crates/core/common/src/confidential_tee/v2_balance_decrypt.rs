//! Owner-local opening of shared V2 balance and immutable amount envelopes.

use alloy_primitives::{Address, U256};
use outbe_core_common::nod_encryption::{
    EncryptedNodV1, NodQuoteContextV1, NOD_AMOUNT_NONCE_INFO, NOD_BLOB_LEN, NOD_NOMINAL_NONCE_INFO,
    NOD_QUOTE_NONCE_INFO,
};
use outbe_tee::tee_decrypt::{
    tee_decrypt_counter_slot, tee_decrypt_v2_envelope, CounterSlotContext, V2EnvelopeDomain,
};
use outbe_tee::TeeCryptoError;

use super::errors::LocalCryptoError;

#[derive(Clone, Copy)]
enum V2BalanceLedger {
    Gratis,
    Promis,
}

impl V2BalanceLedger {
    const fn marker(self) -> &'static [u8; 4] {
        match self {
            Self::Gratis => b"GRA2",
            Self::Promis => b"PRO2",
        }
    }

    const fn key_info(self) -> &'static [u8] {
        match self {
            Self::Gratis => b"outbe/gratis/amount-key/v2",
            Self::Promis => b"outbe/promis/amount-key/v2",
        }
    }

    const fn nonce_info(self) -> &'static [u8] {
        match self {
            Self::Gratis => b"outbe/gratis/amount-nonce/v2",
            Self::Promis => b"outbe/promis/amount-nonce/v2",
        }
    }

    const fn invalid_balance(self) -> LocalCryptoError {
        match self {
            Self::Gratis => LocalCryptoError::InvalidGratisBalance,
            Self::Promis => LocalCryptoError::InvalidPromisBalance,
        }
    }

    fn decryption_failed(self, error: TeeCryptoError) -> LocalCryptoError {
        match self {
            Self::Gratis => LocalCryptoError::GratisBalanceDecryptionFailed(error),
            Self::Promis => LocalCryptoError::PromisBalanceDecryptionFailed(error),
        }
    }

    fn amount_length(self, error: TeeCryptoError) -> LocalCryptoError {
        match self {
            Self::Gratis => LocalCryptoError::InvalidGratisAmountLength(error),
            Self::Promis => LocalCryptoError::InvalidPromisAmountLength(error),
        }
    }

    fn map_error(self, error: TeeCryptoError) -> LocalCryptoError {
        match error {
            TeeCryptoError::InvalidEnvelope => self.invalid_balance(),
            TeeCryptoError::HkdfExpand => LocalCryptoError::HkdfExpand(error),
            TeeCryptoError::HkdfFill => LocalCryptoError::HkdfFill(error),
            TeeCryptoError::AeadKey => self.decryption_failed(error),
            TeeCryptoError::InvalidPlaintextLength { .. } => self.amount_length(error),
            TeeCryptoError::DecryptionFailed
            | TeeCryptoError::RngFailure
            | TeeCryptoError::AeadSeal => self.decryption_failed(error),
        }
    }
}

fn decrypt_v2_balance(
    ledger: V2BalanceLedger,
    view_key: &[u8; 32],
    account: Address,
    blob: &[u8],
) -> Result<U256, LocalCryptoError> {
    if blob.is_empty() {
        return Ok(U256::ZERO);
    }
    if blob.len() != 92 {
        return Err(ledger.invalid_balance());
    }
    let opened = tee_decrypt_v2_envelope(
        view_key,
        account,
        V2EnvelopeDomain {
            scope: &[0],
            marker: ledger.marker(),
            key_info: ledger.key_info(),
            nonce_info: ledger.nonce_info(),
        },
        blob,
    )
    .map_err(|err| ledger.map_error(err))?;
    let amount = opened
        .try_into_array::<32>()
        .map_err(|err| ledger.map_error(err))?;
    Ok(U256::from_be_bytes(amount))
}

/// Open a `GRA2` Gratis balance envelope with the account view key.
pub fn decrypt_gratis_balance(
    view_key: &[u8; 32],
    account: Address,
    blob: &[u8],
) -> Result<U256, LocalCryptoError> {
    decrypt_v2_balance(V2BalanceLedger::Gratis, view_key, account, blob)
}

/// Open a `PRO2` Promis balance envelope with the account view key.
pub fn decrypt_promis_balance(
    view_key: &[u8; 32],
    account: Address,
    blob: &[u8],
) -> Result<U256, LocalCryptoError> {
    decrypt_v2_balance(V2BalanceLedger::Promis, view_key, account, blob)
}

const FIDELITY_V2_DOMAIN: V2EnvelopeDomain<'static> = V2EnvelopeDomain {
    scope: &[],
    marker: b"FID2",
    key_info: b"outbe/fidelity/cohort-key/v2",
    nonce_info: b"outbe/fidelity/cohort-nonce/v2",
};

fn map_fidelity(error: TeeCryptoError) -> LocalCryptoError {
    match error {
        TeeCryptoError::InvalidEnvelope => LocalCryptoError::InvalidFidelityCohortEnvelope,
        TeeCryptoError::HkdfExpand => LocalCryptoError::HkdfExpand(error),
        TeeCryptoError::HkdfFill => LocalCryptoError::HkdfFill(error),
        TeeCryptoError::AeadKey
        | TeeCryptoError::DecryptionFailed
        | TeeCryptoError::RngFailure
        | TeeCryptoError::AeadSeal
        | TeeCryptoError::InvalidPlaintextLength { .. } => {
            LocalCryptoError::FidelityCohortDecryptionFailed(error)
        }
    }
}

/// Open a `FID2` Fidelity cohort record with the account view key.
pub fn decrypt_fidelity_cohorts(
    view_key: &[u8; 32],
    account: Address,
    blob: &[u8],
) -> Result<Vec<u8>, LocalCryptoError> {
    if blob.is_empty() {
        return Ok(Vec::new());
    }
    let opened = tee_decrypt_v2_envelope(view_key, account, FIDELITY_V2_DOMAIN, blob)
        .map_err(map_fidelity)?;
    Ok(opened.as_slice().to_vec())
}

fn map_nod(error: TeeCryptoError) -> LocalCryptoError {
    match error {
        TeeCryptoError::InvalidEnvelope => LocalCryptoError::InvalidNodEncoding,
        TeeCryptoError::HkdfExpand => LocalCryptoError::HkdfExpand(error),
        TeeCryptoError::HkdfFill => LocalCryptoError::HkdfFill(error),
        TeeCryptoError::AeadKey => LocalCryptoError::InvalidNodAmountKey(error),
        TeeCryptoError::InvalidPlaintextLength { .. } => {
            LocalCryptoError::InvalidNodAmountLength(error)
        }
        TeeCryptoError::DecryptionFailed
        | TeeCryptoError::RngFailure
        | TeeCryptoError::AeadSeal => LocalCryptoError::NodAmountDecryptionFailed(error),
    }
}

struct CounterAmountInput<'a> {
    view_key: &'a [u8; 32],
    context_digest: &'a [u8],
    key_info: &'a [u8],
    crypto_slot: &'a [u8],
    nonce_info: &'a [u8],
    ciphertext: &'a [u8],
    map_error: fn(TeeCryptoError) -> LocalCryptoError,
}

fn open_counter_amount(input: CounterAmountInput<'_>) -> Result<U256, LocalCryptoError> {
    let opened = tee_decrypt_counter_slot(
        input.view_key,
        CounterSlotContext {
            context_digest: input.context_digest,
            key_info: input.key_info,
            crypto_slot: input.crypto_slot,
            nonce_info: input.nonce_info,
        },
        input.ciphertext,
    )
    .map_err(input.map_error)?;
    let amount = opened.try_into_array::<32>().map_err(input.map_error)?;
    Ok(U256::from_be_bytes(amount))
}

/// Open an [`EncryptedNodV1`] amount with the owner's NOD view key.
pub fn decrypt_nod_for_owner(
    view_key: &[u8; 32],
    nod: &EncryptedNodV1,
) -> Result<U256, LocalCryptoError> {
    open_nod_amount(
        view_key,
        nod,
        NOD_AMOUNT_NONCE_INFO,
        &nod.encrypted_gratis_amount,
    )
}

/// Open an [`EncryptedNodV1`] nominal with the owner's NOD view key.
pub fn decrypt_nod_nominal_for_owner(
    view_key: &[u8; 32],
    nod: &EncryptedNodV1,
) -> Result<U256, LocalCryptoError> {
    open_nod_amount(
        view_key,
        nod,
        NOD_NOMINAL_NONCE_INFO,
        &nod.encrypted_nominal_amount,
    )
}

fn open_nod_amount(
    view_key: &[u8; 32],
    nod: &EncryptedNodV1,
    nonce_info: &[u8],
    ciphertext: &[u8],
) -> Result<U256, LocalCryptoError> {
    if !nod.has_valid_encoding() {
        return Err(LocalCryptoError::InvalidNodEncoding);
    }
    let digest = nod.context_digest();
    let key_info = nod.amount_key_info();
    let slot = nod.crypto_slot();
    open_counter_amount(CounterAmountInput {
        view_key,
        context_digest: digest.as_slice(),
        key_info: &key_info,
        crypto_slot: slot.as_slice(),
        nonce_info,
        ciphertext,
        map_error: map_nod,
    })
}

/// Open an encrypted NOD settlement quote payment with the owner's NOD view key.
pub fn decrypt_nod_quote_for_owner(
    view_key: &[u8; 32],
    context: &NodQuoteContextV1,
    encrypted_payment: &[u8],
) -> Result<U256, LocalCryptoError> {
    if encrypted_payment.len() != NOD_BLOB_LEN || encrypted_payment[..8] != 1u64.to_be_bytes() {
        return Err(LocalCryptoError::InvalidNodEncoding);
    }
    let digest = context.context_digest();
    let key_info = context.quote_key_info();
    let slot = context.crypto_slot();
    open_counter_amount(CounterAmountInput {
        view_key,
        context_digest: digest.as_slice(),
        key_info: &key_info,
        crypto_slot: slot.as_slice(),
        nonce_info: NOD_QUOTE_NONCE_INFO,
        ciphertext: encrypted_payment,
        map_error: map_nod,
    })
}
