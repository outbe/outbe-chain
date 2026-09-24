use super::*;

fn transition_evidence(
    intent: &RegistrationIntentV1,
    enclave_signer: &ed25519_dalek::SigningKey,
) -> Vec<u8> {
    let candidate_manifest = initialization_manifest_for_intent(intent, [0xa7; 32]);
    let mut proof = TransitionKeyReadyProofV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        transition_intent_hash: intent.intent_hash().unwrap(),
        candidate_manifest_hash: candidate_manifest.authorization_hash().unwrap(),
        transition_nonce: intent.transition_nonce,
        resident_offer_public: OFFER_PUBLIC,
        candidate_attestation_signature: [0; 64],
    };
    proof.candidate_attestation_signature = enclave_signer
        .sign(proof.signing_hash().unwrap().as_slice())
        .to_bytes();
    AttestationEvidenceV1::Dcap(DcapEvidenceV1 {
        intent: intent.clone(),
        quote: vec![0x51],
        components: (1_u8..=8)
            .map(|kind| DcapCollateralComponentV1 {
                kind: DcapCollateralKind::try_from(kind).unwrap(),
                bytes: vec![kind],
            })
            .collect(),
        transition_key_ready_proof: Some(proof),
    })
    .encode_canonical()
    .unwrap()
}

pub(super) fn install_offer_key(registry: &mut TeeRegistry<'_>, policy: &TeePolicyV1) {
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

#[test]
fn existing_validator_transitions_to_staged_measurement_before_activation() {
    let genesis_hash = B256::repeat_byte(0x16);
    let current = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
    let mut successor = current.clone();
    successor.policy_version = 2;
    successor.activation_height = 50;
    successor.predecessor_policy_hash = current.policy_hash().unwrap();
    for rule in &mut successor.measurement_rules {
        rule.mrenclave = B256::repeat_byte(0x92);
        rule.admit_from_height = 50;
        rule.admit_until_height_exclusive = 500;
    }

    let node_signer = OutbeEvmSigner::from_secret_bytes([0x31; 32]).unwrap();
    let old_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x32; 32]);
    let new_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x33; 32]);
    let initial = registration_intent(
        &current,
        &node_signer,
        CONSENSUS_KEY,
        &old_enclave,
        0x51,
        0x61,
    );
    let transition =
        measurement_transition_intent(&initial, &successor, &new_enclave, 0x52, 0x62, NOW + 3_600);
    let (initial_node, initial_enclave) = signatures(&initial, &node_signer, &old_enclave);
    let (transition_node, transition_enclave) = signatures(&transition, &node_signer, &new_enclave);
    let transition_evidence = transition_evidence(&transition, &new_enclave);
    let call = IRegisterEnclaveV1Test::transitionEnclaveMeasurementCall {
        evidence: transition_evidence.clone().into(),
        nodeSignature: transition_node.to_vec().into(),
        enclaveSignature: transition_enclave.to_vec().into(),
    }
    .abi_encode();
    let mut next_verdict = verdict(DcapPlatformTcbStatusV1::UpToDate);
    next_verdict.mrenclave = B256::repeat_byte(0x92);
    let mut provider = storage(genesis_hash);

    StorageHandle::enter(&mut provider, |storage| {
        register_validator(storage.clone(), &node_signer, CONSENSUS_KEY);
        let mut registry = TeeRegistry::new(storage.clone());
        registry.install_initial_policy_v1(&current).unwrap();
        install_offer_key(&mut registry, &current);
        register_same_key_node_for_lifecycle_test(
            &mut registry,
            &initial,
            &node_signer,
            &initial_node,
            &initial_enclave,
            PostVerifierDcapCapabilityV1::new(verdict(DcapPlatformTcbStatusV1::UpToDate)),
        )
        .unwrap();
        registry
            .stage_successor_policy_v1(U256::from(7), &successor)
            .unwrap();

        let mut wrong_offer =
            AttestationEvidenceV1::decode_canonical(&transition_evidence).unwrap();
        let AttestationEvidenceV1::Dcap(wrong_offer) = &mut wrong_offer else {
            unreachable!();
        };
        let proof = wrong_offer.transition_key_ready_proof.as_mut().unwrap();
        proof.resident_offer_public = [0xc1; 32];
        proof.candidate_attestation_signature = new_enclave
            .sign(proof.signing_hash().unwrap().as_slice())
            .to_bytes();
        let wrong_call = IRegisterEnclaveV1Test::transitionEnclaveMeasurementCall {
            evidence: AttestationEvidenceV1::Dcap(wrong_offer.clone())
                .encode_canonical()
                .unwrap()
                .into(),
            nodeSignature: transition_node.to_vec().into(),
            enclaveSignature: transition_enclave.to_vec().into(),
        }
        .abi_encode();
        assert!(dispatch_transition_after_verifier_for_test(
            storage.clone(),
            node_signer.address(),
            &wrong_call,
            &transition,
            PostVerifierDcapCapabilityV1::new(next_verdict.clone()),
        )
        .is_err());
        assert_eq!(
            registry
                .validator_enclave_binding_v1(node_signer.address())
                .unwrap()
                .unwrap()
                .enclave_id,
            initial.enclave_id
        );
        dispatch_transition_after_verifier_for_test(
            storage.clone(),
            node_signer.address(),
            &call,
            &transition,
            PostVerifierDcapCapabilityV1::new(next_verdict),
        )
        .unwrap();

        let registry = TeeRegistry::new(storage);
        let binding = registry
            .validator_enclave_binding_v1(node_signer.address())
            .unwrap()
            .unwrap();
        assert_eq!(binding.enclave_id, transition.enclave_id);
        assert_eq!(binding.mrenclave, B256::repeat_byte(0x92));
        assert_eq!(binding.transition_nonce, 1);
        assert_eq!(binding.policy_hash, successor.policy_hash().unwrap());
        assert_eq!(registry.active_policy_v1().unwrap(), current);
    });
}

#[test]
fn activation_preserves_old_lease_but_old_policy_cannot_register_renew_or_replace() {
    let genesis_hash = B256::repeat_byte(0x19);
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
    let proposal_id = U256::from(10);
    let node_signer = OutbeEvmSigner::from_secret_bytes([0x37; 32]).unwrap();
    let enclave_signer = ed25519_dalek::SigningKey::from_bytes(&[0x38; 32]);
    let initial = registration_intent(
        &current,
        &node_signer,
        CONSENSUS_KEY,
        &enclave_signer,
        0x55,
        0x65,
    );
    let renewal = renewal_intent(&initial, NOW + 6_000);
    let replacement_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x39; 32]);
    let replacement = replacement_intent(&initial, &replacement_enclave, 0x56, 0x66, NOW + 6_000);
    let newcomer_signer = OutbeEvmSigner::from_secret_bytes([0x3c; 32]).unwrap();
    let newcomer_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x3d; 32]);
    let newcomer_consensus_key = [0x3e; 48];
    let newcomer = registration_intent(
        &current,
        &newcomer_signer,
        newcomer_consensus_key,
        &newcomer_enclave,
        0x57,
        0x67,
    );
    let (initial_node, initial_enclave) = signatures(&initial, &node_signer, &enclave_signer);
    let (renewal_node, renewal_enclave) = signatures(&renewal, &node_signer, &enclave_signer);
    let (replacement_node, replacement_signature) =
        signatures(&replacement, &node_signer, &replacement_enclave);
    let (newcomer_node, newcomer_signature) =
        signatures(&newcomer, &newcomer_signer, &newcomer_enclave);
    let mut provider = storage(genesis_hash);

    StorageHandle::enter(&mut provider, |storage| {
        register_validator(storage.clone(), &node_signer, CONSENSUS_KEY);
        let mut registry = TeeRegistry::new(storage);
        registry.install_initial_policy_v1(&current).unwrap();
        register_same_key_node_for_lifecycle_test(
            &mut registry,
            &initial,
            &node_signer,
            &initial_node,
            &initial_enclave,
            PostVerifierDcapCapabilityV1::new(verdict(DcapPlatformTcbStatusV1::UpToDate)),
        )
        .unwrap();
        registry
            .stage_successor_policy_v1(proposal_id, &successor)
            .unwrap();
    });

    provider.set_block_number(50);
    StorageHandle::enter(&mut provider, |storage| {
        register_validator(storage.clone(), &newcomer_signer, newcomer_consensus_key);
        let mut registry = TeeRegistry::new(storage);
        registry
            .promote_staged_successor_policy_v1(proposal_id, 50)
            .unwrap();
        assert!(registry
            .is_validator_enclave_ready_v1(node_signer.address())
            .unwrap());
        assert!(revert_message(
            registry
                .register_enclave_after_verifier_for_test(
                    &newcomer,
                    &newcomer_node,
                    &newcomer_signature,
                    PostVerifierDcapCapabilityV1::new(verdict(DcapPlatformTcbStatusV1::UpToDate,)),
                )
                .unwrap_err()
        )
        .contains("authoritative V1 policy"));
        assert!(revert_message(
            registry
                .renew_enclave_after_verifier_for_test(
                    &renewal,
                    &renewal_node,
                    &renewal_enclave,
                    PostVerifierDcapCapabilityV1::new(verdict(DcapPlatformTcbStatusV1::UpToDate,)),
                )
                .unwrap_err()
        )
        .contains("authoritative V1 policy"));
        assert!(revert_message(
            registry
                .replace_enclave_binding_after_verifier_for_test(
                    &replacement,
                    &replacement_node,
                    &replacement_signature,
                    PostVerifierDcapCapabilityV1::new(verdict(DcapPlatformTcbStatusV1::UpToDate,)),
                )
                .unwrap_err()
        )
        .contains("authoritative V1 policy"));
    });
}

#[test]
fn full_node_uses_the_same_bounded_transition_abi_and_staged_policy() {
    let genesis_hash = B256::repeat_byte(0x18);
    let current = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
    let mut successor = current.clone();
    successor.policy_version = 2;
    successor.activation_height = 50;
    successor.predecessor_policy_hash = current.policy_hash().unwrap();
    for rule in &mut successor.measurement_rules {
        rule.mrenclave = B256::repeat_byte(0x93);
        rule.admit_from_height = 50;
        rule.admit_until_height_exclusive = 500;
    }

    let node_signer = k256::ecdsa::SigningKey::from_bytes((&[0x34; 32]).into()).unwrap();
    let admission_signer = OutbeEvmSigner::from_secret_bytes([0x37; 32]).unwrap();
    let old_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x35; 32]);
    let new_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x36; 32]);
    let initial = full_node_registration_intent(&current, &node_signer, &old_enclave, 0x53, 0x63);
    let transition =
        measurement_transition_intent(&initial, &successor, &new_enclave, 0x54, 0x64, NOW + 3_600);
    let (initial_node, initial_enclave) =
        full_node_signatures(&initial, &node_signer, &old_enclave);
    let (node_binding, validator_signature, node_binding_signature) =
        validator_node_binding_authorization_for_p2p_node(
            &initial,
            &admission_signer,
            &node_signer,
        );
    let (transition_node, transition_enclave) =
        full_node_signatures(&transition, &node_signer, &new_enclave);
    let call = IRegisterEnclaveV1Test::transitionEnclaveMeasurementCall {
        evidence: transition_evidence(&transition, &new_enclave).into(),
        nodeSignature: transition_node.to_vec().into(),
        enclaveSignature: transition_enclave.to_vec().into(),
    }
    .abi_encode();
    let mut next_verdict = verdict(DcapPlatformTcbStatusV1::UpToDate);
    next_verdict.mrenclave = B256::repeat_byte(0x93);
    let p2p_public = full_node_public(&initial);
    let mut provider = storage(genesis_hash);

    StorageHandle::enter(&mut provider, |storage| {
        let mut registry = TeeRegistry::new(storage.clone());
        registry.install_initial_policy_v1(&current).unwrap();
        install_offer_key(&mut registry, &current);
        registry
            .register_enclave_and_bind_after_verifier_for_test(
                &initial,
                &initial_node,
                &initial_enclave,
                &node_binding,
                &validator_signature,
                &node_binding_signature,
                PostVerifierDcapCapabilityV1::new(verdict(DcapPlatformTcbStatusV1::UpToDate)),
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

        let binding = TeeRegistry::new(storage)
            .node_host_enclave_binding_v1(p2p_public)
            .unwrap()
            .unwrap();
        assert_eq!(binding.enclave_id, transition.enclave_id);
        assert_eq!(binding.mrenclave, B256::repeat_byte(0x93));
        assert_eq!(binding.transition_nonce, 1);
        assert_eq!(binding.policy_hash, successor.policy_hash().unwrap());
    });
}
