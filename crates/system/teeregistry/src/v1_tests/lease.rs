use super::fixtures::MRSIGNER;
use super::*;

#[test]
fn full_node_renewal_and_replacement_follow_the_shared_lease_lifecycle() {
    assert_full_node_lease_lifecycle(MRSIGNER, MRSIGNER);
}

#[test]
fn independently_signed_enclaves_register_renew_and_replace_under_the_same_policy() {
    assert_full_node_lease_lifecycle(B256::repeat_byte(0x91), B256::repeat_byte(0x92));
    assert_full_node_lease_lifecycle(B256::repeat_byte(0x93), B256::repeat_byte(0x94));
}

fn assert_full_node_lease_lifecycle(initial_signer: B256, replacement_signer: B256) {
    let genesis_hash = B256::repeat_byte(0x25);
    let mut active_policy = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
    active_policy.maximum_lease = 3_600;
    if initial_signer != MRSIGNER || replacement_signer != MRSIGNER {
        active_policy.measurement_rules[0].mrsigner = B256::ZERO;
    }
    let node_signer = k256::ecdsa::SigningKey::from_bytes((&[0x7B; 32]).into()).unwrap();
    let old_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x7C; 32]);
    let new_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x7D; 32]);
    let initial =
        full_node_registration_intent(&active_policy, &node_signer, &old_enclave, 0x6D, 0x6E);
    let renewal = renewal_intent(
        &initial,
        initial.requested_valid_until + active_policy.maximum_lease,
    );
    let replacement = replacement_intent(&renewal, &new_enclave, 0x6F, 0x70, NOW + 6_000);
    let (initial_node, initial_enclave) =
        full_node_signatures(&initial, &node_signer, &old_enclave);
    let (renewal_node, renewal_enclave) =
        full_node_signatures(&renewal, &node_signer, &old_enclave);
    let (replacement_node, replacement_enclave) =
        full_node_signatures(&replacement, &node_signer, &new_enclave);
    let p2p_public = full_node_public(&initial);
    let mut accepted = verdict(DcapPlatformTcbStatusV1::UpToDate);
    accepted.mrsigner = initial_signer;
    accepted.collateral_valid_until = NOW + 12_000;
    let mut provider = storage(genesis_hash);

    StorageHandle::enter(&mut provider, |storage| {
        let mut registry = TeeRegistry::new(storage.clone());
        registry.install_initial_policy_v1(&active_policy).unwrap();
        registry
            .register_enclave_after_verifier_for_test(
                &initial,
                &initial_node,
                &initial_enclave,
                PostVerifierDcapCapabilityV1::new(accepted.clone()),
            )
            .unwrap();
        assert_eq!(
            registry
                .node_host_enclave_binding_v1(p2p_public)
                .unwrap()
                .unwrap()
                .mrsigner,
            initial_signer
        );
        storage
            .set_block_timestamp(U256::from(NOW + 2_400))
            .unwrap();
        registry
            .renew_enclave_after_verifier_for_test(
                &renewal,
                &renewal_node,
                &renewal_enclave,
                PostVerifierDcapCapabilityV1::new(accepted.clone()),
            )
            .unwrap();
        // Renewal preserves this instance's identity; replacement admits a
        // fresh instance signed by another operator under the same policy.
        accepted.mrsigner = replacement_signer;
        registry
            .replace_enclave_binding_after_verifier_for_test(
                &replacement,
                &replacement_node,
                &replacement_enclave,
                PostVerifierDcapCapabilityV1::new(accepted),
            )
            .unwrap();

        let binding = registry
            .node_host_enclave_binding_v1(p2p_public)
            .unwrap()
            .unwrap();
        assert_eq!(binding.enclave_id, replacement.enclave_id);
        assert_eq!(binding.mrsigner, replacement_signer);
        assert_eq!(binding.binding_version, 2);
        assert_eq!(binding.registration_version, 2);
        assert_eq!(binding.renewal_nonce, 1);
        assert!(registry.is_node_host_enclave_ready_v1(p2p_public).unwrap());
    });
}

#[test]
fn an_existing_lease_is_not_retroactively_filtered_by_the_policy_anchor() {
    let genesis_hash = B256::repeat_byte(0x26);
    let mut active_policy = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
    active_policy.maximum_lease = 3_600;
    let node_signer = OutbeEvmSigner::from_secret_bytes([0x7E; 32]).unwrap();
    let enclave_signer = ed25519_dalek::SigningKey::from_bytes(&[0x7F; 32]);
    let intent = registration_intent(
        &active_policy,
        &node_signer,
        CONSENSUS_KEY,
        &enclave_signer,
        0x71,
        0x72,
    );
    let (node_signature, enclave_signature) = signatures(&intent, &node_signer, &enclave_signer);
    let mut provider = storage(genesis_hash);

    StorageHandle::enter(&mut provider, |storage| {
        register_validator(storage.clone(), &node_signer, CONSENSUS_KEY);
        let mut registry = TeeRegistry::new(storage);
        registry.install_initial_policy_v1(&active_policy).unwrap();
        register_same_key_node_for_lifecycle_test(
            &mut registry,
            &intent,
            &node_signer,
            &node_signature,
            &enclave_signature,
            PostVerifierDcapCapabilityV1::new(verdict(DcapPlatformTcbStatusV1::UpToDate)),
        )
        .unwrap();
        let admitted_policy_hash = active_policy.policy_hash().unwrap();
        registry
            .active_v1_policy_hash
            .write(B256::repeat_byte(0xEE))
            .unwrap();

        assert!(registry
            .is_validator_enclave_ready_v1(node_signer.address())
            .unwrap());
        assert_eq!(
            registry
                .validator_enclave_binding_v1(node_signer.address())
                .unwrap()
                .unwrap()
                .policy_hash,
            admitted_policy_hash
        );
    });
}

#[test]
fn renewal_window_is_half_open_extends_from_deadline_and_does_not_drift() {
    let genesis_hash = B256::repeat_byte(0x21);
    let mut active_policy = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
    active_policy.minimum_lease = 3_600;
    active_policy.maximum_lease = 3_600;
    let node_signer = OutbeEvmSigner::from_secret_bytes([0x71; 32]).unwrap();
    let enclave_signer = ed25519_dalek::SigningKey::from_bytes(&[0x72; 32]);
    let initial = registration_intent(
        &active_policy,
        &node_signer,
        CONSENSUS_KEY,
        &enclave_signer,
        0x61,
        0x62,
    );
    let (initial_node, initial_enclave) = signatures(&initial, &node_signer, &enclave_signer);
    let renewal = renewal_intent(&initial, initial.requested_valid_until + 3_600);
    let (renewal_node, renewal_enclave) = signatures(&renewal, &node_signer, &enclave_signer);
    let mut fresh_verdict = verdict(DcapPlatformTcbStatusV1::UpToDate);
    fresh_verdict.collateral_valid_until = NOW + 20_000;
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
            PostVerifierDcapCapabilityV1::new(fresh_verdict.clone()),
        )
        .unwrap();

        storage
            .set_block_timestamp(U256::from(NOW + 1_799))
            .unwrap();
        assert!(revert_message(
            registry
                .renew_enclave_after_verifier_for_test(
                    &renewal,
                    &renewal_node,
                    &renewal_enclave,
                    PostVerifierDcapCapabilityV1::new(fresh_verdict.clone()),
                )
                .unwrap_err()
        )
        .contains("renewal window"));

        storage
            .set_block_timestamp(U256::from(NOW + 1_800))
            .unwrap();
        assert_eq!(
            registry
                .renew_enclave_after_verifier_for_test(
                    &renewal,
                    &renewal_node,
                    &renewal_enclave,
                    PostVerifierDcapCapabilityV1::new(fresh_verdict.clone()),
                )
                .unwrap(),
            V1RegistrationOutcome::Created
        );
        assert_eq!(
            registry
                .renew_enclave_after_verifier_for_test(
                    &renewal,
                    &renewal_node,
                    &renewal_enclave,
                    PostVerifierDcapCapabilityV1::new(fresh_verdict.clone()),
                )
                .unwrap(),
            V1RegistrationOutcome::Idempotent
        );
        assert!(revert_message(
            registry
                .renew_enclave_after_verifier_for_test(
                    &renewal,
                    &renewal_node,
                    &renewal_enclave,
                    PostVerifierDcapCapabilityV1::with_evidence_hash(
                        fresh_verdict.clone(),
                        B256::repeat_byte(0xED),
                    ),
                )
                .unwrap_err()
        )
        .contains("exact evidence replay"));
        let binding = registry
            .validator_enclave_binding_v1(node_signer.address())
            .unwrap()
            .unwrap();
        assert_eq!(binding.registration_version, 1);
        assert_eq!(binding.renewal_nonce, 1);
        assert_eq!(binding.lease_started_at, NOW + 1_800);
        assert_eq!(
            binding.valid_until,
            initial.requested_valid_until + active_policy.maximum_lease
        );

        let mut stale = renewal.clone();
        stale.registration_version = 0;
        stale.renewal_nonce = 0;
        let (stale_node, stale_enclave) = signatures(&stale, &node_signer, &enclave_signer);
        assert!(revert_message(
            registry
                .renew_enclave_after_verifier_for_test(
                    &stale,
                    &stale_node,
                    &stale_enclave,
                    PostVerifierDcapCapabilityV1::new(fresh_verdict.clone()),
                )
                .unwrap_err()
        )
        .contains("next renewal"));
    });

    let mut late_provider = storage(genesis_hash);
    StorageHandle::enter(&mut late_provider, |storage| {
        register_validator(storage.clone(), &node_signer, CONSENSUS_KEY);
        let mut registry = TeeRegistry::new(storage.clone());
        registry.install_initial_policy_v1(&active_policy).unwrap();
        register_same_key_node_for_lifecycle_test(
            &mut registry,
            &initial,
            &node_signer,
            &initial_node,
            &initial_enclave,
            PostVerifierDcapCapabilityV1::new(fresh_verdict.clone()),
        )
        .unwrap();
        storage
            .set_block_timestamp(U256::from(initial.requested_valid_until))
            .unwrap();
        let expired_renewal = renewal_intent(
            &initial,
            initial.requested_valid_until + active_policy.maximum_lease,
        );
        let (node_signature, enclave_signature) =
            signatures(&expired_renewal, &node_signer, &enclave_signer);
        assert!(revert_message(
            registry
                .renew_enclave_after_verifier_for_test(
                    &expired_renewal,
                    &node_signature,
                    &enclave_signature,
                    PostVerifierDcapCapabilityV1::new(fresh_verdict.clone()),
                )
                .unwrap_err()
        )
        .contains("expired"));
    });

    let mut no_drift_provider = storage(genesis_hash);
    StorageHandle::enter(&mut no_drift_provider, |storage| {
        register_validator(storage.clone(), &node_signer, CONSENSUS_KEY);
        let mut registry = TeeRegistry::new(storage.clone());
        registry.install_initial_policy_v1(&active_policy).unwrap();
        register_same_key_node_for_lifecycle_test(
            &mut registry,
            &initial,
            &node_signer,
            &initial_node,
            &initial_enclave,
            PostVerifierDcapCapabilityV1::new(fresh_verdict.clone()),
        )
        .unwrap();
        storage
            .set_block_timestamp(U256::from(initial.requested_valid_until - 1))
            .unwrap();
        registry
            .renew_enclave_after_verifier_for_test(
                &renewal,
                &renewal_node,
                &renewal_enclave,
                PostVerifierDcapCapabilityV1::new(fresh_verdict.clone()),
            )
            .unwrap();

        let second = renewal_intent(
            &renewal,
            renewal.requested_valid_until + active_policy.maximum_lease,
        );
        let (second_node, second_enclave) = signatures(&second, &node_signer, &enclave_signer);
        let second_opens_at = renewal.requested_valid_until - active_policy.maximum_lease / 2;
        storage
            .set_block_timestamp(U256::from(second_opens_at))
            .unwrap();
        registry
            .renew_enclave_after_verifier_for_test(
                &second,
                &second_node,
                &second_enclave,
                PostVerifierDcapCapabilityV1::new(fresh_verdict),
            )
            .unwrap();
        assert_eq!(
            registry
                .validator_enclave_binding_v1(node_signer.address())
                .unwrap()
                .unwrap()
                .valid_until,
            initial.requested_valid_until + 2 * active_policy.maximum_lease
        );
    });
}

#[test]
fn renewal_rejects_the_wrong_evm_caller_before_replay_or_state_change() {
    let genesis_hash = B256::repeat_byte(0x2C);
    let mut active_policy = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
    active_policy.minimum_lease = 3_600;
    active_policy.maximum_lease = 3_600;
    let owner = OutbeEvmSigner::from_secret_bytes([0x2D; 32]).unwrap();
    let wrong = OutbeEvmSigner::from_secret_bytes([0x2E; 32]).unwrap();
    let enclave = ed25519_dalek::SigningKey::from_bytes(&[0x2F; 32]);
    let initial = registration_intent(&active_policy, &owner, CONSENSUS_KEY, &enclave, 0x30, 0x31);
    let renewal = renewal_intent(&initial, initial.requested_valid_until + 3_600);
    let (initial_node, initial_enclave) = signatures(&initial, &owner, &enclave);
    let (renewal_node, renewal_enclave) = signatures(&renewal, &owner, &enclave);
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
            .set_block_timestamp(U256::from(NOW + 1_800))
            .unwrap();
        let before = registry
            .validator_enclave_binding_v1(owner.address())
            .unwrap()
            .unwrap();

        let error = registry
            .renew_enclave_after_verifier_for_test_as(
                wrong.address(),
                &renewal,
                &renewal_node,
                &renewal_enclave,
                PostVerifierDcapCapabilityV1::new(accepted.clone()),
            )
            .unwrap_err();
        assert!(revert_message(error).contains("caller"));
        assert_eq!(
            registry
                .validator_enclave_binding_v1(owner.address())
                .unwrap()
                .unwrap(),
            before
        );

        assert_eq!(
            registry
                .renew_enclave_after_verifier_for_test_as(
                    owner.address(),
                    &renewal,
                    &renewal_node,
                    &renewal_enclave,
                    PostVerifierDcapCapabilityV1::new(accepted.clone()),
                )
                .unwrap(),
            V1RegistrationOutcome::Created
        );
        assert!(revert_message(
            registry
                .renew_enclave_after_verifier_for_test_as(
                    wrong.address(),
                    &renewal,
                    &renewal_node,
                    &renewal_enclave,
                    PostVerifierDcapCapabilityV1::new(accepted),
                )
                .unwrap_err()
        )
        .contains("caller"));
    });
}

#[test]
fn renewal_rejects_collateral_margin_underflow_without_extending_state() {
    let genesis_hash = B256::repeat_byte(0x22);
    let mut active_policy = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
    active_policy.maximum_lease = 3_600;
    let node_signer = OutbeEvmSigner::from_secret_bytes([0x73; 32]).unwrap();
    let enclave_signer = ed25519_dalek::SigningKey::from_bytes(&[0x74; 32]);
    let initial = registration_intent(
        &active_policy,
        &node_signer,
        CONSENSUS_KEY,
        &enclave_signer,
        0x63,
        0x64,
    );
    let (initial_node, initial_enclave) = signatures(&initial, &node_signer, &enclave_signer);
    let renewal = renewal_intent(
        &initial,
        initial.requested_valid_until + active_policy.maximum_lease,
    );
    let (renewal_node, renewal_enclave) = signatures(&renewal, &node_signer, &enclave_signer);
    let mut provider = storage(genesis_hash);

    StorageHandle::enter(&mut provider, |storage| {
        register_validator(storage.clone(), &node_signer, CONSENSUS_KEY);
        let mut registry = TeeRegistry::new(storage.clone());
        registry.install_initial_policy_v1(&active_policy).unwrap();
        let mut initial_verdict = verdict(DcapPlatformTcbStatusV1::UpToDate);
        initial_verdict.collateral_valid_until = NOW + 12_000;
        register_same_key_node_for_lifecycle_test(
            &mut registry,
            &initial,
            &node_signer,
            &initial_node,
            &initial_enclave,
            PostVerifierDcapCapabilityV1::new(initial_verdict),
        )
        .unwrap();
        storage
            .set_block_timestamp(U256::from(NOW + 2_400))
            .unwrap();
        let before = registry
            .validator_enclave_binding_v1(node_signer.address())
            .unwrap()
            .unwrap();
        let mut underflow = verdict(DcapPlatformTcbStatusV1::UpToDate);
        underflow.collateral_valid_until = active_policy.collateral_margin - 1;
        assert!(revert_message(
            registry
                .renew_enclave_after_verifier_for_test(
                    &renewal,
                    &renewal_node,
                    &renewal_enclave,
                    PostVerifierDcapCapabilityV1::new(underflow),
                )
                .unwrap_err()
        )
        .contains("safety margin"));
        assert_eq!(
            registry
                .validator_enclave_binding_v1(node_signer.address())
                .unwrap()
                .unwrap(),
            before
        );
    });
}
