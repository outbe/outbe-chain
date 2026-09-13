use super::*;

#[test]
fn initial_role_neutral_registration_atomically_records_the_address_association_without_a_role() {
    let genesis_hash = B256::repeat_byte(0xA1);
    let active_policy = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
    let validator_signer = OutbeEvmSigner::from_secret_bytes([0xA2; 32]).unwrap();
    let node_signer = k256::ecdsa::SigningKey::from_bytes((&[0xA3; 32]).into()).unwrap();
    let enclave_signer = ed25519_dalek::SigningKey::from_bytes(&[0xA4; 32]);
    let intent =
        full_node_registration_intent(&active_policy, &node_signer, &enclave_signer, 0xA5, 0xA6);
    let (node_registration_signature, enclave_signature) =
        full_node_signatures(&intent, &node_signer, &enclave_signer);
    let node_id_hash = intent.node_id.node_id_hash().unwrap();
    let (binding, validator_signature, node_binding_signature) =
        validator_node_binding_authorization_for_p2p_node(&intent, &validator_signer, &node_signer);
    let mut provider = storage(genesis_hash);

    StorageHandle::enter(&mut provider, |storage| {
        let mut registry = TeeRegistry::new(storage.clone());
        registry.install_initial_policy_v1(&active_policy).unwrap();
        assert_eq!(
            registry
                .register_enclave_and_bind_after_verifier_for_test(
                    &intent,
                    &node_registration_signature,
                    &enclave_signature,
                    &binding,
                    &validator_signature,
                    &node_binding_signature,
                    PostVerifierDcapCapabilityV1::new(verdict(DcapPlatformTcbStatusV1::UpToDate,)),
                )
                .unwrap(),
            V1RegistrationOutcome::Created
        );

        assert!(ValidatorSet::new(storage.clone())
            .get_validator(validator_signer.address())
            .unwrap()
            .is_none());
        assert!(!registry
            .is_validator_enclave_ready_v1(validator_signer.address())
            .unwrap());
        assert_eq!(
            registry
                .register_enclave_and_bind_after_verifier_for_test(
                    &intent,
                    &node_registration_signature,
                    &enclave_signature,
                    &binding,
                    &validator_signature,
                    &node_binding_signature,
                    PostVerifierDcapCapabilityV1::new(verdict(DcapPlatformTcbStatusV1::UpToDate,)),
                )
                .unwrap(),
            V1RegistrationOutcome::Idempotent
        );
        register_validator(storage, &validator_signer, CONSENSUS_KEY);
        assert!(registry
            .is_validator_enclave_ready_v1(validator_signer.address())
            .unwrap());
        assert_eq!(
            registry
                .validator_v1_node_hash
                .read(&validator_signer.address())
                .unwrap(),
            node_id_hash
        );
    });
}

#[test]
fn atomic_initial_registration_is_active_idempotent_and_expires_without_relay_authority() {
    let genesis_hash = B256::repeat_byte(0x11);
    let active_policy = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
    let node_signer = OutbeEvmSigner::from_secret_bytes([0x61; 32]).unwrap();
    let enclave_signer = ed25519_dalek::SigningKey::from_bytes(&[0x62; 32]);
    let intent = registration_intent(
        &active_policy,
        &node_signer,
        CONSENSUS_KEY,
        &enclave_signer,
        0x41,
        0x51,
    );
    let (node_signature, enclave_signature) = signatures(&intent, &node_signer, &enclave_signer);
    let (binding, validator_signature, node_binding_signature) =
        validator_node_binding_authorization_for_evm_node(&intent, &node_signer, &node_signer);
    let accepted_verdict = verdict(DcapPlatformTcbStatusV1::SWHardeningNeeded);
    let mut provider = storage(genesis_hash);

    StorageHandle::enter(&mut provider, |storage| {
        register_validator(storage.clone(), &node_signer, CONSENSUS_KEY);
        let mut registry = TeeRegistry::new(storage.clone());
        registry.install_initial_policy_v1(&active_policy).unwrap();
        assert!(!registry
            .is_validator_enclave_ready_v1(node_signer.address())
            .unwrap());

        assert_eq!(
            registry
                .register_enclave_and_bind_after_verifier_for_test(
                    &intent,
                    &node_signature,
                    &enclave_signature,
                    &binding,
                    &validator_signature,
                    &node_binding_signature,
                    PostVerifierDcapCapabilityV1::new(accepted_verdict.clone()),
                )
                .unwrap(),
            V1RegistrationOutcome::Created
        );
        assert!(registry
            .is_validator_enclave_ready_v1(node_signer.address())
            .unwrap());
        let stored_binding = registry
            .validator_enclave_binding_v1(node_signer.address())
            .unwrap()
            .unwrap();
        assert_eq!(stored_binding.enclave_id, intent.enclave_id);
        assert_eq!(stored_binding.binding_id, intent.binding_id);
        assert_eq!(stored_binding.intent_hash, intent.intent_hash().unwrap());
        assert_eq!(stored_binding.evidence_hash, B256::repeat_byte(0xEC));
        assert_eq!(stored_binding.valid_until, intent.requested_valid_until);
        assert_ne!(stored_binding.verdict_hash, B256::ZERO);

        assert_eq!(
            registry
                .register_enclave_and_bind_after_verifier_for_test(
                    &intent,
                    &node_signature,
                    &enclave_signature,
                    &binding,
                    &validator_signature,
                    &node_binding_signature,
                    PostVerifierDcapCapabilityV1::new(accepted_verdict.clone()),
                )
                .unwrap(),
            V1RegistrationOutcome::Idempotent
        );

        let conflict = registry
            .register_enclave_and_bind_after_verifier_for_test(
                &intent,
                &node_signature,
                &enclave_signature,
                &binding,
                &validator_signature,
                &node_binding_signature,
                PostVerifierDcapCapabilityV1::with_evidence_hash(
                    accepted_verdict,
                    B256::repeat_byte(0xED),
                ),
            )
            .unwrap_err();
        assert!(revert_message(conflict).contains("not an exact evidence replay"));

        storage
            .set_block_timestamp(U256::from(intent.requested_valid_until))
            .unwrap();
        assert!(!registry
            .is_validator_enclave_ready_v1(node_signer.address())
            .unwrap());
    });

    assert_eq!(
        provider
            .get_events(outbe_primitives::addresses::TEE_REGISTRY_ADDRESS)
            .len(),
        2,
        "initial registration emits node plus association exactly once"
    );
}

#[test]
fn bootstrap_fixture_registers_exactly_thirty_two_validators_after_private_verifier() {
    let genesis_hash = B256::repeat_byte(0x19);
    let active_policy = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
    let fixtures = (1_u8..=32)
        .map(|index| {
            let node_signer = OutbeEvmSigner::from_secret_bytes([index; 32]).unwrap();
            let enclave_signer =
                ed25519_dalek::SigningKey::from_bytes(&[index.wrapping_add(64); 32]);
            let consensus_key = [index; 48];
            let intent = registration_intent(
                &active_policy,
                &node_signer,
                consensus_key,
                &enclave_signer,
                index,
                index.wrapping_add(96),
            );
            let (node_signature, enclave_signature) =
                signatures(&intent, &node_signer, &enclave_signer);
            (
                node_signer,
                consensus_key,
                intent,
                node_signature,
                enclave_signature,
            )
        })
        .collect::<Vec<_>>();
    let mut provider = storage(genesis_hash);
    provider.set_block_number(1);

    StorageHandle::enter(&mut provider, |storage| {
        for (node_signer, consensus_key, ..) in &fixtures {
            register_validator(storage.clone(), node_signer, *consensus_key);
        }
        let mut registry = TeeRegistry::new(storage.clone());
        registry.install_initial_policy_v1(&active_policy).unwrap();

        let started = std::time::Instant::now();
        for (index, (node_signer, _, intent, node_signature, enclave_signature)) in
            fixtures.iter().enumerate()
        {
            assert_eq!(
                register_same_key_node_for_lifecycle_test(
                    &mut registry,
                    intent,
                    node_signer,
                    node_signature,
                    enclave_signature,
                    PostVerifierDcapCapabilityV1::with_evidence_hash(
                        verdict(DcapPlatformTcbStatusV1::UpToDate),
                        B256::repeat_byte(u8::try_from(index + 1).unwrap()),
                    ),
                )
                .unwrap(),
                V1RegistrationOutcome::Created
            );
            assert!(registry
                .is_validator_enclave_ready_v1(node_signer.address())
                .unwrap());
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "hardware-free post-verifier bootstrap fixture exceeded its test budget"
        );
    });
}

#[test]
fn invalid_or_conflicting_initial_association_rolls_back_the_node_registration() {
    let genesis_hash = B256::repeat_byte(0x21);
    let active_policy = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
    let admission_signer = OutbeEvmSigner::from_secret_bytes([0x22; 32]).unwrap();
    let first_node = k256::ecdsa::SigningKey::from_bytes((&[0x23; 32]).into()).unwrap();
    let second_node = k256::ecdsa::SigningKey::from_bytes((&[0x24; 32]).into()).unwrap();
    let first_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x25; 32]);
    let second_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x26; 32]);
    let first_intent =
        full_node_registration_intent(&active_policy, &first_node, &first_enclave, 0x27, 0x28);
    let second_intent =
        full_node_registration_intent(&active_policy, &second_node, &second_enclave, 0x29, 0x2A);
    let (first_node_signature, first_enclave_signature) =
        full_node_signatures(&first_intent, &first_node, &first_enclave);
    let (second_node_signature, second_enclave_signature) =
        full_node_signatures(&second_intent, &second_node, &second_enclave);
    let (first_binding, first_validator_signature, first_binding_node_signature) =
        validator_node_binding_authorization_for_p2p_node(
            &first_intent,
            &admission_signer,
            &first_node,
        );
    let (second_binding, second_validator_signature, second_binding_node_signature) =
        validator_node_binding_authorization_for_p2p_node(
            &second_intent,
            &admission_signer,
            &second_node,
        );
    let second_admission_signer = OutbeEvmSigner::from_secret_bytes([0x2B; 32]).unwrap();
    let (
        second_node_own_binding,
        second_node_own_validator_signature,
        second_node_own_binding_signature,
    ) = validator_node_binding_authorization_for_p2p_node(
        &second_intent,
        &second_admission_signer,
        &second_node,
    );
    let first_node_hash = first_intent.node_id.node_id_hash().unwrap();
    let second_node_hash = second_intent.node_id.node_id_hash().unwrap();
    let mut provider = storage(genesis_hash);

    StorageHandle::enter(&mut provider, |storage| {
        let mut registry = TeeRegistry::new(storage);
        registry.install_initial_policy_v1(&active_policy).unwrap();

        let mut invalid_validator_signature = first_validator_signature;
        invalid_validator_signature[0] ^= 1;
        let invalid = registry
            .register_enclave_and_bind_after_verifier_for_test(
                &first_intent,
                &first_node_signature,
                &first_enclave_signature,
                &first_binding,
                &invalid_validator_signature,
                &first_binding_node_signature,
                PostVerifierDcapCapabilityV1::new(verdict(DcapPlatformTcbStatusV1::UpToDate)),
            )
            .unwrap_err();
        assert!(revert_message(invalid).contains("proof of possession"));
        assert!(registry
            .node_host_enclave_binding_v1(first_intent.node_id.reth_p2p_public)
            .unwrap()
            .is_none());
        assert_eq!(
            registry
                .validator_v1_node_hash
                .read(&admission_signer.address())
                .unwrap(),
            B256::ZERO
        );

        registry
            .register_enclave_and_bind_after_verifier_for_test(
                &second_intent,
                &second_node_signature,
                &second_enclave_signature,
                &second_node_own_binding,
                &second_node_own_validator_signature,
                &second_node_own_binding_signature,
                PostVerifierDcapCapabilityV1::new(verdict(DcapPlatformTcbStatusV1::UpToDate)),
            )
            .unwrap();
        let mismatched_target = registry
            .register_enclave_and_bind_after_verifier_for_test(
                &first_intent,
                &first_node_signature,
                &first_enclave_signature,
                &second_binding,
                &second_validator_signature,
                &second_binding_node_signature,
                PostVerifierDcapCapabilityV1::new(verdict(DcapPlatformTcbStatusV1::UpToDate)),
            )
            .unwrap_err();
        assert!(revert_message(mismatched_target).contains("same NodeHost"));
        assert!(registry
            .node_host_enclave_binding_v1(first_intent.node_id.reth_p2p_public)
            .unwrap()
            .is_none());
        assert_eq!(
            registry
                .validator_v1_node_hash
                .read(&admission_signer.address())
                .unwrap(),
            B256::ZERO
        );

        registry
            .register_enclave_and_bind_after_verifier_for_test(
                &first_intent,
                &first_node_signature,
                &first_enclave_signature,
                &first_binding,
                &first_validator_signature,
                &first_binding_node_signature,
                PostVerifierDcapCapabilityV1::new(verdict(DcapPlatformTcbStatusV1::UpToDate)),
            )
            .unwrap();
        let conflict = registry
            .register_enclave_and_bind_after_verifier_for_test(
                &second_intent,
                &second_node_signature,
                &second_enclave_signature,
                &second_binding,
                &second_validator_signature,
                &second_binding_node_signature,
                PostVerifierDcapCapabilityV1::new(verdict(DcapPlatformTcbStatusV1::UpToDate)),
            )
            .unwrap_err();
        assert!(revert_message(conflict).contains("not associated with the existing NodeHost"));
        assert_eq!(
            registry
                .validator_v1_node_hash
                .read(&admission_signer.address())
                .unwrap(),
            first_node_hash
        );
        assert_ne!(first_node_hash, second_node_hash);
        assert!(registry
            .node_host_enclave_binding_v1(second_intent.node_id.reth_p2p_public)
            .unwrap()
            .is_some());
        assert_eq!(
            registry
                .validator_v1_node_hash
                .read(&second_admission_signer.address())
                .unwrap(),
            second_node_hash
        );
    });
}

#[test]
fn full_node_binding_is_idempotent_expires_and_rejects_validator_credentials() {
    let genesis_hash = B256::repeat_byte(0x19);
    let active_policy = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
    let node_signer = k256::ecdsa::SigningKey::from_bytes((&[0x6A; 32]).into()).unwrap();
    let other_node = k256::ecdsa::SigningKey::from_bytes((&[0x6C; 32]).into()).unwrap();
    let validator_signer = OutbeEvmSigner::from_secret_bytes([0x6D; 32]).unwrap();
    let enclave_signer = ed25519_dalek::SigningKey::from_bytes(&[0x6B; 32]);
    let other_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x6E; 32]);
    let intent =
        full_node_registration_intent(&active_policy, &node_signer, &enclave_signer, 0x49, 0x59);
    let reth_p2p_public = full_node_public(&intent);
    let (node_signature, enclave_signature) =
        full_node_signatures(&intent, &node_signer, &enclave_signer);
    let mut provider = storage(genesis_hash);

    StorageHandle::enter(&mut provider, |storage| {
        let mut registry = TeeRegistry::new(storage.clone());
        registry.install_initial_policy_v1(&active_policy).unwrap();
        assert!(!registry
            .is_node_host_enclave_ready_v1(reth_p2p_public)
            .unwrap());
        assert_eq!(
            registry
                .register_enclave_after_verifier_for_test(
                    &intent,
                    &node_signature,
                    &enclave_signature,
                    PostVerifierDcapCapabilityV1::new(verdict(
                        DcapPlatformTcbStatusV1::SWHardeningNeeded,
                    )),
                )
                .unwrap(),
            V1RegistrationOutcome::Created
        );
        assert!(registry
            .is_node_host_enclave_ready_v1(reth_p2p_public)
            .unwrap());
        let binding = registry
            .node_host_enclave_binding_v1(reth_p2p_public)
            .unwrap()
            .unwrap();
        assert_eq!(binding.enclave_id, intent.enclave_id);
        assert_eq!(binding.intent_hash, intent.intent_hash().unwrap());

        assert_eq!(
            registry
                .register_enclave_after_verifier_for_test(
                    &intent,
                    &node_signature,
                    &enclave_signature,
                    PostVerifierDcapCapabilityV1::new(verdict(
                        DcapPlatformTcbStatusV1::SWHardeningNeeded,
                    )),
                )
                .unwrap(),
            V1RegistrationOutcome::Idempotent
        );

        let (wrong_p2p_signature, _) = full_node_signatures(&intent, &other_node, &enclave_signer);
        assert!(revert_message(
            registry
                .register_enclave_after_verifier_for_test(
                    &intent,
                    &wrong_p2p_signature,
                    &enclave_signature,
                    PostVerifierDcapCapabilityV1::new(verdict(DcapPlatformTcbStatusV1::UpToDate)),
                )
                .unwrap_err()
        )
        .contains("node proof"));

        let validator_signature = validator_signer
            .sign_hash(&intent.intent_hash().unwrap())
            .unwrap();
        assert!(revert_message(
            registry
                .register_enclave_after_verifier_for_test(
                    &intent,
                    &validator_signature,
                    &enclave_signature,
                    PostVerifierDcapCapabilityV1::new(verdict(DcapPlatformTcbStatusV1::UpToDate)),
                )
                .unwrap_err()
        )
        .contains("node proof"));

        let wrong_enclave_signature = other_enclave
            .sign(intent.intent_hash().unwrap().as_slice())
            .to_bytes();
        assert!(revert_message(
            registry
                .register_enclave_after_verifier_for_test(
                    &intent,
                    &node_signature,
                    &wrong_enclave_signature,
                    PostVerifierDcapCapabilityV1::new(verdict(DcapPlatformTcbStatusV1::UpToDate)),
                )
                .unwrap_err()
        )
        .contains("enclave proof"));

        let mut stale = intent.clone();
        stale.renewal_nonce = 1;
        let (stale_node_signature, stale_enclave_signature) =
            full_node_signatures(&stale, &node_signer, &enclave_signer);
        assert!(revert_message(
            registry
                .register_enclave_after_verifier_for_test(
                    &stale,
                    &stale_node_signature,
                    &stale_enclave_signature,
                    PostVerifierDcapCapabilityV1::new(verdict(DcapPlatformTcbStatusV1::UpToDate)),
                )
                .unwrap_err()
        )
        .contains("must renew"));

        let mut wrong_measurement = verdict(DcapPlatformTcbStatusV1::UpToDate);
        wrong_measurement.mrenclave = B256::repeat_byte(0x99);
        assert!(revert_message(
            registry
                .register_enclave_after_verifier_for_test(
                    &intent,
                    &node_signature,
                    &enclave_signature,
                    PostVerifierDcapCapabilityV1::new(wrong_measurement),
                )
                .unwrap_err()
        )
        .contains("measurement rule"));

        let mut excessive_lease = intent.clone();
        excessive_lease.requested_valid_until = NOW + active_policy.maximum_lease + 1;
        let (lease_node_signature, lease_enclave_signature) =
            full_node_signatures(&excessive_lease, &node_signer, &enclave_signer);
        assert!(revert_message(
            registry
                .register_enclave_after_verifier_for_test(
                    &excessive_lease,
                    &lease_node_signature,
                    &lease_enclave_signature,
                    PostVerifierDcapCapabilityV1::new(verdict(DcapPlatformTcbStatusV1::UpToDate)),
                )
                .unwrap_err()
        )
        .contains("must renew"));

        let same_enclave_other_node =
            full_node_registration_intent(&active_policy, &other_node, &enclave_signer, 0x4B, 0x59);
        assert_eq!(same_enclave_other_node.enclave_id, intent.enclave_id);
        let (other_node_signature, same_enclave_signature) =
            full_node_signatures(&same_enclave_other_node, &other_node, &enclave_signer);
        assert!(revert_message(
            registry
                .register_enclave_after_verifier_for_test(
                    &same_enclave_other_node,
                    &other_node_signature,
                    &same_enclave_signature,
                    PostVerifierDcapCapabilityV1::new(verdict(DcapPlatformTcbStatusV1::UpToDate)),
                )
                .unwrap_err()
        )
        .contains("already bound to another node"));

        let conflict = registry
            .register_enclave_after_verifier_for_test(
                &intent,
                &node_signature,
                &enclave_signature,
                PostVerifierDcapCapabilityV1::with_evidence_hash(
                    verdict(DcapPlatformTcbStatusV1::UpToDate),
                    B256::repeat_byte(0xED),
                ),
            )
            .unwrap_err();
        assert!(revert_message(conflict).contains("not an exact evidence replay"));

        storage
            .set_block_timestamp(U256::from(intent.requested_valid_until))
            .unwrap();
        assert!(!registry
            .is_node_host_enclave_ready_v1(reth_p2p_public)
            .unwrap());
    });

    assert_eq!(
        provider
            .get_events(outbe_primitives::addresses::TEE_REGISTRY_ADDRESS)
            .len(),
        1
    );
}

#[test]
fn role_neutral_registration_rejects_node_enclave_nonce_and_measurement_errors() {
    let genesis_hash = B256::repeat_byte(0x12);
    let active_policy = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
    let node_signer = OutbeEvmSigner::from_secret_bytes([0x63; 32]).unwrap();
    let other_node = OutbeEvmSigner::from_secret_bytes([0x64; 32]).unwrap();
    let enclave_signer = ed25519_dalek::SigningKey::from_bytes(&[0x65; 32]);
    let other_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x66; 32]);
    let intent = registration_intent(
        &active_policy,
        &node_signer,
        CONSENSUS_KEY,
        &enclave_signer,
        0x42,
        0x52,
    );
    let (node_signature, enclave_signature) = signatures(&intent, &node_signer, &enclave_signer);
    let mut provider = storage(genesis_hash);

    StorageHandle::enter(&mut provider, |storage| {
        register_validator(storage.clone(), &node_signer, CONSENSUS_KEY);
        let mut registry = TeeRegistry::new(storage);
        registry.install_initial_policy_v1(&active_policy).unwrap();

        let wrong_node_signature = other_node
            .sign_hash(&intent.intent_hash().unwrap())
            .unwrap();
        assert!(revert_message(
            registry
                .register_enclave_after_verifier_for_test(
                    &intent,
                    &wrong_node_signature,
                    &enclave_signature,
                    PostVerifierDcapCapabilityV1::new(verdict(DcapPlatformTcbStatusV1::UpToDate,)),
                )
                .unwrap_err()
        )
        .contains("node proof"));

        let full_node_signer = k256::ecdsa::SigningKey::from_bytes((&[0x6E; 32]).into()).unwrap();
        let (full_node_signature, _) =
            full_node_signatures(&intent, &full_node_signer, &enclave_signer);
        assert!(revert_message(
            registry
                .register_enclave_after_verifier_for_test(
                    &intent,
                    &full_node_signature,
                    &enclave_signature,
                    PostVerifierDcapCapabilityV1::new(verdict(DcapPlatformTcbStatusV1::UpToDate)),
                )
                .unwrap_err()
        )
        .contains("node proof"));

        let wrong_enclave_signature = other_enclave
            .sign(intent.intent_hash().unwrap().as_slice())
            .to_bytes();
        assert!(revert_message(
            registry
                .register_enclave_after_verifier_for_test(
                    &intent,
                    &node_signature,
                    &wrong_enclave_signature,
                    PostVerifierDcapCapabilityV1::new(verdict(DcapPlatformTcbStatusV1::UpToDate,)),
                )
                .unwrap_err()
        )
        .contains("enclave proof"));

        let mut stale = intent.clone();
        stale.renewal_nonce = 1;
        let (stale_node_signature, stale_enclave_signature) =
            signatures(&stale, &node_signer, &enclave_signer);
        assert!(revert_message(
            registry
                .register_enclave_after_verifier_for_test(
                    &stale,
                    &stale_node_signature,
                    &stale_enclave_signature,
                    PostVerifierDcapCapabilityV1::new(verdict(DcapPlatformTcbStatusV1::UpToDate,)),
                )
                .unwrap_err()
        )
        .contains("versions and nonces"));

        let mut wrong_measurement = verdict(DcapPlatformTcbStatusV1::UpToDate);
        wrong_measurement.mrenclave = B256::repeat_byte(0x99);
        assert!(revert_message(
            registry
                .register_enclave_after_verifier_for_test(
                    &intent,
                    &node_signature,
                    &enclave_signature,
                    PostVerifierDcapCapabilityV1::new(wrong_measurement),
                )
                .unwrap_err()
        )
        .contains("measurement rule"));

        assert!(registry
            .validator_enclave_binding_v1(node_signer.address())
            .unwrap()
            .is_none());
    });
}

#[test]
fn one_to_one_binding_and_strict_platform_policy_reject_conflicts() {
    let genesis_hash = B256::repeat_byte(0x13);
    let broad_policy = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
    let first_node = OutbeEvmSigner::from_secret_bytes([0x67; 32]).unwrap();
    let second_node = OutbeEvmSigner::from_secret_bytes([0x68; 32]).unwrap();
    let enclave_signer = ed25519_dalek::SigningKey::from_bytes(&[0x69; 32]);
    let replacement_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x6a; 32]);
    let first = registration_intent(
        &broad_policy,
        &first_node,
        CONSENSUS_KEY,
        &enclave_signer,
        0x44,
        0x54,
    );
    let (first_node_signature, first_enclave_signature) =
        signatures(&first, &first_node, &enclave_signer);
    let mut provider = storage(genesis_hash);

    StorageHandle::enter(&mut provider, |storage| {
        register_validator(storage.clone(), &first_node, CONSENSUS_KEY);
        register_validator(storage.clone(), &second_node, [0x34; 48]);
        let mut registry = TeeRegistry::new(storage);
        registry.install_initial_policy_v1(&broad_policy).unwrap();
        registry
            .register_enclave_after_verifier_for_test(
                &first,
                &first_node_signature,
                &first_enclave_signature,
                PostVerifierDcapCapabilityV1::new(verdict(DcapPlatformTcbStatusV1::UpToDate)),
            )
            .unwrap();

        let second_enclave = registration_intent(
            &broad_policy,
            &first_node,
            CONSENSUS_KEY,
            &replacement_enclave,
            0x45,
            0x55,
        );
        let (second_enclave_node_sig, second_enclave_sig) =
            signatures(&second_enclave, &first_node, &replacement_enclave);
        assert!(revert_message(
            registry
                .register_enclave_after_verifier_for_test(
                    &second_enclave,
                    &second_enclave_node_sig,
                    &second_enclave_sig,
                    PostVerifierDcapCapabilityV1::new(verdict(DcapPlatformTcbStatusV1::UpToDate,)),
                )
                .unwrap_err()
        )
        .contains("must renew"));

        let mut same_enclave_other_node = registration_intent(
            &broad_policy,
            &second_node,
            [0x34; 48],
            &enclave_signer,
            0x46,
            0x54,
        );
        same_enclave_other_node.recipient_x25519 = first.recipient_x25519;
        same_enclave_other_node.noise_responder_x25519 = first.noise_responder_x25519;
        same_enclave_other_node.node_host_authorization_hash = first.node_host_authorization_hash;
        same_enclave_other_node.enclave_id = same_enclave_other_node.derived_enclave_id().unwrap();
        assert_eq!(same_enclave_other_node.enclave_id, first.enclave_id);
        let (second_node_signature, same_enclave_signature) =
            signatures(&same_enclave_other_node, &second_node, &enclave_signer);
        assert!(revert_message(
            registry
                .register_enclave_after_verifier_for_test(
                    &same_enclave_other_node,
                    &second_node_signature,
                    &same_enclave_signature,
                    PostVerifierDcapCapabilityV1::new(verdict(DcapPlatformTcbStatusV1::UpToDate,)),
                )
                .unwrap_err()
        )
        .contains("already bound to another node"));
    });

    let strict_policy = policy(genesis_hash, PlatformTcbStatusSetV1::UpToDateOnly);
    let strict_node = OutbeEvmSigner::from_secret_bytes([0x6b; 32]).unwrap();
    let strict_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x6c; 32]);
    let strict_intent = registration_intent(
        &strict_policy,
        &strict_node,
        CONSENSUS_KEY,
        &strict_enclave,
        0x47,
        0x57,
    );
    let (strict_node_signature, strict_enclave_signature) =
        signatures(&strict_intent, &strict_node, &strict_enclave);
    let mut strict_provider = storage(genesis_hash);
    StorageHandle::enter(&mut strict_provider, |storage| {
        register_validator(storage.clone(), &strict_node, CONSENSUS_KEY);
        let mut registry = TeeRegistry::new(storage);
        registry.install_initial_policy_v1(&strict_policy).unwrap();
        for status in [
            DcapPlatformTcbStatusV1::SWHardeningNeeded,
            DcapPlatformTcbStatusV1::ConfigurationAndSWHardeningNeeded,
        ] {
            assert!(revert_message(
                registry
                    .register_enclave_after_verifier_for_test(
                        &strict_intent,
                        &strict_node_signature,
                        &strict_enclave_signature,
                        PostVerifierDcapCapabilityV1::new(verdict(status)),
                    )
                    .unwrap_err()
            )
            .contains("stricter than active policy"));
        }
    });
}
