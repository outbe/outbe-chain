use super::*;
use alloy_primitives::U256;
use outbe_primitives::tee_attestation_v1::{
    AttestationEvidenceV1, AttestationMode, AttestationOperationV1, DcapCollateralComponentV1,
    DcapCollateralKind, DcapEvidenceV1, EnclaveInitializationManifestV1, NodeIdV1,
    RegistrationIntentV1,
};
use outbe_primitives::tee_test_utils::sign_node_host_hash_for_test;
use outbe_tee::dcap_protocol::{
    dcap_onboarding_request_hash, dcap_verification_request_hash, DcapPckCaV1,
    DcapPlatformTcbStatusV1, DcapVerdictV1,
};

fn signed_onboarding_fixture() -> (
    CompleteDcapVerificationV1,
    EnclaveInitializationManifestV1,
    crate::transport::DerivedTributeOfferKey,
) {
    let keys = crate::keys::EnclaveKeys::new([0x81; 32], Some([0x81; 32])).unwrap();
    let node_key = k256::ecdsa::SigningKey::from_bytes((&[0x82; 32]).into()).unwrap();
    let node_public: [u8; 33] = node_key
        .verifying_key()
        .to_encoded_point(true)
        .as_bytes()
        .try_into()
        .unwrap();
    let manifest = EnclaveInitializationManifestV1 {
        chain_id: [0x83; 32],
        genesis_hash: B256::repeat_byte(0x84),
        attestation_mode: AttestationMode::DcapRequired,
        node_id: NodeIdV1 {
            reth_p2p_public: node_public,
        },
        initialization_challenge: [0x85; 32],
        node_host_noise_x25519: [0x86; 32],
        recipient_x25519: keys.tribute_offer_public(),
        attestation_ed25519: keys.attestation_pub(),
        noise_responder_x25519: keys.noise_public(),
    };
    let mut intent = RegistrationIntentV1 {
        policy_hash: B256::repeat_byte(0x87),
        binding_id: B256::repeat_byte(0x88),
        ..crate::initialization::test_support::registration_intent_for_manifest(&manifest)
    };
    intent.enclave_id = intent.derived_enclave_id().unwrap();
    let intent_hash = intent.intent_hash().unwrap();
    let node_signature = sign_node_host_hash_for_test(&node_key, intent_hash);
    let enclave_signature = keys.sign_attestation(intent_hash.as_slice());
    let evidence = AttestationEvidenceV1::Dcap(DcapEvidenceV1 {
        intent,
        quote: vec![0x89],
        components: (1_u8..=8)
            .map(|kind| DcapCollateralComponentV1 {
                kind: DcapCollateralKind::try_from(kind).unwrap(),
                bytes: vec![kind],
            })
            .collect(),
        transition_key_ready_proof: None,
    })
    .encode_canonical()
    .unwrap();
    let resident =
        crate::transport::DerivedTributeOfferKey::for_test([0x8a; 32], vec![0x8b; 96], 2, 3);
    let request = CompleteDcapVerificationV1 {
        request_hash: B256::repeat_byte(0x8c),
        evidence,
        policy: vec![0x8d],
        block_timestamp: 1,
        purpose: VerificationPurposeV1::RegisterOnboarding {
            node_signature,
            enclave_signature,
            expected_tribute_offer_public: resident.public(),
            key_epoch: 2,
            tribute_offer_epoch: 3,
        },
    };
    (request, manifest, resident)
}

fn accepted_outcome() -> DcapVerificationOutcomeV1 {
    let (mrenclave, mrsigner, isv_prod_id, isv_svn) = accepted_code_identity();
    DcapVerificationOutcomeV1::Accepted(DcapVerdictV1 {
        mrenclave,
        mrsigner,
        isv_prod_id,
        isv_svn,
        pck_ca: DcapPckCaV1::Processor,
        fmspc: [0x93; 6],
        pce_id: 1,
        platform_tcb_status: DcapPlatformTcbStatusV1::UpToDate,
        advisory_ids: Vec::new(),
        tcb_evaluation_data_number: 1,
        qe_tcb_evaluation_data_number: 1,
        collateral_valid_until: 10_000,
    })
}

fn accepted_code_identity() -> (B256, B256, u16, u16) {
    (B256::repeat_byte(0x91), B256::repeat_byte(0x92), 1, 1)
}

#[test]
fn gramine_direct_dev_onboarding_is_bound_to_testnet_genesis_policy() {
    use outbe_primitives::{
        chain::TESTNET_CHAIN_ID,
        tee_genesis_v1::{initial_tee_policy_v1, InitialTeeProfileV1},
    };

    let genesis_hash = B256::repeat_byte(0xA1);
    let policy = initial_tee_policy_v1(
        InitialTeeProfileV1::GramineDirectDev,
        TESTNET_CHAIN_ID,
        genesis_hash,
    )
    .unwrap();
    let policy_hash = policy.policy_hash().unwrap();
    let source_keys = crate::keys::EnclaveKeys::new([0xA2; 32], Some([0xA2; 32])).unwrap();
    let source_node_key = k256::ecdsa::SigningKey::from_bytes((&[0xA3; 32]).into()).unwrap();
    let source_node_public: [u8; 33] = source_node_key
        .verifying_key()
        .to_encoded_point(true)
        .as_bytes()
        .try_into()
        .unwrap();
    let source_manifest = EnclaveInitializationManifestV1 {
        chain_id: U256::from(TESTNET_CHAIN_ID).to_be_bytes(),
        genesis_hash,
        attestation_mode: AttestationMode::GramineDirectDev,
        node_id: NodeIdV1 {
            reth_p2p_public: source_node_public,
        },
        initialization_challenge: [0xA4; 32],
        node_host_noise_x25519: [0xA5; 32],
        recipient_x25519: source_keys.tribute_offer_public(),
        attestation_ed25519: source_keys.attestation_pub(),
        noise_responder_x25519: source_keys.noise_public(),
    };

    let target_keys = crate::keys::EnclaveKeys::new([0xA6; 32], Some([0xA6; 32])).unwrap();
    let target_node_key = k256::ecdsa::SigningKey::from_bytes((&[0xA7; 32]).into()).unwrap();
    let target_node_public: [u8; 33] = target_node_key
        .verifying_key()
        .to_encoded_point(true)
        .as_bytes()
        .try_into()
        .unwrap();
    let target_manifest = EnclaveInitializationManifestV1 {
        chain_id: U256::from(TESTNET_CHAIN_ID).to_be_bytes(),
        genesis_hash,
        attestation_mode: AttestationMode::GramineDirectDev,
        node_id: NodeIdV1 {
            reth_p2p_public: target_node_public,
        },
        initialization_challenge: [0xA8; 32],
        node_host_noise_x25519: [0xA9; 32],
        recipient_x25519: target_keys.tribute_offer_public(),
        attestation_ed25519: target_keys.attestation_pub(),
        noise_responder_x25519: target_keys.noise_public(),
    };
    let mut intent = RegistrationIntentV1 {
        chain_id: target_manifest.chain_id,
        genesis_hash,
        operation: AttestationOperationV1::RegisterEnclave,
        attestation_mode: AttestationMode::GramineDirectDev,
        policy_hash,
        node_id: target_manifest.node_id.clone(),
        enclave_id: target_manifest.enclave_id().unwrap(),
        binding_id: B256::repeat_byte(0xAA),
        binding_version: 1,
        registration_version: 0,
        renewal_nonce: 0,
        transition_nonce: 0,
        requested_valid_until: 7_200,
        recipient_x25519: target_manifest.recipient_x25519,
        attestation_ed25519: target_manifest.attestation_ed25519,
        noise_responder_x25519: target_manifest.noise_responder_x25519,
        node_host_authorization_hash: target_manifest.node_host_authorization_hash().unwrap(),
    };
    intent.enclave_id = intent.derived_enclave_id().unwrap();
    let intent_hash = intent.intent_hash().unwrap();
    let resident =
        crate::transport::DerivedTributeOfferKey::for_test([0xAB; 32], vec![0xAC; 96], 2, 3);
    let context = DcapOnboardingContextV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        intent_hash,
        node_id_hash: intent.node_id.node_id_hash().unwrap(),
        enclave_id: intent.enclave_id,
        binding_id: intent.binding_id,
        policy_hash: intent.policy_hash,
        recipient_x25519: intent.recipient_x25519,
        network_encryption_public: resident.public(),
        key_epoch: 2,
        network_encryption_epoch: 3,
    };
    let request_hash = context.context_hash();
    let canonical_context = context.encode_canonical();

    let response = complete_gramine_direct_dev_onboarding_response(
        request_hash,
        &canonical_context,
        Some(&resident),
        Some(&source_manifest),
    );
    let EnclaveResponse::GramineDirectDevOnboardingArtifactPreparedV1 {
        request_hash: actual_hash,
        onboarding_artifact,
        ..
    } = response
    else {
        panic!("testnet GramineDirectDev onboarding was rejected: {response:?}");
    };
    assert_eq!(actual_hash, request_hash);
    let artifact = DcapOnboardingArtifactV1::decode_canonical(&onboarding_artifact).unwrap();
    assert_eq!(artifact.context.chain_id, intent.chain_id);
    assert_eq!(artifact.context.genesis_hash, intent.genesis_hash);
    assert_eq!(artifact.context.policy_hash, intent.policy_hash);
    assert_eq!(artifact.context.intent_hash, intent_hash);
    assert_eq!(artifact.context.recipient_x25519, intent.recipient_x25519);

    let response_v2 = complete_gramine_direct_dev_onboarding_response_v2(
        context,
        Some(&resident),
        Some(&source_manifest),
        &source_keys,
    );
    let EnclaveResponse::GramineDirectDevOnboardingArtifactPreparedV2 {
        request_hash: v2_hash,
        onboarding_artifact: v2_artifact,
        attestation_tag,
    } = response_v2
    else {
        panic!("V2 DirectDev onboarding failed: {response_v2:?}");
    };
    assert_eq!(v2_hash, request_hash);
    let preimage = outbe_tee::tee_call::canonical_attestation_preimage(
        b"outbe/tee/gramine-direct-dev-onboarding-result/v2",
        v2_hash,
        &v2_artifact,
    )
    .unwrap();
    outbe_tee::tee_call::verify_enclave_attestation(
        &source_keys.attestation_pub(),
        &preimage,
        &attestation_tag,
    )
    .unwrap();

    let mut wrong_source_mode = source_manifest.clone();
    wrong_source_mode.attestation_mode = AttestationMode::DcapRequired;
    let rejected = complete_gramine_direct_dev_onboarding_response(
        request_hash,
        &canonical_context,
        Some(&resident),
        Some(&wrong_source_mode),
    );
    assert!(matches!(
        rejected,
        EnclaveResponse::Error { ref message }
            if message.contains("source network binding mismatch")
    ));

    let mut wrong_network_context = context;
    wrong_network_context.genesis_hash = B256::repeat_byte(0xAD);
    let rejected = complete_gramine_direct_dev_onboarding_response(
        wrong_network_context.context_hash(),
        &wrong_network_context.encode_canonical(),
        Some(&resident),
        Some(&source_manifest),
    );
    assert!(matches!(
        rejected,
        EnclaveResponse::Error { ref message }
            if message.contains("source network binding mismatch")
    ));

    let wrong_resident =
        crate::transport::DerivedTributeOfferKey::for_test([0xAE; 32], vec![0xAF; 96], 2, 3);
    let rejected = complete_gramine_direct_dev_onboarding_response(
        request_hash,
        &canonical_context,
        Some(&wrong_resident),
        Some(&source_manifest),
    );
    assert!(matches!(
        rejected,
        EnclaveResponse::Error { ref message }
            if message.contains("offer-key context mismatch")
    ));
}

#[test]
fn bounded_upload_reassembles_the_exact_committed_inputs() {
    let evidence = vec![0x11; 70_000];
    let policy = vec![0x22; 1_000];
    let timestamp = 1_700_000_000;
    let hash = dcap_verification_request_hash(&evidence, &policy, timestamp).unwrap();
    let mut session = DcapVerificationSessionV1::default();
    assert!(matches!(
        session
            .handle(EnclaveRequest::BeginDcapVerificationV1 {
                request_hash: hash,
                evidence_len: evidence.len() as u32,
                policy_len: policy.len() as u32,
                block_timestamp: timestamp,
            })
            .unwrap(),
        DcapVerificationProgressV1::Started { request_hash } if request_hash == hash
    ));
    let mut combined = evidence.clone();
    combined.extend_from_slice(&policy);
    let first = combined[..MAX_DCAP_VERIFICATION_CHUNK_BYTES].to_vec();
    assert!(matches!(
        session
            .handle(EnclaveRequest::DcapVerificationChunkV1 {
                request_hash: hash,
                offset: 0,
                bytes: first,
            })
            .unwrap(),
        DcapVerificationProgressV1::ChunkAccepted { next_offset, .. }
            if next_offset == MAX_DCAP_VERIFICATION_CHUNK_BYTES as u32
    ));
    session
        .handle(EnclaveRequest::DcapVerificationChunkV1 {
            request_hash: hash,
            offset: MAX_DCAP_VERIFICATION_CHUNK_BYTES as u32,
            bytes: combined[MAX_DCAP_VERIFICATION_CHUNK_BYTES..].to_vec(),
        })
        .unwrap();
    let DcapVerificationProgressV1::Complete(complete) = session
        .handle(EnclaveRequest::FinishDcapVerificationV1 { request_hash: hash })
        .unwrap()
    else {
        panic!("finish did not return the complete verifier request");
    };
    assert_eq!(complete.request_hash, hash);
    assert_eq!(complete.evidence, evidence);
    assert_eq!(complete.policy, policy);
    assert_eq!(complete.block_timestamp, timestamp);
}

#[test]
fn upload_caps_reject_before_accepting_any_chunk() {
    let mut session = DcapVerificationSessionV1::default();
    let hash = B256::repeat_byte(0x41);
    assert!(session
        .handle(EnclaveRequest::BeginDcapVerificationV1 {
            request_hash: hash,
            evidence_len: (MAX_ATTESTATION_EVIDENCE_BYTES + 1) as u32,
            policy_len: 1,
            block_timestamp: 1,
        })
        .is_err());
    assert!(session
        .handle(EnclaveRequest::BeginDcapVerificationV1 {
            request_hash: hash,
            evidence_len: 1,
            policy_len: (MAX_TEE_POLICY_BYTES + 1) as u32,
            block_timestamp: 1,
        })
        .is_err());
    session
        .handle(EnclaveRequest::BeginDcapVerificationV1 {
            request_hash: hash,
            evidence_len: 1,
            policy_len: 1,
            block_timestamp: 1,
        })
        .unwrap();
    assert!(session
        .handle(EnclaveRequest::DcapVerificationChunkV1 {
            request_hash: hash,
            offset: 0,
            bytes: vec![0; MAX_DCAP_VERIFICATION_CHUNK_BYTES + 1],
        })
        .is_err());
    assert!(session
        .handle(EnclaveRequest::DcapVerificationChunkV1 {
            request_hash: hash,
            offset: 0,
            bytes: vec![0],
        })
        .is_err());
}

#[test]
fn upload_order_hash_or_truncation_fault_clears_the_session() {
    let evidence = [0x51, 0x52];
    let policy = [0x61, 0x62];
    let timestamp = 7;
    let hash = dcap_verification_request_hash(&evidence, &policy, timestamp).unwrap();
    let begin = || EnclaveRequest::BeginDcapVerificationV1 {
        request_hash: hash,
        evidence_len: evidence.len() as u32,
        policy_len: policy.len() as u32,
        block_timestamp: timestamp,
    };
    let mut session = DcapVerificationSessionV1::default();

    session.handle(begin()).unwrap();
    assert!(session.handle(begin()).is_err());
    assert!(session
        .handle(EnclaveRequest::FinishDcapVerificationV1 { request_hash: hash })
        .is_err());

    session.handle(begin()).unwrap();
    assert!(session
        .handle(EnclaveRequest::DcapVerificationChunkV1 {
            request_hash: hash,
            offset: 1,
            bytes: vec![0x51],
        })
        .is_err());
    assert!(session
        .handle(EnclaveRequest::FinishDcapVerificationV1 { request_hash: hash })
        .is_err());

    session.handle(begin()).unwrap();
    session
        .handle(EnclaveRequest::DcapVerificationChunkV1 {
            request_hash: hash,
            offset: 0,
            bytes: evidence.to_vec(),
        })
        .unwrap();
    assert!(session
        .handle(EnclaveRequest::FinishDcapVerificationV1 { request_hash: hash })
        .is_err());

    session.handle(begin()).unwrap();
    let mut combined = evidence.to_vec();
    combined.extend_from_slice(&policy);
    session
        .handle(EnclaveRequest::DcapVerificationChunkV1 {
            request_hash: hash,
            offset: 0,
            bytes: combined,
        })
        .unwrap();
    assert!(session
        .handle(EnclaveRequest::FinishDcapVerificationV1 {
            request_hash: B256::repeat_byte(0x99),
        })
        .is_err());
}

#[test]
fn onboarding_upload_commits_exact_authorization_and_epochs() {
    let evidence = [0x71, 0x72];
    let policy = [0x73];
    let node_signature = [0x74; 65];
    let enclave_signature = [0x75; 64];
    let offer_public = [0x76; 32];
    let request_hash = dcap_onboarding_request_hash(RegistrationVerificationRequest {
        evidence: &evidence,
        policy: &policy,
        block_timestamp: 77,
        node_signature: &node_signature,
        enclave_signature: &enclave_signature,
        expected_network_encryption_public: offer_public,
        key_epoch: 2,
        network_encryption_epoch: 3,
    })
    .unwrap();
    let mut session = DcapVerificationSessionV1::default();
    session
        .handle(EnclaveRequest::BeginDcapOnboardingVerificationV1 {
            request_hash,
            evidence_len: evidence.len() as u32,
            policy_len: policy.len() as u32,
            block_timestamp: 77,
            node_signature: node_signature.to_vec(),
            enclave_signature: enclave_signature.to_vec(),
            expected_network_encryption_public: offer_public,
            key_epoch: 2,
            network_encryption_epoch: 3,
        })
        .unwrap();
    let mut combined = evidence.to_vec();
    combined.extend_from_slice(&policy);
    session
        .handle(EnclaveRequest::DcapVerificationChunkV1 {
            request_hash,
            offset: 0,
            bytes: combined,
        })
        .unwrap();
    let DcapVerificationProgressV1::Complete(complete) = session
        .handle(EnclaveRequest::FinishDcapVerificationV1 { request_hash })
        .unwrap()
    else {
        panic!("onboarding upload did not complete");
    };
    assert_eq!(
        complete.purpose,
        VerificationPurposeV1::RegisterOnboarding {
            node_signature,
            enclave_signature,
            expected_tribute_offer_public: offer_public,
            key_epoch: 2,
            tribute_offer_epoch: 3,
        }
    );
}

#[test]
fn accepted_onboarding_requires_both_exact_signatures_before_artifact_creation() {
    let (request, manifest, resident) = signed_onboarding_fixture();
    let mut outcome = accepted_outcome();
    let artifact = build_onboarding_artifact(
        &request,
        &mut outcome,
        Some(&resident),
        Some(&manifest),
        accepted_code_identity(),
    )
    .unwrap();
    assert!(artifact.is_some());

    let mut bad_node = request;
    let VerificationPurposeV1::RegisterOnboarding { node_signature, .. } = &mut bad_node.purpose
    else {
        unreachable!()
    };
    node_signature[0] ^= 1;
    let mut outcome = accepted_outcome();
    assert!(build_onboarding_artifact(
        &bad_node,
        &mut outcome,
        Some(&resident),
        Some(&manifest),
        accepted_code_identity(),
    )
    .unwrap()
    .is_none());
    assert_eq!(
        outcome,
        DcapVerificationOutcomeV1::Rejected(DcapRejectCodeV1::NodeSignatureInvalid)
    );

    let (mut bad_enclave, _, _) = signed_onboarding_fixture();
    let VerificationPurposeV1::RegisterOnboarding {
        enclave_signature, ..
    } = &mut bad_enclave.purpose
    else {
        unreachable!()
    };
    enclave_signature[0] ^= 1;
    let mut outcome = accepted_outcome();
    assert!(build_onboarding_artifact(
        &bad_enclave,
        &mut outcome,
        Some(&resident),
        Some(&manifest),
        accepted_code_identity(),
    )
    .unwrap()
    .is_none());
    assert_eq!(
        outcome,
        DcapVerificationOutcomeV1::Rejected(DcapRejectCodeV1::EnclaveSignatureInvalid)
    );
}

#[test]
fn rejected_qvl_outcome_never_creates_an_onboarding_artifact() {
    let (request, manifest, resident) = signed_onboarding_fixture();
    let mut outcome = DcapVerificationOutcomeV1::Rejected(DcapRejectCodeV1::QuoteMalformed);
    assert!(build_onboarding_artifact(
        &request,
        &mut outcome,
        Some(&resident),
        Some(&manifest),
        accepted_code_identity(),
    )
    .unwrap()
    .is_none());
    assert_eq!(
        outcome,
        DcapVerificationOutcomeV1::Rejected(DcapRejectCodeV1::QuoteMalformed)
    );
}

#[test]
fn onboarding_rejects_host_policy_that_admits_another_enclave_binary() {
    let (request, manifest, resident) = signed_onboarding_fixture();
    let mut outcome = accepted_outcome();
    let source_identity = (B256::repeat_byte(0xa1), B256::repeat_byte(0xa2), 1, 2);
    assert!(build_onboarding_artifact(
        &request,
        &mut outcome,
        Some(&resident),
        Some(&manifest),
        source_identity,
    )
    .unwrap()
    .is_none());
    assert_eq!(
        outcome,
        DcapVerificationOutcomeV1::Rejected(DcapRejectCodeV1::MeasurementRejected)
    );
}
