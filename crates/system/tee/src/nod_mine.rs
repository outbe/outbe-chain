//! Confidential NOD exercise and authenticated internal calculation reads.
use crate::{
    protocol::{EnclaveRequest, EnclaveResponse, FidelityOpOutcome, FidelityOpSection, ModifyAuth},
    TransportError,
};
use alloy_primitives::{B256, U256};
use outbe_primitives::nod_encryption::EncryptedNodV2;
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MineEncryptedNodRequestV2 {
    pub nod: EncryptedNodV2,
    pub current_balance: Vec<u8>,
    pub modify_auth: ModifyAuth,
    pub fidelity: FidelityOpSection,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MineEncryptedNodResultV2 {
    pub new_balance: Vec<u8>,
    pub next_op_nonce: u64,
    pub fidelity: FidelityOpOutcome,
    pub inputs_canonical_hash: B256,
    pub attestation_tag: Vec<u8>,
}
pub fn inputs_hash(request: &MineEncryptedNodRequestV2) -> Result<B256, serde_json::Error> {
    crate::nod_materialization::hash(b"outbe/nod/mine-inputs/v2", request)
}
pub fn result_preimage(result: &MineEncryptedNodResultV2) -> Result<Vec<u8>, serde_json::Error> {
    let mut result = result.clone();
    result.attestation_tag.clear();
    let mut bytes = b"outbe/nod/mine-result/v2".to_vec();
    bytes.extend_from_slice(&serde_json::to_vec(&result)?);
    Ok(bytes)
}
pub fn mine_encrypted_nod(
    request: MineEncryptedNodRequestV2,
) -> Result<MineEncryptedNodResultV2, TransportError> {
    let expected = inputs_hash(&request).map_err(codec)?;
    let (key, response) =
        crate::tribute_client::request_local_enclave(EnclaveRequest::MineEncryptedNodV2 {
            request: Box::new(request),
        })?;
    match response {
        EnclaveResponse::EncryptedNodMinedV2 { result }
            if result.inputs_canonical_hash == expected =>
        {
            let preimage = result_preimage(&result).map_err(codec)?;
            crate::tribute_v2::verify_attestation(&key, &preimage, &result.attestation_tag)?;
            Ok(*result)
        }
        EnclaveResponse::EncryptedNodMintRejectedV2 {
            reason,
            inputs_canonical_hash,
            attestation_tag,
        } if inputs_canonical_hash == expected => {
            let preimage = crate::nod_materialization::attestation(
                b"outbe/nod/mint-rejected/v2",
                expected,
                &reason,
            )
            .map_err(codec)?;
            crate::tribute_v2::verify_attestation(&key, &preimage, &attestation_tag)?;
            Err(TransportError::NodMintRejected(reason))
        }
        EnclaveResponse::Error { message } => Err(TransportError::EnclaveError(message)),
        _ => Err(TransportError::UnexpectedResponse),
    }
}
pub fn nod_read_hash(nod: &EncryptedNodV2) -> Result<B256, serde_json::Error> {
    crate::nod_materialization::hash(b"outbe/nod/read-inputs/v2", nod)
}
pub fn nod_read_preimage(inputs: B256, amount: U256) -> Vec<u8> {
    let mut bytes = b"outbe/nod/read-result/v2".to_vec();
    bytes.extend_from_slice(inputs.as_slice());
    bytes.extend_from_slice(&amount.to_be_bytes::<32>());
    bytes
}
pub fn read_nod_amount(nod: &EncryptedNodV2) -> Result<U256, TransportError> {
    let expected = nod_read_hash(nod).map_err(codec)?;
    let (key, response) =
        crate::tribute_client::request_local_enclave(EnclaveRequest::ReadNodAmountV2 {
            nod: nod.clone(),
        })?;
    match response {
        EnclaveResponse::NodAmountReadV2 {
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
fn codec(error: serde_json::Error) -> TransportError {
    TransportError::Codec(error.to_string())
}

#[cfg(feature = "e2e-test")]
pub fn create_nod_for_test(
    terms: outbe_primitives::nod_encryption::NodTermsV2,
    creator_public: [u8; 32],
    amount: U256,
) -> Result<EncryptedNodV2, TransportError> {
    let expected = crate::nod_materialization::hash(
        b"outbe/nod/test-create-inputs/v2",
        &(&terms, &creator_public, amount),
    )
    .map_err(codec)?;
    let (key, response) =
        crate::tribute_client::request_local_enclave(EnclaveRequest::CreateNodForTestV2 {
            terms,
            creator_public,
            amount,
        })?;
    match response {
        EnclaveResponse::NodCreatedForTestV2 {
            nod,
            inputs_canonical_hash,
            attestation_tag,
        } if inputs_canonical_hash == expected => {
            let preimage = crate::nod_materialization::attestation(
                b"outbe/nod/test-create-result/v2",
                expected,
                &nod,
            )
            .map_err(codec)?;
            crate::tribute_v2::verify_attestation(&key, &preimage, &attestation_tag)?;
            Ok(nod)
        }
        EnclaveResponse::Error { message } => Err(TransportError::EnclaveError(message)),
        _ => Err(TransportError::UnexpectedResponse),
    }
}
