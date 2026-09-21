use crate::transport::tests::*;

#[test]
fn generated_quote_binding_rejects_post_syscall_report_data_substitution() {
    let expected = [0xA1; 64];
    let mut quote = vec![0u8; outbe_tee::quote::MIN_QUOTE_LEN];
    let report_data_offset = outbe_tee::quote::MIN_QUOTE_LEN - 64;
    quote[report_data_offset..].copy_from_slice(&expected);
    assert_eq!(
        validate_generated_quote_binding(expected, quote.clone()).unwrap(),
        quote
    );

    let mut substituted = quote;
    substituted[report_data_offset] ^= 1;
    assert!(validate_generated_quote_binding(expected, substituted)
        .unwrap_err()
        .contains("does not match requested intent"));
}

#[test]
fn transition_quote_requires_resident_offer_key_and_signs_candidate_manifest() {
    use outbe_primitives::tee_attestation_v1::{
        AttestationMode, AttestationOperationV1, RegistrationIntentV1, TransitionKeyReadyProofV1,
    };

    fn quote(report_data: &[u8; 64]) -> Result<Vec<u8>, String> {
        let mut quote = vec![0_u8; outbe_tee::quote::MIN_QUOTE_LEN];
        let offset = quote.len() - report_data.len();
        quote[offset..].copy_from_slice(report_data);
        Ok(quote)
    }

    let root = tempfile::tempdir().unwrap();
    let boot = EnclaveBootConfig::new(testnet_chain_word(), root.path().to_path_buf(), 0);
    let keys = EnclaveKeys::new([0x31; 32], Some([0x31; 32])).unwrap();
    let initialization = production_dcap_state(Arc::new(boot.clone()), &keys);
    let challenge = match initialization.challenge_response(&keys).unwrap() {
        EnclaveResponse::InitializationChallenge { challenge, .. } => challenge,
        response => panic!("unexpected challenge response: {response:?}"),
    };
    let (manifest, node_signature) = signed_initialization_manifest(&keys, challenge, [0x32; 32]);
    let pending = initialization
        .prepare(
            &manifest.encode_canonical().unwrap(),
            &node_signature,
            &keys,
        )
        .unwrap();
    initialization.commit(pending, &keys).unwrap();
    let intent = RegistrationIntentV1 {
        chain_id: manifest.chain_id,
        genesis_hash: manifest.genesis_hash,
        operation: AttestationOperationV1::TransitionEnclaveMeasurement,
        attestation_mode: AttestationMode::DcapRequired,
        policy_hash: B256::repeat_byte(0x33),
        node_id: manifest.node_id.clone(),
        enclave_id: manifest.enclave_id().unwrap(),
        binding_id: B256::repeat_byte(0x34),
        binding_version: 2,
        registration_version: 1,
        renewal_nonce: 0,
        transition_nonce: 5,
        requested_valid_until: 7_200,
        recipient_x25519: manifest.recipient_x25519,
        attestation_ed25519: manifest.attestation_ed25519,
        noise_responder_x25519: manifest.noise_responder_x25519,
        node_host_authorization_hash: manifest.node_host_authorization_hash().unwrap(),
    };
    let request = EnclaveRequest::GenerateDcapQuote {
        intent: intent.encode_canonical().unwrap(),
    };
    let offer_key: SharedTributeOfferKey = Arc::new(OnceLock::new());
    let mut dkg = DkgSessionStore::new();
    let missing = dispatch_with_initialization(
        request.clone(),
        &keys,
        &mut dkg,
        &offer_key,
        B256::from(manifest.chain_id),
        DispatchInitializationContext {
            boot: Some(&boot),
            initialization: Some(&initialization),
            quote_generator: quote,
        },
    );
    assert!(matches!(
        missing,
        EnclaveResponse::Error { ref message }
            if message.contains("requires the permanent offer key")
    ));

    let resident = DerivedTributeOfferKey::from_secret_and_group_sig(
        Zeroizing::new([0x35; 32]),
        Zeroizing::new(vec![0x36; 96]),
    );
    let resident_public = resident.public();
    offer_key.set(resident).ok().expect("install offer key");
    let response = dispatch_with_initialization(
        request,
        &keys,
        &mut dkg,
        &offer_key,
        B256::from(manifest.chain_id),
        DispatchInitializationContext {
            boot: Some(&boot),
            initialization: Some(&initialization),
            quote_generator: quote,
        },
    );
    let EnclaveResponse::DcapQuote {
        transition_key_ready_proof,
        ..
    } = response
    else {
        panic!("expected transition quote response");
    };
    let proof = TransitionKeyReadyProofV1::decode_canonical(&transition_key_ready_proof).unwrap();
    assert_eq!(
        proof.candidate_manifest_hash,
        manifest.authorization_hash().unwrap()
    );
    assert_eq!(proof.resident_offer_public, resident_public);
    proof
        .verify_for_transition(&intent, resident_public)
        .unwrap();
}

#[test]
fn sgx_no_attest_production_session_signs_only_gramine_direct_dev_evidence() {
    use outbe_primitives::tee_attestation_v1::{
        AttestationMode, AttestationOperationV1, RegistrationIntentV1,
    };

    let root = tempfile::tempdir().unwrap();
    let boot = EnclaveBootConfig::new(testnet_chain_word(), root.path().to_path_buf(), 0);
    let keys = EnclaveKeys::new([0x41; 32], Some([0x41; 32])).unwrap();
    let initialization = InitializationState::production_with_challenge_and_attestation(
        Arc::new(boot.clone()),
        &keys,
        [0x42; 32],
        crate::gramine::AttestationType::SgxNoAttest,
    )
    .unwrap();
    let (manifest, node_signature) = signed_initialization_manifest_for_mode(
        &keys,
        [0x42; 32],
        [0x43; 32],
        AttestationMode::GramineDirectDev,
    );
    let pending = initialization
        .prepare(
            &manifest.encode_canonical().unwrap(),
            &node_signature,
            &keys,
        )
        .unwrap();
    initialization.commit(pending, &keys).unwrap();
    let intent = RegistrationIntentV1 {
        chain_id: manifest.chain_id,
        genesis_hash: manifest.genesis_hash,
        operation: AttestationOperationV1::RegisterEnclave,
        attestation_mode: AttestationMode::GramineDirectDev,
        policy_hash: B256::repeat_byte(0x44),
        node_id: manifest.node_id.clone(),
        enclave_id: manifest.enclave_id().unwrap(),
        binding_id: B256::repeat_byte(0x45),
        binding_version: 1,
        registration_version: 0,
        renewal_nonce: 0,
        transition_nonce: 0,
        requested_valid_until: 7_200,
        recipient_x25519: manifest.recipient_x25519,
        attestation_ed25519: manifest.attestation_ed25519,
        noise_responder_x25519: manifest.noise_responder_x25519,
        node_host_authorization_hash: manifest.node_host_authorization_hash().unwrap(),
    };
    let canonical = intent.encode_canonical().unwrap();
    let offer_key: SharedTributeOfferKey = Arc::new(OnceLock::new());
    let mut dkg = DkgSessionStore::new();
    let response = dispatch_with_initialization(
        EnclaveRequest::SignRegistrationIntentDevV1 {
            intent: canonical.clone(),
        },
        &keys,
        &mut dkg,
        &offer_key,
        B256::from(manifest.chain_id),
        DispatchInitializationContext {
            boot: Some(&boot),
            initialization: Some(&initialization),
            quote_generator: |_| panic!("SGX-no-attest dev evidence must not request DCAP"),
        },
    );
    let EnclaveResponse::RegistrationIntentSignedDevV1 {
        intent: echoed,
        enclave_signature,
    } = response
    else {
        panic!("unexpected response: {response:?}")
    };
    assert_eq!(echoed, canonical);
    let signature: [u8; 64] = enclave_signature.try_into().unwrap();
    assert!(intent.verify_enclave_signature(&signature));
}

#[cfg(all(feature = "native-dcap", target_arch = "x86_64", target_os = "linux"))]
#[test]
fn authenticated_noise_rpc_replays_real_processor_acceptance_through_enclave_qvl() {
    use outbe_primitives::tee_attestation_v1::{
        AttestationEvidenceV1, AttestationMode, GramineDirectEvidenceV1,
    };
    use outbe_tee::{
        dcap_protocol::{DcapPlatformTcbStatusV1, DcapRejectCodeV1, DcapVerificationOutcomeV1},
        AuthorizedEnclaveClient, NodeHostNoiseKey,
    };

    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("enclave.sock");
    let endpoint = socket.to_str().unwrap().to_string();
    let boot = Arc::new(EnclaveBootConfig::new(
        testnet_chain_word(),
        root.path().to_path_buf(),
        0,
    ));
    let keys = Arc::new(EnclaveKeys::new([0x73; 32], Some([0x73; 32])).unwrap());
    let initialization = Arc::new(production_dcap_state(boot.clone(), &keys));
    let offer_key: SharedTributeOfferKey = Arc::new(OnceLock::new());
    let listener = UnixListener::bind(&socket).unwrap();
    let server_keys = keys.clone();
    let server_initialization = initialization.clone();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let (stream, _) = listener.accept().unwrap();
            serve_connection_with(
                stream,
                &server_keys,
                &offer_key,
                Some(&boot),
                &server_initialization,
            )
            .unwrap();
        }
    });

    let challenge = AuthorizedEnclaveClient::discover_endpoint(&endpoint).unwrap();
    let node_host_path = root.path().join("node-host-noise.key");
    let node_host = NodeHostNoiseKey::create_new(&node_host_path).unwrap();
    let (manifest, node_signature) =
        signed_initialization_manifest(&keys, challenge.challenge, node_host.public());
    let mut client = AuthorizedEnclaveClient::initialize_endpoint(
        &endpoint,
        &manifest,
        &node_signature,
        &node_host,
    )
    .unwrap();
    let (evidence, policy) = intent_bound_processor_fixture_wire_bytes();

    let DcapVerificationOutcomeV1::Accepted(verdict) = client
        .verify_dcap_evidence_v1(&evidence, &policy, 1_787_850_648)
        .unwrap()
    else {
        panic!("testnet policy must accept the authenticated Processor fixture")
    };
    assert_eq!(
        verdict.platform_tcb_status,
        DcapPlatformTcbStatusV1::ConfigurationAndSWHardeningNeeded
    );
    assert_eq!(
        verdict.advisory_ids,
        vec!["INTEL-SA-00289".to_string(), "INTEL-SA-00615".to_string()]
    );

    let AttestationEvidenceV1::Dcap(dcap) =
        AttestationEvidenceV1::decode_canonical(&evidence).unwrap()
    else {
        unreachable!("fixture must be DCAP evidence")
    };
    let mut unattested_intent = dcap.intent;
    unattested_intent.attestation_mode = AttestationMode::GramineDirectDev;
    let unattested = AttestationEvidenceV1::GramineDirectDev(GramineDirectEvidenceV1 {
        intent: unattested_intent,
        dev_attestation_public: [0x91; 32],
        dev_signature: [0x92; 64],
    })
    .encode_canonical()
    .unwrap();
    assert_eq!(
        client
            .verify_dcap_evidence_v1(&unattested, &policy, 1_787_850_648)
            .unwrap(),
        DcapVerificationOutcomeV1::Rejected(DcapRejectCodeV1::EvidenceNonCanonical)
    );
    drop(client);
    server.join().unwrap();
}

#[test]
fn gramine_direct_dev_onboarding_is_mode_gated_and_persists_before_activation() {
    use outbe_tee::dcap_protocol::DcapOnboardingContextV1;

    let source_keys = EnclaveKeys::new([0x20; 32], Some([0x20; 32])).unwrap();
    let (source_manifest, _) = signed_initialization_manifest_for_mode(
        &source_keys,
        [0x21; 32],
        [0x22; 32],
        AttestationMode::GramineDirectDev,
    );
    let group_signature = vec![0x24; 96];
    let (offer_secret, offer_public) = crate::crypto::derive_tribute_offer_secret_from_group_sig(
        &group_signature,
        B256::from(testnet_chain_word()),
        5,
    )
    .unwrap();
    let resident = DerivedTributeOfferKey::for_test(*offer_secret, group_signature, 4, 5);
    assert_eq!(resident.public(), offer_public);

    let target_root = tempfile::tempdir().unwrap();
    let target_boot = Arc::new(EnclaveBootConfig::new(
        testnet_chain_word(),
        target_root.path().to_path_buf(),
        1,
    ));
    let target_keys = EnclaveKeys::new([0x25; 32], Some([0x25; 32])).unwrap();
    let target_initialization = InitializationState::production_with_challenge_and_attestation(
        target_boot.clone(),
        &target_keys,
        [0x26; 32],
        crate::gramine::AttestationType::SgxNoAttest,
    )
    .unwrap();
    let (target_manifest, target_node_signature) = signed_initialization_manifest_for_mode(
        &target_keys,
        [0x26; 32],
        [0x27; 32],
        AttestationMode::GramineDirectDev,
    );
    let pending = target_initialization
        .prepare(
            &target_manifest.encode_canonical().unwrap(),
            &target_node_signature,
            &target_keys,
        )
        .unwrap();
    target_initialization.commit(pending, &target_keys).unwrap();

    let context = DcapOnboardingContextV1 {
        chain_id: target_manifest.chain_id,
        genesis_hash: target_manifest.genesis_hash,
        intent_hash: B256::repeat_byte(0x28),
        node_id_hash: target_manifest.node_id.node_id_hash().unwrap(),
        enclave_id: target_manifest.enclave_id().unwrap(),
        binding_id: B256::repeat_byte(0x29),
        policy_hash: B256::repeat_byte(0x2a),
        recipient_x25519: target_manifest.recipient_x25519,
        tribute_offer_public: resident.public(),
        key_epoch: resident.key_epoch(),
        tribute_offer_epoch: resident.tribute_offer_epoch(),
    };
    let prepared = complete_gramine_direct_dev_onboarding_response(
        context.context_hash(),
        &context.encode_canonical(),
        Some(&resident),
        Some(&source_manifest),
    );
    let EnclaveResponse::GramineDirectDevOnboardingArtifactPreparedV1 {
        onboarding_artifact,
        ..
    } = prepared
    else {
        panic!("DirectDev source rejected exact context: {prepared:?}");
    };

    let dcap_root = tempfile::tempdir().unwrap();
    let dcap_boot = Arc::new(EnclaveBootConfig::new(
        testnet_chain_word(),
        dcap_root.path().to_path_buf(),
        1,
    ));
    let dcap_keys = EnclaveKeys::new([0x2b; 32], Some([0x2b; 32])).unwrap();
    let dcap_initialization = production_dcap_state(dcap_boot, &dcap_keys);
    let dcap_challenge = match dcap_initialization.challenge_response(&dcap_keys).unwrap() {
        EnclaveResponse::InitializationChallenge { challenge, .. } => challenge,
        response => panic!("unexpected challenge response: {response:?}"),
    };
    let (dcap_manifest, dcap_node_signature) =
        signed_initialization_manifest(&dcap_keys, dcap_challenge, [0x2c; 32]);
    let pending = dcap_initialization
        .prepare(
            &dcap_manifest.encode_canonical().unwrap(),
            &dcap_node_signature,
            &dcap_keys,
        )
        .unwrap();
    dcap_initialization.commit(pending, &dcap_keys).unwrap();

    let request = EnclaveRequest::IngestGramineDirectDevOnboardingArtifactV1 {
        artifact: onboarding_artifact.clone(),
        expected_intent_hash: context.intent_hash,
        expected_tribute_offer_public: context.tribute_offer_public,
        expected_key_epoch: context.key_epoch,
        expected_tribute_offer_epoch: context.tribute_offer_epoch,
    };
    assert_eq!(
        dcap_initialization.authorize_command(&request, false, SessionAuthorityV1::LocalNodeHost,),
        Err("GramineDirectDev onboarding is forbidden by the initialized network")
    );

    let target_offer_key: SharedTributeOfferKey = Arc::new(OnceLock::new());
    assert!(target_initialization
        .authorize_command(&request, false, SessionAuthorityV1::LocalNodeHost)
        .is_ok());
    let response = complete_gramine_direct_dev_onboarding_ingest_response(
        &onboarding_artifact,
        context.intent_hash,
        context.tribute_offer_public,
        context.key_epoch,
        context.tribute_offer_epoch,
        &target_keys,
        &target_offer_key,
        Some(&target_boot),
        Some(&target_initialization),
    );
    let EnclaveResponse::GramineDirectDevOnboardingArtifactIngestedV1 {
        tribute_offer_public,
    } = response
    else {
        panic!("DirectDev target rejected exact artifact: {response:?}");
    };
    assert_eq!(tribute_offer_public, context.tribute_offer_public);
    assert_eq!(
        target_offer_key.get().map(DerivedTributeOfferKey::public),
        Some(context.tribute_offer_public)
    );
    assert!(
        target_boot.sealed_root_path().exists(),
        "permanent key must be durable before the ready slot is published"
    );
    assert!(target_initialization
        .authorize_command(&request, true, SessionAuthorityV1::LocalNodeHost)
        .is_err());
}

#[test]
fn purpose_bound_target_derivation_requires_exact_manifest_context_and_nonce() {
    let keys = EnclaveKeys::new([0x35; 32], None).unwrap();
    let (manifest, _) = signed_initialization_manifest(&keys, [0x36; 32], [0x37; 32]);
    let group_sig = b"deterministic founding group signature";
    let (offer_secret, offer_public) = crate::crypto::derive_tribute_offer_secret_from_group_sig(
        group_sig,
        B256::from(manifest.chain_id),
        4,
    )
    .unwrap();
    let intent_hash = B256::repeat_byte(0x38);
    let context = outbe_tee::dcap_protocol::DcapOnboardingContextV1 {
        chain_id: manifest.chain_id,
        genesis_hash: manifest.genesis_hash,
        intent_hash,
        node_id_hash: manifest.node_id.node_id_hash().unwrap(),
        enclave_id: manifest.enclave_id().unwrap(),
        binding_id: B256::repeat_byte(0x39),
        policy_hash: B256::repeat_byte(0x3a),
        recipient_x25519: manifest.recipient_x25519,
        tribute_offer_public: offer_public,
        key_epoch: 3,
        tribute_offer_epoch: 4,
    };
    let artifact = crate::crypto::encrypt_onboarding_artifact_v1(&offer_secret, context, group_sig)
        .unwrap()
        .encode_canonical()
        .unwrap();
    let derived = derive_onboarding_offer_key_v1(
        &keys,
        &manifest,
        &artifact,
        intent_hash,
        offer_public,
        3,
        4,
    )
    .unwrap();
    assert_eq!(derived.public(), offer_public);
    assert_eq!(derived.group_sig(), group_sig);

    let reject_context = |mutated: outbe_tee::dcap_protocol::DcapOnboardingContextV1| {
        let mut artifact =
            crate::crypto::encrypt_onboarding_artifact_v1(&offer_secret, context, group_sig)
                .unwrap();
        artifact.context = mutated;
        let artifact = artifact.encode_canonical().unwrap();
        assert!(derive_onboarding_offer_key_v1(
            &keys,
            &manifest,
            &artifact,
            intent_hash,
            offer_public,
            3,
            4,
        )
        .is_err());
    };
    let mut mutated = context;
    mutated.chain_id[0] ^= 1;
    reject_context(mutated);
    let mut mutated = context;
    mutated.genesis_hash = B256::repeat_byte(0x40);
    reject_context(mutated);
    let mut mutated = context;
    mutated.intent_hash = B256::repeat_byte(0x41);
    reject_context(mutated);
    let mut mutated = context;
    mutated.node_id_hash = B256::repeat_byte(0x42);
    reject_context(mutated);
    let mut mutated = context;
    mutated.enclave_id = B256::repeat_byte(0x43);
    reject_context(mutated);
    let mut mutated = context;
    mutated.recipient_x25519 = [0x44; 32];
    reject_context(mutated);
    let mut mutated = context;
    mutated.tribute_offer_public = [0x45; 32];
    reject_context(mutated);
    let mut mutated = context;
    mutated.key_epoch = 5;
    reject_context(mutated);
    let mut mutated = context;
    mutated.tribute_offer_epoch = 6;
    reject_context(mutated);

    assert!(derive_onboarding_offer_key_v1(
        &keys,
        &manifest,
        &artifact,
        B256::repeat_byte(0xff),
        offer_public,
        3,
        4,
    )
    .is_err());
    assert!(derive_onboarding_offer_key_v1(
        &keys,
        &manifest,
        &artifact,
        intent_hash,
        offer_public,
        3,
        5,
    )
    .is_err());

    let mut tampered = artifact;
    let nonce_offset = 1 + context.encode_canonical().len();
    tampered[nonce_offset] ^= 1;
    assert!(derive_onboarding_offer_key_v1(
        &keys,
        &manifest,
        &tampered,
        intent_hash,
        offer_public,
        3,
        4,
    )
    .is_err());
}

#[test]
fn onboarding_cannot_decrypt_or_activate_before_finalized_admission_verifies() {
    let enclave = Enclave::new(0x39);
    let artifact = outbe_tee::dcap_protocol::DcapOnboardingArtifactV1 {
        context: outbe_tee::dcap_protocol::DcapOnboardingContextV1 {
            chain_id: testnet_chain_word(),
            genesis_hash: B256::repeat_byte(0x71),
            intent_hash: B256::repeat_byte(0x72),
            node_id_hash: B256::repeat_byte(0x73),
            enclave_id: B256::repeat_byte(0x74),
            binding_id: B256::repeat_byte(0x78),
            policy_hash: B256::repeat_byte(0x79),
            recipient_x25519: enclave.keys.tribute_offer_public(),
            tribute_offer_public: [0x75; 32],
            key_epoch: 0,
            tribute_offer_epoch: 0,
        },
        nonce: [0x76; 12],
        ciphertext: vec![0x77; 112],
    }
    .encode_canonical()
    .unwrap();

    let response = complete_onboarding_artifact_ingest_response(
        CompleteOnboardingArtifactIngestV1 {
            request_hash: B256::repeat_byte(0x70),
            artifact,
            expected_intent_hash: B256::repeat_byte(0x72),
            expected_tribute_offer_public: [0x75; 32],
            expected_key_epoch: 0,
            expected_tribute_offer_epoch: 0,
            verified_admission: crate::finalized_admission::VerifiedAdmissionAnchorV1 {
                block_number: 1,
                block_hash: B256::repeat_byte(0x7a),
                state_root: B256::repeat_byte(0x7b),
                consensus_timestamp: 1,
            },
        },
        &enclave.keys,
        &enclave.offer_key,
        Some(&enclave.boot),
        &enclave.initialization,
    );
    let EnclaveResponse::Error { message } = response else {
        panic!("unproved onboarding unexpectedly succeeded: {response:?}");
    };
    assert!(
        message.contains(
            "onboarding artifact does not match initialized identity or finalized Registry context"
        ),
        "{message}"
    );
    assert!(
        enclave.offer_key.get().is_none(),
        "unproved ciphertext must never activate a resident key"
    );
    assert!(
        !enclave.boot.sealed_root_path().exists(),
        "unproved ciphertext must never reach durable key storage"
    );
}
