use super::*;

fn sealed_offer_artifact(intent: &RegistrationIntentV1, offer_public: B256, fill: u8) -> Vec<u8> {
    DcapOnboardingArtifactV1 {
        context: DcapOnboardingContextV1 {
            chain_id: intent.chain_id,
            genesis_hash: intent.genesis_hash,
            intent_hash: intent.intent_hash().unwrap(),
            node_id_hash: intent.node_id.node_id_hash().unwrap(),
            enclave_id: intent.derived_enclave_id().unwrap(),
            binding_id: intent.binding_id,
            policy_hash: intent.policy_hash,
            recipient_x25519: intent.recipient_x25519,
            tribute_offer_public: offer_public.0,
            key_epoch: 0,
            tribute_offer_epoch: 0,
        },
        nonce: [fill; 12],
        ciphertext: vec![fill; 112],
    }
    .encode_canonical()
    .unwrap()
}

#[test]
fn public_v1_registration_emits_onboarding_only_for_created_binding() {
    let genesis_hash = B256::repeat_byte(0x2A);
    let active_policy = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
    let node_signer = OutbeEvmSigner::from_secret_bytes([0x31; 32]).unwrap();
    let enclave_signer = ed25519_dalek::SigningKey::from_bytes(&[0x32; 32]);
    let intent = registration_intent(
        &active_policy,
        &node_signer,
        CONSENSUS_KEY,
        &enclave_signer,
        0x33,
        0x34,
    );
    let (node_signature, enclave_signature) = signatures(&intent, &node_signer, &enclave_signer);
    let (binding, validator_signature, node_binding_signature) =
        validator_node_binding_authorization_for_evm_node(&intent, &node_signer, &node_signer);
    let call = IRegisterEnclaveV1Test::registerEnclaveCall {
        evidence: vec![0xA8; 4_096].into(),
        nodeSignature: node_signature.to_vec().into(),
        enclaveSignature: enclave_signature.to_vec().into(),
        validatorNodeBinding: binding.encode_canonical().unwrap().into(),
        validatorSignature: validator_signature.to_vec().into(),
        nodeBindingSignature: node_binding_signature.to_vec().into(),
    }
    .abi_encode();
    let offer_public = B256::repeat_byte(0x41);
    let sealed = sealed_offer_artifact(&intent, offer_public, 0x42);
    let node_id_hash = intent.node_id.node_id_hash().unwrap();
    let mut provider = storage(genesis_hash);

    StorageHandle::enter(&mut provider, |storage| {
        register_validator(storage.clone(), &node_signer, CONSENSUS_KEY);
        let mut registry = TeeRegistry::new(storage);
        registry.install_initial_policy_v1(&active_policy).unwrap();
        registry
            .tribute_offer_public_key
            .write(offer_public)
            .unwrap();
    });

    let created = StorageHandle::enter(&mut provider, |storage| {
        dispatch_register_with_onboarding_after_verifier_for_test(
            storage,
            node_signer.address(),
            &call,
            &intent,
            PostVerifierDcapCapabilityV1::new(verdict(DcapPlatformTcbStatusV1::UpToDate)),
            |recipient| {
                assert_eq!(recipient, intent.recipient_x25519);
                Ok(Some(sealed.clone()))
            },
        )
        .unwrap()
    });
    assert_eq!(created, V1RegistrationOutcome::Created);

    let onboarding = provider
        .get_ordered_events()
        .iter()
        .filter(|log| log.topics().first() == Some(&OfferKeySealedForRegistryV1::SIGNATURE_HASH))
        .collect::<Vec<_>>();
    assert_eq!(onboarding.len(), 1);
    let decoded = OfferKeySealedForRegistryV1::decode_log(onboarding[0]).unwrap();
    assert_eq!(decoded.nodeIdHash, node_id_hash);
    assert_eq!(decoded.sealedOfferKey.as_ref(), sealed.as_slice());

    let idempotent = StorageHandle::enter(&mut provider, |storage| {
        dispatch_register_with_onboarding_after_verifier_for_test(
            storage,
            node_signer.address(),
            &call,
            &intent,
            PostVerifierDcapCapabilityV1::new(verdict(DcapPlatformTcbStatusV1::UpToDate)),
            |_| panic!("idempotent registration must not ask the enclave to reseal the offer key"),
        )
        .unwrap()
    });
    assert_eq!(idempotent, V1RegistrationOutcome::Idempotent);
    assert_eq!(
        provider
            .get_ordered_events()
            .iter()
            .filter(|log| {
                log.topics().first() == Some(&OfferKeySealedForRegistryV1::SIGNATURE_HASH)
            })
            .count(),
        1,
        "idempotent replay must not redeliver the permanent offer key"
    );
}

#[test]
fn verified_onboarding_artifact_must_match_the_committed_binding_exactly() {
    let genesis_hash = B256::repeat_byte(0x91);
    let active_policy = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
    let node_signer = OutbeEvmSigner::from_secret_bytes([0x92; 32]).unwrap();
    let enclave_signer = ed25519_dalek::SigningKey::from_bytes(&[0x93; 32]);
    let intent = registration_intent(
        &active_policy,
        &node_signer,
        CONSENSUS_KEY,
        &enclave_signer,
        0x94,
        0x95,
    );
    let (node_signature, enclave_signature) = signatures(&intent, &node_signer, &enclave_signer);
    let node_id_hash = intent.node_id.node_id_hash().unwrap();
    let offer_public = B256::repeat_byte(0x96);
    let artifact = DcapOnboardingArtifactV1 {
        context: DcapOnboardingContextV1 {
            chain_id: intent.chain_id,
            genesis_hash: intent.genesis_hash,
            intent_hash: intent.intent_hash().unwrap(),
            node_id_hash,
            enclave_id: intent.derived_enclave_id().unwrap(),
            binding_id: intent.binding_id,
            policy_hash: intent.policy_hash,
            recipient_x25519: intent.recipient_x25519,
            tribute_offer_public: offer_public.0,
            key_epoch: 0,
            tribute_offer_epoch: 0,
        },
        nonce: [0x97; 12],
        ciphertext: vec![0x98; 112],
    };
    let mut provider = storage(genesis_hash);
    StorageHandle::enter(&mut provider, |storage| {
        register_validator(storage.clone(), &node_signer, CONSENSUS_KEY);
        let mut registry = TeeRegistry::new(storage);
        registry.install_initial_policy_v1(&active_policy).unwrap();
        registry
            .tribute_offer_public_key
            .write(offer_public)
            .unwrap();
        let registration = registry
            .register_enclave_after_verifier_for_test(
                &intent,
                &node_signature,
                &enclave_signature,
                PostVerifierDcapCapabilityV1::new(verdict(DcapPlatformTcbStatusV1::UpToDate)),
            )
            .unwrap();
        registry
            .emit_verified_onboarding_artifact_v1(
                &V1OnboardingOutcome {
                    registration,
                    artifact: Some(artifact.clone()),
                },
                node_id_hash,
            )
            .unwrap();
    });
    let events = provider
        .get_ordered_events()
        .iter()
        .filter(|event| {
            event.topics().first() == Some(&OfferKeySealedForRegistryV1::SIGNATURE_HASH)
        })
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 1);
    assert_eq!(
        OfferKeySealedForRegistryV1::decode_log(events[0])
            .unwrap()
            .sealedOfferKey
            .as_ref(),
        artifact.encode_canonical().unwrap().as_slice()
    );

    let error = StorageHandle::enter(&mut provider, |storage| {
        let mut wrong = artifact;
        wrong.context.intent_hash = B256::repeat_byte(0xff);
        TeeRegistry::new(storage)
            .emit_verified_onboarding_artifact_v1(
                &V1OnboardingOutcome {
                    registration: V1RegistrationOutcome::Created,
                    artifact: Some(wrong),
                },
                node_id_hash,
            )
            .unwrap_err()
    });
    assert!(matches!(error, PrecompileError::Fatal(_)));
}
