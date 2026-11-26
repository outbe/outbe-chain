//! Stateless NOD encryption under the `NOD` confidential domain.

use alloy_primitives::{Address, B256, U256};
use outbe_core_common::confidential_tee::{attestation, hash, SlotCipherDomain, TeeError, NOD};
use outbe_core_common::nod_encryption::{
    EncryptedNodGroupLoadV1, EncryptedNodV1, NodQuoteContextV1, NodTermsV1, NOD_AMOUNT_NONCE_INFO,
    NOD_NOMINAL_NONCE_INFO, NOD_QUOTE_NONCE_INFO,
};
use outbe_tee::resident::{ResidentOfferMaterial, StateKeyHandle};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use zeroize::Zeroizing;

const AMOUNT: SlotCipherDomain = SlotCipherDomain {
    nonce_info: NOD_AMOUNT_NONCE_INFO,
};
const NOMINAL: SlotCipherDomain = SlotCipherDomain {
    nonce_info: NOD_NOMINAL_NONCE_INFO,
};
const GROUP_LOAD: SlotCipherDomain = SlotCipherDomain {
    nonce_info: b"outbe/nod/group-load-nonce/v1",
};
const QUOTE_AMOUNT: SlotCipherDomain = SlotCipherDomain {
    nonce_info: NOD_QUOTE_NONCE_INFO,
};

/// Failure of a pure NOD encryption or group-load transition.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum NodEncryptionError {
    /// Resident or AEAD failure. Display stays the historical sentence.
    #[error(transparent)]
    Crypto(#[from] TeeError),
    /// The predecessor record is not the same chain and scope.
    #[error("invalid Nod group load predecessor")]
    InvalidGroupLoadPredecessor,
    /// Adding the delta overflowed the group-load total.
    #[error("Nod group load overflow")]
    GroupLoadOverflow,
    /// Subtracting the delta underflowed the group-load total.
    #[error("Nod group load underflow")]
    GroupLoadUnderflow,
    /// Quote context does not name the encrypted NOD.
    #[error("NOD quote context mismatch")]
    QuoteContextMismatch,
    /// Canonical JSON for an inputs hash did not serialize.
    #[error("NOD inputs hash failed")]
    InputsHash(#[source] serde_json::Error),
}

impl From<outbe_tee::resident::TeeCryptoError> for NodEncryptionError {
    fn from(error: outbe_tee::resident::TeeCryptoError) -> Self {
        Self::Crypto(TeeError::from(error))
    }
}

/// First [`NodEncryptionError`] in `error`, including `error` itself.
#[must_use]
pub fn encryption_cause<'a>(
    error: &'a (dyn std::error::Error + 'static),
) -> Option<&'a NodEncryptionError> {
    let mut current = Some(error);
    while let Some(cause) = current {
        if let Some(nod) = cause.downcast_ref::<NodEncryptionError>() {
            return Some(nod);
        }
        current = std::error::Error::source(cause);
    }
    None
}

/// Arithmetic transition applied to an [`EncryptedNodGroupLoadV1`] record.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum NodGroupLoadOperationV1 {
    /// Add an encrypted series load aggregate.
    AddSeriesLoad {
        /// Encrypted series load aggregate to add.
        series_load: Box<EncryptedNodGroupLoadV1>,
    },
    /// Add one encrypted NOD's Gratis load.
    AddNod {
        /// Encrypted NOD to add.
        nod: Box<EncryptedNodV1>,
    },
    /// Subtract one encrypted NOD's Gratis load.
    SubNod {
        /// Encrypted NOD to subtract.
        nod: Box<EncryptedNodV1>,
    },
    /// Add or subtract a transient plaintext load delta.
    AdjustTransient {
        /// Transient load delta in minor units.
        load_minor: U256,
        /// True to add, false to subtract.
        add: bool,
    },
    /// Add one encrypted NOD's nominal.
    AddNodNominal {
        /// Encrypted NOD whose nominal to add.
        nod: Box<EncryptedNodV1>,
    },
}

/// Request to update an [`EncryptedNodGroupLoadV1`] record.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NodGroupLoadOpRequestV1 {
    /// Chain identifier.
    pub chain_id: u64,
    /// Series or group scope digest.
    pub scope: B256,
    /// Previous encrypted group-load record, or `None` for initialization.
    pub previous: Option<EncryptedNodGroupLoadV1>,
    /// Arithmetic operation to apply.
    pub operation: NodGroupLoadOperationV1,
}

/// Canonical commitment over a `ReadNodAmountV1` input record.
pub fn nod_read_hash(nod: &EncryptedNodV1) -> Result<B256, serde_json::Error> {
    hash(b"outbe/nod/read-inputs/v1", nod)
}

/// Domain-separated preimage signed by the enclave for `NodAmountReadV1`.
#[must_use]
pub fn nod_read_preimage(inputs: B256, amount: U256) -> Vec<u8> {
    let mut bytes = b"outbe/nod/read-result/v1".to_vec();
    bytes.extend_from_slice(inputs.as_slice());
    bytes.extend_from_slice(&amount.to_be_bytes::<32>());
    bytes
}

/// Canonical commitment over a [`NodGroupLoadOpRequestV1`].
pub fn nod_group_load_inputs_hash(
    request: &NodGroupLoadOpRequestV1,
) -> Result<B256, serde_json::Error> {
    hash(b"outbe/nod/group-load-inputs/v1", request)
}

/// Domain-separated preimage signed by the enclave for `NodGroupLoadOpAppliedV1`.
pub fn nod_group_load_attestation_preimage(
    inputs_hash: B256,
    record: &EncryptedNodGroupLoadV1,
) -> Result<Vec<u8>, serde_json::Error> {
    attestation(b"outbe/nod/group-load-result/v1", inputs_hash, record)
}

/// Canonical commitment over a `ReadNodGroupLoadV1` input record.
pub fn nod_group_load_read_hash(
    record: &EncryptedNodGroupLoadV1,
) -> Result<B256, serde_json::Error> {
    hash(b"outbe/nod/group-load-read-inputs/v1", record)
}

/// Domain-separated preimage signed by the enclave for `NodGroupLoadReadV1`.
#[must_use]
pub fn nod_group_load_read_preimage(inputs: B256, amount: U256) -> Vec<u8> {
    let mut bytes = b"outbe/nod/group-load-read-result/v1".to_vec();
    bytes.extend_from_slice(inputs.as_slice());
    bytes.extend_from_slice(&amount.to_be_bytes::<32>());
    bytes
}

/// Canonical commitment over an `EncryptNodQuoteV1` input.
pub fn nod_quote_inputs_hash(
    nod: &EncryptedNodV1,
    context: &NodQuoteContextV1,
    payment_minor: U256,
) -> Result<B256, serde_json::Error> {
    hash(
        b"outbe/nod/quote-encrypt-inputs/v1",
        &(nod, context, payment_minor),
    )
}

/// Derive the resident NOD state key from the DKG group signature.
pub fn derive_nod_state_key(
    material: &ResidentOfferMaterial,
    chain_id: B256,
    epoch: u64,
) -> Result<StateKeyHandle, NodEncryptionError> {
    Ok(NOD.derive_state_key(material, chain_id, epoch)?)
}

/// Derive the per-owner NOD view key from `nod_state_key`.
pub fn derive_view_key(
    state_key: &StateKeyHandle,
    owner: Address,
) -> Result<StateKeyHandle, NodEncryptionError> {
    Ok(NOD.account_keys.derive_view_key(state_key, owner)?)
}

/// Encrypt already authenticated immutable terms with the Gratis load and the nominal.
pub fn encrypt_nod(
    nod_state_key: &StateKeyHandle,
    terms: NodTermsV1,
    amount: U256,
    nominal: U256,
) -> Result<EncryptedNodV1, NodEncryptionError> {
    let mut nod = EncryptedNodV1 {
        encryption_binding: binding(nod_state_key, &terms, amount, nominal)?,
        terms,
        encrypted_gratis_amount: Vec::new(),
        encrypted_nominal_amount: Vec::new(),
    };
    let key = amount_key(nod_state_key, &nod)?;
    let load = Zeroizing::new(amount.to_be_bytes::<32>());
    nod.encrypted_gratis_amount = AMOUNT
        .slot(&key, nod.crypto_slot(), 0)
        .write_blob(0, load.as_ref())?;
    let nominal = Zeroizing::new(nominal.to_be_bytes::<32>());
    nod.encrypted_nominal_amount = NOMINAL
        .slot(&key, nod.crypto_slot(), 0)
        .write_blob(0, nominal.as_ref())?;
    Ok(nod)
}

/// Open the Gratis load of an [`EncryptedNodV1`] with the NOD state key.
pub fn decrypt_nod(
    nod_state_key: &StateKeyHandle,
    nod: &EncryptedNodV1,
) -> Result<U256, NodEncryptionError> {
    Ok(open_nod(nod_state_key, nod)?.0)
}

/// Open the nominal of an [`EncryptedNodV1`] with the NOD state key.
pub fn decrypt_nod_nominal(
    nod_state_key: &StateKeyHandle,
    nod: &EncryptedNodV1,
) -> Result<U256, NodEncryptionError> {
    Ok(open_nod(nod_state_key, nod)?.1)
}

fn open_nod(
    nod_state_key: &StateKeyHandle,
    nod: &EncryptedNodV1,
) -> Result<(U256, U256), NodEncryptionError> {
    if !nod.has_valid_encoding() {
        return Err(decrypt_refused());
    }
    let key = amount_key(nod_state_key, nod)?;
    let open = |domain: &SlotCipherDomain, blob: &[u8]| -> Result<U256, NodEncryptionError> {
        let (_, plaintext) = domain.slot(&key, nod.crypto_slot(), 0).read_blob(blob)?;
        let plaintext = Zeroizing::new(plaintext);
        plaintext_amount(plaintext.as_slice())
    };
    let amount = open(&AMOUNT, &nod.encrypted_gratis_amount)?;
    let nominal = open(&NOMINAL, &nod.encrypted_nominal_amount)?;
    if binding(nod_state_key, &nod.terms, amount, nominal)? != nod.encryption_binding {
        return Err(decrypt_refused());
    }
    Ok((amount, nominal))
}

/// Seal a group-load total under the NOD state key.
pub fn seal_group_load(
    nod_state_key: &StateKeyHandle,
    (chain_id, scope): (u64, B256),
    (previous_version, inputs_hash): (u64, B256),
    amount: U256,
) -> Result<EncryptedNodGroupLoadV1, NodEncryptionError> {
    let tag = nod_state_key.compute_domain_mac(
        b"outbe/nod/group-load-operation/v1",
        &[*scope, *inputs_hash],
    )?;
    let mut record = EncryptedNodGroupLoadV1 {
        chain_id,
        scope,
        operation_hash: B256::from_slice(tag.as_ref()),
        encrypted_load: Vec::new(),
    };
    record.encrypted_load = GROUP_LOAD
        .slot(nod_state_key, record.crypto_slot(), 0)
        .write_u256(previous_version, amount)?;
    Ok(record)
}

/// Open an [`EncryptedNodGroupLoadV1`] total with the NOD state key.
pub fn read_nod_group_load(
    nod_state_key: &StateKeyHandle,
    record: &EncryptedNodGroupLoadV1,
) -> Result<U256, NodEncryptionError> {
    Ok(GROUP_LOAD
        .slot(nod_state_key, record.crypto_slot(), 0)
        .read_u256(record.version(), &record.encrypted_load)?)
}

/// Apply an arithmetic transition to an [`EncryptedNodGroupLoadV1`] record.
pub fn apply_nod_group_load_operation(
    nod_state_key: &StateKeyHandle,
    request: &NodGroupLoadOpRequestV1,
) -> Result<EncryptedNodGroupLoadV1, NodEncryptionError> {
    let (previous_version, amount) = previous_load(nod_state_key, request)?;
    let (delta, add) = operation_delta(nod_state_key, request)?;
    let next = apply_delta(amount, delta, add)?;
    let inputs_hash =
        nod_group_load_inputs_hash(request).map_err(NodEncryptionError::InputsHash)?;
    seal_group_load(
        nod_state_key,
        (request.chain_id, request.scope),
        (previous_version, inputs_hash),
        next,
    )
}

/// Encrypt a NOD quote payment under the owner's view key.
pub fn encrypt_nod_quote(
    nod_state_key: &StateKeyHandle,
    nod: &EncryptedNodV1,
    context: &NodQuoteContextV1,
    payment_minor: U256,
) -> Result<Vec<u8>, NodEncryptionError> {
    if nod.terms.chain_id != context.chain_id
        || nod.terms.nod_id != context.nod_id
        || nod.terms.owner != context.owner
    {
        return Err(NodEncryptionError::QuoteContextMismatch);
    }
    let _ = decrypt_nod(nod_state_key, nod)?;
    let view_key = derive_view_key(nod_state_key, context.owner)?;
    let key = view_key.derive_with_salt(
        context.context_digest().as_slice(),
        &context.quote_key_info(),
    )?;
    let plaintext = Zeroizing::new(payment_minor.to_be_bytes::<32>());
    Ok(QUOTE_AMOUNT
        .slot(&key, context.crypto_slot(), 0)
        .write_blob(0, plaintext.as_ref())?)
}

fn decrypt_refused() -> NodEncryptionError {
    NodEncryptionError::Crypto(TeeError::DecryptFailed(None))
}

fn plaintext_amount(bytes: &[u8]) -> Result<U256, NodEncryptionError> {
    let amount: &[u8; 32] = bytes.try_into().map_err(|_| decrypt_refused())?;
    Ok(U256::from_be_bytes(*amount))
}

fn binding(
    nod_state_key: &StateKeyHandle,
    terms: &NodTermsV1,
    amount: U256,
    nominal: U256,
) -> Result<B256, NodEncryptionError> {
    Ok(B256::from(nod_state_key.compute_domain_mac(
        b"outbe/nod/issuance-binding/v3",
        &[
            *terms.digest(),
            amount.to_be_bytes::<32>(),
            nominal.to_be_bytes::<32>(),
        ],
    )?))
}

fn amount_key(
    nod_state_key: &StateKeyHandle,
    nod: &EncryptedNodV1,
) -> Result<StateKeyHandle, NodEncryptionError> {
    let view_key = derive_view_key(nod_state_key, nod.terms.owner)?;
    Ok(view_key.derive_with_salt(nod.context_digest().as_slice(), &nod.amount_key_info())?)
}

fn previous_load(
    nod_state_key: &StateKeyHandle,
    request: &NodGroupLoadOpRequestV1,
) -> Result<(u64, U256), NodEncryptionError> {
    let Some(previous) = &request.previous else {
        return Ok((0, U256::ZERO));
    };
    if previous.chain_id != request.chain_id || previous.scope != request.scope {
        return Err(NodEncryptionError::InvalidGroupLoadPredecessor);
    }
    let version = previous.version().ok_or_else(decrypt_refused)?;
    Ok((version, read_nod_group_load(nod_state_key, previous)?))
}

fn operation_delta(
    nod_state_key: &StateKeyHandle,
    request: &NodGroupLoadOpRequestV1,
) -> Result<(U256, bool), NodEncryptionError> {
    match &request.operation {
        NodGroupLoadOperationV1::AddSeriesLoad { series_load } => {
            same_chain(series_load.chain_id, request.chain_id)?;
            Ok((read_nod_group_load(nod_state_key, series_load)?, true))
        }
        NodGroupLoadOperationV1::AddNod { nod } => {
            same_chain(nod.terms.chain_id, request.chain_id)?;
            Ok((decrypt_nod(nod_state_key, nod)?, true))
        }
        NodGroupLoadOperationV1::SubNod { nod } => {
            same_chain(nod.terms.chain_id, request.chain_id)?;
            Ok((decrypt_nod(nod_state_key, nod)?, false))
        }
        NodGroupLoadOperationV1::AdjustTransient { load_minor, add } => Ok((*load_minor, *add)),
        NodGroupLoadOperationV1::AddNodNominal { nod } => {
            same_chain(nod.terms.chain_id, request.chain_id)?;
            Ok((decrypt_nod_nominal(nod_state_key, nod)?, true))
        }
    }
}

fn same_chain(found: u64, expected: u64) -> Result<(), NodEncryptionError> {
    if found == expected {
        Ok(())
    } else {
        Err(NodEncryptionError::InvalidGroupLoadPredecessor)
    }
}

fn apply_delta(amount: U256, delta: U256, add: bool) -> Result<U256, NodEncryptionError> {
    let next = if add {
        amount.checked_add(delta)
    } else {
        amount.checked_sub(delta)
    };
    next.ok_or(if add {
        NodEncryptionError::GroupLoadOverflow
    } else {
        NodEncryptionError::GroupLoadUnderflow
    })
}
