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
        AttestationEvidenceV1::NoAttestationDev(value)
            if value.intent.operation == AttestationOperationV1::RegisterEnclave
                && value.dev_signature == *enclave_signature =>
        {
            Ok(value.intent)
        }
        AttestationEvidenceV1::Dcap(_) | AttestationEvidenceV1::NoAttestationDev(_) => {
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
        AttestationEvidenceV1::NoAttestationDev(value)
            if is_candidate_promotion_operation(value.intent.operation)
                && &value.dev_signature == enclave_signature =>
        {
            if value.intent.operation == AttestationOperationV1::TransitionEnclaveMeasurement {
                validate_direct_dev_transition_proof(manifest, value)?;
            }
            Ok(&value.intent)
        }
        AttestationEvidenceV1::Dcap(_) | AttestationEvidenceV1::NoAttestationDev(_) => {
            Err(TransportError::Codec(disallowed_operation.into()))
        }
    }
}

impl super::CommittedJoinSubmissionV1 {
    /// Inspect old public material only after chain reconciliation; never reuse a lost recipient.
    #[cfg(test)]
    pub(crate) fn intent_for_live_manifest_v1(
        &self,
        live: &EnclaveInitializationManifestV1,
    ) -> crate::in_process::Result<RegistrationIntentV1> {
        use crate::in_process::InProcessError;
        let evidence = AttestationEvidenceV1::decode_canonical(self.evidence())?;
        let intent = match evidence {
            AttestationEvidenceV1::Dcap(value) => value.intent,
            AttestationEvidenceV1::NoAttestationDev(value) => {
                if value.dev_signature != *self.enclave_signature() {
                    return Err(InProcessError::IdentityMismatch);
                }
                value.intent
            }
        };
        live.validate_intent_binding(&intent)?;
        if intent.operation != AttestationOperationV1::RegisterEnclave
            || !intent.verify_node_signature(self.node_signature())
            || !intent.verify_enclave_signature(self.enclave_signature())
        {
            return Err(InProcessError::IdentityMismatch);
        }
        Ok(intent)
    }
}

#[cfg(test)]
mod live_submission_tests {
    use super::*;
    use ed25519_dalek::Signer as _;
    use outbe_primitives::tee_attestation_v1::NoAttestationDevEvidenceV1;

    #[test]
    fn lost_recipient_submission_is_rejected_without_deleting_recovery_evidence() {
        let active = super::super::identity::process_identity_tests::manifest(9, [4; 32]);
        let fresh = super::super::identity::process_identity_tests::manifest(10, [4; 32]);
        let intent = RegistrationIntentV1 {
            binding_version: 1,
            registration_version: 1,
            renewal_nonce: 0,
            transition_nonce: 0,
            requested_valid_until: 2000,
            recipient_x25519: active.recipient_x25519,
            chain_id: active.chain_id,
            genesis_hash: active.genesis_hash,
            operation: AttestationOperationV1::RegisterEnclave,
            attestation_mode: active.attestation_mode,
            policy_hash: alloy_primitives::B256::repeat_byte(5),
            node_id: active.node_id.clone(),
            enclave_id: active.enclave_id().unwrap(),
            binding_id: alloy_primitives::B256::repeat_byte(6),
            attestation_ed25519: active.attestation_ed25519,
            noise_responder_x25519: active.noise_responder_x25519,
            node_host_authorization_hash: active.node_host_authorization_hash().unwrap(),
        };
        let node = k256::ecdsa::SigningKey::from_slice(&[3; 32]).unwrap();
        let hash = intent.intent_hash().unwrap();
        let node_signature =
            outbe_primitives::tee_test_utils::sign_node_host_hash_for_test(&node, hash);
        let enclave_signature = ed25519_dalek::SigningKey::from_bytes(&[9; 32])
            .sign(hash.as_slice())
            .to_bytes();
        let evidence = AttestationEvidenceV1::NoAttestationDev(NoAttestationDevEvidenceV1 {
            dev_attestation_public: active.attestation_ed25519,
            intent,
            dev_signature: enclave_signature,
            transition_key_ready_proof: None,
        });
        let submission = super::super::CommittedJoinSubmissionV1::new(
            alloy_primitives::Address::repeat_byte(0x42),
            evidence.encode_canonical().unwrap(),
            node_signature,
            enclave_signature,
        );
        let before = submission.encode_canonical().unwrap();
        submission.intent_for_live_manifest_v1(&active).unwrap();
        assert!(submission.intent_for_live_manifest_v1(&fresh).is_err());
        assert_eq!(submission.encode_canonical().unwrap(), before);
    }
}
