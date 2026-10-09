use super::*;

/// The onboarding artifact of `intent` with the offer key `offer_public`.
/// `nonce_fill` and `ciphertext_fill` fill the sealed offer payload.
fn offer_artifact(
    intent: &RegistrationIntentV1,
    offer_public: B256,
    nonce_fill: u8,
    ciphertext_fill: u8,
) -> DcapOnboardingArtifactV1 {
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
        nonce: [nonce_fill; 12],
        ciphertext: vec![ciphertext_fill; 112],
    }
}

fn sealed_offer_artifact(intent: &RegistrationIntentV1, offer_public: B256, fill: u8) -> Vec<u8> {
    offer_artifact(intent, offer_public, fill, fill)
        .encode_canonical()
        .unwrap()
}

/// The `OfferKeySealedForRegistryV1` events of `provider`, in emission order.
fn offer_key_sealed_events(provider: &HashMapStorageProvider) -> Vec<&alloy_primitives::Log> {
    provider
        .get_ordered_events()
        .iter()
        .filter(|log| log.topics().first() == Some(&OfferKeySealedForRegistryV1::SIGNATURE_HASH))
        .collect()
}

/// Asserts that `provider` emitted exactly one `OfferKeySealedForRegistryV1`
/// event. Returns the decoded event.
fn single_offer_key_sealed(
    provider: &HashMapStorageProvider,
) -> alloy_primitives::Log<OfferKeySealedForRegistryV1> {
    let events = offer_key_sealed_events(provider);
    assert_eq!(events.len(), 1);
    OfferKeySealedForRegistryV1::decode_log(events[0]).unwrap()
}

/// Dispatches the `registerEnclave` call `call` of the initial intent of
/// `validator` on `provider` with an up-to-date verdict. The enclave seals the
/// offer key with `reseal`.
fn dispatch_onboarding(
    provider: &mut HashMapStorageProvider,
    validator: &LifecycleValidator,
    call: &[u8],
    reseal: impl FnOnce([u8; 32]) -> Result<Option<Vec<u8>>, String>,
) -> V1RegistrationOutcome {
    StorageHandle::enter(provider, |storage| {
        dispatch_register_with_onboarding_after_verifier_for_test(
            PostVerifierCall::new(
                storage,
                validator.node_signer.address(),
                call,
                &validator.initial,
            ),
            PostVerifierDcapCapabilityV1::new(verdict(DcapPlatformTcbStatusV1::UpToDate)),
            reseal,
        )
        .unwrap()
    })
}

#[test]
fn public_v1_registration_emits_onboarding_only_for_created_binding() {
    let genesis_hash = B256::repeat_byte(0x2A);
    let validator = LifecycleValidator::new(
        hardening_policy(genesis_hash),
        0x31,
        0x32,
        EnclaveBindingSeeds::new(0x33, 0x34),
    );
    let call = validator.register_call(
        &validator.initial,
        &validator.enclave_signer,
        &[0xA8; 4_096],
    );
    let offer_public = B256::repeat_byte(0x41);
    let sealed = sealed_offer_artifact(&validator.initial, offer_public, 0x42);
    let node_id_hash = validator.initial.node_id.node_id_hash().unwrap();
    let mut provider = validator.run_as_validator(|_storage, registry| {
        registry
            .tribute_offer_public_key
            .write(offer_public)
            .unwrap();
    });

    let created = dispatch_onboarding(&mut provider, &validator, &call, |recipient| {
        assert_eq!(recipient, validator.initial.recipient_x25519);
        Ok(Some(sealed.clone()))
    });
    assert_eq!(created, V1RegistrationOutcome::Created);

    let decoded = single_offer_key_sealed(&provider);
    assert_eq!(decoded.nodeIdHash, node_id_hash);
    assert_eq!(decoded.sealedOfferKey.as_ref(), sealed.as_slice());

    let idempotent = dispatch_onboarding(&mut provider, &validator, &call, |_| {
        panic!("idempotent registration must not ask the enclave to reseal the offer key")
    });
    assert_eq!(idempotent, V1RegistrationOutcome::Idempotent);
    assert_eq!(
        offer_key_sealed_events(&provider).len(),
        1,
        "idempotent replay must not redeliver the permanent offer key"
    );
}

#[test]
fn verified_onboarding_artifact_must_match_the_committed_binding_exactly() {
    let genesis_hash = B256::repeat_byte(0x91);
    let validator = LifecycleValidator::new(
        hardening_policy(genesis_hash),
        0x92,
        0x93,
        EnclaveBindingSeeds::new(0x94, 0x95),
    );
    let signed_intent = validator.signed_initial();
    let node_id_hash = validator.initial.node_id.node_id_hash().unwrap();
    let offer_public = B256::repeat_byte(0x96);
    let artifact = offer_artifact(&validator.initial, offer_public, 0x97, 0x98);
    let mut provider = validator.run_as_validator(|_storage, mut registry| {
        registry
            .tribute_offer_public_key
            .write(offer_public)
            .unwrap();
        let registration = registry
            .register_enclave_after_verifier_for_test(
                signed_intent.with_verdict(verdict(DcapPlatformTcbStatusV1::UpToDate)),
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
    assert_eq!(
        single_offer_key_sealed(&provider).sealedOfferKey.as_ref(),
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
