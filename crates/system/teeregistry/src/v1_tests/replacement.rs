use super::fixtures::bind_reachable_node_host_authorization;
use super::transitions::install_offer_key;
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

/// Exercise the real DirectDev ABI, including the resident network-key proof.
#[test]
fn strict_measurement_upgrade_allows_overlap_and_owner_recovery_after_retirement() {
    use outbe_primitives::tee_attestation_v1::GramineDirectEvidenceV1;
    for late in [false, true] {
        let genesis = B256::repeat_byte(0xd1);
        let mut current = policy(genesis, PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded);
        current.attestation_mode = AttestationMode::GramineDirectDev;
        let owner =
            OutbeEvmSigner::from_secret_bytes([0xd2; 32]).unwrap();
        let old = ed25519_dalek::SigningKey::from_bytes(&[0xd3; 32]);
        let new = ed25519_dalek::SigningKey::from_bytes(&[0xd4; 32]);
        let mut initial = registration_intent(&current, &owner, CONSENSUS_KEY, &old, 0x51, 0x61);
        initial.attestation_mode = AttestationMode::GramineDirectDev;
        bind_reachable_node_host_authorization(&mut initial, [0xa6; 32]);
        let (node_sig, enclave_sig) = signatures(&initial, &owner, &old);
        let mut provider = storage(genesis);
        let successor = provider.enter(|storage| {
            register_validator(storage.clone(), &owner, CONSENSUS_KEY);
            if late {
                let mut validators = ValidatorSet::new(storage.clone());
                validators
                    .activate_validator_via_boundary_for_test(owner.address())
                    .unwrap();
                validators
                    .jail_validator_for_tee_expiry(owner.address())
                    .unwrap();
            }
            let mut registry = TeeRegistry::new(storage);
            registry.install_initial_policy_v1(&current).unwrap();
            install_offer_key(&mut registry, &current);
            register_same_key_node_for_lifecycle_test(
                &mut registry,
                &initial,
                &owner,
                &node_sig,
                &enclave_sig,
                PostVerifierDcapCapabilityV1::new(verdict(DcapPlatformTcbStatusV1::UpToDate)),
            )
            .unwrap();
            let predecessor = current.policy_hash().unwrap();
            assert!(registry
                .stage_measurement_upgrade_v1(
                    U256::from(7),
                    B256::repeat_byte(0xe1),
                    B256::ZERO,
                    50
                )
                .is_err());
            registry
                .stage_measurement_upgrade_v1(
                    U256::from(7),
                    B256::repeat_byte(0xe1),
                    predecessor,
                    50,
                )
                .unwrap();
            let binding = registry
                .validator_enclave_binding_v1(owner.address())
                .unwrap()
                .unwrap();
            assert!(registry.binding_code_admitted_at_v1(&binding, 49).unwrap());
            assert!(!registry.binding_code_admitted_at_v1(&binding, 50).unwrap());
            assert!(
                binding.valid_until > NOW,
                "a long lease must not bypass code retirement"
            );
            registry.staged_successor_policy_v1().unwrap().unwrap().1
        });
        let now = if late { NOW + 4_000 } else { NOW };
        if late {
            provider.set_block_number(50);
            provider.set_timestamp(U256::from(now));
            provider.enter(|storage| {
                TeeRegistry::new(storage)
                    .promote_staged_successor_policy_v1(U256::from(7), 50)
                    .unwrap()
            });
        }
        let transition =
            measurement_transition_intent(&initial, &successor, &new, 0x52, 0x62, now + 3_600);
        let mut proof = TransitionKeyReadyProofV1 {
            chain_id: transition.chain_id,
            genesis_hash: transition.genesis_hash,
            transition_intent_hash: transition.intent_hash().unwrap(),
            candidate_manifest_hash: initialization_manifest_for_intent(&transition, [0xa7; 32])
                .authorization_hash()
                .unwrap(),
            transition_nonce: transition.transition_nonce,
            resident_offer_public: OFFER_PUBLIC,
            candidate_attestation_signature: [0; 64],
        };
        proof.candidate_attestation_signature = new
            .sign(proof.signing_hash().unwrap().as_slice())
            .to_bytes();
        let (node_sig, enclave_sig) = signatures(&transition, &owner, &new);
        let mut evidence = GramineDirectEvidenceV1 {
            intent: transition.clone(),
            dev_attestation_public: new.verifying_key().to_bytes(),
            dev_signature: enclave_sig,
            transition_key_ready_proof: Some(proof),
        };
        let call = |e: &GramineDirectEvidenceV1| {
            IRegisterEnclaveV1Test::transitionEnclaveMeasurementCall {
                evidence: AttestationEvidenceV1::GramineDirectDev(e.clone())
                    .encode_canonical()
                    .unwrap()
                    .into(),
                nodeSignature: node_sig.to_vec().into(),
                enclaveSignature: enclave_sig.to_vec().into(),
            }
            .abi_encode()
        };
        // Ordinary replacement must not bypass the resident-key transition proof.
        let mut replacement = transition.clone();
        replacement.operation = AttestationOperationV1::ReplaceEnclaveBinding;
        replacement.transition_nonce = initial.transition_nonce;
        let (replace_node, replace_enclave) = signatures(&replacement, &owner, &new);
        let replace_evidence = AttestationEvidenceV1::GramineDirectDev(GramineDirectEvidenceV1 {
            intent: replacement,
            dev_attestation_public: new.verifying_key().to_bytes(),
            dev_signature: replace_enclave,
            transition_key_ready_proof: None,
        })
        .encode_canonical()
        .unwrap();
        let replace_call = IRegisterEnclaveV1Test::replaceEnclaveBindingCall {
            evidence: replace_evidence.into(),
            nodeSignature: replace_node.to_vec().into(),
            enclaveSignature: replace_enclave.to_vec().into(),
        }
        .abi_encode();
        provider.enter(|storage| {
            assert!(revert_message(
                crate::v1_precompile::dispatch(storage, &replace_call, owner.address(), U256::ZERO)
                    .unwrap_err()
            )
            .contains("transitionEnclaveMeasurement"))
        });
        let original = provider.storage.clone();
        provider.enter(|storage| {
            assert!(crate::v1_precompile::dispatch(
                storage.clone(),
                &call(&evidence),
                Address::repeat_byte(0xfa),
                U256::ZERO
            )
            .is_err());
            let proof = evidence.transition_key_ready_proof.as_mut().unwrap();
            proof.resident_offer_public = [0xee; 32];
            proof.candidate_attestation_signature = new
                .sign(proof.signing_hash().unwrap().as_slice())
                .to_bytes();
            assert!(crate::v1_precompile::dispatch(
                storage,
                &call(&evidence),
                owner.address(),
                U256::ZERO
            )
            .is_err());
        });
        assert_eq!(
            provider.storage, original,
            "failed ownership/key proofs must leave state intact"
        );
        let proof = evidence.transition_key_ready_proof.as_mut().unwrap();
        proof.resident_offer_public = OFFER_PUBLIC;
        proof.candidate_attestation_signature = new
            .sign(proof.signing_hash().unwrap().as_slice())
            .to_bytes();
        provider.enable_production_storage_gas_metering();
        provider.set_gas_limit(u64::MAX);
        let calldata = call(&evidence);
        let gas_schedule = TeeRegistryGasScheduleV1::normative();
        let maximum = gas_schedule
            .maximum_transaction_gas(
                RegistryMutatorV1::TransitionEnclaveMeasurement,
                calldata.len(),
                AttestationEvidenceV1::GramineDirectDev(evidence.clone())
                    .encode_canonical()
                    .unwrap()
                    .len(),
                1,
                AttestationMode::GramineDirectDev,
            )
            .unwrap();
        let intrinsic = gas_schedule
            .maximum_calldata_intrinsic_gas(calldata.len())
            .unwrap();
        provider.enter(|storage| {
            crate::v1_precompile::dispatch(
                storage.clone(),
                &call(&evidence),
                owner.address(),
                U256::ZERO,
            )
            .unwrap();
            assert!(
                storage.gas_used().unwrap() + intrinsic + 200 <= maximum,
                "strict transition exceeds advertised transaction gas"
            );
            // Exact replay must remain idempotent, including after retirement.
            crate::v1_precompile::dispatch(
                storage.clone(),
                &call(&evidence),
                owner.address(),
                U256::ZERO,
            )
            .unwrap();
            let registry = TeeRegistry::new(storage);
            let binding = registry
                .validator_enclave_binding_v1(owner.address())
                .unwrap()
                .unwrap();
            assert_eq!(binding.policy_hash, successor.policy_hash().unwrap());
            assert_eq!(binding.mrenclave, B256::repeat_byte(0xe1));
            assert!(registry.binding_code_admitted_at_v1(&binding, 49).unwrap());
            assert!(registry.binding_code_admitted_at_v1(&binding, 50).unwrap());
            assert_eq!(binding.transition_nonce, 1);
        });
    }
}

#[test]
fn legacy_upgrade_probe_does_not_add_metered_storage_reads_or_gas() {
    let mut provider = storage(B256::repeat_byte(0xf1));
    provider.enable_production_storage_gas_metering();
    provider.enter(|storage| {
        let registry = TeeRegistry::new(storage);
        assert_eq!(
            registry.enclave_upgrade_v1().unwrap(),
            crate::upgrade::EnclaveUpgradeV1::default()
        );
        assert!(!registry.strict_upgrade_pending_v1().unwrap());
        assert!(registry.upgrade_sweep_due_v1().unwrap().is_none());
    });
    assert_eq!(provider.gas_used(), 0);
    assert_eq!(provider.metered_storage_operations(), (0, 0));
}

#[test]
fn pending_upgrade_preserves_active_binding_and_survives_renewal() {
    use outbe_primitives::tee_attestation_v1::GramineDirectEvidenceV1;
    use outbe_primitives::tee_registry_abi_v1::ITeeRegistryV1;
    let genesis = B256::repeat_byte(0xd1);
    let mut current = policy(genesis, PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded);
    current.attestation_mode = AttestationMode::GramineDirectDev;
    let owner =
        OutbeEvmSigner::from_secret_bytes([0xd2; 32]).unwrap();
    let old = ed25519_dalek::SigningKey::from_bytes(&[0xd3; 32]);
    let new = ed25519_dalek::SigningKey::from_bytes(&[0xd4; 32]);
    let mut initial = registration_intent(&current, &owner, CONSENSUS_KEY, &old, 0x51, 0x61);
    initial.attestation_mode = AttestationMode::GramineDirectDev;
    bind_reachable_node_host_authorization(&mut initial, [0xa6; 32]);
    let (node_sig, enclave_sig) = signatures(&initial, &owner, &old);
    let mut provider = storage(genesis);
    let successor = provider.enter(|storage| {
        register_validator(storage.clone(), &owner, CONSENSUS_KEY);
        let mut registry = TeeRegistry::new(storage);
        registry.install_initial_policy_v1(&current).unwrap();
        install_offer_key(&mut registry, &current);
        register_same_key_node_for_lifecycle_test(
            &mut registry,
            &initial,
            &owner,
            &node_sig,
            &enclave_sig,
            PostVerifierDcapCapabilityV1::new(verdict(DcapPlatformTcbStatusV1::UpToDate)),
        )
        .unwrap();
        registry
            .stage_measurement_upgrade_v1(
                U256::from(7),
                B256::repeat_byte(0xe1),
                current.policy_hash().unwrap(),
                50,
            )
            .unwrap();
        registry.staged_successor_policy_v1().unwrap().unwrap().1
    });
    let node = initial.node_id.node_id_hash().unwrap();
    let mut prepare =
        measurement_transition_intent(&initial, &successor, &new, 0x52, 0x62, NOW + 7_200);
    prepare.operation = AttestationOperationV1::PrepareEnclaveUpgrade;
    let prepare_call = |intent: &RegistrationIntentV1| {
        let (node_sig, enclave_sig) = signatures(intent, &owner, &new);
        ITeeRegistryV1::prepareEnclaveUpgradeCall {
            evidence: AttestationEvidenceV1::GramineDirectDev(GramineDirectEvidenceV1 {
                intent: intent.clone(),
                dev_attestation_public: new.verifying_key().to_bytes(),
                dev_signature: enclave_sig,
                transition_key_ready_proof: None,
            })
            .encode_canonical()
            .unwrap()
            .into(),
            nodeSignature: node_sig.to_vec().into(),
            enclaveSignature: enclave_sig.to_vec().into(),
        }
        .abi_encode()
    };
    let before = provider.storage.clone();
    provider.enter(|s| {
        assert!(crate::v1_precompile::dispatch(
            s,
            &prepare_call(&prepare),
            Address::repeat_byte(99),
            U256::ZERO
        )
        .is_err())
    });
    assert_eq!(before, provider.storage);
    // Concurrent ordinary renewal changes only the active renewal counters. The
    // already signed prepare remains valid because its source binding is stable.
    provider.enter(|storage| {
        let registry = TeeRegistry::new(storage);
        registry
            .v1_node_registration_version
            .write(&node, initial.registration_version + 1)
            .unwrap();
        registry
            .v1_node_renewal_nonce
            .write(&node, initial.renewal_nonce + 1)
            .unwrap();
        registry
            .v1_node_valid_until
            .write(&node, initial.requested_valid_until + current.maximum_lease)
            .unwrap();
    });
    let old_binding = provider.enter(|s| {
        TeeRegistry::new(s)
            .validator_enclave_binding_v1(owner.address())
            .unwrap()
            .unwrap()
    });
    provider.enable_production_storage_gas_metering();
    provider.set_gas_limit(u64::MAX);
    let bytes = prepare_call(&prepare);
    provider.enter(|storage| {
        crate::v1_precompile::dispatch(storage.clone(), &bytes, owner.address(), U256::ZERO)
            .unwrap();
        let registry = TeeRegistry::new(storage.clone());
        assert_eq!(
            registry
                .validator_enclave_binding_v1(owner.address())
                .unwrap()
                .unwrap(),
            old_binding
        );
        assert_eq!(registry.upgrade_candidate_nonce.read(&node).unwrap(), 1);
        assert_eq!(
            registry.upgrade_candidate_source.read(&node).unwrap(),
            initial.binding_id
        );
        assert_eq!(
            registry.upgrade_candidate_context.base_slot(),
            U256::from(53)
        );
        assert_eq!(
            registry.upgrade_candidate_expiry.base_slot(),
            U256::from(54)
        );
        assert_eq!(
            registry.upgrade_candidate_source.base_slot(),
            U256::from(55)
        );
        assert_eq!(registry.upgrade_candidate_nonce.base_slot(), U256::from(57));
        let maximum = TeeRegistryGasScheduleV1::normative()
            .maximum_transaction_gas(
                RegistryMutatorV1::PrepareEnclaveUpgrade,
                bytes.len(),
                AttestationEvidenceV1::decode_canonical(
                    &ITeeRegistryV1::prepareEnclaveUpgradeCall::abi_decode(&bytes)
                        .unwrap()
                        .evidence,
                )
                .unwrap()
                .encode_canonical()
                .unwrap()
                .len(),
                1,
                AttestationMode::GramineDirectDev,
            )
            .unwrap();
        let intrinsic = TeeRegistryGasScheduleV1::normative()
            .maximum_calldata_intrinsic_gas(bytes.len())
            .unwrap();
        assert!(storage.gas_used().unwrap() + intrinsic <= maximum);
    });
    // Exact replay has no mutations.
    let prepared_state = provider.storage.clone();
    provider
        .enter(|s| crate::v1_precompile::dispatch(s, &bytes, owner.address(), U256::ZERO).unwrap());
    assert_eq!(prepared_state, provider.storage);
    let context_hash = provider.enter(|s| {
        TeeRegistry::new(s)
            .upgrade_candidate_context
            .read(&node)
            .unwrap()
    });
    provider.enter(|s| {
        assert!(TeeRegistry::new(s)
            .cancel_enclave_upgrade_v1(Address::repeat_byte(98), node, context_hash)
            .is_err())
    });
    provider.enter(|s| {
        TeeRegistry::new(s)
            .cancel_enclave_upgrade_v1(owner.address(), node, context_hash)
            .unwrap()
    });
    provider.enter(|s| {
        assert!(crate::v1_precompile::dispatch(s, &bytes, owner.address(), U256::ZERO).is_err())
    });
    prepare.transition_nonce = 2;
    provider.enter(|s| {
        crate::v1_precompile::dispatch(s, &prepare_call(&prepare), owner.address(), U256::ZERO)
            .unwrap()
    });

    let mut transition = prepare.clone();
    transition.operation = AttestationOperationV1::TransitionEnclaveMeasurement;
    transition.requested_valid_until = NOW + 14_400;
    transition.registration_version = old_binding.registration_version + 1;
    transition.renewal_nonce = old_binding.renewal_nonce;
    transition.transition_nonce = old_binding.transition_nonce + 1;
    let mut proof = TransitionKeyReadyProofV1 {
        chain_id: transition.chain_id,
        genesis_hash: transition.genesis_hash,
        transition_intent_hash: transition.intent_hash().unwrap(),
        candidate_manifest_hash: initialization_manifest_for_intent(&transition, [0xa7; 32])
            .authorization_hash()
            .unwrap(),
        transition_nonce: transition.transition_nonce,
        resident_offer_public: OFFER_PUBLIC,
        candidate_attestation_signature: [0; 64],
    };
    proof.candidate_attestation_signature = new
        .sign(proof.signing_hash().unwrap().as_slice())
        .to_bytes();
    let (node_sig, enclave_sig) = signatures(&transition, &owner, &new);
    let evidence = AttestationEvidenceV1::GramineDirectDev(GramineDirectEvidenceV1 {
        intent: transition.clone(),
        dev_attestation_public: new.verifying_key().to_bytes(),
        dev_signature: enclave_sig,
        transition_key_ready_proof: Some(proof),
    })
    .encode_canonical()
    .unwrap();
    // Candidate permission expires independently of both the old lease and the
    // final transition lease; a live old node cannot revive an expired candidate.
    let before_expiry_check = provider.storage.clone();
    provider.set_timestamp(U256::from(NOW + 7_200));
    provider.enter(|s| {
        assert!(TeeRegistry::new(s)
            .transition_enclave_measurement_with_staged_policy_v1(
                owner.address(),
                &evidence,
                &node_sig,
                &enclave_sig
            )
            .is_err())
    });
    assert_eq!(provider.storage, before_expiry_check);
    provider.set_timestamp(U256::from(NOW));
    let live_context = provider.enter(|s| {
        TeeRegistry::new(s)
            .upgrade_candidate_context
            .read(&node)
            .unwrap()
    });
    provider.enter(|s| {
        TeeRegistry::new(s)
            .cancel_enclave_upgrade_v1(owner.address(), node, live_context)
            .unwrap()
    });
    // Clearing pending state must not let a prepared binding use the legacy
    // no-pending transition route with its otherwise valid resident-key proof.
    provider.enter(|s| {
        assert!(TeeRegistry::new(s)
            .transition_enclave_measurement_with_staged_policy_v1(
                owner.address(),
                &evidence,
                &node_sig,
                &enclave_sig
            )
            .is_err())
    });
    prepare.transition_nonce = 3;
    provider.enter(|s| {
        crate::v1_precompile::dispatch(s, &prepare_call(&prepare), owner.address(), U256::ZERO)
            .unwrap()
    });
    provider.enter(|s| {
        let mut registry = TeeRegistry::new(s);
        registry
            .transition_enclave_measurement_with_staged_policy_v1(
                owner.address(),
                &evidence,
                &node_sig,
                &enclave_sig,
            )
            .unwrap();
        assert!(registry
            .upgrade_candidate_context
            .read(&node)
            .unwrap()
            .is_zero());
        assert_eq!(registry.upgrade_candidate_nonce.read(&node).unwrap(), 3);
        assert_eq!(
            registry
                .validator_enclave_binding_v1(owner.address())
                .unwrap()
                .unwrap()
                .enclave_id,
            transition.enclave_id
        );
    });
}
