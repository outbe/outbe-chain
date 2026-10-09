use std::collections::VecDeque;

use alloy_primitives::B256;
use outbe_tee::{
    dcap_protocol::DcapOnboardingContextV1,
    errors::TransportError,
    finalized_admission::{
        upgrade_key_transfer_request_hash_v1, FinalizedAdmissionRecordKindV1,
        MAX_COMMITTEE_TRANSITION_RECORD_BYTES, MAX_FINALIZED_ADMISSION_RECORD_BYTES,
        MAX_ONBOARDING_INGEST_CHUNK_BYTES,
    },
    protocol::{EnclaveRequest, EnclaveResponse},
    upgrade_transfer::{export_placeholder, transfer, UpgradeKeyProofV1, MAX_UPGRADE_COMMITTEES},
};

fn context() -> DcapOnboardingContextV1 {
    outbe_tee::test_utils::onboarding_context_fixture(
        [0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19],
        20,
        21,
    )
}

fn proof() -> UpgradeKeyProofV1 {
    UpgradeKeyProofV1 {
        anchor_outcome: vec![0xa1, 0xa2].into(),
        committee_transitions: vec![vec![0xb1, 0xb2].into()],
        admission: vec![0xc1, 0xc2].into(),
    }
}

fn error_text(result: Result<EnclaveResponse, TransportError>) -> String {
    result.unwrap_err().to_string()
}

fn reply(request: &EnclaveRequest, hash: B256, export: bool) -> EnclaveResponse {
    match request {
        EnclaveRequest::BeginUpgradeKeyTransferV1 { .. } => {
            EnclaveResponse::DcapOnboardingArtifactIngestStartedV1 { request_hash: hash }
        }
        EnclaveRequest::DcapOnboardingArtifactChunkV1 { offset, bytes, .. } => {
            EnclaveResponse::DcapOnboardingArtifactChunkAcceptedV1 {
                request_hash: hash,
                next_offset: *offset + u32::try_from(bytes.len()).unwrap(),
            }
        }
        EnclaveRequest::CommitDcapOnboardingArtifactRecordV1 { kind, .. } => {
            EnclaveResponse::DcapOnboardingArtifactRecordAcceptedV1 {
                request_hash: hash,
                kind: *kind,
            }
        }
        EnclaveRequest::FinishDcapOnboardingArtifactIngestV1 { .. } if export => {
            EnclaveResponse::UpgradeKeyExportedV1 {
                request_hash: hash,
                artifact: vec![0xd1, 0xd2],
            }
        }
        EnclaveRequest::FinishDcapOnboardingArtifactIngestV1 { .. } => {
            EnclaveResponse::FinalizedAdmissionIngestedV1 {
                request_hash: hash,
                tribute_offer_public: [0xe1; 32],
            }
        }
        _ => panic!("unexpected request in upgrade transfer"),
    }
}

#[test]
fn proof_validation_preserves_dimension_record_and_aggregate_error_order() {
    let mut p = proof();
    p.anchor_outcome = Vec::<u8>::new().into();
    p.committee_transitions = vec![Vec::<u8>::new().into()];
    assert_eq!(
        p.validate().unwrap_err().to_string(),
        "enclave returned error: upgrade proof dimensions exceed limits"
    );
    p = proof();
    p.committee_transitions = vec![Vec::<u8>::new().into()];
    assert_eq!(
        p.validate().unwrap_err().to_string(),
        "enclave returned error: invalid upgrade committee record size"
    );
    p = proof();
    p.committee_transitions = vec![Vec::<u8>::new().into(); MAX_UPGRADE_COMMITTEES + 1];
    assert_eq!(
        p.validate().unwrap_err().to_string(),
        "enclave returned error: upgrade proof dimensions exceed limits"
    );
    p = proof();
    p.committee_transitions = vec![vec![0xb1; MAX_COMMITTEE_TRANSITION_RECORD_BYTES + 1].into()];
    assert_eq!(
        p.validate().unwrap_err().to_string(),
        "enclave returned error: invalid upgrade committee record size"
    );
    p = proof();
    p.committee_transitions = vec![vec![0xb1; MAX_COMMITTEE_TRANSITION_RECORD_BYTES].into()];
    p.admission = vec![0xc1; MAX_FINALIZED_ADMISSION_RECORD_BYTES].into();
    assert_eq!(
        p.validate().unwrap_err().to_string(),
        "enclave returned error: upgrade proof exceeds aggregate limit"
    );
}

#[test]
fn invalid_proof_and_artifact_stop_before_the_first_request() {
    let mut p = proof();
    p.admission = Vec::<u8>::new().into();
    let mut calls = 0;
    assert_eq!(
        error_text(transfer(
            |_| {
                calls += 1;
                Err(TransportError::UnexpectedResponse)
            },
            &p,
            b"malformed",
            true,
        )),
        "enclave returned error: upgrade proof dimensions exceed limits"
    );
    assert_eq!(calls, 0);
    assert_eq!(
        error_text(transfer(
            |_| {
                calls += 1;
                Err(TransportError::UnexpectedResponse)
            },
            &proof(),
            b"malformed",
            true,
        )),
        "enclave returned error: finalized admission proof is malformed: upgrade artifact"
    );
    assert_eq!(calls, 0);
}

fn expected_transfer_requests(
    proof: &UpgradeKeyProofV1,
    artifact: &[u8],
    hash: B256,
    export: bool,
) -> VecDeque<EnclaveRequest> {
    VecDeque::from(vec![
        EnclaveRequest::BeginUpgradeKeyTransferV1 {
            request_hash: hash,
            artifact: artifact.to_vec(),
            anchor_outcome: proof.anchor_outcome.to_vec(),
            export,
        },
        EnclaveRequest::DcapOnboardingArtifactChunkV1 {
            request_hash: hash,
            kind: FinalizedAdmissionRecordKindV1::CommitteeTransition,
            offset: 0,
            bytes: vec![0xb1; MAX_ONBOARDING_INGEST_CHUNK_BYTES],
        },
        EnclaveRequest::DcapOnboardingArtifactChunkV1 {
            request_hash: hash,
            kind: FinalizedAdmissionRecordKindV1::CommitteeTransition,
            offset: MAX_ONBOARDING_INGEST_CHUNK_BYTES as u32,
            bytes: vec![0xb1],
        },
        EnclaveRequest::CommitDcapOnboardingArtifactRecordV1 {
            request_hash: hash,
            kind: FinalizedAdmissionRecordKindV1::CommitteeTransition,
        },
        EnclaveRequest::DcapOnboardingArtifactChunkV1 {
            request_hash: hash,
            kind: FinalizedAdmissionRecordKindV1::CommitteeTransition,
            offset: 0,
            bytes: vec![0xb2, 0xb3],
        },
        EnclaveRequest::CommitDcapOnboardingArtifactRecordV1 {
            request_hash: hash,
            kind: FinalizedAdmissionRecordKindV1::CommitteeTransition,
        },
        EnclaveRequest::DcapOnboardingArtifactChunkV1 {
            request_hash: hash,
            kind: FinalizedAdmissionRecordKindV1::Admission,
            offset: 0,
            bytes: vec![0xc1, 0xc2],
        },
        EnclaveRequest::CommitDcapOnboardingArtifactRecordV1 {
            request_hash: hash,
            kind: FinalizedAdmissionRecordKindV1::Admission,
        },
        EnclaveRequest::FinishDcapOnboardingArtifactIngestV1 { request_hash: hash },
    ])
}

#[test]
fn transfer_preserves_exact_record_order_offsets_bytes_and_terminal_mode() {
    let mut p = proof();
    p.committee_transitions = vec![
        vec![0xb1; MAX_ONBOARDING_INGEST_CHUNK_BYTES + 1].into(),
        vec![0xb2, 0xb3].into(),
    ];
    let artifact = export_placeholder(context()).unwrap();
    for export in [true, false] {
        let hash =
            upgrade_key_transfer_request_hash_v1(&artifact, &p.anchor_outcome, export).unwrap();
        let mut expected = expected_transfer_requests(&p, &artifact, hash, export);
        let response = transfer(
            |request| {
                assert_eq!(Some(request), expected.front());
                expected.pop_front();
                Ok(reply(request, hash, export))
            },
            &p,
            &artifact,
            export,
        )
        .unwrap();
        assert!(expected.is_empty());
        match response {
            EnclaveResponse::UpgradeKeyExportedV1 {
                request_hash,
                artifact,
            } if export => {
                assert_eq!(request_hash, hash);
                assert_eq!(artifact, vec![0xd1, 0xd2]);
            }
            EnclaveResponse::FinalizedAdmissionIngestedV1 {
                request_hash,
                tribute_offer_public,
            } if !export => {
                assert_eq!(request_hash, hash);
                assert_eq!(tribute_offer_public, [0xe1; 32]);
            }
            _ => panic!("wrong terminal response"),
        }
    }
}

#[test]
fn transfer_stops_at_the_first_bad_acknowledgement() {
    let p = proof();
    let artifact = export_placeholder(context()).unwrap();
    let hash = upgrade_key_transfer_request_hash_v1(&artifact, &p.anchor_outcome, true).unwrap();
    for fault_at in 0..6 {
        let mut calls = 0;
        let result = transfer(
            |request| {
                let index = calls;
                calls += 1;
                if index == fault_at {
                    Ok(EnclaveResponse::DcapOnboardingArtifactIngestStartedV1 {
                        request_hash: B256::ZERO,
                    })
                } else {
                    Ok(reply(request, hash, true))
                }
            },
            &p,
            &artifact,
            true,
        );
        assert_eq!(error_text(result), "unexpected response from enclave");
        assert_eq!(calls, fault_at + 1);
    }
}

#[test]
fn transfer_rejects_wrong_chunk_offset_commit_kind_and_terminal_mode() {
    let p = proof();
    let artifact = export_placeholder(context()).unwrap();
    let hash = upgrade_key_transfer_request_hash_v1(&artifact, &p.anchor_outcome, true).unwrap();
    for fault_at in [1, 2, 5] {
        let mut calls = 0;
        let result = transfer(
            |request| {
                let index = calls;
                calls += 1;
                if index == fault_at {
                    Ok(match fault_at {
                        1 => EnclaveResponse::DcapOnboardingArtifactChunkAcceptedV1 {
                            request_hash: hash,
                            next_offset: 0,
                        },
                        2 => EnclaveResponse::DcapOnboardingArtifactRecordAcceptedV1 {
                            request_hash: hash,
                            kind: FinalizedAdmissionRecordKindV1::Admission,
                        },
                        5 => EnclaveResponse::FinalizedAdmissionIngestedV1 {
                            request_hash: hash,
                            tribute_offer_public: [0xe1; 32],
                        },
                        _ => unreachable!(),
                    })
                } else {
                    Ok(reply(request, hash, true))
                }
            },
            &p,
            &artifact,
            true,
        );
        assert_eq!(error_text(result), "unexpected response from enclave");
        assert_eq!(calls, fault_at + 1);
    }
}
