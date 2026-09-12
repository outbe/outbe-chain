use super::*;

fn same_enclave_rejoin_intent(
    current: &RegistrationIntentV1,
    binding_seed: u8,
    requested_valid_until: u64,
) -> RegistrationIntentV1 {
    let mut intent = current.clone();
    intent.operation = AttestationOperationV1::RegisterEnclave;
    intent.binding_id = B256::repeat_byte(binding_seed);
    intent.binding_version += 1;
    intent.registration_version += 1;
    intent.requested_valid_until = requested_valid_until;
    intent
}

fn new_enclave_rejoin_intent(
    current: &RegistrationIntentV1,
    enclave_signer: &ed25519_dalek::SigningKey,
    binding_seed: u8,
    key_seed: u8,
    requested_valid_until: u64,
) -> RegistrationIntentV1 {
    let mut intent = replacement_intent(
        current,
        enclave_signer,
        binding_seed,
        key_seed,
        requested_valid_until,
    );
    intent.operation = AttestationOperationV1::RegisterEnclave;
    intent
}

#[test]
fn expired_same_enclave_rejoin_is_authorized_monotonic_and_idempotent() {
    let genesis_hash = B256::repeat_byte(0x32);
    let mut active_policy = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
    active_policy.minimum_lease = 3_600;
    active_policy.maximum_lease = 3_600;
    let owner = OutbeEvmSigner::from_secret_bytes([0x33; 32]).unwrap();
    let wrong = OutbeEvmSigner::from_secret_bytes([0x34; 32]).unwrap();
    let enclave = ed25519_dalek::SigningKey::from_bytes(&[0x35; 32]);
    let initial = registration_intent(&active_policy, &owner, CONSENSUS_KEY, &enclave, 0x36, 0x37);
    let rejoin = same_enclave_rejoin_intent(&initial, 0x38, NOW + 7_200);
    let (initial_node, initial_enclave) = signatures(&initial, &owner, &enclave);
    let (rejoin_node, rejoin_enclave) = signatures(&rejoin, &owner, &enclave);
    let (binding, validator_signature, node_binding_signature) =
        validator_node_binding_authorization_for_evm_node(&rejoin, &owner, &owner);
    let mut accepted = verdict(DcapPlatformTcbStatusV1::UpToDate);
    accepted.collateral_valid_until = NOW + 20_000;
    let mut provider = storage(genesis_hash);

    StorageHandle::enter(&mut provider, |storage| {
        let mut registry = TeeRegistry::new(storage.clone());
        registry.install_initial_policy_v1(&active_policy).unwrap();
        register_same_key_node_for_lifecycle_test(
            &mut registry,
            &initial,
            &owner,
            &initial_node,
            &initial_enclave,
            PostVerifierDcapCapabilityV1::new(accepted.clone()),
        )
        .unwrap();
        let node_hash = initial.node_id.node_id_hash().unwrap();
        storage
            .set_block_timestamp(U256::from(initial.requested_valid_until))
            .unwrap();

        assert!(revert_message(
            registry
                .register_enclave_and_bind_after_verifier_for_test_as(
                    wrong.address(),
                    &rejoin,
                    &rejoin_node,
                    &rejoin_enclave,
                    &binding,
                    &validator_signature,
                    &node_binding_signature,
                    PostVerifierDcapCapabilityV1::new(accepted.clone()),
                )
                .unwrap_err()
        )
        .contains("caller"));
        assert_eq!(
            registry
                .register_enclave_and_bind_after_verifier_for_test_as(
                    owner.address(),
                    &rejoin,
                    &rejoin_node,
                    &rejoin_enclave,
                    &binding,
                    &validator_signature,
                    &node_binding_signature,
                    PostVerifierDcapCapabilityV1::new(accepted.clone()),
                )
                .unwrap(),
            V1RegistrationOutcome::Created
        );
        let stored = registry
            .validator_enclave_binding_v1(owner.address())
            .unwrap()
            .unwrap();
        assert_eq!(stored.binding_id, rejoin.binding_id);
        assert_eq!(stored.binding_version, initial.binding_version + 1);
        assert_eq!(
            stored.registration_version,
            initial.registration_version + 1
        );
        assert_eq!(stored.renewal_nonce, initial.renewal_nonce);
        assert_eq!(stored.transition_nonce, initial.transition_nonce);
        assert_eq!(stored.valid_until, NOW + 7_200);
        assert_eq!(
            registry
                .validator_v1_node_hash
                .read(&owner.address())
                .unwrap(),
            node_hash
        );
        assert_eq!(
            registry
                .register_enclave_and_bind_after_verifier_for_test_as(
                    owner.address(),
                    &rejoin,
                    &rejoin_node,
                    &rejoin_enclave,
                    &binding,
                    &validator_signature,
                    &node_binding_signature,
                    PostVerifierDcapCapabilityV1::new(accepted),
                )
                .unwrap(),
            V1RegistrationOutcome::Idempotent
        );
    });
}

#[test]
fn expired_new_enclave_rejoin_preserves_historical_reverse_ownership() {
    let genesis_hash = B256::repeat_byte(0x39);
    let mut active_policy = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
    active_policy.minimum_lease = 3_600;
    active_policy.maximum_lease = 3_600;
    let owner = OutbeEvmSigner::from_secret_bytes([0x3A; 32]).unwrap();
    let initial_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x3B; 32]);
    let next_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x3C; 32]);
    let initial = registration_intent(
        &active_policy,
        &owner,
        CONSENSUS_KEY,
        &initial_enclave,
        0x3D,
        0x3E,
    );
    let rejoin = new_enclave_rejoin_intent(&initial, &next_enclave, 0x3F, 0x40, NOW + 7_200);
    let (initial_node, initial_enclave_signature) = signatures(&initial, &owner, &initial_enclave);
    let (rejoin_node, rejoin_enclave_signature) = signatures(&rejoin, &owner, &next_enclave);
    let (binding, validator_signature, node_binding_signature) =
        validator_node_binding_authorization_for_evm_node(&rejoin, &owner, &owner);
    let mut accepted = verdict(DcapPlatformTcbStatusV1::UpToDate);
    accepted.collateral_valid_until = NOW + 20_000;
    let mut provider = storage(genesis_hash);

    StorageHandle::enter(&mut provider, |storage| {
        let mut registry = TeeRegistry::new(storage.clone());
        registry.install_initial_policy_v1(&active_policy).unwrap();
        register_same_key_node_for_lifecycle_test(
            &mut registry,
            &initial,
            &owner,
            &initial_node,
            &initial_enclave_signature,
            PostVerifierDcapCapabilityV1::new(accepted.clone()),
        )
        .unwrap();
        let node_hash = initial.node_id.node_id_hash().unwrap();
        storage
            .set_block_timestamp(U256::from(initial.requested_valid_until))
            .unwrap();

        assert_eq!(
            registry
                .register_enclave_and_bind_after_verifier_for_test_as(
                    owner.address(),
                    &rejoin,
                    &rejoin_node,
                    &rejoin_enclave_signature,
                    &binding,
                    &validator_signature,
                    &node_binding_signature,
                    PostVerifierDcapCapabilityV1::new(accepted),
                )
                .unwrap(),
            V1RegistrationOutcome::Created
        );
        let stored = registry
            .validator_enclave_binding_v1(owner.address())
            .unwrap()
            .unwrap();
        assert_eq!(stored.enclave_id, rejoin.enclave_id);
        assert_eq!(stored.binding_id, rejoin.binding_id);
        assert_eq!(stored.binding_version, initial.binding_version + 1);
        assert_eq!(
            stored.registration_version,
            initial.registration_version + 1
        );
        assert_eq!(
            registry
                .v1_enclave_node_hash
                .read(&initial.enclave_id)
                .unwrap(),
            node_hash
        );
        assert_eq!(
            registry
                .v1_binding_node_hash
                .read(&initial.binding_id)
                .unwrap(),
            node_hash
        );
        assert_eq!(
            registry
                .v1_enclave_node_hash
                .read(&rejoin.enclave_id)
                .unwrap(),
            node_hash
        );
        assert_eq!(
            registry
                .v1_binding_node_hash
                .read(&rejoin.binding_id)
                .unwrap(),
            node_hash
        );
    });
}

#[test]
fn expired_new_enclave_rejoin_abi_fits_normative_register_gas() {
    let genesis_hash = B256::repeat_byte(0x52);
    let mut active_policy = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
    active_policy.minimum_lease = 3_600;
    active_policy.maximum_lease = 3_600;
    let owner = OutbeEvmSigner::from_secret_bytes([0x53; 32]).unwrap();
    let initial_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x54; 32]);
    let next_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x55; 32]);
    let initial = registration_intent(
        &active_policy,
        &owner,
        CONSENSUS_KEY,
        &initial_enclave,
        0x56,
        0x57,
    );
    let rejoin = new_enclave_rejoin_intent(&initial, &next_enclave, 0x58, 0x59, NOW + 7_200);
    let (initial_node, initial_enclave_signature) = signatures(&initial, &owner, &initial_enclave);
    let (rejoin_node, rejoin_enclave_signature) = signatures(&rejoin, &owner, &next_enclave);
    let (binding, validator_signature, node_binding_signature) =
        validator_node_binding_authorization_for_evm_node(&rejoin, &owner, &owner);
    let evidence = vec![0xD1; 4_096];
    let call = IRegisterEnclaveV1Test::registerEnclaveCall {
        evidence: evidence.clone().into(),
        nodeSignature: rejoin_node.to_vec().into(),
        enclaveSignature: rejoin_enclave_signature.to_vec().into(),
        validatorNodeBinding: binding.encode_canonical().unwrap().into(),
        validatorSignature: validator_signature.to_vec().into(),
        nodeBindingSignature: node_binding_signature.to_vec().into(),
    }
    .abi_encode();
    let mut accepted = verdict(DcapPlatformTcbStatusV1::UpToDate);
    accepted.collateral_valid_until = NOW + 20_000;
    let mut provider = storage(genesis_hash);
    StorageHandle::enter(&mut provider, |storage| {
        let mut registry = TeeRegistry::new(storage.clone());
        registry.install_initial_policy_v1(&active_policy).unwrap();
        register_same_key_node_for_lifecycle_test(
            &mut registry,
            &initial,
            &owner,
            &initial_node,
            &initial_enclave_signature,
            PostVerifierDcapCapabilityV1::new(accepted.clone()),
        )
        .unwrap();
        storage
            .set_block_timestamp(U256::from(initial.requested_valid_until))
            .unwrap();
    });
    provider.enable_production_storage_gas_metering();
    provider.set_gas_limit(u64::MAX);
    let outcome = StorageHandle::enter(&mut provider, |storage| {
        dispatch_register_after_verifier_for_test(
            storage,
            owner.address(),
            &call,
            &rejoin,
            PostVerifierDcapCapabilityV1::new(accepted),
        )
        .unwrap()
    });
    assert_eq!(outcome, V1RegistrationOutcome::Created);

    let schedule = TeeRegistryGasScheduleV1::normative();
    let allowance = schedule.register_storage_gas_allowance();
    let maximum = schedule
        .maximum_transaction_gas(
            RegistryMutatorV1::RegisterEnclave,
            call.len(),
            evidence.len(),
            active_policy.measurement_rules.len(),
            active_policy.attestation_mode,
        )
        .unwrap();
    let intrinsic = schedule.maximum_calldata_intrinsic_gas(call.len()).unwrap();
    let (reads, writes) = provider.metered_storage_operations();
    let storage_gas = reads * 100 + writes * 5_000;
    assert!(storage_gas <= allowance);
    assert_eq!(
        intrinsic + 200 + provider.gas_used(),
        maximum - allowance + storage_gas
    );
    assert!(intrinsic + 200 + provider.gas_used() <= maximum);
}

#[test]
fn expired_rejoin_fails_closed_on_corrupt_current_reverse_ownership() {
    let genesis_hash = B256::repeat_byte(0x5A);
    let mut active_policy = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
    active_policy.minimum_lease = 3_600;
    active_policy.maximum_lease = 3_600;
    let owner = OutbeEvmSigner::from_secret_bytes([0x5B; 32]).unwrap();
    let enclave = ed25519_dalek::SigningKey::from_bytes(&[0x5C; 32]);
    let initial = registration_intent(&active_policy, &owner, CONSENSUS_KEY, &enclave, 0x5D, 0x5E);
    let rejoin = same_enclave_rejoin_intent(&initial, 0x5F, NOW + 7_200);
    let (initial_node, initial_enclave) = signatures(&initial, &owner, &enclave);
    let (rejoin_node, rejoin_enclave) = signatures(&rejoin, &owner, &enclave);
    let (binding, validator_signature, node_binding_signature) =
        validator_node_binding_authorization_for_evm_node(&rejoin, &owner, &owner);
    let mut accepted = verdict(DcapPlatformTcbStatusV1::UpToDate);
    accepted.collateral_valid_until = NOW + 20_000;
    let mut provider = storage(genesis_hash);

    StorageHandle::enter(&mut provider, |storage| {
        let mut registry = TeeRegistry::new(storage.clone());
        registry.install_initial_policy_v1(&active_policy).unwrap();
        register_same_key_node_for_lifecycle_test(
            &mut registry,
            &initial,
            &owner,
            &initial_node,
            &initial_enclave,
            PostVerifierDcapCapabilityV1::new(accepted.clone()),
        )
        .unwrap();
        storage
            .set_block_timestamp(U256::from(initial.requested_valid_until))
            .unwrap();
        let before = registry
            .validator_enclave_binding_v1(owner.address())
            .unwrap()
            .unwrap();
        registry
            .v1_binding_node_hash
            .write(&initial.binding_id, B256::ZERO)
            .unwrap();

        assert!(matches!(
            registry
                .register_enclave_and_bind_after_verifier_for_test_as(
                    owner.address(),
                    &rejoin,
                    &rejoin_node,
                    &rejoin_enclave,
                    &binding,
                    &validator_signature,
                    &node_binding_signature,
                    PostVerifierDcapCapabilityV1::new(accepted),
                )
                .unwrap_err(),
            PrecompileError::Fatal(message) if message.contains("reverse ownership")
        ));
        assert_eq!(
            registry
                .validator_enclave_binding_v1(owner.address())
                .unwrap()
                .unwrap(),
            before
        );
    });
}

#[test]
fn expired_jailed_validator_must_unjail_before_rejoin() {
    let genesis_hash = B256::repeat_byte(0x41);
    let mut active_policy = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
    active_policy.minimum_lease = 3_600;
    active_policy.maximum_lease = 3_600;
    let owner = OutbeEvmSigner::from_secret_bytes([0x42; 32]).unwrap();
    let enclave = ed25519_dalek::SigningKey::from_bytes(&[0x43; 32]);
    let initial = registration_intent(&active_policy, &owner, CONSENSUS_KEY, &enclave, 0x44, 0x45);
    let rejoin = same_enclave_rejoin_intent(&initial, 0x46, NOW + 7_200);
    let (initial_node, initial_enclave) = signatures(&initial, &owner, &enclave);
    let (rejoin_node, rejoin_enclave) = signatures(&rejoin, &owner, &enclave);
    let (binding, validator_signature, node_binding_signature) =
        validator_node_binding_authorization_for_evm_node(&rejoin, &owner, &owner);
    let mut accepted = verdict(DcapPlatformTcbStatusV1::UpToDate);
    accepted.collateral_valid_until = NOW + 20_000;
    let mut provider = storage(genesis_hash);

    StorageHandle::enter(&mut provider, |storage| {
        register_validator(storage.clone(), &owner, CONSENSUS_KEY);
        let mut registry = TeeRegistry::new(storage.clone());
        registry.install_initial_policy_v1(&active_policy).unwrap();
        register_same_key_node_for_lifecycle_test(
            &mut registry,
            &initial,
            &owner,
            &initial_node,
            &initial_enclave,
            PostVerifierDcapCapabilityV1::new(accepted.clone()),
        )
        .unwrap();
        let mut validators = ValidatorSet::new(storage.clone());
        validators
            .activate_validator_via_boundary_for_test(owner.address())
            .unwrap();
        validators.jail_validator(owner.address()).unwrap();
        storage
            .set_block_timestamp(U256::from(initial.requested_valid_until))
            .unwrap();
        let before = registry
            .validator_enclave_binding_v1(owner.address())
            .unwrap()
            .unwrap();

        assert!(revert_message(
            registry
                .register_enclave_and_bind_after_verifier_for_test_as(
                    owner.address(),
                    &rejoin,
                    &rejoin_node,
                    &rejoin_enclave,
                    &binding,
                    &validator_signature,
                    &node_binding_signature,
                    PostVerifierDcapCapabilityV1::new(accepted),
                )
                .unwrap_err()
        )
        .contains("unjail"));
        assert_eq!(
            registry
                .validator_enclave_binding_v1(owner.address())
                .unwrap()
                .unwrap(),
            before
        );
    });
}

#[test]
fn expired_binding_rejects_replace_and_transition_without_state_change() {
    let genesis_hash = B256::repeat_byte(0x47);
    let current = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
    let mut successor = current.clone();
    successor.policy_version = 2;
    successor.activation_height = 50;
    successor.predecessor_policy_hash = current.policy_hash().unwrap();
    for rule in &mut successor.measurement_rules {
        rule.mrenclave = B256::repeat_byte(0x94);
        rule.admit_from_height = 50;
        rule.admit_until_height_exclusive = 500;
    }
    let owner = OutbeEvmSigner::from_secret_bytes([0x48; 32]).unwrap();
    let initial_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x49; 32]);
    let replacement_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x4A; 32]);
    let transition_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x4B; 32]);
    let initial = registration_intent(
        &current,
        &owner,
        CONSENSUS_KEY,
        &initial_enclave,
        0x4C,
        0x4D,
    );
    let replacement = replacement_intent(&initial, &replacement_enclave, 0x4E, 0x4F, NOW + 7_200);
    let transition = measurement_transition_intent(
        &initial,
        &successor,
        &transition_enclave,
        0x50,
        0x51,
        NOW + 7_200,
    );
    let (initial_node, initial_enclave_signature) = signatures(&initial, &owner, &initial_enclave);
    let (replacement_node, replacement_enclave_signature) =
        signatures(&replacement, &owner, &replacement_enclave);
    let (transition_node, transition_enclave_signature) =
        signatures(&transition, &owner, &transition_enclave);
    let mut accepted = verdict(DcapPlatformTcbStatusV1::UpToDate);
    accepted.collateral_valid_until = NOW + 20_000;
    let mut transition_verdict = accepted.clone();
    transition_verdict.mrenclave = B256::repeat_byte(0x94);
    let mut provider = storage(genesis_hash);

    StorageHandle::enter(&mut provider, |storage| {
        let mut registry = TeeRegistry::new(storage.clone());
        registry.install_initial_policy_v1(&current).unwrap();
        register_same_key_node_for_lifecycle_test(
            &mut registry,
            &initial,
            &owner,
            &initial_node,
            &initial_enclave_signature,
            PostVerifierDcapCapabilityV1::new(accepted.clone()),
        )
        .unwrap();
        registry
            .stage_successor_policy_v1(U256::from(11), &successor)
            .unwrap();
        storage
            .set_block_timestamp(U256::from(initial.requested_valid_until))
            .unwrap();
        let before = registry
            .validator_enclave_binding_v1(owner.address())
            .unwrap()
            .unwrap();

        assert!(revert_message(
            registry
                .replace_enclave_binding_after_verifier_with_active_policy_for_test(
                    owner.address(),
                    &replacement,
                    &replacement_node,
                    &replacement_enclave_signature,
                    &current,
                    PostVerifierDcapCapabilityV1::new(accepted),
                )
                .unwrap_err()
        )
        .contains("expired"));
        assert!(revert_message(
            registry
                .transition_enclave_measurement_after_verifier_for_test(
                    owner.address(),
                    &transition,
                    &transition_node,
                    &transition_enclave_signature,
                    PostVerifierDcapCapabilityV1::new(transition_verdict),
                )
                .unwrap_err()
        )
        .contains("expired"));
        assert_eq!(
            registry
                .validator_enclave_binding_v1(owner.address())
                .unwrap()
                .unwrap(),
            before
        );
    });
}
