//! Direct runtime adapters preserving the existing witness response envelopes.
use super::admission::DcapRegistrationSealV1;
use super::{InProcessError, InProcessTeeContext, Result};
use crate::{dcap_protocol::*, protocol::EnclaveResponse};
use outbe_primitives::tee_attestation_v1::{AttestationEvidenceV1, AttestationMode};

impl InProcessTeeContext {
    /// Verify evidence with the native verifier and sign the frozen generic outcome envelope.
    pub(crate) fn verify_dcap_evidence_response_v1(
        &self,
        evidence: &[u8],
        policy: &[u8],
        block_timestamp: u64,
    ) -> Result<EnclaveResponse> {
        self.proof_source
            .ensure_available(AttestationMode::DcapRequired)?;
        let request_hash = dcap_verification_request_hash(evidence, policy, block_timestamp)
            .map_err(InProcessError::Dcap)?;
        #[cfg(not(feature = "native-dcap"))]
        let _ = request_hash;
        #[cfg(not(feature = "native-dcap"))]
        {
            Err(InProcessError::HardwareProofUnavailable)
        }
        #[cfg(feature = "native-dcap")]
        {
            let outcome =
                crate::verifier::verify_complete_request(evidence, policy, block_timestamp)
                    .encode_canonical()
                    .map_err(InProcessError::Dcap)?;
            let preimage = dcap_verification_attestation_preimage(request_hash, &outcome)
                .map_err(InProcessError::Dcap)?;
            Ok(EnclaveResponse::DcapVerificationFinishedV1 {
                request_hash,
                outcome,
                attestation_tag: self.sign_domain_attestation(&preimage).to_vec(),
            })
        }
    }
    /// Adapt the existing public verifier inputs without changing its signed refusal semantics.
    pub(crate) fn verify_dcap_registration_response_v1(
        &self,
        request: RegistrationVerificationRequest<'_>,
    ) -> Result<EnclaveResponse> {
        self.proof_source
            .ensure_available(AttestationMode::DcapRequired)?;
        if !cfg!(feature = "native-dcap") {
            return Err(InProcessError::HardwareProofUnavailable);
        }
        let request_hash = dcap_onboarding_request_hash(request).map_err(InProcessError::Dcap)?;
        let evidence = match AttestationEvidenceV1::decode_canonical(request.evidence) {
            Ok(AttestationEvidenceV1::Dcap(evidence)) => evidence,
            _ => {
                let outcome =
                    DcapVerificationOutcomeV1::Rejected(DcapRejectCodeV1::EvidenceNonCanonical)
                        .encode_canonical()
                        .map_err(InProcessError::Dcap)?;
                let onboarding_artifact = Vec::new();
                let preimage = dcap_onboarding_attestation_preimage(
                    request_hash,
                    &outcome,
                    &onboarding_artifact,
                )
                .map_err(InProcessError::Dcap)?;
                return Ok(EnclaveResponse::DcapOnboardingVerificationFinishedV1 {
                    request_hash,
                    outcome,
                    onboarding_artifact,
                    attestation_tag: self.sign_domain_attestation(&preimage).to_vec(),
                });
            }
        };
        let intent = evidence.intent;
        let context = DcapOnboardingContextV1 {
            chain_id: intent.chain_id,
            genesis_hash: intent.genesis_hash,
            intent_hash: intent.intent_hash()?,
            node_id_hash: intent.node_id.node_id_hash()?,
            enclave_id: intent.enclave_id,
            binding_id: intent.binding_id,
            policy_hash: intent.policy_hash,
            recipient_x25519: intent.recipient_x25519,
            network_encryption_public: request.expected_network_encryption_public,
            key_epoch: request.key_epoch,
            network_encryption_epoch: request.network_encryption_epoch,
        };
        let response = self.verify_dcap_registration_and_seal_v1(DcapRegistrationSealV1 {
            evidence: request.evidence,
            policy: request.policy,
            block_timestamp: request.block_timestamp,
            node_signature: request.node_signature,
            enclave_signature: request.enclave_signature,
            context,
        })?;
        Ok(EnclaveResponse::DcapOnboardingVerificationFinishedV1 {
            request_hash: response.request_hash,
            outcome: response.outcome,
            onboarding_artifact: response.onboarding_artifact,
            attestation_tag: response.attestation_tag.to_vec(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        client_global::set_in_process_tee_context_for_test,
        protocol::{EnclaveRequest, PayloadLane},
        CallMeta,
    };
    use metrics_util::debugging::{DebugValue, DebuggingRecorder};
    use std::sync::Arc;

    #[test]
    fn direct_execution_balances_metrics_without_charging_other_lanes() {
        let context = super::super::test_support::context(
            AttestationMode::NoAttestationDev,
            super::super::ProofSourceV1::NoHardware,
        );
        let _runtime = set_in_process_tee_context_for_test(Some((Arc::new(context), |_, _, _| {
            Err(crate::InProcessDispatchError::UnsupportedRequest.into())
        })));
        let recorder = DebuggingRecorder::new();
        let snapshots = recorder.snapshotter();
        metrics::with_local_recorder(&recorder, || {
            let request = EnclaveRequest::Payload {
                lane: PayloadLane::Canary,
                payload: Vec::new(),
            };
            assert!(
                crate::canary_request_with_meta(CallMeta::session_only("probe"), &request).is_err()
            );
            assert!(!snapshots
                .snapshot()
                .into_vec()
                .iter()
                .any(|(key, _, _, _)| key.key().name().contains("execution_")));
            let request = EnclaveRequest::Payload {
                lane: PayloadLane::Execution,
                payload: Vec::new(),
            };
            assert!(
                crate::try_in_process_request(CallMeta::recorded("transition"), &request)
                    .unwrap()
                    .is_err()
            );
        });
        let values = snapshots.snapshot().into_vec();
        let metric = |name: &str| {
            &values
                .iter()
                .find(|(key, _, _, _)| key.key().name() == name)
                .unwrap()
                .3
        };
        assert_eq!(
            metric("outbe_tee_client_execution_in_flight_requests"),
            &DebugValue::Gauge(0.0.into())
        );
        assert_eq!(
            metric("outbe_tee_client_execution_lock_wait_ms"),
            &DebugValue::Histogram(vec![0.0.into()])
        );
    }
    #[test]
    fn direct_verifier_fails_closed_without_hardware_and_donor_checks_finality() {
        let context = Arc::new(super::super::test_support::context(
            AttestationMode::NoAttestationDev,
            super::super::ProofSourceV1::NoHardware,
        ));
        context
            .activate_for_test(super::super::test_support::material(&context, 5))
            .unwrap();
        let _runtime =
            set_in_process_tee_context_for_test(Some((Arc::clone(&context), |_, _, _| {
                Err(crate::InProcessDispatchError::UnsupportedRequest.into())
            })));
        let error = crate::verify_dcap_evidence_v1(&[1], &[2], 100).unwrap_err();
        assert!(matches!(
            error,
            crate::TransportError::InProcess(crate::InProcessDispatchError::Resident(
                InProcessError::HardwareProofUnavailable
            ))
        ));
        let mut donor =
            crate::test_utils::onboarding_context_fixture([1, 2, 3, 4, 5, 6, 7, 8, 9], 0, 0);
        donor.chain_id = context.manifest().chain_id;
        donor.genesis_hash = context.manifest().genesis_hash;
        donor.network_encryption_public = context.network_encryption_public().unwrap();
        let artifact = crate::prepare_no_attestation_dev_onboarding_artifact_v1(donor).unwrap();
        assert_eq!(artifact.context, donor);
        let proof = crate::upgrade_transfer::UpgradeKeyProofV1 {
            anchor_outcome: vec![1].into(),
            committee_transitions: Vec::new(),
            admission: vec![2].into(),
        };
        let before = context.network_encryption_public().unwrap();
        let error =
            crate::upgrade_transfer::export_from_network_source(donor, &proof, false).unwrap_err();
        assert!(matches!(error, crate::TransportError::InProcess(_)));
        assert_eq!(context.network_encryption_public().unwrap(), before);
        let encoded =
            crate::upgrade_transfer::export_from_network_source(donor, &proof, true).unwrap();
        assert_eq!(
            DcapOnboardingArtifactV1::decode_canonical(&encoded).unwrap(),
            artifact
        );
    }
}
