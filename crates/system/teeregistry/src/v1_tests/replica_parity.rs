use super::*;

#[test]
fn proposer_validator_and_follower_apply_identical_full_state_verdict_and_gas() {
    let genesis_hash = B256::repeat_byte(0x18);
    let active_policy = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
    let node_signer = OutbeEvmSigner::from_secret_bytes([0x68; 32]).unwrap();
    let enclave_signer = ed25519_dalek::SigningKey::from_bytes(&[0x69; 32]);
    let intent = registration_intent(
        &active_policy,
        &node_signer,
        CONSENSUS_KEY,
        &enclave_signer,
        0x48,
        0x58,
    );
    let (node_signature, enclave_signature) = signatures(&intent, &node_signer, &enclave_signer);
    let (binding, validator_signature, node_binding_signature) =
        validator_node_binding_authorization_for_evm_node(&intent, &node_signer, &node_signer);
    let accepted_verdict = verdict(DcapPlatformTcbStatusV1::SWHardeningNeeded);
    let evidence = vec![0xA5; 4_096];
    let call = IRegisterEnclaveV1Test::registerEnclaveCall {
        evidence: evidence.clone().into(),
        nodeSignature: node_signature.to_vec().into(),
        enclaveSignature: enclave_signature.to_vec().into(),
        validatorNodeBinding: binding.encode_canonical().unwrap().into(),
        validatorSignature: validator_signature.to_vec().into(),
        nodeBindingSignature: node_binding_signature.to_vec().into(),
    }
    .abi_encode();
    let schedule = TeeRegistryGasScheduleV1::normative();
    let storage_allowance = schedule.register_storage_gas_allowance();
    let maximum = schedule
        .maximum_transaction_gas(
            RegistryMutatorV1::RegisterEnclave,
            call.len(),
            evidence.len(),
            active_policy.measurement_rules.len(),
            AttestationMode::DcapRequired,
        )
        .unwrap();
    let intrinsic = schedule.maximum_calldata_intrinsic_gas(call.len()).unwrap();

    let execute_replica = || {
        let mut provider = storage(genesis_hash);
        StorageHandle::enter(&mut provider, |storage| {
            register_validator(storage.clone(), &node_signer, CONSENSUS_KEY);
            TeeRegistry::new(storage)
                .install_initial_policy_v1(&active_policy)
                .unwrap();
        });
        provider.enable_production_storage_gas_metering();
        provider.set_gas_limit(u64::MAX);
        let outcome = StorageHandle::enter(&mut provider, |storage| {
            dispatch_register_after_verifier_for_test(
                storage,
                node_signer.address(),
                &call,
                &intent,
                PostVerifierDcapCapabilityV1::new(accepted_verdict.clone()),
            )
            .unwrap()
        });
        (provider, outcome)
    };

    let (proposer, proposer_outcome) = execute_replica();
    let (validator, validator_outcome) = execute_replica();
    let (follower, follower_outcome) = execute_replica();
    assert_eq!(proposer_outcome, V1RegistrationOutcome::Created);
    assert_eq!(validator_outcome, proposer_outcome);
    assert_eq!(follower_outcome, proposer_outcome);
    let expected_operations = proposer.metered_storage_operations();
    let (reads, writes) = expected_operations;
    assert!(reads > 0);
    assert_eq!(writes, 25, "fresh V1 binding storage schema drifted");
    let storage_gas = reads * 100 + writes * 5_000;
    assert!(storage_gas <= storage_allowance);
    assert_eq!(
        intrinsic + 200 + proposer.gas_used(),
        maximum - storage_allowance + storage_gas
    );
    assert!(intrinsic + 200 + proposer.gas_used() <= maximum);

    for replica in [&validator, &follower] {
        assert_eq!(replica.storage, proposer.storage);
        assert_eq!(replica.get_ordered_events(), proposer.get_ordered_events());
        assert_eq!(replica.metered_storage_operations(), expected_operations);
        assert_eq!(replica.gas_used(), proposer.gas_used());
    }
}

#[test]
fn full_node_proposer_validator_and_follower_apply_identical_abi_state_and_gas() {
    let genesis_hash = B256::repeat_byte(0x1A);
    let active_policy = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
    let node_signer = k256::ecdsa::SigningKey::from_bytes((&[0x7A; 32]).into()).unwrap();
    let enclave_signer = ed25519_dalek::SigningKey::from_bytes(&[0x7B; 32]);
    let intent =
        full_node_registration_intent(&active_policy, &node_signer, &enclave_signer, 0x4A, 0x5A);
    let (node_signature, enclave_signature) =
        full_node_signatures(&intent, &node_signer, &enclave_signer);
    let admission_signer = OutbeEvmSigner::from_secret_bytes([0x7C; 32]).unwrap();
    let (binding, validator_signature, node_binding_signature) =
        validator_node_binding_authorization_for_p2p_node(&intent, &admission_signer, &node_signer);
    let accepted_verdict = verdict(DcapPlatformTcbStatusV1::SWHardeningNeeded);
    let evidence = vec![0xA6; 4_096];
    let call = IRegisterEnclaveV1Test::registerEnclaveCall {
        evidence: evidence.clone().into(),
        nodeSignature: node_signature.to_vec().into(),
        enclaveSignature: enclave_signature.to_vec().into(),
        validatorNodeBinding: binding.encode_canonical().unwrap().into(),
        validatorSignature: validator_signature.to_vec().into(),
        nodeBindingSignature: node_binding_signature.to_vec().into(),
    }
    .abi_encode();
    let schedule = TeeRegistryGasScheduleV1::normative();
    let storage_allowance = schedule.register_storage_gas_allowance();
    let maximum = schedule
        .maximum_transaction_gas(
            RegistryMutatorV1::RegisterEnclave,
            call.len(),
            evidence.len(),
            active_policy.measurement_rules.len(),
            AttestationMode::DcapRequired,
        )
        .unwrap();
    let intrinsic = schedule.maximum_calldata_intrinsic_gas(call.len()).unwrap();

    let execute_replica = || {
        let mut provider = storage(genesis_hash);
        StorageHandle::enter(&mut provider, |storage| {
            TeeRegistry::new(storage)
                .install_initial_policy_v1(&active_policy)
                .unwrap();
        });
        provider.enable_production_storage_gas_metering();
        provider.set_gas_limit(u64::MAX);
        let outcome = StorageHandle::enter(&mut provider, |storage| {
            dispatch_register_after_verifier_for_test(
                storage,
                admission_signer.address(),
                &call,
                &intent,
                PostVerifierDcapCapabilityV1::new(accepted_verdict.clone()),
            )
            .unwrap()
        });
        (provider, outcome)
    };

    let (proposer, proposer_outcome) = execute_replica();
    let (validator, validator_outcome) = execute_replica();
    let (follower, follower_outcome) = execute_replica();
    assert_eq!(proposer_outcome, V1RegistrationOutcome::Created);
    assert_eq!(validator_outcome, proposer_outcome);
    assert_eq!(follower_outcome, proposer_outcome);
    let expected_operations = proposer.metered_storage_operations();
    let (reads, writes) = expected_operations;
    assert!(reads > 0);
    assert_eq!(
        writes, 25,
        "fresh FullNode V1 binding storage schema drifted"
    );
    let storage_gas = reads * 100 + writes * 5_000;
    assert!(storage_gas <= storage_allowance);
    assert_eq!(
        intrinsic + 200 + proposer.gas_used(),
        maximum - storage_allowance + storage_gas
    );
    assert!(intrinsic + 200 + proposer.gas_used() <= maximum);

    for replica in [&validator, &follower] {
        assert_eq!(replica.storage, proposer.storage);
        assert_eq!(replica.get_ordered_events(), proposer.get_ordered_events());
        assert_eq!(replica.metered_storage_operations(), expected_operations);
        assert_eq!(replica.gas_used(), proposer.gas_used());
    }
}
