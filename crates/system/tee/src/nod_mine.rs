//! Confidential NOD mining and authenticated internal calculation reads.
use crate::{
    protocol::{EnclaveRequest, EnclaveResponse, FidelityOpOutcome, FidelityOpSection, ModifyAuth},
    response_auth::{codec, NodOperation},
    TransportError,
};
use alloy_primitives::{B256, U256};
use outbe_primitives::{
    nod_encryption::{EncryptedNodGroupLoadV1, EncryptedNodV1, NodQuoteContextV1, NodTermsV1},
    tribute_day_encryption::EncryptedTributeDayFractionsV1,
    tribute_encryption::EncryptedTributeV2,
};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MineEncryptedNodRequestV1 {
    pub nod: EncryptedNodV1,
    pub current_balance: Vec<u8>,
    pub modify_auth: ModifyAuth,
    pub fidelity: FidelityOpSection,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MineEncryptedNodResultV1 {
    pub new_balance: Vec<u8>,
    #[serde(default)]
    pub encrypted_event_amount: Vec<u8>,
    pub next_op_nonce: u64,
    pub fidelity: FidelityOpOutcome,
    pub inputs_canonical_hash: B256,
    pub attestation_tag: Vec<u8>,
}
pub fn inputs_hash(request: &MineEncryptedNodRequestV1) -> Result<B256, serde_json::Error> {
    crate::response_auth::hash(b"outbe/nod/mine-inputs/v1", request)
}
pub fn result_preimage(result: &MineEncryptedNodResultV1) -> Result<Vec<u8>, serde_json::Error> {
    let mut result = result.clone();
    result.attestation_tag.clear();
    let mut bytes = b"outbe/nod/mine-result/v1".to_vec();
    bytes.extend_from_slice(&serde_json::to_vec(&result)?);
    Ok(bytes)
}
pub fn mine_encrypted_nod(
    request: MineEncryptedNodRequestV1,
) -> Result<MineEncryptedNodResultV1, TransportError> {
    let expected = inputs_hash(&request).map_err(codec)?;
    let (key, response) =
        crate::tribute_client::request_local_enclave(EnclaveRequest::MineEncryptedNodV1 {
            request: Box::new(request),
        })?;
    match response {
        EnclaveResponse::EncryptedNodMinedV1 { result }
            if result.inputs_canonical_hash == expected =>
        {
            let preimage = result_preimage(&result).map_err(codec)?;
            crate::tribute_v2::verify_attestation(&key, &preimage, &result.attestation_tag)?;
            Ok(*result)
        }
        response => NodOperation::Mine.reject_or_unexpected(&key, expected, response),
    }
}
pub fn nod_read_hash(nod: &EncryptedNodV1) -> Result<B256, serde_json::Error> {
    crate::response_auth::hash(b"outbe/nod/read-inputs/v1", nod)
}
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
) -> Result<T, TransportError> {
    if inputs_canonical_hash != expected {
        return Err(TransportError::UnexpectedResponse);
    }
    let preimage = crate::response_auth::attestation(domain, expected, &output).map_err(codec)?;
    crate::tribute_v2::verify_attestation(key, &preimage, &attestation_tag)?;
    Ok(output)
}

fn request_read_u256(
    expected: Result<B256, serde_json::Error>,
    request: EnclaveRequest,
    preimage_fn: fn(B256, U256) -> Vec<u8>,
) -> Result<U256, TransportError> {
    let expected = expected.map_err(codec)?;
    let (key, response) = crate::tribute_client::request_local_enclave(request)?;
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
        EnclaveResponse::Error { message } => return Err(TransportError::EnclaveError(message)),
        _ => return Err(TransportError::UnexpectedResponse),
    };
    if inputs_canonical_hash != expected {
        return Err(TransportError::UnexpectedResponse);
    }
    crate::tribute_v2::verify_attestation(&key, &preimage_fn(expected, amount), &attestation_tag)?;
    Ok(amount)
}

pub fn read_nod_amount(nod: &EncryptedNodV1) -> Result<U256, TransportError> {
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
) -> Result<EncryptedNodV1, TransportError> {
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
        EnclaveResponse::Error { message } => Err(TransportError::EnclaveError(message)),
        _ => Err(TransportError::UnexpectedResponse),
    }
}
pub fn encrypt_nod_for_tribute(
    tribute: &EncryptedTributeV2,
    terms: NodTermsV1,
    amount: U256,
) -> Result<EncryptedNodV1, TransportError> {
    let expected = crate::response_auth::hash(
        b"outbe/nod/tribute-create-inputs/v1",
        &(tribute, &terms, amount),
    )
    .map_err(codec)?;
    let (key, response) =
        crate::tribute_client::request_local_enclave(EnclaveRequest::EncryptNodForTributeV1 {
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IssueNodFromTributeRequestV1 {
    pub tribute: EncryptedTributeV2,
    pub day_fractions: EncryptedTributeDayFractionsV1,
    pub terms: NodTermsV1,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum IssueNodFromTributeOutcomeV1 {
    Issued(EncryptedNodV1),
    LysisArithmetic(String),
    ZeroSettlementCost,
}

pub fn issue_nod_from_tribute(
    request: IssueNodFromTributeRequestV1,
) -> Result<IssueNodFromTributeOutcomeV1, TransportError> {
    let expected = crate::response_auth::hash(b"outbe/nod/issue-from-tribute-inputs/v1", &request)
        .map_err(codec)?;
    let (key, response) =
        crate::tribute_client::request_local_enclave(EnclaveRequest::IssueNodFromTributeV1 {
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
        EnclaveResponse::Error { message } => Err(TransportError::EnclaveError(message)),
        _ => Err(TransportError::UnexpectedResponse),
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum NodGroupLoadOperationV1 {
    AddSeriesLoad {
        series_load: Box<EncryptedNodGroupLoadV1>,
    },
    AddNod {
        nod: Box<EncryptedNodV1>,
    },
    SubNod {
        nod: Box<EncryptedNodV1>,
    },
    AdjustTransient {
        load_minor: U256,
        add: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NodGroupLoadOpRequestV1 {
    pub chain_id: u64,
    pub scope: B256,
    pub previous: Option<EncryptedNodGroupLoadV1>,
    pub operation: NodGroupLoadOperationV1,
}

pub fn nod_group_load_inputs_hash(
    request: &NodGroupLoadOpRequestV1,
) -> Result<B256, serde_json::Error> {
    crate::response_auth::hash(b"outbe/nod/group-load-inputs/v1", request)
}

pub fn nod_group_load_attestation_preimage(
    inputs_hash: B256,
    record: &EncryptedNodGroupLoadV1,
) -> Result<Vec<u8>, serde_json::Error> {
    crate::response_auth::attestation(b"outbe/nod/group-load-result/v1", inputs_hash, record)
}

pub fn apply_nod_group_load_operation(
    request: NodGroupLoadOpRequestV1,
) -> Result<EncryptedNodGroupLoadV1, TransportError> {
    let expected = nod_group_load_inputs_hash(&request).map_err(codec)?;
    let chain_id = request.chain_id;
    let scope = request.scope;
    let version = request
        .previous
        .as_ref()
        .map_or(Some(1), |previous| previous.version()?.checked_add(1))
        .ok_or_else(|| TransportError::EnclaveError("invalid Nod group load version".into()))?;
    let (key, response) =
        crate::tribute_client::request_local_enclave(EnclaveRequest::ApplyNodGroupLoadOpV1 {
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
            crate::tribute_v2::verify_attestation(&key, &preimage, &attestation_tag)?;
            Ok(record)
        }
        EnclaveResponse::Error { message } => Err(TransportError::EnclaveError(message)),
        _ => Err(TransportError::UnexpectedResponse),
    }
}

pub fn nod_group_load_read_hash(
    record: &EncryptedNodGroupLoadV1,
) -> Result<B256, serde_json::Error> {
    crate::response_auth::hash(b"outbe/nod/group-load-read-inputs/v1", record)
}

pub fn nod_group_load_read_preimage(inputs: B256, amount: U256) -> Vec<u8> {
    let mut bytes = b"outbe/nod/group-load-read-result/v1".to_vec();
    bytes.extend_from_slice(inputs.as_slice());
    bytes.extend_from_slice(&amount.to_be_bytes::<32>());
    bytes
}

pub fn read_nod_group_load(record: &EncryptedNodGroupLoadV1) -> Result<U256, TransportError> {
    request_read_u256(
        nod_group_load_read_hash(record),
        EnclaveRequest::ReadNodGroupLoadV1 {
            record: record.clone(),
        },
        nod_group_load_read_preimage,
    )
}

pub fn encrypt_nod_quote(
    nod: &EncryptedNodV1,
    context: NodQuoteContextV1,
    payment_minor: U256,
) -> Result<Vec<u8>, TransportError> {
    let expected = crate::response_auth::hash(
        b"outbe/nod/quote-encrypt-inputs/v1",
        &(nod, &context, payment_minor),
    )
    .map_err(codec)?;
    let (key, response) =
        crate::tribute_client::request_local_enclave(EnclaveRequest::EncryptNodQuoteV1 {
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
        EnclaveResponse::Error { message } => Err(TransportError::EnclaveError(message)),
        _ => Err(TransportError::UnexpectedResponse),
    }
}
