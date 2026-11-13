//! Confidential NOD mining and authenticated internal calculation reads.

use alloy_primitives::{B256, U256};
use outbe_core_common::{
    nod_encryption::{EncryptedNodGroupLoadV1, EncryptedNodV1, NodQuoteContextV1, NodTermsV1},
    tribute_day_encryption::EncryptedTributeDayFractionsV1,
    tribute_encryption::EncryptedTributeV2,
};
use outbe_tee::{tee_call::verify_enclave_attestation, TransportError};
use serde::{Deserialize, Serialize};

use super::{
    errors::EnclaveClientError,
    protocol::{EnclaveRequest, EnclaveResponse, FidelityOpOutcome, FidelityOpSection, ModifyAuth},
    response_auth::{self, codec, NodOperation},
    tribute_client::request_local_enclave,
};

fn verify_nod_attestation(
    key: &[u8; 32],
    preimage: &[u8],
    tag: &[u8],
) -> Result<(), EnclaveClientError> {
    verify_enclave_attestation(key, preimage, tag).map_err(EnclaveClientError::NodAttestation)
}

/// Request to mine Gratis from an encrypted NOD record.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MineEncryptedNodRequestV1 {
    /// Encrypted NOD being redeemed.
    pub nod: EncryptedNodV1,
    /// Owner's current encrypted Gratis balance blob.
    pub current_balance: Vec<u8>,
    /// Owner's modify-key MAC authorization.
    pub modify_auth: ModifyAuth,
    /// Co-located Fidelity cohort acquisition section.
    pub fidelity: FidelityOpSection,
}

/// Result of mining Gratis from an encrypted NOD record.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MineEncryptedNodResultV1 {
    /// Updated encrypted Gratis balance blob.
    pub new_balance: Vec<u8>,
    /// Next expected Gratis op nonce.
    pub next_op_nonce: u64,
    /// Updated Fidelity cohort outcome.
    pub fidelity: FidelityOpOutcome,
    /// Echoed canonical input commitment.
    pub inputs_canonical_hash: B256,
    /// Enclave Ed25519 attestation signature (cleared inside canonical witness payloads).
    pub attestation_tag: Vec<u8>,
    /// Owner-encrypted Gratis mint event amount.
    pub encrypted_event_amount: Vec<u8>,
}

/// Canonical commitment over a [`MineEncryptedNodRequestV1`].
pub fn inputs_hash(request: &MineEncryptedNodRequestV1) -> Result<B256, serde_json::Error> {
    response_auth::hash(b"outbe/nod/mine-inputs/v1", request)
}

/// Domain-separated preimage signed by the enclave for `EncryptedNodMinedV1`.
pub fn result_preimage(result: &MineEncryptedNodResultV1) -> Result<Vec<u8>, serde_json::Error> {
    let mut result = result.clone();
    result.attestation_tag.clear();
    let mut bytes = b"outbe/nod/mine-result/v1".to_vec();
    bytes.extend_from_slice(&serde_json::to_vec(&result)?);
    Ok(bytes)
}

/// Mine Gratis from an encrypted NOD via the local enclave session.
pub fn mine_encrypted_nod(
    request: MineEncryptedNodRequestV1,
) -> Result<MineEncryptedNodResultV1, EnclaveClientError> {
    let expected = inputs_hash(&request).map_err(codec)?;
    let (key, response) = request_local_enclave(EnclaveRequest::MineEncryptedNodV1 {
        request: Box::new(request),
    })?;
    match response {
        EnclaveResponse::EncryptedNodMinedV1 { result }
            if result.inputs_canonical_hash == expected =>
        {
            let preimage = result_preimage(&result).map_err(codec)?;
            verify_nod_attestation(&key, &preimage, &result.attestation_tag)?;
            Ok(*result)
        }
        response => NodOperation::Mine.reject_or_unexpected(&key, expected, response),
    }
}

/// Canonical commitment over a `ReadNodAmountV1` input record.
pub fn nod_read_hash(nod: &EncryptedNodV1) -> Result<B256, serde_json::Error> {
    response_auth::hash(b"outbe/nod/read-inputs/v1", nod)
}

/// Domain-separated preimage signed by the enclave for `NodAmountReadV1`.
#[must_use]
pub fn nod_read_preimage(inputs: B256, amount: U256) -> Vec<u8> {
    let mut bytes = b"outbe/nod/read-result/v1".to_vec();
    bytes.extend_from_slice(inputs.as_slice());
    bytes.extend_from_slice(&amount.to_be_bytes::<32>());
    bytes
}

fn verify_attested_output<T: Serialize>(
    key: &[u8; 32],
    domain: &[u8],
    expected: B256,
    (output, inputs_canonical_hash, attestation_tag): (T, B256, Vec<u8>),
) -> Result<T, EnclaveClientError> {
    if inputs_canonical_hash != expected {
        return Err(TransportError::UnexpectedResponse.into());
    }
    let preimage = response_auth::attestation(domain, expected, &output).map_err(codec)?;
    verify_nod_attestation(key, &preimage, &attestation_tag)?;
    Ok(output)
}

fn request_read_u256(
    expected: Result<B256, serde_json::Error>,
    request: EnclaveRequest,
    preimage_fn: fn(B256, U256) -> Vec<u8>,
) -> Result<U256, EnclaveClientError> {
    let expected = expected.map_err(codec)?;
    let (key, response) = request_local_enclave(request)?;
    let (amount, inputs_canonical_hash, attestation_tag) = match response {
        EnclaveResponse::NodAmountReadV1 {
            amount,
            inputs_canonical_hash,
            attestation_tag,
        }
        | EnclaveResponse::NodGroupLoadReadV1 {
            amount,
            inputs_canonical_hash,
            attestation_tag,
        } => (amount, inputs_canonical_hash, attestation_tag),
        _ => return Err(TransportError::UnexpectedResponse.into()),
    };
    if inputs_canonical_hash != expected {
        return Err(TransportError::UnexpectedResponse.into());
    }
    verify_nod_attestation(&key, &preimage_fn(expected, amount), &attestation_tag)?;
    Ok(amount)
}

/// Read the decrypted Gratis amount inside an [`EncryptedNodV1`].
pub fn read_nod_amount(nod: &EncryptedNodV1) -> Result<U256, EnclaveClientError> {
    request_read_u256(
        nod_read_hash(nod),
        EnclaveRequest::ReadNodAmountV1 { nod: nod.clone() },
        nod_read_preimage,
    )
}

fn verify_created_nod(
    key: &[u8; 32],
    domain: &[u8],
    expected: B256,
    response: EnclaveResponse,
) -> Result<EncryptedNodV1, EnclaveClientError> {
    match response {
        EnclaveResponse::NodEncryptedForTributeV1 {
            nod,
            inputs_canonical_hash,
            attestation_tag,
        } => verify_attested_output(
            key,
            domain,
            expected,
            (nod, inputs_canonical_hash, attestation_tag),
        ),
        _ => Err(TransportError::UnexpectedResponse.into()),
    }
}

/// Encrypt a newly issued NOD record bound to an [`EncryptedTributeV2`].
pub fn encrypt_nod_for_tribute(
    tribute: &EncryptedTributeV2,
    terms: NodTermsV1,
    amount: U256,
) -> Result<EncryptedNodV1, EnclaveClientError> {
    let expected = response_auth::hash(
        b"outbe/nod/tribute-create-inputs/v1",
        &(tribute, &terms, amount),
    )
    .map_err(codec)?;
    let (key, response) = request_local_enclave(EnclaveRequest::EncryptNodForTributeV1 {
        tribute: Box::new(tribute.clone()),
        terms,
        amount,
    })?;
    verify_created_nod(
        &key,
        b"outbe/nod/tribute-create-result/v1",
        expected,
        response,
    )
}

/// Request to issue an encrypted NOD from a Tribute and settled day fractions.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IssueNodFromTributeRequestV1 {
    /// Source encrypted Tribute record.
    pub tribute: EncryptedTributeV2,
    /// Settled encrypted day-fractions record for `tribute.context.worldwide_day`.
    pub day_fractions: EncryptedTributeDayFractionsV1,
    /// Public NOD terms for the issued asset.
    pub terms: NodTermsV1,
}

/// Outcome of [`issue_nod_from_tribute`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum IssueNodFromTributeOutcomeV1 {
    /// Encrypted NOD issued with positive settlement cost.
    Issued(EncryptedNodV1),
    /// Deterministic Lysis arithmetic error message.
    LysisArithmetic(String),
    /// Computed settlement cost rounded down to zero.
    ZeroSettlementCost,
}

/// Issue an encrypted NOD from a Tribute and settled day fractions via the local enclave.
pub fn issue_nod_from_tribute(
    request: IssueNodFromTributeRequestV1,
) -> Result<IssueNodFromTributeOutcomeV1, EnclaveClientError> {
    let expected =
        response_auth::hash(b"outbe/nod/issue-from-tribute-inputs/v1", &request).map_err(codec)?;
    let (key, response) = request_local_enclave(EnclaveRequest::IssueNodFromTributeV1 {
        request: Box::new(request),
    })?;
    match response {
        EnclaveResponse::NodIssuedFromTributeV1 {
            outcome,
            inputs_canonical_hash,
            attestation_tag,
        } => verify_attested_output(
            &key,
            b"outbe/nod/issue-from-tribute-result/v1",
            expected,
            (outcome, inputs_canonical_hash, attestation_tag),
        ),
        _ => Err(TransportError::UnexpectedResponse.into()),
    }
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

/// Canonical commitment over a [`NodGroupLoadOpRequestV1`].
pub fn nod_group_load_inputs_hash(
    request: &NodGroupLoadOpRequestV1,
) -> Result<B256, serde_json::Error> {
    response_auth::hash(b"outbe/nod/group-load-inputs/v1", request)
}

/// Domain-separated preimage signed by the enclave for `NodGroupLoadOpAppliedV1`.
pub fn nod_group_load_attestation_preimage(
    inputs_hash: B256,
    record: &EncryptedNodGroupLoadV1,
) -> Result<Vec<u8>, serde_json::Error> {
    response_auth::attestation(b"outbe/nod/group-load-result/v1", inputs_hash, record)
}

/// Apply an arithmetic transition to an [`EncryptedNodGroupLoadV1`] record.
pub fn apply_nod_group_load_operation(
    request: NodGroupLoadOpRequestV1,
) -> Result<EncryptedNodGroupLoadV1, EnclaveClientError> {
    let expected = nod_group_load_inputs_hash(&request).map_err(codec)?;
    let chain_id = request.chain_id;
    let scope = request.scope;
    let version = request
        .previous
        .as_ref()
        .map_or(Some(1), |previous| previous.version()?.checked_add(1))
        .ok_or_else(|| TransportError::EnclaveError("invalid Nod group load version".into()))?;
    let (key, response) = request_local_enclave(EnclaveRequest::ApplyNodGroupLoadOpV1 {
        request: Box::new(request),
    })?;
    match response {
        EnclaveResponse::NodGroupLoadOpAppliedV1 {
            record,
            inputs_canonical_hash,
            attestation_tag,
        } if inputs_canonical_hash == expected
            && record.chain_id == chain_id
            && record.scope == scope
            && record.version() == Some(version) =>
        {
            let preimage = nod_group_load_attestation_preimage(expected, &record).map_err(codec)?;
            verify_nod_attestation(&key, &preimage, &attestation_tag)?;
            Ok(record)
        }
        _ => Err(TransportError::UnexpectedResponse.into()),
    }
}

/// Canonical commitment over a `ReadNodGroupLoadV1` input record.
pub fn nod_group_load_read_hash(
    record: &EncryptedNodGroupLoadV1,
) -> Result<B256, serde_json::Error> {
    response_auth::hash(b"outbe/nod/group-load-read-inputs/v1", record)
}

/// Domain-separated preimage signed by the enclave for `NodGroupLoadReadV1`.
#[must_use]
pub fn nod_group_load_read_preimage(inputs: B256, amount: U256) -> Vec<u8> {
    let mut bytes = b"outbe/nod/group-load-read-result/v1".to_vec();
    bytes.extend_from_slice(inputs.as_slice());
    bytes.extend_from_slice(&amount.to_be_bytes::<32>());
    bytes
}

/// Read the decrypted load total inside an [`EncryptedNodGroupLoadV1`].
pub fn read_nod_group_load(record: &EncryptedNodGroupLoadV1) -> Result<U256, EnclaveClientError> {
    request_read_u256(
        nod_group_load_read_hash(record),
        EnclaveRequest::ReadNodGroupLoadV1 {
            record: record.clone(),
        },
        nod_group_load_read_preimage,
    )
}

/// Encrypt a NOD quote payment amount under the owner's view key.
pub fn encrypt_nod_quote(
    nod: &EncryptedNodV1,
    context: NodQuoteContextV1,
    payment_minor: U256,
) -> Result<Vec<u8>, EnclaveClientError> {
    let expected = response_auth::hash(
        b"outbe/nod/quote-encrypt-inputs/v1",
        &(nod, &context, payment_minor),
    )
    .map_err(codec)?;
    let (key, response) = request_local_enclave(EnclaveRequest::EncryptNodQuoteV1 {
        nod: Box::new(nod.clone()),
        context,
        payment_minor,
    })?;
    match response {
        EnclaveResponse::NodQuoteEncryptedV1 {
            encrypted_payment,
            inputs_canonical_hash,
            attestation_tag,
        } => verify_attested_output(
            &key,
            b"outbe/nod/quote-encrypt-result/v1",
            expected,
            (encrypted_payment, inputs_canonical_hash, attestation_tag),
        ),
        _ => Err(TransportError::UnexpectedResponse.into()),
    }
}
