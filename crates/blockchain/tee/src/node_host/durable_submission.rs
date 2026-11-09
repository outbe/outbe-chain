//! Exact durable evidence verification shared by committed and replacement journals.
//! Operation selection and transition proof remain policy-specific; the manifest
//! binding and proof-of-possession tail run once after that selection.

use super::codec_error;
use super::replacement::{
    is_candidate_promotion_operation, validate_candidate_key_ready_proof,
    validate_direct_dev_transition_proof,
};
use crate::TransportError;
use outbe_primitives::tee_attestation_v1::{
    AttestationEvidenceV1, AttestationOperationV1, EnclaveInitializationManifestV1,
    RegistrationIntentV1,
};

#[derive(Clone, Copy)]
pub(super) enum DurableSubmissionKind {
    CommittedJoin,
    Replacement,
}

pub(super) struct DurableSubmission<'a> {
    pub evidence: &'a [u8],
    pub node_signature: &'a [u8; 65],
    pub enclave_signature: &'a [u8; 64],
    pub kind: DurableSubmissionKind,
}

pub(super) fn verify_durable_submission(
    manifest: &EnclaveInitializationManifestV1,
    submission: DurableSubmission<'_>,
) -> Result<RegistrationIntentV1, TransportError> {
    let evidence =
        AttestationEvidenceV1::decode_canonical(submission.evidence).map_err(codec_error)?;
    let intent = match submission.kind {
        DurableSubmissionKind::CommittedJoin => {
            committed_join_intent(evidence, submission.enclave_signature)?
        }
        DurableSubmissionKind::Replacement => candidate_promotion_intent(
            manifest,
            &evidence,
            submission.enclave_signature,
            "durable submission is not an allowed registration or successor operation",
        )?
        .clone(),
    };
    let possession_error = match submission.kind {
        DurableSubmissionKind::CommittedJoin => {
            "committed join submission proof of possession is invalid"
        }
        DurableSubmissionKind::Replacement => {
            "durable replacement submission proof of possession is invalid"
        }
    };
    verify_bound_possession(
        manifest,
        &intent,
        submission.node_signature,
        submission.enclave_signature,
        PossessionErrors {
            node_signature: possession_error,
            enclave_signature: possession_error,
        },
    )?;
    Ok(intent)
}

/// The errors of the proof-of-possession checks. Each submission path keeps
/// its own messages.
pub(super) struct PossessionErrors {
    pub node_signature: &'static str,
    pub enclave_signature: &'static str,
}

/// Check the manifest binding of `intent`, then the node signature, then the
/// enclave signature. The first failed check gives the error.
pub(super) fn verify_bound_possession(
    manifest: &EnclaveInitializationManifestV1,
    intent: &RegistrationIntentV1,
    node_signature: &[u8; 65],
    enclave_signature: &[u8; 64],
    errors: PossessionErrors,
) -> Result<(), TransportError> {
    manifest
        .validate_intent_binding(intent)
        .map_err(codec_error)?;
    if !intent.verify_node_signature(node_signature) {
        return Err(TransportError::Codec(errors.node_signature.into()));
    }
    if !intent.verify_enclave_signature(enclave_signature) {
        return Err(TransportError::Codec(errors.enclave_signature.into()));
    }
    Ok(())
}

fn committed_join_intent(
    evidence: AttestationEvidenceV1,
    enclave_signature: &[u8; 64],
) -> Result<RegistrationIntentV1, TransportError> {
    match evidence {
        AttestationEvidenceV1::Dcap(value)
            if value.intent.operation == AttestationOperationV1::RegisterEnclave =>
        {
            Ok(value.intent)
        }
        AttestationEvidenceV1::GramineDirectDev(value)
            if value.intent.operation == AttestationOperationV1::RegisterEnclave
                && value.dev_signature == *enclave_signature =>
        {
            Ok(value.intent)
        }
        AttestationEvidenceV1::Dcap(_) | AttestationEvidenceV1::GramineDirectDev(_) => {
            Err(TransportError::Codec(
                "committed join submission is not RegisterEnclave evidence".into(),
            ))
        }
    }
}

/// Select the candidate promotion intent of `evidence`. The operation must be
/// a registration or successor operation. DCAP evidence needs a valid
/// key-ready proof. Direct development evidence must carry `enclave_signature`
/// and, for a measurement transition, a valid transition proof. Other evidence
/// gives `disallowed_operation`.
pub(super) fn candidate_promotion_intent<'a>(
    manifest: &EnclaveInitializationManifestV1,
    evidence: &'a AttestationEvidenceV1,
    enclave_signature: &[u8; 64],
    disallowed_operation: &'static str,
) -> Result<&'a RegistrationIntentV1, TransportError> {
    match evidence {
        AttestationEvidenceV1::Dcap(value)
            if is_candidate_promotion_operation(value.intent.operation) =>
        {
            validate_candidate_key_ready_proof(manifest, value)?;
            Ok(&value.intent)
        }
        AttestationEvidenceV1::GramineDirectDev(value)
            if is_candidate_promotion_operation(value.intent.operation)
                && &value.dev_signature == enclave_signature =>
        {
            if value.intent.operation == AttestationOperationV1::TransitionEnclaveMeasurement {
                validate_direct_dev_transition_proof(manifest, value)?;
            }
            Ok(&value.intent)
        }
        AttestationEvidenceV1::Dcap(_) | AttestationEvidenceV1::GramineDirectDev(_) => {
            Err(TransportError::Codec(disallowed_operation.into()))
        }
    }
}
