use super::*;
use crate::v1::NodeHostAssociationV1;

/// Asserts the rejections that both node roles share for the initial intent
/// `initial`, in this order: a signature of `other_enclave` over the intent
/// hash, the stale intent `signed_stale` with `stale_message`, and a wrong
/// measurement.
fn assert_shared_initial_rejections(
    registry: &mut TeeRegistry<'_>,
    initial: &SignedIntent<'_>,
    other_enclave: &ed25519_dalek::SigningKey,
    signed_stale: &SignedIntent<'_>,
    stale_message: &str,
) {
    let wrong_enclave = SignedIntent {
        enclave_signature: other_enclave
            .sign(initial.intent.intent_hash().unwrap().as_slice())
            .to_bytes(),
        ..*initial
    };
    assert_reverts(
        registry.register_enclave_after_verifier_for_test(
            wrong_enclave.with_verdict(verdict(DcapPlatformTcbStatusV1::UpToDate)),
        ),
        "enclave proof",
    );
    assert_reverts(
        registry.register_enclave_after_verifier_for_test(
            signed_stale.with_verdict(verdict(DcapPlatformTcbStatusV1::UpToDate)),
        ),
        stale_message,
    );
    let mut wrong_measurement = verdict(DcapPlatformTcbStatusV1::UpToDate);
    wrong_measurement.mrenclave = B256::repeat_byte(0x99);
    assert_reverts(
        registry.register_enclave_after_verifier_for_test(initial.with_verdict(wrong_measurement)),
        "measurement rule",
    );
}

/// Asserts that a registration of `signed` with `node_signature` in place of
/// its node signature reverts at the node proof.
fn assert_node_proof_rejected(
    registry: &mut TeeRegistry<'_>,
    signed: &SignedIntent<'_>,
    node_signature: [u8; 65],
) {
    let forged = SignedIntent {
        node_signature,
        ..*signed
    };
    assert_reverts(
        registry.register_enclave_after_verifier_for_test(
            forged.with_verdict(verdict(DcapPlatformTcbStatusV1::UpToDate)),
        ),
        "node proof",
    );
}

/// `initial` with the renewal nonce 1. An initial registration must not carry
/// a renewal nonce.
fn stale_initial_intent(initial: &RegistrationIntentV1) -> RegistrationIntentV1 {
    let mut stale = initial.clone();
    stale.renewal_nonce = 1;
    stale
}

/// Asserts that `other` binds the enclave of the `existing` intent and that
/// its registration reverts because that enclave is already bound to another
/// node.
fn assert_enclave_bound_to_other_node(
    registry: &mut TeeRegistry<'_>,
    other: &SignedIntent<'_>,
    existing: &RegistrationIntentV1,
) {
    assert_eq!(other.intent.enclave_id, existing.enclave_id);
    assert_reverts(
        registry.register_enclave_after_verifier_for_test(
            other.with_verdict(verdict(DcapPlatformTcbStatusV1::UpToDate)),
        ),
        "already bound to another node",
    );
}

/// Asserts that `registry` has no binding for the node of `intent` and no node
/// for `validator`.
fn assert_node_registration_rolled_back(
    registry: &TeeRegistry<'_>,
    intent: &RegistrationIntentV1,
    validator: Address,
) {
    assert!(registry
        .node_host_enclave_binding_v1(intent.node_id.reth_p2p_public)
        .unwrap()
        .is_none());
    assert_eq!(
        registry.validator_v1_node_hash.read(&validator).unwrap(),
        B256::ZERO
    );
}

#[test]
fn initial_role_neutral_registration_atomically_records_the_address_association_without_a_role() {
    let genesis_hash = B256::repeat_byte(0xA1);
    let full_node = LifecycleFullNode::new(
        hardening_policy(genesis_hash),
        0xA3,
        0xA4,
        EnclaveBindingSeeds::new(0xA5, 0xA6),
    );
    let validator_signer = OutbeEvmSigner::from_secret_bytes([0xA2; 32]).unwrap();
    let signed_intent = full_node.signed_initial();
    let node_id_hash = full_node.initial.node_id.node_id_hash().unwrap();
    let association = full_node.association(&validator_signer);
    full_node.run_installed(|storage, mut registry| {
        assert_created_then_idempotent(
            &mut registry,
            |registry| {
                registry.register_enclave_and_bind_after_verifier_for_test(
                    signed_intent.with_verdict(verdict(DcapPlatformTcbStatusV1::UpToDate)),
                    association.input(),
                )
            },
            |registry| {
                assert!(ValidatorSet::new(storage.clone())
                    .get_validator(validator_signer.address())
                    .unwrap()
                    .is_none());
                assert!(!registry
                    .is_validator_enclave_ready_v1(validator_signer.address())
                    .unwrap());
            },
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
    let validator = LifecycleValidator::new(
        hardening_policy(genesis_hash),
        0x61,
        0x62,
        EnclaveBindingSeeds::new(0x41, 0x51),
    );
    let signed_intent = validator.signed_initial();
    let association = validator.association(&validator.initial);
    let accepted_verdict = verdict(DcapPlatformTcbStatusV1::SWHardeningNeeded);
    let provider = validator.run_as_validator(|storage, mut registry| {
        assert!(!registry
            .is_validator_enclave_ready_v1(validator.node_signer.address())
            .unwrap());

        assert_created_then_idempotent(
            &mut registry,
            |registry| {
                registry.register_enclave_and_bind_after_verifier_for_test(
                    signed_intent.with_verdict(accepted_verdict.clone()),
                    association.input(),
                )
            },
            |registry| {
                assert!(registry
                    .is_validator_enclave_ready_v1(validator.node_signer.address())
                    .unwrap());
                let stored_binding = validator.stored_binding(registry);
                assert_eq!(stored_binding.enclave_id, validator.initial.enclave_id);
                assert_eq!(stored_binding.binding_id, validator.initial.binding_id);
                assert_eq!(
                    stored_binding.intent_hash,
                    validator.initial.intent_hash().unwrap()
                );
                assert_eq!(stored_binding.evidence_hash, B256::repeat_byte(0xEC));
                assert_eq!(
                    stored_binding.valid_until,
                    validator.initial.requested_valid_until
                );
                assert_ne!(stored_binding.verdict_hash, B256::ZERO);
            },
        );

        assert_reverts(
            registry.register_enclave_and_bind_after_verifier_for_test(
                signed_intent.with_conflicting_evidence(accepted_verdict),
                association.input(),
            ),
            "not an exact evidence replay",
        );

        storage
            .set_block_timestamp(U256::from(validator.initial.requested_valid_until))
            .unwrap();
        assert!(!registry
            .is_validator_enclave_ready_v1(validator.node_signer.address())
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
    let active_policy = hardening_policy(genesis_hash);
    let validators = (1_u8..=32)
        .map(|index| {
            LifecycleValidator::new(
                active_policy.clone(),
                index,
                index.wrapping_add(64),
                EnclaveBindingSeeds::new(index, index.wrapping_add(96)),
            )
        })
        .collect::<Vec<_>>();
    let signed_intents = validators
        .iter()
        .map(LifecycleValidator::signed_initial)
        .collect::<Vec<_>>();
    let mut provider = storage(genesis_hash);
    provider.set_block_number(1);

    StorageHandle::enter(&mut provider, |storage| {
        for (validator, index) in validators.iter().zip(1_u8..) {
            register_validator(storage.clone(), &validator.node_signer, [index; 48]);
        }
        let mut registry = installed_registry(storage.clone(), &active_policy);

        let started = std::time::Instant::now();
        for (index, (validator, signed_intent)) in
            validators.iter().zip(&signed_intents).enumerate()
        {
            assert_eq!(
                register_same_key_node_for_lifecycle_test(
                    &mut registry,
                    &validator.node_signer,
                    signed_intent.verified(PostVerifierDcapCapabilityV1::with_evidence_hash(
                        verdict(DcapPlatformTcbStatusV1::UpToDate),
                        B256::repeat_byte(u8::try_from(index + 1).unwrap()),
                    ))
                )
                .unwrap(),
                V1RegistrationOutcome::Created
            );
            assert!(registry
                .is_validator_enclave_ready_v1(validator.node_signer.address())
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
    let active_policy = hardening_policy(genesis_hash);
    let admission_signer = OutbeEvmSigner::from_secret_bytes([0x22; 32]).unwrap();
    let first = LifecycleFullNode::new(
        active_policy.clone(),
        0x23,
        0x25,
        EnclaveBindingSeeds::new(0x27, 0x28),
    );
    let second = LifecycleFullNode::new(
        active_policy.clone(),
        0x24,
        0x26,
        EnclaveBindingSeeds::new(0x29, 0x2A),
    );
    let signed_first_intent = first.signed_initial();
    let signed_second_intent = second.signed_initial();
    let first_association = first.association(&admission_signer);
    let second_association = second.association(&admission_signer);
    let second_admission_signer = OutbeEvmSigner::from_secret_bytes([0x2B; 32]).unwrap();
    let second_node_own_association = second.association(&second_admission_signer);
    let first_node_hash = first.initial.node_id.node_id_hash().unwrap();
    let second_node_hash = second.initial.node_id.node_id_hash().unwrap();
    first.run_installed(|_storage, mut registry| {
        let mut invalid_validator_signature = first_association.validator_signature;
        invalid_validator_signature[0] ^= 1;
        assert_reverts(
            registry.register_enclave_and_bind_after_verifier_for_test(
                signed_first_intent.with_verdict(verdict(DcapPlatformTcbStatusV1::UpToDate)),
                NodeHostAssociationV1 {
                    binding: &first_association.binding,
                    validator_signature: &invalid_validator_signature,
                    node_binding_signature: &first_association.node_binding_signature,
                },
            ),
            "proof of possession",
        );
        assert_node_registration_rolled_back(&registry, &first.initial, admission_signer.address());

        registry
            .register_enclave_and_bind_after_verifier_for_test(
                signed_second_intent.with_verdict(verdict(DcapPlatformTcbStatusV1::UpToDate)),
                second_node_own_association.input(),
            )
            .unwrap();
        assert_reverts(
            registry.register_enclave_and_bind_after_verifier_for_test(
                signed_first_intent.with_verdict(verdict(DcapPlatformTcbStatusV1::UpToDate)),
                second_association.input(),
            ),
            "same NodeHost",
        );
        assert_node_registration_rolled_back(&registry, &first.initial, admission_signer.address());

        registry
            .register_enclave_and_bind_after_verifier_for_test(
                signed_first_intent.with_verdict(verdict(DcapPlatformTcbStatusV1::UpToDate)),
                first_association.input(),
            )
            .unwrap();
        assert_reverts(
            registry.register_enclave_and_bind_after_verifier_for_test(
                signed_second_intent.with_verdict(verdict(DcapPlatformTcbStatusV1::UpToDate)),
                second_association.input(),
            ),
            "not associated with the existing NodeHost",
        );
        assert_eq!(
            registry
                .validator_v1_node_hash
                .read(&admission_signer.address())
                .unwrap(),
            first_node_hash
        );
        assert_ne!(first_node_hash, second_node_hash);
        assert!(registry
            .node_host_enclave_binding_v1(second.initial.node_id.reth_p2p_public)
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
    let full_node = LifecycleFullNode::new(
        hardening_policy(genesis_hash),
        0x6A,
        0x6B,
        EnclaveBindingSeeds::new(0x49, 0x59),
    );
    let other_node = k256::ecdsa::SigningKey::from_bytes((&[0x6C; 32]).into()).unwrap();
    let validator_signer = OutbeEvmSigner::from_secret_bytes([0x6D; 32]).unwrap();
    let other_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x6E; 32]);
    let reth_p2p_public = full_node_public(&full_node.initial);
    let signed_intent = full_node.signed_initial();
    let provider = full_node.run_installed(|storage, mut registry| {
        assert!(!registry
            .is_node_host_enclave_ready_v1(reth_p2p_public)
            .unwrap());
        assert_created_then_idempotent(
            &mut registry,
            |registry| {
                registry.register_enclave_after_verifier_for_test(
                    signed_intent.with_verdict(verdict(DcapPlatformTcbStatusV1::SWHardeningNeeded)),
                )
            },
            |registry| {
                assert!(registry
                    .is_node_host_enclave_ready_v1(reth_p2p_public)
                    .unwrap());
                let binding = full_node.stored_binding(registry);
                assert_eq!(binding.enclave_id, full_node.initial.enclave_id);
                assert_eq!(
                    binding.intent_hash,
                    full_node.initial.intent_hash().unwrap()
                );
            },
        );

        let (wrong_p2p_signature, _) =
            full_node_signatures(&full_node.initial, &other_node, &full_node.enclave_signer);
        assert_node_proof_rejected(&mut registry, &signed_intent, wrong_p2p_signature);

        let validator_signature = validator_signer
            .sign_hash(&full_node.initial.intent_hash().unwrap())
            .unwrap();
        assert_node_proof_rejected(&mut registry, &signed_intent, validator_signature);

        let stale = stale_initial_intent(&full_node.initial);
        let signed_stale = full_node.sign(&stale);
        assert_shared_initial_rejections(
            &mut registry,
            &signed_intent,
            &other_enclave,
            &signed_stale,
            "must renew",
        );

        let mut excessive_lease = full_node.initial.clone();
        excessive_lease.requested_valid_until = NOW + full_node.policy.maximum_lease + 1;
        let signed_excessive_lease = full_node.sign(&excessive_lease);
        assert_reverts(
            registry.register_enclave_after_verifier_for_test(
                signed_excessive_lease.with_verdict(verdict(DcapPlatformTcbStatusV1::UpToDate)),
            ),
            "must renew",
        );

        let same_enclave_other_node = full_node_registration_intent(
            &full_node.policy,
            &other_node,
            &full_node.enclave_signer,
            EnclaveBindingSeeds::new(0x4B, 0x59),
        );
        let signed_same_enclave_other_node = SignedIntent::by_full_node(
            &same_enclave_other_node,
            &other_node,
            &full_node.enclave_signer,
        );
        assert_enclave_bound_to_other_node(
            &mut registry,
            &signed_same_enclave_other_node,
            &full_node.initial,
        );

        assert_reverts(
            registry.register_enclave_after_verifier_for_test(
                signed_intent.with_conflicting_evidence(verdict(DcapPlatformTcbStatusV1::UpToDate)),
            ),
            "not an exact evidence replay",
        );

        storage
            .set_block_timestamp(U256::from(full_node.initial.requested_valid_until))
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
    let validator = LifecycleValidator::new(
        hardening_policy(B256::repeat_byte(0x12)),
        0x63,
        0x65,
        EnclaveBindingSeeds::new(0x42, 0x52),
    );
    let other_node = OutbeEvmSigner::from_secret_bytes([0x64; 32]).unwrap();
    let other_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x66; 32]);
    let signed_intent = validator.signed_initial();
    validator.run_as_validator(|_storage, mut registry| {
        let wrong_node_signature = other_node
            .sign_hash(&validator.initial.intent_hash().unwrap())
            .unwrap();
        assert_node_proof_rejected(&mut registry, &signed_intent, wrong_node_signature);

        let full_node_signer = k256::ecdsa::SigningKey::from_bytes((&[0x6E; 32]).into()).unwrap();
        let (full_node_signature, _) = full_node_signatures(
            &validator.initial,
            &full_node_signer,
            &validator.enclave_signer,
        );
        assert_node_proof_rejected(&mut registry, &signed_intent, full_node_signature);

        let stale = stale_initial_intent(&validator.initial);
        let signed_stale = validator.sign(&stale);
        assert_shared_initial_rejections(
            &mut registry,
            &signed_intent,
            &other_enclave,
            &signed_stale,
            "versions and nonces",
        );

        assert!(registry
            .validator_enclave_binding_v1(validator.node_signer.address())
            .unwrap()
            .is_none());
    });
}

#[test]
fn one_to_one_binding_and_strict_platform_policy_reject_conflicts() {
    let genesis_hash = B256::repeat_byte(0x13);
    let first_validator = LifecycleValidator::new(
        hardening_policy(genesis_hash),
        0x67,
        0x69,
        EnclaveBindingSeeds::new(0x44, 0x54),
    );
    let second_node = OutbeEvmSigner::from_secret_bytes([0x68; 32]).unwrap();
    let replacement_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x6a; 32]);
    let signed_first = first_validator.signed_initial();
    first_validator.run_with_second_validator(
        &second_node,
        [0x34; 48],
        |_storage, mut registry| {
            registry
                .register_enclave_after_verifier_for_test(
                    signed_first.with_verdict(verdict(DcapPlatformTcbStatusV1::UpToDate)),
                )
                .unwrap();

            let second_enclave = registration_intent(
                &first_validator.policy,
                &first_validator.node_signer,
                &replacement_enclave,
                EnclaveBindingSeeds::new(0x45, 0x55),
            );
            let signed_second_enclave =
                first_validator.sign_with_enclave(&second_enclave, &replacement_enclave);
            assert_reverts(
                registry.register_enclave_after_verifier_for_test(
                    signed_second_enclave.with_verdict(verdict(DcapPlatformTcbStatusV1::UpToDate)),
                ),
                "must renew",
            );

            let mut same_enclave_other_node = registration_intent(
                &first_validator.policy,
                &second_node,
                &first_validator.enclave_signer,
                EnclaveBindingSeeds::new(0x46, 0x54),
            );
            same_enclave_other_node.recipient_x25519 = first_validator.initial.recipient_x25519;
            same_enclave_other_node.noise_responder_x25519 =
                first_validator.initial.noise_responder_x25519;
            same_enclave_other_node.node_host_authorization_hash =
                first_validator.initial.node_host_authorization_hash;
            same_enclave_other_node.enclave_id =
                same_enclave_other_node.derived_enclave_id().unwrap();
            let signed_same_enclave_other_node = SignedIntent::by_validator(
                &same_enclave_other_node,
                &second_node,
                &first_validator.enclave_signer,
            );
            assert_enclave_bound_to_other_node(
                &mut registry,
                &signed_same_enclave_other_node,
                &first_validator.initial,
            );
        },
    );

    let strict_validator = LifecycleValidator::new(
        policy(genesis_hash, PlatformTcbStatusSetV1::UpToDateOnly),
        0x6b,
        0x6c,
        EnclaveBindingSeeds::new(0x47, 0x57),
    );
    let signed_strict_intent = strict_validator.signed_initial();
    strict_validator.run_as_validator(|_storage, mut registry| {
        for status in [
            DcapPlatformTcbStatusV1::SWHardeningNeeded,
            DcapPlatformTcbStatusV1::ConfigurationAndSWHardeningNeeded,
        ] {
            assert_reverts(
                registry.register_enclave_after_verifier_for_test(
                    signed_strict_intent.with_verdict(verdict(status)),
                ),
                "stricter than active policy",
            );
        }
    });
}
