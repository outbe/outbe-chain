use super::*;
use crate::v1::{NodeEnclaveBindingV1, VerifiedIntentV1};

/// The canonical DCAP evidence of `intent` with a key-ready proof for the
/// resident offer key `resident_offer_public`. `enclave_signer` signs the
/// proof.
fn transition_evidence_with_offer(
    intent: &RegistrationIntentV1,
    enclave_signer: &ed25519_dalek::SigningKey,
    resident_offer_public: [u8; 32],
) -> Vec<u8> {
    let candidate_manifest = initialization_manifest_for_intent(intent, [0xa7; 32]);
    let mut proof = TransitionKeyReadyProofV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        transition_intent_hash: intent.intent_hash().unwrap(),
        candidate_manifest_hash: candidate_manifest.authorization_hash().unwrap(),
        transition_nonce: intent.transition_nonce,
        resident_offer_public,
        candidate_attestation_signature: [0; 64],
    };
    proof.candidate_attestation_signature = enclave_signer
        .sign(proof.signing_hash().unwrap().as_slice())
        .to_bytes();
    synthetic_dcap_evidence(intent, vec![0x51], Some(proof))
        .encode_canonical()
        .unwrap()
}

/// [`transition_evidence_with_offer`] for the installed offer key
/// [`OFFER_PUBLIC`].
fn transition_evidence(
    intent: &RegistrationIntentV1,
    enclave_signer: &ed25519_dalek::SigningKey,
) -> Vec<u8> {
    transition_evidence_with_offer(intent, enclave_signer, OFFER_PUBLIC)
}

fn install_offer_key(registry: &mut TeeRegistry<'_>, policy: &TeePolicyV1) {
    registry
        .write_bootstrap(&TeeBootstrapData {
            tribute_offer_public_key: B256::from(OFFER_PUBLIC),
            policy_hash: policy.policy_hash().unwrap(),
            key_epoch: 0,
            tribute_offer_epoch: 0,
            dkg_transcript_hash: B256::repeat_byte(0xb2),
            committee_snapshot_block: 1,
            committee_snapshot_hash: B256::repeat_byte(0xb3),
            tribute_offer_group_public_key: vec![0xb4; 96].into(),
        })
        .unwrap();
}

/// The `transitionEnclaveMeasurement` calldata for `signed` with `evidence`.
fn transition_calldata(evidence: &[u8], signed: &SignedIntent<'_>) -> Vec<u8> {
    evidence_mutator_calldata(
        RegistryMutatorV1::TransitionEnclaveMeasurement,
        evidence,
        signed,
    )
}

/// Asserts that `binding` is the first measurement transition to the enclave of
/// `transition` with `mrenclave` under the `successor` policy.
fn assert_transitioned_binding(
    binding: &NodeEnclaveBindingV1,
    transition: &RegistrationIntentV1,
    mrenclave: B256,
    successor: &TeePolicyV1,
) {
    assert_eq!(binding.enclave_id, transition.enclave_id);
    assert_eq!(binding.mrenclave, mrenclave);
    assert_eq!(binding.transition_nonce, 1);
    assert_eq!(binding.policy_hash, successor.policy_hash().unwrap());
}

/// Runs `test` on a new chain where `validator` is a registered validator with
/// the policy and the offer key installed, in this order, and then with its
/// initial binding from `initial`. The measurement transition in `test` reads
/// the offer key (`validate_transition_key_ready_proof_v1`). The order (offer
/// key, then binding) is the order of the original test.
fn run_with_offer_key_and_binding(
    validator: &LifecycleValidator,
    initial: VerifiedIntentV1<'_>,
    test: impl FnOnce(StorageHandle<'_>, TeeRegistry<'_>),
) {
    validator.run_as_validator(|storage, mut registry| {
        install_offer_key(&mut registry, &validator.policy);
        register_same_key_node_for_lifecycle_test(&mut registry, &validator.node_signer, initial)
            .unwrap();
        test(storage, registry);
    });
}

#[test]
fn existing_validator_transitions_to_staged_measurement_before_activation() {
    let validator = LifecycleValidator::new(
        hardening_policy(B256::repeat_byte(0x16)),
        0x31,
        0x32,
        EnclaveBindingSeeds::new(0x51, 0x61),
    );
    let successor = measurement_successor(&validator.policy, B256::repeat_byte(0x92));

    let new_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x33; 32]);
    let transition = measurement_transition_intent(
        &validator.initial,
        &successor,
        &new_enclave,
        EnclaveBindingSeeds::new(0x52, 0x62),
        NOW + 3_600,
    );
    let signed_initial = validator.signed_initial();
    let signed_transition = validator.sign_with_enclave(&transition, &new_enclave);
    let transition_evidence = transition_evidence(&transition, &new_enclave);
    let call = transition_calldata(&transition_evidence, &signed_transition);
    let mut next_verdict = verdict(DcapPlatformTcbStatusV1::UpToDate);
    next_verdict.mrenclave = B256::repeat_byte(0x92);
    run_with_offer_key_and_binding(
        &validator,
        signed_initial.with_verdict(verdict(DcapPlatformTcbStatusV1::UpToDate)),
        |storage, mut registry| {
            registry
                .stage_successor_policy_v1(U256::from(7), &successor)
                .unwrap();

            let wrong_call = transition_calldata(
                &transition_evidence_with_offer(&transition, &new_enclave, [0xc1; 32]),
                &signed_transition,
            );
            assert_reverts(
                dispatch_transition_after_verifier_for_test(
                    storage.clone(),
                    validator.node_signer.address(),
                    &wrong_call,
                    &transition,
                    PostVerifierDcapCapabilityV1::new(next_verdict.clone()),
                ),
                "measurement transition key-ready proof is invalid",
            );
            assert_eq!(
                validator.stored_binding(&registry).enclave_id,
                validator.initial.enclave_id
            );
            dispatch_transition_after_verifier_for_test(
                storage.clone(),
                validator.node_signer.address(),
                &call,
                &transition,
                PostVerifierDcapCapabilityV1::new(next_verdict),
            )
            .unwrap();

            let registry = TeeRegistry::new(storage);
            let binding = validator.stored_binding(&registry);
            assert_transitioned_binding(&binding, &transition, B256::repeat_byte(0x92), &successor);
            assert_eq!(registry.active_policy_v1().unwrap(), validator.policy);
        },
    );
}

#[test]
fn activation_preserves_old_lease_but_old_policy_cannot_register_renew_or_replace() {
    let genesis_hash = B256::repeat_byte(0x19);
    let validator = LifecycleValidator::new(
        hardening_policy(genesis_hash),
        0x37,
        0x38,
        EnclaveBindingSeeds::new(0x55, 0x65),
    );
    let successor = measurement_successor(&validator.policy, B256::repeat_byte(0x94));
    let proposal_id = U256::from(10);
    let renewal = renewal_intent(&validator.initial, NOW + 6_000);
    let replacement_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x39; 32]);
    let replacement = replacement_intent(
        &validator.initial,
        &replacement_enclave,
        EnclaveBindingSeeds::new(0x56, 0x66),
        NOW + 6_000,
    );
    let newcomer = LifecycleValidator::new(
        validator.policy.clone(),
        0x3c,
        0x3d,
        EnclaveBindingSeeds::new(0x57, 0x67),
    );
    let newcomer_consensus_key = [0x3e; 48];
    let signed_initial = validator.signed_initial();
    let signed_renewal = validator.sign(&renewal);
    let signed_replacement = validator.sign_with_enclave(&replacement, &replacement_enclave);
    let signed_newcomer = newcomer.signed_initial();
    let mut provider = validator.run_as_validator_with_binding(
        signed_initial.with_verdict(verdict(DcapPlatformTcbStatusV1::UpToDate)),
        |_storage, mut registry| {
            registry
                .stage_successor_policy_v1(proposal_id, &successor)
                .unwrap();
        },
    );

    provider.set_block_number(50);
    StorageHandle::enter(&mut provider, |storage| {
        register_validator(
            storage.clone(),
            &newcomer.node_signer,
            newcomer_consensus_key,
        );
        let mut registry = TeeRegistry::new(storage);
        registry
            .promote_staged_successor_policy_v1(proposal_id, 50)
            .unwrap();
        assert!(registry
            .is_validator_enclave_ready_v1(validator.node_signer.address())
            .unwrap());
        assert_reverts(
            registry.register_enclave_after_verifier_for_test(
                signed_newcomer.with_verdict(verdict(DcapPlatformTcbStatusV1::UpToDate)),
            ),
            "authoritative V1 policy",
        );
        assert_reverts(
            registry.renew_enclave_after_verifier_for_test(
                signed_renewal.with_verdict(verdict(DcapPlatformTcbStatusV1::UpToDate)),
            ),
            "authoritative V1 policy",
        );
        assert_reverts(
            registry.replace_enclave_binding_after_verifier_for_test(
                signed_replacement.with_verdict(verdict(DcapPlatformTcbStatusV1::UpToDate)),
            ),
            "authoritative V1 policy",
        );
    });
}

#[test]
fn full_node_uses_the_same_bounded_transition_abi_and_staged_policy() {
    let genesis_hash = B256::repeat_byte(0x18);
    let full_node = LifecycleFullNode::new(
        hardening_policy(genesis_hash),
        0x34,
        0x35,
        EnclaveBindingSeeds::new(0x53, 0x63),
    );
    let successor = measurement_successor(&full_node.policy, B256::repeat_byte(0x93));

    let admission_signer = OutbeEvmSigner::from_secret_bytes([0x37; 32]).unwrap();
    let new_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x36; 32]);
    let transition = measurement_transition_intent(
        &full_node.initial,
        &successor,
        &new_enclave,
        EnclaveBindingSeeds::new(0x54, 0x64),
        NOW + 3_600,
    );
    let signed_initial = full_node.signed_initial();
    let association = full_node.association(&admission_signer);
    let signed_transition = full_node.sign_with_enclave(&transition, &new_enclave);
    let call = transition_calldata(
        &transition_evidence(&transition, &new_enclave),
        &signed_transition,
    );
    let mut next_verdict = verdict(DcapPlatformTcbStatusV1::UpToDate);
    next_verdict.mrenclave = B256::repeat_byte(0x93);
    full_node.run_installed(|storage, mut registry| {
        install_offer_key(&mut registry, &full_node.policy);
        registry
            .register_enclave_and_bind_after_verifier_for_test(
                signed_initial.with_verdict(verdict(DcapPlatformTcbStatusV1::UpToDate)),
                association.input(),
            )
            .unwrap();
        registry
            .stage_successor_policy_v1(U256::from(9), &successor)
            .unwrap();
        dispatch_transition_after_verifier_for_test(
            storage.clone(),
            admission_signer.address(),
            &call,
            &transition,
            PostVerifierDcapCapabilityV1::new(next_verdict),
        )
        .unwrap();

        let binding = full_node.stored_binding(&TeeRegistry::new(storage));
        assert_transitioned_binding(&binding, &transition, B256::repeat_byte(0x93), &successor);
    });
}
