//! Host transport and attestation checks for confidential balance operations.

use alloy_primitives::B256;
use outbe_primitives::error::{PrecompileError, Result};

use crate::protocol::{
    gratis_op_canonical_hash, promis_op_canonical_hash, EnclaveRequest, EnclaveResponse,
};

#[derive(Clone, Copy)]
enum BalanceOperation {
    Gratis,
    Promis,
}

impl BalanceOperation {
    fn name(self) -> &'static str {
        match self {
            Self::Gratis => "ApplyGratisOp",
            Self::Promis => "ApplyPromisOp",
        }
    }
}

/// Execute a balance request and check its canonical hash and attestation.
pub fn execute_confidential_balance_op(request: EnclaveRequest) -> Result<EnclaveResponse> {
    let (operation, expected_hash) = match &request {
        EnclaveRequest::ApplyGratisOp { request } => {
            (BalanceOperation::Gratis, gratis_op_canonical_hash(request))
        }
        EnclaveRequest::ApplyPromisOp { request } => {
            (BalanceOperation::Promis, promis_op_canonical_hash(request))
        }
        _ => {
            return Err(PrecompileError::Fatal(
                "request is not a confidential balance operation".into(),
            ))
        }
    };
    let (attestation_pub, response) = crate::try_with_enclave(|client| {
        let attestation_pub = client.attestation_pub();
        let response = client.request(&request);
        (attestation_pub, response)
    })
    .ok_or_else(|| PrecompileError::Fatal("tee_sidecar_unavailable".to_string()))?;
    let response = response
        .map_err(|error| PrecompileError::Fatal(format!("tee_sidecar_unavailable: {error}")))?;
    validate_response(operation, expected_hash, &attestation_pub, &response)?;
    Ok(response)
}

fn validate_response(
    operation: BalanceOperation,
    expected_hash: B256,
    attestation_pub: &[u8; 32],
    response: &EnclaveResponse,
) -> Result<()> {
    let actual_hash = match (operation, response) {
        (BalanceOperation::Gratis, EnclaveResponse::GratisOpApplied { result }) => {
            result.inputs_canonical_hash
        }
        (BalanceOperation::Promis, EnclaveResponse::PromisOpApplied { result }) => {
            result.inputs_canonical_hash
        }
        (_, EnclaveResponse::Error { message }) => {
            return Err(PrecompileError::Fatal(format!(
                "enclave {} error: {message}",
                operation.name(),
            )))
        }
        _ => {
            return Err(PrecompileError::Fatal(format!(
                "unexpected enclave response: {response:?}"
            )))
        }
    };
    if actual_hash != expected_hash {
        return Err(PrecompileError::Fatal(
            "tee_enclave_nondeterminism".to_string(),
        ));
    }
    match response {
        EnclaveResponse::GratisOpApplied { result } => crate::verify_gratis_op_attestation(
            attestation_pub,
            actual_hash,
            result,
            &result.attestation_tag,
        )
        .map_err(|error| {
            PrecompileError::Fatal(format!("tee_gratis_attestation_invalid: {error}"))
        }),
        EnclaveResponse::PromisOpApplied { result } => crate::verify_promis_op_attestation(
            attestation_pub,
            actual_hash,
            result,
            &result.attestation_tag,
        )
        .map_err(|error| {
            PrecompileError::Fatal(format!("tee_promis_attestation_invalid: {error}"))
        }),
        _ => unreachable!("response kind was checked before its canonical hash"),
    }
}
