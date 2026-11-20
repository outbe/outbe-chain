use super::in_process_tee_context;
use crate::{
    dcap_protocol::{
        dcap_onboarding_attestation_preimage, dcap_onboarding_request_hash,
        dcap_verification_attestation_preimage, dcap_verification_request_hash,
        DcapOnboardingArtifactV1, DcapOnboardingContextV1, DcapOnboardingVerificationResultV1,
        DcapVerificationOutcomeV1, RegistrationVerificationRequest,
    },
    errors::TeeVerificationError,
    protocol::EnclaveResponse,
    InProcessDispatchError, TransportError,
};
use alloy_primitives::B256;

/// Verify evidence through the resident verifier and existing witness boundary.
pub fn verify_dcap_evidence_v1(
    evidence: &[u8],
    policy: &[u8],
    block_timestamp: u64,
) -> Result<DcapVerificationOutcomeV1, TransportError> {
    let expected = dcap_verification_request_hash(evidence, policy, block_timestamp)
        .map_err(|code| TeeVerificationError::InvalidVerifierInputDimensions { code })?;
    let (key, response) = crate::witness::consensus_call(
        "verify_dcap_evidence_v1",
        || {
            let context =
                in_process_tee_context().ok_or(InProcessDispatchError::ContextNotInstalled)?;
            Ok((
                context.attestation_public(),
                context
                    .verify_dcap_evidence_response_v1(evidence, policy, block_timestamp)
                    .map_err(InProcessDispatchError::from)?,
            ))
        },
        |key, response| {
            validate_dcap_verification_response(*key, expected, response.clone()).map(|_| ())
        },
    )?;
    validate_dcap_verification_response(key, expected, response)
}

/// Verify registration evidence and obtain its one-time onboarding artifact.
pub fn verify_dcap_registration_and_seal_v1(
    request: RegistrationVerificationRequest<'_>,
) -> Result<DcapOnboardingVerificationResultV1, TransportError> {
    let expected = dcap_onboarding_request_hash(request)
        .map_err(|code| TeeVerificationError::InvalidOnboardingVerifierInputDimensions { code })?;
    let (key, response) = crate::witness::consensus_call(
        "verify_dcap_registration_and_seal_v1",
        || {
            let context =
                in_process_tee_context().ok_or(InProcessDispatchError::ContextNotInstalled)?;
            Ok((
                context.attestation_public(),
                context
                    .verify_dcap_registration_response_v1(request)
                    .map_err(InProcessDispatchError::from)?,
            ))
        },
        |key, response| {
            validate_dcap_onboarding_response(*key, expected, response.clone()).map(|_| ())
        },
    )?;
    validate_dcap_onboarding_response(key, expected, response)
}

/// Produce the deterministic onboarding artifact for the exact DirectDev context.
pub fn prepare_no_attestation_dev_onboarding_artifact_v1(
    context: DcapOnboardingContextV1,
) -> Result<DcapOnboardingArtifactV1, TransportError> {
    let expected = context.context_hash();
    let (key, response) = crate::witness::consensus_call(
        "PrepareNoAttestationDevOnboardingArtifactV2",
        || {
            let runtime =
                in_process_tee_context().ok_or(InProcessDispatchError::ContextNotInstalled)?;
            let response = runtime
                .prepare_no_attestation_dev_onboarding_artifact_v2(context)
                .map_err(InProcessDispatchError::from)?;
            Ok((
                runtime.attestation_public(),
                EnclaveResponse::NoAttestationDevOnboardingArtifactPreparedV2 {
                    request_hash: response.request_hash,
                    onboarding_artifact: response.onboarding_artifact,
                    attestation_tag: response.attestation_tag.to_vec(),
                },
            ))
        },
        |key, response| validate_direct_dev_response(key, expected, context, response).map(|_| ()),
    )?;
    validate_direct_dev_response(&key, expected, context, &response)
}

fn validate_direct_dev_response(
    key: &[u8; 32],
    expected: B256,
    context: DcapOnboardingContextV1,
    response: &EnclaveResponse,
) -> Result<DcapOnboardingArtifactV1, TransportError> {
    let EnclaveResponse::NoAttestationDevOnboardingArtifactPreparedV2 {
        request_hash,
        onboarding_artifact,
        attestation_tag,
    } = response
    else {
        return Err(TransportError::UnexpectedResponse);
    };
    if *request_hash != expected {
        return Err(TransportError::UnexpectedResponse);
    }
    let preimage = crate::tee_call::canonical_attestation_preimage(
        b"outbe/tee/gramine-direct-dev-onboarding-result/v2",
        expected,
        onboarding_artifact,
    )?;
    crate::tee_call::verify_enclave_attestation(key, &preimage, attestation_tag)
        .map_err(TeeVerificationError::DirectDevAttestation)?;
    let artifact = DcapOnboardingArtifactV1::decode_canonical(onboarding_artifact)
        .map_err(|code| TeeVerificationError::DirectDevArtifactNonCanonical { code })?;
    if artifact.context != context {
        return Err(TransportError::UnexpectedResponse);
    }
    Ok(artifact)
}

fn validate_dcap_verification_response(
    attestation_pub: [u8; 32],
    expected_request_hash: B256,
    response: EnclaveResponse,
) -> Result<DcapVerificationOutcomeV1, TransportError> {
    use ed25519_dalek::{Signature, VerifyingKey};

    let EnclaveResponse::DcapVerificationFinishedV1 {
        request_hash,
        outcome,
        attestation_tag,
    } = response
    else {
        return Err(TransportError::UnexpectedResponse);
    };
    if request_hash != expected_request_hash {
        return Err(TeeVerificationError::VerificationCommitmentMismatch.into());
    }
    let canonical = DcapVerificationOutcomeV1::decode_canonical(&outcome)
        .map_err(|code| TeeVerificationError::ResponseOutcomeNonCanonical { code })?;
    let preimage = dcap_verification_attestation_preimage(request_hash, &outcome)
        .map_err(|code| TeeVerificationError::ResponseOutcomeOversized { code })?;
    let verifying_key = VerifyingKey::from_bytes(&attestation_pub)
        .map_err(TeeVerificationError::InvalidEnclaveAttestationKey)?;
    let signature_bytes: [u8; 64] = attestation_tag
        .try_into()
        .map_err(|tag: Vec<u8>| TeeVerificationError::InvalidVerificationTagLength(tag.len()))?;
    verifying_key
        .verify_strict(&preimage, &Signature::from_bytes(&signature_bytes))
        .map_err(TeeVerificationError::InvalidVerificationTag)?;
    Ok(canonical)
}

fn validate_dcap_onboarding_response(
    attestation_pub: [u8; 32],
    expected_request_hash: B256,
    response: EnclaveResponse,
) -> Result<DcapOnboardingVerificationResultV1, TransportError> {
    use ed25519_dalek::{Signature, VerifyingKey};

    let EnclaveResponse::DcapOnboardingVerificationFinishedV1 {
        request_hash,
        outcome,
        onboarding_artifact,
        attestation_tag,
    } = response
    else {
        return Err(TransportError::UnexpectedResponse);
    };
    if request_hash != expected_request_hash {
        return Err(TeeVerificationError::OnboardingCommitmentMismatch.into());
    }
    let verified = decode_onboarding_result(&outcome, &onboarding_artifact)?;
    let preimage =
        dcap_onboarding_attestation_preimage(request_hash, &outcome, &onboarding_artifact)
            .map_err(|code| TeeVerificationError::OnboardingResponseOversized { code })?;
    let verifying_key = VerifyingKey::from_bytes(&attestation_pub)
        .map_err(TeeVerificationError::InvalidEnclaveAttestationKey)?;
    let signature_bytes: [u8; 64] = attestation_tag
        .try_into()
        .map_err(|tag: Vec<u8>| TeeVerificationError::InvalidOnboardingTagLength(tag.len()))?;
    verifying_key
        .verify_strict(&preimage, &Signature::from_bytes(&signature_bytes))
        .map_err(TeeVerificationError::InvalidOnboardingTag)?;
    Ok(verified)
}

fn decode_onboarding_result(
    outcome: &[u8],
    onboarding_artifact: &[u8],
) -> Result<DcapOnboardingVerificationResultV1, TransportError> {
    let canonical = DcapVerificationOutcomeV1::decode_canonical(outcome)
        .map_err(|code| TeeVerificationError::OnboardingOutcomeNonCanonical { code })?;
    let artifact = if onboarding_artifact.is_empty() {
        None
    } else {
        Some(
            DcapOnboardingArtifactV1::decode_canonical(onboarding_artifact)
                .map_err(|code| TeeVerificationError::OnboardingArtifactNonCanonical { code })?,
        )
    };
    match (&canonical, &artifact) {
        (DcapVerificationOutcomeV1::Accepted(_), Some(_))
        | (DcapVerificationOutcomeV1::Rejected(_), None) => {}
        _ => return Err(TeeVerificationError::InvalidOnboardingOutcomeArtifactPair.into()),
    }
    Ok(DcapOnboardingVerificationResultV1 {
        outcome: canonical,
        artifact,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dcap_verification_response_requires_exact_hash_canonical_outcome_and_signature() {
        use ed25519_dalek::{Signer as _, SigningKey};

        let signer = SigningKey::from_bytes(&[0x71; 32]);
        let public = signer.verifying_key().to_bytes();
        let request_hash = B256::repeat_byte(0x72);
        let outcome = DcapVerificationOutcomeV1::Rejected(
            crate::dcap_protocol::DcapRejectCodeV1::PlatformTcbRejected,
        )
        .encode_canonical()
        .unwrap();
        let tag = signer
            .sign(&dcap_verification_attestation_preimage(request_hash, &outcome).unwrap())
            .to_bytes()
            .to_vec();
        let response = || EnclaveResponse::DcapVerificationFinishedV1 {
            request_hash,
            outcome: outcome.clone(),
            attestation_tag: tag.clone(),
        };
        assert_eq!(
            validate_dcap_verification_response(public, request_hash, response()).unwrap(),
            DcapVerificationOutcomeV1::Rejected(
                crate::dcap_protocol::DcapRejectCodeV1::PlatformTcbRejected
            )
        );

        assert!(
            validate_dcap_verification_response(public, B256::repeat_byte(0x73), response())
                .is_err()
        );
        let mut bad_outcome = response();
        if let EnclaveResponse::DcapVerificationFinishedV1 { outcome, .. } = &mut bad_outcome {
            outcome.push(0);
        }
        let error =
            validate_dcap_verification_response(public, request_hash, bad_outcome).unwrap_err();
        assert!(matches!(
            error,
            TransportError::Verification(TeeVerificationError::ResponseOutcomeNonCanonical {
                code: crate::dcap_protocol::DcapRejectCodeV1::NativeOutputMalformed,
            })
        ));
        assert_eq!(
            error.to_string(),
            "DCAP verification channel invalid: enclave response outcome is non-canonical: 0x0403"
        );
        let mut bad_tag = response();
        if let EnclaveResponse::DcapVerificationFinishedV1 {
            attestation_tag, ..
        } = &mut bad_tag
        {
            attestation_tag[0] ^= 1;
        }
        assert!(validate_dcap_verification_response(public, request_hash, bad_tag).is_err());
    }

    #[test]
    fn onboarding_response_requires_signed_exact_artifact_and_valid_pairing() {
        use ed25519_dalek::{Signer as _, SigningKey};

        let signer = SigningKey::from_bytes(&[0x31; 32]);
        let public = signer.verifying_key().to_bytes();
        let request_hash = B256::repeat_byte(0x32);
        let outcome = DcapVerificationOutcomeV1::Accepted(crate::dcap_protocol::DcapVerdictV1 {
            mrenclave: B256::repeat_byte(0x33),
            mrsigner: B256::repeat_byte(0x34),
            isv_prod_id: 1,
            isv_svn: 2,
            pck_ca: crate::dcap_protocol::DcapPckCaV1::Processor,
            fmspc: [0x35; 6],
            pce_id: 3,
            platform_tcb_status: crate::dcap_protocol::DcapPlatformTcbStatusV1::UpToDate,
            advisory_ids: Vec::new(),
            tcb_evaluation_data_number: 4,
            qe_tcb_evaluation_data_number: 5,
            collateral_valid_until: 6,
        })
        .encode_canonical()
        .unwrap();
        let artifact = DcapOnboardingArtifactV1 {
            context: crate::test_utils::onboarding_context_fixture(
                [0x41, 0x42, 0x43, 0x44, 0x45, 0x4a, 0x4b, 0x46, 0x47],
                7,
                8,
            ),
            nonce: [0x48; 12],
            ciphertext: vec![0x49; 112],
        }
        .encode_canonical()
        .unwrap();
        let tag = signer
            .sign(&dcap_onboarding_attestation_preimage(request_hash, &outcome, &artifact).unwrap())
            .to_bytes()
            .to_vec();
        let response = || EnclaveResponse::DcapOnboardingVerificationFinishedV1 {
            request_hash,
            outcome: outcome.clone(),
            onboarding_artifact: artifact.clone(),
            attestation_tag: tag.clone(),
        };
        let result = validate_dcap_onboarding_response(public, request_hash, response()).unwrap();
        assert!(matches!(
            result.outcome,
            DcapVerificationOutcomeV1::Accepted(_)
        ));
        assert!(result.artifact.is_some());

        let mut tampered = response();
        if let EnclaveResponse::DcapOnboardingVerificationFinishedV1 {
            onboarding_artifact,
            ..
        } = &mut tampered
        {
            let last = onboarding_artifact.len() - 1;
            onboarding_artifact[last] ^= 1;
        }
        assert!(validate_dcap_onboarding_response(public, request_hash, tampered).is_err());

        let rejected_with_artifact = EnclaveResponse::DcapOnboardingVerificationFinishedV1 {
            request_hash,
            outcome: DcapVerificationOutcomeV1::Rejected(
                crate::dcap_protocol::DcapRejectCodeV1::NodeSignatureInvalid,
            )
            .encode_canonical()
            .unwrap(),
            onboarding_artifact: artifact,
            attestation_tag: tag,
        };
        assert!(
            validate_dcap_onboarding_response(public, request_hash, rejected_with_artifact)
                .is_err()
        );
    }
}
