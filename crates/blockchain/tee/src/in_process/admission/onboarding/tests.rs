use super::*;
use crate::dcap_protocol::*;
use crate::in_process::{test_support, HardwareProofSourceV1, ProofSourceV1};
use outbe_primitives::{tee_attestation_v1::*, tee_test_utils::sign_node_host_hash_for_test};
use std::sync::Arc;

struct Hardware;
impl HardwareProofSourceV1 for Hardware {
    fn code_identity(&self) -> Result<(B256, B256, u16, u16)> {
        Ok((B256::repeat_byte(0x91), B256::repeat_byte(0x92), 1, 1))
    }
    fn quote(&self, _: [u8; 64]) -> Result<Vec<u8>> {
        Err(InProcessError::HardwareProofUnavailable)
    }
}
fn accepted() -> DcapVerificationOutcomeV1 {
    DcapVerificationOutcomeV1::Accepted(DcapVerdictV1 {
        mrenclave: B256::repeat_byte(0x91),
        mrsigner: B256::repeat_byte(0x92),
        isv_prod_id: 1,
        isv_svn: 1,
        pck_ca: DcapPckCaV1::Processor,
        fmspc: [0x93; 6],
        pce_id: 1,
        platform_tcb_status: DcapPlatformTcbStatusV1::UpToDate,
        advisory_ids: Vec::new(),
        tcb_evaluation_data_number: 1,
        qe_tcb_evaluation_data_number: 1,
        collateral_valid_until: 10000,
    })
}
#[test]
fn in_process_dcap_outcomes_and_artifact_are_canonically_signed() {
    use ed25519_dalek::Verifier as _;
    let context = test_support::context(
        AttestationMode::DcapRequired,
        ProofSourceV1::Hardware(Arc::new(Hardware)),
    );
    context
        .activate(test_support::material(&context, 5), None)
        .unwrap();
    let intent = test_support::registration(&context);
    let hash = intent.intent_hash().unwrap();
    let node_signature = sign_node_host_hash_for_test(&test_support::node_key(), hash);
    let enclave_signature = context.sign_domain_attestation(hash.as_slice());
    let onboarding = DcapOnboardingContextV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        intent_hash: hash,
        node_id_hash: intent.node_id.node_id_hash().unwrap(),
        enclave_id: intent.enclave_id,
        binding_id: intent.binding_id,
        policy_hash: intent.policy_hash,
        recipient_x25519: intent.recipient_x25519,
        network_encryption_public: context.network_encryption_public().unwrap(),
        key_epoch: 0,
        network_encryption_epoch: 0,
    };
    let evidence = AttestationEvidenceV1::Dcap(DcapEvidenceV1 {
        intent,
        quote: vec![0x89],
        components: (1..=8)
            .map(|kind| DcapCollateralComponentV1 {
                kind: DcapCollateralKind::try_from(kind).unwrap(),
                bytes: vec![kind],
            })
            .collect(),
        transition_key_ready_proof: None,
    })
    .encode_canonical()
    .unwrap();
    for (rejection, source_epoch_mismatch) in [
        (None, false),
        (Some(DcapRejectCodeV1::MeasurementRejected), false),
        (Some(DcapRejectCodeV1::OnboardingContextMismatch), false),
        (Some(DcapRejectCodeV1::OnboardingContextMismatch), true),
    ] {
        let mut outcome = accepted();
        let mut recipient_context = onboarding;
        match rejection {
            Some(DcapRejectCodeV1::MeasurementRejected) => {
                let DcapVerificationOutcomeV1::Accepted(verdict) = &mut outcome else {
                    unreachable!()
                };
                verdict.mrenclave = B256::repeat_byte(0x99);
            }
            Some(DcapRejectCodeV1::OnboardingContextMismatch) => {
                if source_epoch_mismatch {
                    recipient_context.key_epoch += 1;
                } else {
                    recipient_context.intent_hash = B256::repeat_byte(0x99);
                }
            }
            _ => {}
        }
        // The seam substitutes only the hardware verifier verdict; production admission,
        // artifact construction, canonical encodings and signing are the same functions.
        let result = context
            .finish_dcap_onboarding(
                DcapRegistrationSealV1 {
                    evidence: &evidence,
                    policy: &[1],
                    block_timestamp: 1000,
                    node_signature: &node_signature,
                    enclave_signature: &enclave_signature,
                    context: recipient_context,
                },
                outcome,
            )
            .unwrap();
        let decoded = DcapVerificationOutcomeV1::decode_canonical(&result.outcome).unwrap();
        match rejection {
            Some(code) => {
                assert_eq!(decoded, DcapVerificationOutcomeV1::Rejected(code));
                assert!(result.onboarding_artifact.is_empty());
            }
            None => {
                assert!(matches!(decoded, DcapVerificationOutcomeV1::Accepted(_)));
                assert!(!result.onboarding_artifact.is_empty());
            }
        }
        let preimage = dcap_onboarding_attestation_preimage(
            result.request_hash,
            &result.outcome,
            &result.onboarding_artifact,
        )
        .unwrap();
        ed25519_dalek::VerifyingKey::from_bytes(&context.attestation_public())
            .unwrap()
            .verify(
                &preimage,
                &ed25519_dalek::Signature::from_bytes(&result.attestation_tag),
            )
            .unwrap();
        if let Some(code) = rejection {
            assert_eq!(
                code as u16,
                if code == DcapRejectCodeV1::MeasurementRejected {
                    0x0601
                } else {
                    0x0704
                }
            );
        }
    }
}
