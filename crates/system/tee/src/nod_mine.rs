//! Confidential NOD mining and authenticated internal calculation reads.
use crate::{
    protocol::{EnclaveRequest, EnclaveResponse, FidelityOpOutcome, FidelityOpSection, ModifyAuth},
    response_auth::{codec, NodOperation},
    TransportError,
};
use alloy_primitives::{B256, U256};
use outbe_primitives::{
    nod_encryption::{EncryptedNodV1, NodTermsV1},
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
pub fn read_nod_amount(nod: &EncryptedNodV1) -> Result<U256, TransportError> {
    let expected = nod_read_hash(nod).map_err(codec)?;
    let (key, response) =
        crate::tribute_client::request_local_enclave(EnclaveRequest::ReadNodAmountV1 {
            nod: nod.clone(),
        })?;
    match response {
        EnclaveResponse::NodAmountReadV1 {
            amount,
            inputs_canonical_hash,
            attestation_tag,
        } if inputs_canonical_hash == expected => {
            crate::tribute_v2::verify_attestation(
                &key,
                &nod_read_preimage(expected, amount),
                &attestation_tag,
            )?;
            Ok(amount)
        }
        EnclaveResponse::Error { message } => Err(TransportError::EnclaveError(message)),
        _ => Err(TransportError::UnexpectedResponse),
    }
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
        }
        | EnclaveResponse::NodCreatedForTestV1 {
            nod,
            inputs_canonical_hash,
            attestation_tag,
        } if inputs_canonical_hash == expected => {
            let preimage =
                crate::response_auth::attestation(domain, expected, &nod).map_err(codec)?;
            crate::tribute_v2::verify_attestation(key, &preimage, &attestation_tag)?;
            Ok(nod)
        }
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
#[cfg(feature = "e2e-test")]
pub fn create_nod_for_test(
    terms: outbe_primitives::nod_encryption::NodTermsV1,
    creator_public: [u8; 32],
    amount: U256,
) -> Result<EncryptedNodV1, TransportError> {
    let expected = crate::response_auth::hash(
        b"outbe/nod/test-create-inputs/v1",
        &(&terms, &creator_public, amount),
    )
    .map_err(codec)?;
    let (key, response) =
        crate::tribute_client::request_local_enclave(EnclaveRequest::CreateNodForTestV1 {
            terms,
            creator_public,
            amount,
        })?;
    verify_created_nod(&key, b"outbe/nod/test-create-result/v1", expected, response)
}
