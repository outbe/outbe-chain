use super::*;

#[test]
fn full_node_renewal_and_replacement_follow_the_shared_lease_lifecycle() {
    let genesis_hash = B256::repeat_byte(0x25);
    let full_node = LifecycleFullNode::new(
        capped_lease_policy(genesis_hash),
        0x7B,
        0x7C,
        EnclaveBindingSeeds::new(0x6D, 0x6E),
    );
    let new_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x7D; 32]);
    let (renewal, replacement) = full_node.renewal_then_replacement(
        &new_enclave,
        EnclaveBindingSeeds::new(0x6F, 0x70),
        NOW + 6_000,
    );
    let signed_initial = full_node.signed_initial();
    let signed_renewal = full_node.sign(&renewal);
    let signed_replacement = full_node.sign_with_enclave(&replacement, &new_enclave);
    let p2p_public = full_node_public(&full_node.initial);
    let accepted = up_to_date_verdict_until(NOW + 12_000);
    full_node.run_installed(|storage, mut registry| {
        registry
            .register_enclave_after_verifier_for_test(signed_initial.with_verdict(accepted.clone()))
            .unwrap();
        storage
            .set_block_timestamp(U256::from(NOW + 2_400))
            .unwrap();
        registry
            .renew_enclave_after_verifier_for_test(signed_renewal.with_verdict(accepted.clone()))
            .unwrap();
        registry
            .replace_enclave_binding_after_verifier_for_test(
                signed_replacement.with_verdict(accepted),
            )
            .unwrap();

        let binding = full_node.stored_binding(&registry);
        assert_eq!(binding.enclave_id, replacement.enclave_id);
        assert_eq!(binding.binding_version, 2);
        assert_eq!(binding.registration_version, 2);
        assert_eq!(binding.renewal_nonce, 1);
        assert!(registry.is_node_host_enclave_ready_v1(p2p_public).unwrap());
    });
}

#[test]
fn an_existing_lease_is_not_retroactively_filtered_by_the_policy_anchor() {
    let validator = LifecycleValidator::new(
        capped_lease_policy(B256::repeat_byte(0x26)),
        0x7E,
        0x7F,
        EnclaveBindingSeeds::new(0x71, 0x72),
    );
    let signed_intent = validator.signed_initial();
    validator.run_as_validator_with_binding(
        signed_intent.with_verdict(verdict(DcapPlatformTcbStatusV1::UpToDate)),
        |_storage, registry| {
            let admitted_policy_hash = validator.policy.policy_hash().unwrap();
            registry
                .active_v1_policy_hash
                .write(B256::repeat_byte(0xEE))
                .unwrap();

            assert!(registry
                .is_validator_enclave_ready_v1(validator.node_signer.address())
                .unwrap());
            assert_eq!(
                validator.stored_binding(&registry).policy_hash,
                admitted_policy_hash
            );
        },
    );
}

#[test]
fn renewal_window_is_half_open_extends_from_deadline_and_does_not_drift() {
    let validator = LifecycleValidator::new(
        fixed_lease_policy(B256::repeat_byte(0x21)),
        0x71,
        0x72,
        EnclaveBindingSeeds::new(0x61, 0x62),
    );
    let signed_initial = validator.signed_initial();
    let renewal = max_lease_renewal(&validator.initial, &validator.policy);
    let signed_renewal = validator.sign(&renewal);
    let fresh_verdict = up_to_date_verdict_until(NOW + 20_000);
    validator.run_as_validator_with_binding_at(
        NOW + 1_799,
        signed_initial.with_verdict(fresh_verdict.clone()),
        |storage, mut registry| {
            assert_reverts(
                registry.renew_enclave_after_verifier_for_test(
                    signed_renewal.with_verdict(fresh_verdict.clone()),
                ),
                "renewal window",
            );

            storage
                .set_block_timestamp(U256::from(NOW + 1_800))
                .unwrap();
            assert_created_then_idempotent(
                &mut registry,
                |registry| {
                    registry.renew_enclave_after_verifier_for_test(
                        signed_renewal.with_verdict(fresh_verdict.clone()),
                    )
                },
                |_| {},
            );
            assert_reverts(
                registry.renew_enclave_after_verifier_for_test(
                    signed_renewal.with_conflicting_evidence(fresh_verdict.clone()),
                ),
                "exact evidence replay",
            );
            let binding = validator.stored_binding(&registry);
            assert_eq!(binding.registration_version, 1);
            assert_eq!(binding.renewal_nonce, 1);
            assert_eq!(binding.lease_started_at, NOW + 1_800);
            assert_eq!(
                binding.valid_until,
                validator.initial.requested_valid_until + validator.policy.maximum_lease
            );

            let mut stale = renewal.clone();
            stale.registration_version = 0;
            stale.renewal_nonce = 0;
            let signed_stale = validator.sign(&stale);
            assert_reverts(
                registry.renew_enclave_after_verifier_for_test(
                    signed_stale.with_verdict(fresh_verdict.clone()),
                ),
                "next renewal",
            );
        },
    );

    validator.run_as_validator_with_binding_at(
        validator.initial.requested_valid_until,
        signed_initial.with_verdict(fresh_verdict.clone()),
        |_storage, mut registry| {
            assert_reverts(
                registry.renew_enclave_after_verifier_for_test(
                    signed_renewal.with_verdict(fresh_verdict.clone()),
                ),
                "expired",
            );
        },
    );

    validator.run_as_validator_with_binding_at(
        validator.initial.requested_valid_until - 1,
        signed_initial.with_verdict(fresh_verdict.clone()),
        |storage, mut registry| {
            registry
                .renew_enclave_after_verifier_for_test(
                    signed_renewal.with_verdict(fresh_verdict.clone()),
                )
                .unwrap();

            let second = max_lease_renewal(&renewal, &validator.policy);
            let signed_second = validator.sign(&second);
            let second_opens_at =
                renewal.requested_valid_until - validator.policy.maximum_lease / 2;
            storage
                .set_block_timestamp(U256::from(second_opens_at))
                .unwrap();
            registry
                .renew_enclave_after_verifier_for_test(signed_second.with_verdict(fresh_verdict))
                .unwrap();
            assert_eq!(
                validator.stored_binding(&registry).valid_until,
                validator.initial.requested_valid_until + 2 * validator.policy.maximum_lease
            );
        },
    );
}

#[test]
fn renewal_rejects_the_wrong_evm_caller_before_replay_or_state_change() {
    let validator = LifecycleValidator::new(
        fixed_lease_policy(B256::repeat_byte(0x2C)),
        0x2D,
        0x2F,
        EnclaveBindingSeeds::new(0x30, 0x31),
    );
    let wrong = OutbeEvmSigner::from_secret_bytes([0x2E; 32]).unwrap();
    let renewal = max_lease_renewal(&validator.initial, &validator.policy);
    let signed_initial = validator.signed_initial();
    let signed_renewal = validator.sign(&renewal);
    let accepted = up_to_date_verdict_until(NOW + 20_000);
    validator.run_with_binding_at(
        NOW + 1_800,
        signed_initial.with_verdict(accepted.clone()),
        |_storage, mut registry| {
            validator.assert_binding_unchanged_by(&mut registry, |registry| {
                assert_reverts(
                    registry.renew_enclave_after_verifier_for_test_as(
                        wrong.address(),
                        signed_renewal.with_verdict(accepted.clone()),
                    ),
                    "caller",
                );
            });

            assert_eq!(
                registry
                    .renew_enclave_after_verifier_for_test_as(
                        validator.node_signer.address(),
                        signed_renewal.with_verdict(accepted.clone())
                    )
                    .unwrap(),
                V1RegistrationOutcome::Created
            );
            assert_reverts(
                registry.renew_enclave_after_verifier_for_test_as(
                    wrong.address(),
                    signed_renewal.with_verdict(accepted),
                ),
                "caller",
            );
        },
    );
}

#[test]
fn renewal_rejects_collateral_margin_underflow_without_extending_state() {
    let validator = LifecycleValidator::new(
        capped_lease_policy(B256::repeat_byte(0x22)),
        0x73,
        0x74,
        EnclaveBindingSeeds::new(0x63, 0x64),
    );
    let signed_initial = validator.signed_initial();
    let renewal = max_lease_renewal(&validator.initial, &validator.policy);
    let signed_renewal = validator.sign(&renewal);
    validator.run_as_validator_with_binding_at(
        NOW + 2_400,
        signed_initial.with_verdict(up_to_date_verdict_until(NOW + 12_000)),
        |_storage, mut registry| {
            validator.assert_binding_unchanged_by(&mut registry, |registry| {
                let underflow = up_to_date_verdict_until(validator.policy.collateral_margin - 1);
                assert_reverts(
                    registry.renew_enclave_after_verifier_for_test(
                        signed_renewal.with_verdict(underflow),
                    ),
                    "safety margin",
                );
            });
        },
    );
}
