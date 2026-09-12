use super::*;

#[test]
fn candidate_generated_quote_intent_reaches_registry_replacement_exactly() {
    use std::{
        os::unix::net::UnixListener,
        sync::{Arc, OnceLock},
    };

    use outbe_primitives::tee_attestation_v1::{
        AttestationEvidenceV1, DcapCollateralComponentV1, DcapCollateralKind, DcapEvidenceV1,
    };
    use outbe_tee::{
        connect_or_initialize_node_host_enclave, load_replacement_candidate_submission,
        persist_replacement_candidate_submission, prepare_node_host_enclave_replacement_candidate,
        NodeHostIdentityV1,
    };
    use outbe_tee_enclave::{
        initialization::InitializationState,
        keys::EnclaveKeys,
        seal::EnclaveBootConfig,
        transport::{serve_connection_with_synthetic_dcap, SharedTributeOfferKey},
    };

    let genesis_hash = B256::repeat_byte(0x23);
    let active_policy = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
    let node_signer = OutbeEvmSigner::from_secret_bytes([0x75; 32]).unwrap();
    let identity = NodeHostIdentityV1 {
        network_binding: active_policy.network_binding(),
        reth_p2p_public: reth_p2p_public_for_evm_signer(&node_signer),
    };
    let sign = |hash: B256| {
        node_signer
            .sign_hash(&hash)
            .map_err(|error| error.to_string())
    };

    let root = tempfile::tempdir().unwrap();
    let socket_a = root.path().join("active-enclave.sock");
    let socket_b = root.path().join("candidate-enclave.sock");
    let endpoint_a = socket_a.to_str().unwrap().to_owned();
    let endpoint_b = socket_b.to_str().unwrap().to_owned();
    let boot_a = Arc::new(EnclaveBootConfig::new(
        active_policy.chain_id,
        root.path().join("active-enclave-state"),
        0,
    ));
    let boot_b = Arc::new(EnclaveBootConfig::new(
        active_policy.chain_id,
        root.path().join("candidate-enclave-state"),
        0,
    ));
    std::fs::create_dir(&boot_a.tee_dir).unwrap();
    std::fs::create_dir(&boot_b.tee_dir).unwrap();
    let keys_a = Arc::new(EnclaveKeys::new([0x76; 32], Some([0x76; 32])).unwrap());
    let keys_b = Arc::new(EnclaveKeys::new([0x77; 32], Some([0x77; 32])).unwrap());
    let initialization_a = Arc::new(
        InitializationState::production_with_synthetic_dcap_for_test(boot_a.clone(), &keys_a)
            .unwrap(),
    );
    let initialization_b = Arc::new(
        InitializationState::production_with_synthetic_dcap_for_test(boot_b.clone(), &keys_b)
            .unwrap(),
    );

    let listener_a = UnixListener::bind(&socket_a).unwrap();
    let server_keys_a = keys_a.clone();
    let server_a = std::thread::spawn(move || {
        let offer_key: SharedTributeOfferKey = Arc::new(OnceLock::new());
        for _ in 0..2 {
            let (stream, _) = listener_a.accept().unwrap();
            serve_connection_with_synthetic_dcap(
                stream,
                &server_keys_a,
                &offer_key,
                Some(&boot_a),
                &initialization_a,
            )
            .unwrap();
        }
    });
    let listener_b = UnixListener::bind(&socket_b).unwrap();
    let server_keys_b = keys_b.clone();
    let server_b = std::thread::spawn(move || {
        let offer_key: SharedTributeOfferKey = Arc::new(OnceLock::new());
        for _ in 0..2 {
            let (stream, _) = listener_b.accept().unwrap();
            serve_connection_with_synthetic_dcap(
                stream,
                &server_keys_b,
                &offer_key,
                Some(&boot_b),
                &initialization_b,
            )
            .unwrap();
        }
    });

    let node_data_dir = root.path().join("node-data");
    std::fs::create_dir(&node_data_dir).unwrap();
    drop(
        connect_or_initialize_node_host_enclave(&endpoint_a, &node_data_dir, identity, sign)
            .unwrap(),
    );
    let active_manifest_bytes = std::fs::read(
        node_data_dir
            .join(outbe_tee::node_host::NODE_HOST_DIRECTORY_V1)
            .join(outbe_tee::node_host::NODE_HOST_MANIFEST_V1),
    )
    .unwrap();
    let active_manifest =
        EnclaveInitializationManifestV1::decode_canonical(&active_manifest_bytes).unwrap();
    let initial = RegistrationIntentV1 {
        chain_id: active_manifest.chain_id,
        genesis_hash: active_manifest.genesis_hash,
        operation: AttestationOperationV1::RegisterEnclave,
        attestation_mode: AttestationMode::DcapRequired,
        policy_hash: active_policy.policy_hash().unwrap(),
        node_id: active_manifest.node_id.clone(),
        enclave_id: active_manifest.enclave_id().unwrap(),
        binding_id: B256::repeat_byte(0x66),
        binding_version: 1,
        registration_version: 0,
        renewal_nonce: 0,
        transition_nonce: 0,
        requested_valid_until: NOW + 3_600,
        recipient_x25519: active_manifest.recipient_x25519,
        attestation_ed25519: active_manifest.attestation_ed25519,
        noise_responder_x25519: active_manifest.noise_responder_x25519,
        node_host_authorization_hash: active_manifest.node_host_authorization_hash().unwrap(),
    };
    active_manifest.validate_intent_binding(&initial).unwrap();
    let initial_hash = initial.intent_hash().unwrap();
    let initial_node_signature = node_signer.sign_hash(&initial_hash).unwrap();
    let initial_enclave_signature = keys_a.sign_attestation(initial_hash.as_slice());

    let mut candidate = prepare_node_host_enclave_replacement_candidate(
        &endpoint_b,
        &node_data_dir,
        identity,
        sign,
    )
    .unwrap();
    let candidate_manifest = candidate.manifest().clone();
    let mut replacement = initial.clone();
    replacement.operation = AttestationOperationV1::ReplaceEnclaveBinding;
    replacement.enclave_id = candidate_manifest.enclave_id().unwrap();
    replacement.binding_id = B256::repeat_byte(0x68);
    replacement.binding_version = 2;
    replacement.registration_version = 1;
    replacement.requested_valid_until = NOW + 3_600;
    replacement.recipient_x25519 = candidate_manifest.recipient_x25519;
    replacement.attestation_ed25519 = candidate_manifest.attestation_ed25519;
    replacement.noise_responder_x25519 = candidate_manifest.noise_responder_x25519;
    replacement.node_host_authorization_hash =
        candidate_manifest.node_host_authorization_hash().unwrap();
    candidate_manifest
        .validate_intent_binding(&replacement)
        .unwrap();
    assert_eq!(
        replacement.node_host_authorization_hash,
        initial.node_host_authorization_hash
    );

    let generated = candidate.generate_dcap_quote(&replacement).unwrap();
    let replacement_hash = replacement.intent_hash().unwrap();
    let replacement_node_signature = node_signer.sign_hash(&replacement_hash).unwrap();
    let evidence = AttestationEvidenceV1::Dcap(DcapEvidenceV1 {
        intent: replacement.clone(),
        quote: generated.quote_body.clone(),
        components: (1_u8..=8)
            .map(|kind| DcapCollateralComponentV1 {
                kind: DcapCollateralKind::try_from(kind).unwrap(),
                bytes: vec![kind],
            })
            .collect(),
        transition_key_ready_proof: None,
    });
    let exact_evidence = evidence.encode_canonical().unwrap();
    let submission = persist_replacement_candidate_submission(
        &node_data_dir,
        &evidence,
        &replacement_node_signature,
        &generated.enclave_signature,
    )
    .unwrap();
    assert_eq!(submission.evidence(), exact_evidence);
    assert_eq!(
        load_replacement_candidate_submission(&node_data_dir)
            .unwrap()
            .unwrap(),
        submission
    );
    let AttestationEvidenceV1::Dcap(submitted) =
        AttestationEvidenceV1::decode_canonical(submission.evidence()).unwrap()
    else {
        unreachable!();
    };
    assert_eq!(
        submitted.intent.encode_canonical().unwrap(),
        replacement.encode_canonical().unwrap()
    );
    assert_eq!(submitted.quote, generated.quote_body);

    let mut accepted = verdict(DcapPlatformTcbStatusV1::UpToDate);
    accepted.collateral_valid_until = NOW + 12_000;
    let mut provider = storage(genesis_hash);
    StorageHandle::enter(&mut provider, |storage| {
        register_validator(storage.clone(), &node_signer, CONSENSUS_KEY);
        let mut registry = TeeRegistry::new(storage.clone());
        registry.install_initial_policy_v1(&active_policy).unwrap();
        register_same_key_node_for_lifecycle_test(
            &mut registry,
            &initial,
            &node_signer,
            &initial_node_signature,
            &initial_enclave_signature,
            PostVerifierDcapCapabilityV1::new(accepted.clone()),
        )
        .unwrap();
        assert_eq!(
            registry
                .replace_enclave_binding_after_verifier_for_test(
                    &submitted.intent,
                    submission.node_signature(),
                    submission.enclave_signature(),
                    PostVerifierDcapCapabilityV1::new(accepted),
                )
                .unwrap(),
            V1RegistrationOutcome::Created
        );
        let binding = registry
            .validator_enclave_binding_v1(node_signer.address())
            .unwrap()
            .unwrap();
        assert_eq!(binding.enclave_id, candidate_manifest.enclave_id().unwrap());
        assert_eq!(binding.binding_id, replacement.binding_id);
        assert_eq!(binding.binding_version, replacement.binding_version);
        assert_eq!(
            binding.registration_version,
            replacement.registration_version
        );
    });

    drop(candidate);
    server_a.join().unwrap();
    server_b.join().unwrap();
}

#[test]
fn replacement_candidate_intent_reaches_registry_unchanged_and_never_reuses_consumed_ids() {
    let genesis_hash = B256::repeat_byte(0x23);
    let active_policy = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
    let node_signer = OutbeEvmSigner::from_secret_bytes([0x75; 32]).unwrap();
    let old_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x76; 32]);
    let new_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x77; 32]);
    let initial = registration_intent(
        &active_policy,
        &node_signer,
        CONSENSUS_KEY,
        &old_enclave,
        0x65,
        0x66,
    );
    let replacement = replacement_intent(&initial, &new_enclave, 0x67, 0x68, NOW + 3_600);
    let active_manifest = initialization_manifest_for_intent(&initial, [0xa6; 32]);
    let candidate_manifest = initialization_manifest_for_intent(&replacement, [0xa7; 32]);
    assert_ne!(
        active_manifest.authorization_hash().unwrap(),
        candidate_manifest.authorization_hash().unwrap()
    );
    assert_eq!(
        active_manifest.node_host_authorization_hash().unwrap(),
        candidate_manifest.node_host_authorization_hash().unwrap()
    );
    candidate_manifest
        .validate_intent_binding(&replacement)
        .unwrap();
    let candidate_intent_bytes = replacement.encode_canonical().unwrap();
    let (initial_node, initial_enclave) = signatures(&initial, &node_signer, &old_enclave);
    let (replacement_node, replacement_enclave) =
        signatures(&replacement, &node_signer, &new_enclave);
    let mut accepted = verdict(DcapPlatformTcbStatusV1::UpToDate);
    accepted.collateral_valid_until = NOW + 12_000;
    let mut provider = storage(genesis_hash);

    StorageHandle::enter(&mut provider, |storage| {
        register_validator(storage.clone(), &node_signer, CONSENSUS_KEY);
        let mut registry = TeeRegistry::new(storage.clone());
        registry.install_initial_policy_v1(&active_policy).unwrap();
        register_same_key_node_for_lifecycle_test(
            &mut registry,
            &initial,
            &node_signer,
            &initial_node,
            &initial_enclave,
            PostVerifierDcapCapabilityV1::new(accepted.clone()),
        )
        .unwrap();
        assert_eq!(
            registry
                .replace_enclave_binding_after_verifier_for_test(
                    &replacement,
                    &replacement_node,
                    &replacement_enclave,
                    PostVerifierDcapCapabilityV1::new(accepted.clone()),
                )
                .unwrap(),
            V1RegistrationOutcome::Created
        );
        assert_eq!(
            registry
                .replace_enclave_binding_after_verifier_for_test(
                    &replacement,
                    &replacement_node,
                    &replacement_enclave,
                    PostVerifierDcapCapabilityV1::new(accepted.clone()),
                )
                .unwrap(),
            V1RegistrationOutcome::Idempotent
        );
        let binding = registry
            .validator_enclave_binding_v1(node_signer.address())
            .unwrap()
            .unwrap();
        assert_eq!(binding.enclave_id, replacement.enclave_id);
        assert_eq!(binding.binding_id, replacement.binding_id);
        assert_eq!(binding.binding_version, 2);
        assert_eq!(binding.registration_version, 1);
        assert_eq!(
            replacement.encode_canonical().unwrap(),
            candidate_intent_bytes
        );

        let old_renewal = renewal_intent(&initial, NOW + 6_000);
        let (old_node, old_enclave_signature) =
            signatures(&old_renewal, &node_signer, &old_enclave);
        storage
            .set_block_timestamp(U256::from(NOW + 2_400))
            .unwrap();
        assert!(revert_message(
            registry
                .renew_enclave_after_verifier_for_test(
                    &old_renewal,
                    &old_node,
                    &old_enclave_signature,
                    PostVerifierDcapCapabilityV1::new(accepted.clone()),
                )
                .unwrap_err()
        )
        .contains("superseded"));

        let current = replacement.clone();
        let attempted_reuse = replacement_intent(&current, &old_enclave, 0x65, 0x66, NOW + 6_000);
        let (reuse_node, reuse_enclave) = signatures(&attempted_reuse, &node_signer, &old_enclave);
        assert!(revert_message(
            registry
                .replace_enclave_binding_after_verifier_for_test(
                    &attempted_reuse,
                    &reuse_node,
                    &reuse_enclave,
                    PostVerifierDcapCapabilityV1::new(accepted),
                )
                .unwrap_err()
        )
        .contains("already been used"));
    });
}

#[test]
fn renew_and_replace_abi_are_replica_deterministic_and_fit_normative_gas() {
    let genesis_hash = B256::repeat_byte(0x24);
    let mut active_policy = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
    active_policy.maximum_lease = 3_600;
    let node_signer = OutbeEvmSigner::from_secret_bytes([0x78; 32]).unwrap();
    let old_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x79; 32]);
    let new_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x7A; 32]);
    let initial = registration_intent(
        &active_policy,
        &node_signer,
        CONSENSUS_KEY,
        &old_enclave,
        0x69,
        0x6A,
    );
    let renewal = renewal_intent(
        &initial,
        initial.requested_valid_until + active_policy.maximum_lease,
    );
    let replacement = replacement_intent(&renewal, &new_enclave, 0x6B, 0x6C, NOW + 6_000);
    let (initial_node, initial_enclave) = signatures(&initial, &node_signer, &old_enclave);
    let (renewal_node, renewal_enclave) = signatures(&renewal, &node_signer, &old_enclave);
    let (replacement_node, replacement_enclave) =
        signatures(&replacement, &node_signer, &new_enclave);
    let evidence = vec![0xA7; 4_096];
    let renewal_call = IRegisterEnclaveV1Test::renewEnclaveCall {
        evidence: evidence.clone().into(),
        nodeSignature: renewal_node.to_vec().into(),
        enclaveSignature: renewal_enclave.to_vec().into(),
    }
    .abi_encode();
    let replacement_call = IRegisterEnclaveV1Test::replaceEnclaveBindingCall {
        evidence: evidence.clone().into(),
        nodeSignature: replacement_node.to_vec().into(),
        enclaveSignature: replacement_enclave.to_vec().into(),
    }
    .abi_encode();
    let mut accepted = verdict(DcapPlatformTcbStatusV1::UpToDate);
    accepted.collateral_valid_until = NOW + 12_000;

    let execute = || {
        let mut provider = storage(genesis_hash);
        StorageHandle::enter(&mut provider, |storage| {
            register_validator(storage.clone(), &node_signer, CONSENSUS_KEY);
            let mut registry = TeeRegistry::new(storage.clone());
            registry.install_initial_policy_v1(&active_policy).unwrap();
            register_same_key_node_for_lifecycle_test(
                &mut registry,
                &initial,
                &node_signer,
                &initial_node,
                &initial_enclave,
                PostVerifierDcapCapabilityV1::new(accepted.clone()),
            )
            .unwrap();
            storage
                .set_block_timestamp(U256::from(NOW + 2_400))
                .unwrap();
        });
        provider.enable_production_storage_gas_metering();
        provider.set_gas_limit(u64::MAX);
        StorageHandle::enter(&mut provider, |storage| {
            dispatch_renew_after_verifier_for_test(
                storage.clone(),
                node_signer.address(),
                &renewal_call,
                &renewal,
                PostVerifierDcapCapabilityV1::new(accepted.clone()),
            )
            .unwrap();
            dispatch_replace_after_verifier_for_test(
                storage,
                node_signer.address(),
                &replacement_call,
                &replacement,
                PostVerifierDcapCapabilityV1::new(accepted.clone()),
            )
            .unwrap();
        });
        provider
    };
    let proposer = execute();
    let validator = execute();
    let follower = execute();
    for replica in [&validator, &follower] {
        assert_eq!(replica.storage, proposer.storage);
        assert_eq!(replica.get_ordered_events(), proposer.get_ordered_events());
        assert_eq!(
            replica.metered_storage_operations(),
            proposer.metered_storage_operations()
        );
        assert_eq!(replica.gas_used(), proposer.gas_used());
    }

    let schedule = TeeRegistryGasScheduleV1::normative();
    let mut maximum_total = 0_u64;
    let mut intrinsic_total = 0_u64;
    let mut allowance_total = 0_u64;
    for (kind, call) in [
        (RegistryMutatorV1::RenewEnclave, &renewal_call),
        (RegistryMutatorV1::ReplaceEnclaveBinding, &replacement_call),
    ] {
        let maximum = schedule
            .maximum_transaction_gas(
                kind,
                call.len(),
                evidence.len(),
                active_policy.measurement_rules.len(),
                AttestationMode::DcapRequired,
            )
            .unwrap();
        assert!(maximum < 30_000_000);
        maximum_total += maximum;
        intrinsic_total += schedule.maximum_calldata_intrinsic_gas(call.len()).unwrap();
        allowance_total += schedule.mutator_storage_gas_allowance(kind);
    }
    let (reads, writes) = proposer.metered_storage_operations();
    let storage_gas = reads * 100 + writes * 5_000;
    assert!(storage_gas <= allowance_total);
    assert_eq!(
        intrinsic_total + 2 * 200 + proposer.gas_used(),
        maximum_total - allowance_total + storage_gas
    );
    assert!(intrinsic_total + 2 * 200 + proposer.gas_used() <= maximum_total);
}
