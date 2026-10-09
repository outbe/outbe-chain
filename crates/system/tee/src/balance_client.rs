//! Host transport and attestation checks for confidential balance operations.

use alloy_primitives::B256;
use outbe_primitives::error::{PrecompileError, Result};

use crate::protocol::{
    gratis_op_canonical_hash, promis_op_canonical_hash, EnclaveRequest, EnclaveResponse,
};
use crate::TransportError;

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
    .ok_or_else(|| PrecompileError::EnclaveUnavailable("tee_sidecar_unavailable".to_string()))?;
    let response = response.map_err(request_failure)?;
    validate_response(operation, expected_hash, &attestation_pub, &response)?;
    Ok(response)
}

/// A balance op's `Error` answer only reports this enclave's state (no group key, session,
/// readiness); input-driven refusals come back as a `Rejected` status instead.
fn request_failure(error: TransportError) -> PrecompileError {
    if error.is_node_local() || matches!(error, TransportError::EnclaveError(_)) {
        PrecompileError::EnclaveUnavailable(format!("tee_sidecar_unavailable: {error}"))
    } else {
        PrecompileError::Fatal(format!("enclave request failed: {error}"))
    }
}

fn validate_response(
    operation: BalanceOperation,
    expected_hash: B256,
    attestation_pub: &[u8; 32],
    response: &EnclaveResponse,
) -> Result<()> {
    match (operation, response) {
        (BalanceOperation::Gratis, EnclaveResponse::GratisOpApplied { result }) => {
            let actual_hash = result.inputs_canonical_hash;
            check_canonical_hash(expected_hash, actual_hash)?;
            crate::verify_gratis_op_attestation(
                attestation_pub,
                actual_hash,
                result,
                &result.attestation_tag,
            )
            .map_err(|error| {
                PrecompileError::Fatal(format!("tee_gratis_attestation_invalid: {error}"))
            })
        }
        (BalanceOperation::Promis, EnclaveResponse::PromisOpApplied { result }) => {
            let actual_hash = result.inputs_canonical_hash;
            check_canonical_hash(expected_hash, actual_hash)?;
            crate::verify_promis_op_attestation(
                attestation_pub,
                actual_hash,
                result,
                &result.attestation_tag,
            )
            .map_err(|error| {
                PrecompileError::Fatal(format!("tee_promis_attestation_invalid: {error}"))
            })
        }
        (_, EnclaveResponse::Error { message }) => Err(PrecompileError::EnclaveUnavailable(
            format!("enclave {} error: {message}", operation.name(),),
        )),
        _ => Err(PrecompileError::Fatal(format!(
            "unexpected enclave response: {response:?}"
        ))),
    }
}

fn check_canonical_hash(expected_hash: B256, actual_hash: B256) -> Result<()> {
    if actual_hash != expected_hash {
        return Err(PrecompileError::Fatal(
            "tee_enclave_nondeterminism".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{
        gratis_op_attestation_preimage, promis_op_attestation_preimage, GratisOpResult,
        GratisOpStatus, PromisOpResult, PromisOpStatus,
    };
    use alloy_primitives::U256;
    use ed25519_dalek::{Signer, SigningKey};

    fn gratis_result(hash: B256) -> GratisOpResult {
        GratisOpResult {
            status: GratisOpStatus::Applied,
            new_balance: Vec::new(),
            new_pledged: Vec::new(),
            event_amount: U256::ZERO,
            next_op_nonce: 0,
            fidelity: None,
            inputs_canonical_hash: hash,
            attestation_tag: Vec::new(),
        }
    }

    fn promis_result(hash: B256) -> PromisOpResult {
        PromisOpResult {
            status: PromisOpStatus::Applied,
            new_balance: Vec::new(),
            event_amount: U256::ZERO,
            next_op_nonce: 0,
            inputs_canonical_hash: hash,
            attestation_tag: Vec::new(),
        }
    }

    #[test]
    fn enclave_error_and_wrong_variant_precede_hash_validation() {
        let hash = B256::repeat_byte(7);
        let key = [0; 32];
        let response = EnclaveResponse::Error {
            message: "denied".into(),
        };
        for (operation, name) in [
            (BalanceOperation::Gratis, "ApplyGratisOp"),
            (BalanceOperation::Promis, "ApplyPromisOp"),
        ] {
            let expected = format!("enclave {name} error: denied");
            assert!(matches!(
                validate_response(operation, hash, &key, &response),
                Err(PrecompileError::EnclaveUnavailable(message)) if message == expected
            ));
        }

        let wrong_for_gratis = EnclaveResponse::PromisOpApplied {
            result: Box::new(promis_result(B256::ZERO)),
        };
        let expected = format!("unexpected enclave response: {wrong_for_gratis:?}");
        assert!(matches!(
            validate_response(BalanceOperation::Gratis, hash, &key, &wrong_for_gratis),
            Err(PrecompileError::Fatal(message)) if message == expected
        ));
        let wrong_for_promis = EnclaveResponse::GratisOpApplied {
            result: Box::new(gratis_result(B256::ZERO)),
        };
        let expected = format!("unexpected enclave response: {wrong_for_promis:?}");
        assert!(matches!(
            validate_response(BalanceOperation::Promis, hash, &key, &wrong_for_promis),
            Err(PrecompileError::Fatal(message)) if message == expected
        ));
    }

    #[test]
    fn an_enclave_refusal_or_outage_is_unavailable_and_a_bad_answer_is_fatal() {
        for error in [
            TransportError::Unavailable("enclave is not initialized".into()),
            TransportError::EnclaveError("no resident group key".into()),
            TransportError::Noise("x".into()),
            TransportError::Handshake("x".into()),
            TransportError::SessionRevoked("x"),
            TransportError::IdentityMismatch("x".into()),
        ] {
            assert!(matches!(
                request_failure(error),
                PrecompileError::EnclaveUnavailable(_)
            ));
        }
        for error in [
            TransportError::Codec("x".into()),
            TransportError::UnexpectedResponse,
            TransportError::GratisOpAttestation("x".into()),
        ] {
            assert!(matches!(request_failure(error), PrecompileError::Fatal(_)));
        }
    }

    #[test]
    fn canonical_hash_mismatch_precedes_attestation_verification() {
        let expected_hash = B256::repeat_byte(7);
        let key = [0; 32];
        let responses = [
            (
                BalanceOperation::Gratis,
                EnclaveResponse::GratisOpApplied {
                    result: Box::new(gratis_result(B256::ZERO)),
                },
            ),
            (
                BalanceOperation::Promis,
                EnclaveResponse::PromisOpApplied {
                    result: Box::new(promis_result(B256::ZERO)),
                },
            ),
        ];
        for (operation, response) in responses {
            assert!(matches!(
                validate_response(operation, expected_hash, &key, &response),
                Err(PrecompileError::Fatal(message)) if message == "tee_enclave_nondeterminism"
            ));
        }
    }

    #[test]
    fn matching_hash_uses_operation_specific_attestation() {
        let hash = B256::repeat_byte(7);
        let key = [0; 32];
        let gratis = EnclaveResponse::GratisOpApplied {
            result: Box::new(gratis_result(hash)),
        };
        assert!(matches!(
            validate_response(BalanceOperation::Gratis, hash, &key, &gratis),
            Err(PrecompileError::Fatal(message))
                if message.starts_with("tee_gratis_attestation_invalid:")
        ));
        let promis = EnclaveResponse::PromisOpApplied {
            result: Box::new(promis_result(hash)),
        };
        assert!(matches!(
            validate_response(BalanceOperation::Promis, hash, &key, &promis),
            Err(PrecompileError::Fatal(message))
                if message.starts_with("tee_promis_attestation_invalid:")
        ));

        let signer = SigningKey::from_bytes(&[7; 32]);
        let attestation_pub = signer.verifying_key().to_bytes();
        let mut gratis_result = gratis_result(hash);
        gratis_result.attestation_tag = signer
            .sign(&gratis_op_attestation_preimage(hash, &gratis_result))
            .to_bytes()
            .to_vec();
        assert!(validate_response(
            BalanceOperation::Gratis,
            hash,
            &attestation_pub,
            &EnclaveResponse::GratisOpApplied {
                result: Box::new(gratis_result),
            },
        )
        .is_ok());
        let mut promis_result = promis_result(hash);
        promis_result.attestation_tag = signer
            .sign(&promis_op_attestation_preimage(hash, &promis_result))
            .to_bytes()
            .to_vec();
        assert!(validate_response(
            BalanceOperation::Promis,
            hash,
            &attestation_pub,
            &EnclaveResponse::PromisOpApplied {
                result: Box::new(promis_result),
            },
        )
        .is_ok());
    }
}
