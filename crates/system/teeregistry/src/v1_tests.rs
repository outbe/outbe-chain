use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::{sol, SolCall, SolEvent};
use ed25519_dalek::Signer as _;
use k256::ecdsa::signature::hazmat::PrehashSigner as _;
use outbe_primitives::{
    error::PrecompileError,
    signer::OutbeEvmSigner,
    storage::{hashmap::HashMapStorageProvider, PrecompileStorageProvider, StorageHandle},
    tee_attestation_v1::{
        AttestationEvidenceV1, AttestationMode, AttestationOperationV1, DcapCollateralComponentV1,
        DcapCollateralKind, DcapEvidenceV1, EnclaveInitializationManifestV1, NodeIdV1,
        PlatformTcbStatusSetV1, QvlTcbStatusV1, RegistrationIntentV1, RegistryMutatorV1,
        TeeMeasurementRuleV1, TeePolicyV1, TeeRegistryGasScheduleV1, TransitionKeyReadyProofV1,
        ValidatorNodeBindingV1,
    },
};
use outbe_tee::dcap_protocol::{
    DcapOnboardingArtifactV1, DcapOnboardingContextV1, DcapPckCaV1, DcapPlatformTcbStatusV1,
    DcapVerdictV1,
};
use outbe_validatorset::contract::ValidatorSet;

use crate::{
    runtime::TeeBootstrapData,
    schema::TeeRegistry,
    v1::{
        OfferKeySealedForRegistryV1, PostVerifierDcapCapabilityV1, V1OnboardingOutcome,
        V1RegistrationOutcome,
    },
    v1_precompile::{
        dispatch_register_after_verifier_for_test,
        dispatch_register_with_onboarding_after_verifier_for_test,
        dispatch_renew_after_verifier_for_test, dispatch_replace_after_verifier_for_test,
        dispatch_transition_after_verifier_for_test,
    },
};

sol! {
    interface IRegisterEnclaveV1Test {
        function registerEnclave(
            bytes calldata evidence,
            bytes calldata nodeSignature,
            bytes calldata enclaveSignature,
            bytes calldata validatorNodeBinding,
            bytes calldata validatorSignature,
            bytes calldata nodeBindingSignature
        ) external returns (bool);

        function renewEnclave(
            bytes calldata evidence,
            bytes calldata nodeSignature,
            bytes calldata enclaveSignature
        ) external returns (bool);

        function replaceEnclaveBinding(
            bytes calldata evidence,
            bytes calldata nodeSignature,
            bytes calldata enclaveSignature
        ) external returns (bool);

        function transitionEnclaveMeasurement(
            bytes calldata evidence,
            bytes calldata nodeSignature,
            bytes calldata enclaveSignature
        ) external returns (bool);
    }
}

const CHAIN_ID: u64 = 1;
const NOW: u64 = 10_000;
const MRENCLAVE: B256 = B256::repeat_byte(0x81);
const MRSIGNER: B256 = B256::repeat_byte(0x82);
const CONSENSUS_KEY: [u8; 48] = [0x32; 48];
const NODE_HOST_NOISE_X25519: [u8; 32] = [0xa5; 32];
const OFFER_PUBLIC: [u8; 32] = [0xb1; 32];

fn policy(genesis_hash: B256, statuses: PlatformTcbStatusSetV1) -> TeePolicyV1 {
    TeePolicyV1 {
        policy_version: 1,
        chain_id: U256::from(CHAIN_ID).to_be_bytes(),
        genesis_hash,
        activation_height: 1,
        predecessor_policy_hash: B256::ZERO,
        attestation_mode: AttestationMode::DcapRequired,
        intel_root_der_hash: B256::repeat_byte(0x71),
        quote_version: 3,
        tee_type: 0,
        attestation_key_type: 2,
        qe_vendor_id: [
            0x93, 0x9a, 0x72, 0x33, 0xf7, 0x9c, 0x4c, 0xa9, 0x94, 0x0a, 0x0d, 0xb3, 0x95, 0x7f,
            0x06, 0x07,
        ],
        certification_data_type: 5,
        tcb_info_schema_version: 3,
        qe_identity_schema_version: 2,
        minimum_tcb_evaluation_data_number: 1,
        accepted_platform_tcb_statuses: statuses,
        accepted_qe_tcb_status: QvlTcbStatusV1::UpToDate,
        minimum_lease: 3_600,
        maximum_lease: 604_800,
        collateral_margin: 3_600,
        resource_schedule_hash: B256::repeat_byte(0x72),
        measurement_rules: vec![TeeMeasurementRuleV1 {
            mrenclave: MRENCLAVE,
            mrsigner: MRSIGNER,
            isv_prod_id: 7,
            minimum_isv_svn: 3,
            admit_from_height: 1,
            admit_until_height_exclusive: 100,
        }],
    }
}

fn storage(genesis_hash: B256) -> HashMapStorageProvider {
    let mut storage = HashMapStorageProvider::new_with_chain_identity(CHAIN_ID, genesis_hash);
    storage.set_block_number(10);
    storage.set_timestamp(U256::from(NOW));
    storage
}

fn sealed_offer_artifact(offer_public: B256, fill: u8) -> Vec<u8> {
    let mut sealed = vec![fill; outbe_tee::protocol::MIN_SEALED_OFFER_KEY_FOR_REGISTRY_BYTES];
    sealed[..32].copy_from_slice(offer_public.as_slice());
    sealed
}

fn register_validator(
    storage: StorageHandle<'_>,
    signer: &OutbeEvmSigner,
    consensus_key: [u8; 48],
) {
    ValidatorSet::new(storage)
        .register_validator(Address::ZERO, signer.address(), &consensus_key)
        .expect("genesis-owner validator registration");
}

fn reth_p2p_public_for_evm_signer(node_signer: &OutbeEvmSigner) -> [u8; 33] {
    let proof_hash = B256::repeat_byte(0xA7);
    let proof = node_signer.sign_hash(&proof_hash).unwrap();
    let proof_signature = k256::ecdsa::Signature::from_slice(&proof[..64]).unwrap();
    let proof_recovery = k256::ecdsa::RecoveryId::from_byte(proof[64]).unwrap();
    k256::ecdsa::VerifyingKey::recover_from_prehash(
        proof_hash.as_slice(),
        &proof_signature,
        proof_recovery,
    )
    .unwrap()
    .to_encoded_point(true)
    .as_bytes()
    .try_into()
    .unwrap()
}

fn initialization_manifest_for_intent(
    intent: &RegistrationIntentV1,
    challenge: [u8; 32],
) -> EnclaveInitializationManifestV1 {
    EnclaveInitializationManifestV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        node_id: intent.node_id.clone(),
        initialization_challenge: challenge,
        node_host_noise_x25519: NODE_HOST_NOISE_X25519,
        recipient_x25519: intent.recipient_x25519,
        attestation_ed25519: intent.attestation_ed25519,
        noise_responder_x25519: intent.noise_responder_x25519,
    }
}

fn bind_reachable_node_host_authorization(intent: &mut RegistrationIntentV1, challenge: [u8; 32]) {
    let manifest = initialization_manifest_for_intent(intent, challenge);
    intent.node_host_authorization_hash = manifest.node_host_authorization_hash().unwrap();
    manifest.validate_intent_binding(intent).unwrap();
}

fn registration_intent(
    policy: &TeePolicyV1,
    node_signer: &OutbeEvmSigner,
    _consensus_key: [u8; 48],
    enclave_signer: &ed25519_dalek::SigningKey,
    binding_seed: u8,
    key_seed: u8,
) -> RegistrationIntentV1 {
    let mut intent = RegistrationIntentV1 {
        chain_id: policy.chain_id,
        genesis_hash: policy.genesis_hash,
        operation: AttestationOperationV1::RegisterEnclave,
        attestation_mode: AttestationMode::DcapRequired,
        policy_hash: policy.policy_hash().unwrap(),
        node_id: NodeIdV1 {
            reth_p2p_public: reth_p2p_public_for_evm_signer(node_signer),
        },
        enclave_id: B256::repeat_byte(0x01),
        binding_id: B256::repeat_byte(binding_seed),
        binding_version: 1,
        registration_version: 0,
        renewal_nonce: 0,
        transition_nonce: 0,
        requested_valid_until: NOW + 3_600,
        recipient_x25519: [key_seed; 32],
        attestation_ed25519: enclave_signer.verifying_key().to_bytes(),
        noise_responder_x25519: [key_seed.wrapping_add(1); 32],
        node_host_authorization_hash: B256::repeat_byte(1),
    };
    intent.enclave_id = intent.derived_enclave_id().unwrap();
    bind_reachable_node_host_authorization(&mut intent, [0xa6; 32]);
    intent
}

fn full_node_registration_intent(
    policy: &TeePolicyV1,
    node_signer: &k256::ecdsa::SigningKey,
    enclave_signer: &ed25519_dalek::SigningKey,
    binding_seed: u8,
    key_seed: u8,
) -> RegistrationIntentV1 {
    let reth_p2p_public = node_signer.verifying_key().to_encoded_point(true);
    let mut intent = RegistrationIntentV1 {
        chain_id: policy.chain_id,
        genesis_hash: policy.genesis_hash,
        operation: AttestationOperationV1::RegisterEnclave,
        attestation_mode: AttestationMode::DcapRequired,
        policy_hash: policy.policy_hash().unwrap(),
        node_id: NodeIdV1 {
            reth_p2p_public: reth_p2p_public.as_bytes().try_into().unwrap(),
        },
        enclave_id: B256::repeat_byte(0x01),
        binding_id: B256::repeat_byte(binding_seed),
        binding_version: 1,
        registration_version: 0,
        renewal_nonce: 0,
        transition_nonce: 0,
        requested_valid_until: NOW + 3_600,
        recipient_x25519: [key_seed; 32],
        attestation_ed25519: enclave_signer.verifying_key().to_bytes(),
        noise_responder_x25519: [key_seed.wrapping_add(1); 32],
        node_host_authorization_hash: B256::repeat_byte(1),
    };
    intent.enclave_id = intent.derived_enclave_id().unwrap();
    bind_reachable_node_host_authorization(&mut intent, [0xa6; 32]);
    intent
}

fn full_node_signatures(
    intent: &RegistrationIntentV1,
    node_signer: &k256::ecdsa::SigningKey,
    enclave_signer: &ed25519_dalek::SigningKey,
) -> ([u8; 65], [u8; 64]) {
    let hash = intent.intent_hash().unwrap();
    let (signature, recovery): (k256::ecdsa::Signature, k256::ecdsa::RecoveryId) = node_signer
        .sign_prehash(hash.as_slice())
        .expect("test P2P key signs registration intent");
    let mut node_signature = [0_u8; 65];
    node_signature[..64].copy_from_slice(signature.to_bytes().as_slice());
    node_signature[64] = recovery.to_byte();
    (
        node_signature,
        enclave_signer.sign(hash.as_slice()).to_bytes(),
    )
}

fn validator_node_binding_authorization_for_p2p_node(
    intent: &RegistrationIntentV1,
    admission_signer: &OutbeEvmSigner,
    node_signer: &k256::ecdsa::SigningKey,
) -> (ValidatorNodeBindingV1, [u8; 65], [u8; 65]) {
    let binding = ValidatorNodeBindingV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        validator: admission_signer.address().into_array(),
        node_id_hash: intent.node_id.node_id_hash().unwrap(),
    };
    let binding_hash = binding.binding_hash().unwrap();
    let validator_signature = admission_signer.sign_hash(&binding_hash).unwrap();
    let (signature, recovery) = node_signer.sign_prehash(binding_hash.as_slice()).unwrap();
    let mut node_signature = [0_u8; 65];
    node_signature[..64].copy_from_slice(signature.to_bytes().as_slice());
    node_signature[64] = recovery.to_byte();
    (binding, validator_signature, node_signature)
}

fn validator_node_binding_authorization_for_evm_node(
    intent: &RegistrationIntentV1,
    admission_signer: &OutbeEvmSigner,
    node_signer: &OutbeEvmSigner,
) -> (ValidatorNodeBindingV1, [u8; 65], [u8; 65]) {
    let binding = ValidatorNodeBindingV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        validator: admission_signer.address().into_array(),
        node_id_hash: intent.node_id.node_id_hash().unwrap(),
    };
    let binding_hash = binding.binding_hash().unwrap();
    (
        binding,
        admission_signer.sign_hash(&binding_hash).unwrap(),
        node_signer.sign_hash(&binding_hash).unwrap(),
    )
}

fn full_node_public(intent: &RegistrationIntentV1) -> [u8; 33] {
    intent.node_id.reth_p2p_public
}

fn signatures(
    intent: &RegistrationIntentV1,
    node_signer: &OutbeEvmSigner,
    enclave_signer: &ed25519_dalek::SigningKey,
) -> ([u8; 65], [u8; 64]) {
    let hash = intent.intent_hash().unwrap();
    (
        node_signer.sign_hash(&hash).unwrap(),
        enclave_signer.sign(hash.as_slice()).to_bytes(),
    )
}

fn register_same_key_node_for_lifecycle_test(
    registry: &mut TeeRegistry<'_>,
    intent: &RegistrationIntentV1,
    node_signer: &OutbeEvmSigner,
    node_signature: &[u8; 65],
    enclave_signature: &[u8; 64],
    capability: PostVerifierDcapCapabilityV1,
) -> Result<V1RegistrationOutcome, PrecompileError> {
    let (binding, validator_signature, node_binding_signature) =
        validator_node_binding_authorization_for_evm_node(intent, node_signer, node_signer);
    registry.register_enclave_and_bind_after_verifier_for_test(
        intent,
        node_signature,
        enclave_signature,
        &binding,
        &validator_signature,
        &node_binding_signature,
        capability,
    )
}

fn verdict(status: DcapPlatformTcbStatusV1) -> DcapVerdictV1 {
    DcapVerdictV1 {
        mrenclave: MRENCLAVE,
        mrsigner: MRSIGNER,
        isv_prod_id: 7,
        isv_svn: 4,
        pck_ca: DcapPckCaV1::Processor,
        fmspc: [0x91; 6],
        pce_id: 2,
        platform_tcb_status: status,
        advisory_ids: Vec::new(),
        tcb_evaluation_data_number: 17,
        qe_tcb_evaluation_data_number: 17,
        collateral_valid_until: NOW + 7_200,
    }
}

fn renewal_intent(
    current: &RegistrationIntentV1,
    requested_valid_until: u64,
) -> RegistrationIntentV1 {
    let mut intent = current.clone();
    intent.operation = AttestationOperationV1::RenewEnclave;
    intent.registration_version += 1;
    intent.renewal_nonce += 1;
    intent.requested_valid_until = requested_valid_until;
    intent
}

fn replacement_intent(
    current: &RegistrationIntentV1,
    enclave_signer: &ed25519_dalek::SigningKey,
    binding_seed: u8,
    key_seed: u8,
    requested_valid_until: u64,
) -> RegistrationIntentV1 {
    let mut intent = current.clone();
    intent.operation = AttestationOperationV1::ReplaceEnclaveBinding;
    intent.binding_id = B256::repeat_byte(binding_seed);
    intent.binding_version += 1;
    intent.registration_version += 1;
    intent.requested_valid_until = requested_valid_until;
    intent.recipient_x25519 = [key_seed; 32];
    intent.attestation_ed25519 = enclave_signer.verifying_key().to_bytes();
    intent.noise_responder_x25519 = [key_seed.wrapping_add(1); 32];
    intent.enclave_id = intent.derived_enclave_id().unwrap();
    let manifest = initialization_manifest_for_intent(&intent, [0xa7; 32]);
    assert_eq!(
        manifest.node_host_authorization_hash().unwrap(),
        current.node_host_authorization_hash
    );
    manifest.validate_intent_binding(&intent).unwrap();
    intent
}

fn measurement_transition_intent(
    current: &RegistrationIntentV1,
    next_policy: &TeePolicyV1,
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
    intent.operation = AttestationOperationV1::TransitionEnclaveMeasurement;
    intent.transition_nonce += 1;
    intent.policy_hash = next_policy.policy_hash().unwrap();
    intent
}

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

fn revert_message(error: PrecompileError) -> String {
    match error {
        PrecompileError::Revert(message) => message,
        other => panic!("expected deterministic revert, got {other:?}"),
    }
}

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
        assert!(revert_message(conflict).contains("already bound to another NodeHost"));
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
    let sealed = sealed_offer_artifact(offer_public, 0x42);
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

#[test]
fn onboarding_artifact_fails_closed_for_missing_enclave_and_malformed_output() {
    let offer_public = B256::repeat_byte(0x51);
    let node_id_hash = B256::repeat_byte(0x52);
    let recipient = [0x53; 32];
    let too_small = vec![0x54; outbe_tee::protocol::MIN_SEALED_OFFER_KEY_FOR_REGISTRY_BYTES - 1];
    let too_large = vec![0x55; outbe_tee::protocol::MAX_SEALED_OFFER_KEY_FOR_REGISTRY_BYTES + 1];
    let wrong_prefix = sealed_offer_artifact(B256::repeat_byte(0x56), 0x57);
    let cases = [
        ("missing enclave", None, "mandatory enclave is unavailable"),
        (
            "below minimum",
            Some(too_small),
            "malformed V1 offer-key onboarding artifact",
        ),
        (
            "above maximum",
            Some(too_large),
            "malformed V1 offer-key onboarding artifact",
        ),
        (
            "wrong offer prefix",
            Some(wrong_prefix),
            "malformed V1 offer-key onboarding artifact",
        ),
    ];

    for (case, supplied, expected) in cases {
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        let error = StorageHandle::enter(&mut provider, |storage| {
            let mut registry = TeeRegistry::new(storage);
            registry
                .tribute_offer_public_key
                .write(offer_public)
                .unwrap();
            registry
                .emit_offer_key_sealed_for_registry_v1_after_sealer_for_test(
                    V1RegistrationOutcome::Created,
                    node_id_hash,
                    recipient,
                    |_| Ok(supplied),
                )
                .unwrap_err()
        });
        assert!(
            matches!(error, PrecompileError::Fatal(ref message) if message.contains(expected)),
            "unexpected {case} outcome: {error:?}"
        );
        assert!(provider.get_ordered_events().is_empty(), "{case}");
    }

    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let error = StorageHandle::enter(&mut provider, |storage| {
        TeeRegistry::new(storage)
            .emit_offer_key_sealed_for_registry_v1_after_sealer_for_test(
                V1RegistrationOutcome::Created,
                node_id_hash,
                recipient,
                |_| panic!("zero commitment must fail before contacting the enclave"),
            )
            .unwrap_err()
    });
    assert!(matches!(
        error,
        PrecompileError::Fatal(message)
            if message.contains("requires the OST3 offer-key commitment")
    ));
    assert!(provider.get_ordered_events().is_empty());
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
        .contains("lease is outside"));

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
fn full_node_renewal_and_replacement_follow_the_shared_lease_lifecycle() {
    let genesis_hash = B256::repeat_byte(0x25);
    let active_policy = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
    let node_signer = k256::ecdsa::SigningKey::from_bytes((&[0x7B; 32]).into()).unwrap();
    let old_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x7C; 32]);
    let new_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x7D; 32]);
    let initial =
        full_node_registration_intent(&active_policy, &node_signer, &old_enclave, 0x6D, 0x6E);
    let renewal = renewal_intent(&initial, NOW + 6_000);
    let replacement = replacement_intent(&renewal, &new_enclave, 0x6F, 0x70, NOW + 6_000);
    let (initial_node, initial_enclave) =
        full_node_signatures(&initial, &node_signer, &old_enclave);
    let (renewal_node, renewal_enclave) =
        full_node_signatures(&renewal, &node_signer, &old_enclave);
    let (replacement_node, replacement_enclave) =
        full_node_signatures(&replacement, &node_signer, &new_enclave);
    let p2p_public = full_node_public(&initial);
    let mut accepted = verdict(DcapPlatformTcbStatusV1::UpToDate);
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
        assert_eq!(binding.binding_version, 2);
        assert_eq!(binding.registration_version, 2);
        assert_eq!(binding.renewal_nonce, 1);
        assert!(registry.is_node_host_enclave_ready_v1(p2p_public).unwrap());
    });
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
        .contains("different enclave binding"));

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

#[test]
fn initial_policy_is_state_authority_and_is_write_once() {
    let genesis_hash = B256::repeat_byte(0x14);
    let first = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
    let mut provider = storage(genesis_hash);
    let bootstrap_policy_hash = B256::repeat_byte(0xD1);
    StorageHandle::enter(&mut provider, |storage| {
        let mut registry = TeeRegistry::new(storage);
        registry.policy_hash.write(bootstrap_policy_hash).unwrap();
        registry.install_initial_policy_v1(&first).unwrap();
        registry.install_initial_policy_v1(&first).unwrap();
        assert_eq!(registry.active_policy_v1().unwrap(), first);
        assert_eq!(registry.policy_hash.read().unwrap(), bootstrap_policy_hash);
        assert_eq!(
            registry.active_v1_policy_hash.read().unwrap(),
            first.policy_hash().unwrap()
        );

        let mut conflicting = first.clone();
        conflicting.minimum_tcb_evaluation_data_number = 2;
        assert!(revert_message(
            registry
                .install_initial_policy_v1(&conflicting)
                .unwrap_err()
        )
        .contains("already installed"));
    });

    let mut wrong_chain_provider = storage(genesis_hash);
    let mut wrong_chain = first;
    wrong_chain.chain_id = U256::from(2).to_be_bytes();
    StorageHandle::enter(&mut wrong_chain_provider, |storage| {
        assert!(revert_message(
            TeeRegistry::new(storage)
                .install_initial_policy_v1(&wrong_chain)
                .unwrap_err()
        )
        .contains("chain identity mismatch"));
    });
}

#[test]
fn stages_exactly_one_predecessor_bound_successor_policy() {
    let genesis_hash = B256::repeat_byte(0x15);
    let current = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
    let mut successor = current.clone();
    successor.policy_version = 2;
    successor.activation_height = 50;
    successor.predecessor_policy_hash = current.policy_hash().unwrap();
    successor.accepted_platform_tcb_statuses = PlatformTcbStatusSetV1::UpToDateOnly;
    for rule in &mut successor.measurement_rules {
        rule.mrenclave = B256::repeat_byte(0x91);
        rule.admit_from_height = 50;
        rule.admit_until_height_exclusive = 500;
    }

    let mut provider = storage(genesis_hash);
    StorageHandle::enter(&mut provider, |storage| {
        let mut registry = TeeRegistry::new(storage);
        registry.install_initial_policy_v1(&current).unwrap();

        let mut wrong_genesis = successor.clone();
        wrong_genesis.genesis_hash = B256::repeat_byte(0xee);
        assert!(revert_message(
            registry
                .stage_successor_policy_v1(U256::from(5), &wrong_genesis)
                .unwrap_err()
        )
        .contains("chain identity"));

        let mut wrong_version = successor.clone();
        wrong_version.policy_version = 3;
        assert!(revert_message(
            registry
                .stage_successor_policy_v1(U256::from(6), &wrong_version)
                .unwrap_err()
        )
        .contains("current plus one"));

        registry
            .stage_successor_policy_v1(U256::from(7), &successor)
            .unwrap();
        registry
            .stage_successor_policy_v1(U256::from(7), &successor)
            .unwrap();
        assert_eq!(
            registry.staged_successor_policy_v1().unwrap(),
            Some((U256::from(7), successor.clone()))
        );

        let mut conflicting = successor.clone();
        conflicting.minimum_tcb_evaluation_data_number = 2;
        assert!(revert_message(
            registry
                .stage_successor_policy_v1(U256::from(8), &conflicting)
                .unwrap_err()
        )
        .contains("already staged"));

        let mut wrong_predecessor = successor.clone();
        wrong_predecessor.predecessor_policy_hash = B256::repeat_byte(0xee);
        assert!(revert_message(
            TeeRegistry::new(registry.storage.clone())
                .stage_successor_policy_v1(U256::from(9), &wrong_predecessor)
                .unwrap_err()
        )
        .contains("predecessor"));
    });
}

#[test]
fn promotes_staged_successor_exactly_at_activation_height_and_replays_idempotently() {
    let genesis_hash = B256::repeat_byte(0x17);
    let current = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
    let mut successor = current.clone();
    successor.policy_version = 2;
    successor.activation_height = 50;
    successor.predecessor_policy_hash = current.policy_hash().unwrap();
    successor.accepted_platform_tcb_statuses = PlatformTcbStatusSetV1::UpToDateOnly;
    for rule in &mut successor.measurement_rules {
        rule.admit_from_height = 50;
        rule.admit_until_height_exclusive = 500;
    }
    let proposal_id = U256::from(8);
    let mut provider = storage(genesis_hash);

    StorageHandle::enter(&mut provider, |storage| {
        let mut registry = TeeRegistry::new(storage);
        registry.install_initial_policy_v1(&current).unwrap();
        registry
            .stage_successor_policy_v1(proposal_id, &successor)
            .unwrap();
        assert_eq!(registry.active_policy_v1().unwrap(), current);
    });

    provider.set_block_number(50);
    StorageHandle::enter(&mut provider, |storage| {
        let mut registry = TeeRegistry::new(storage);
        registry
            .promote_staged_successor_policy_v1(proposal_id, 50)
            .unwrap();
        registry
            .promote_staged_successor_policy_v1(proposal_id, 50)
            .unwrap();
        assert_eq!(registry.active_policy_v1().unwrap(), successor);
        assert_eq!(registry.staged_successor_policy_v1().unwrap(), None);
    });
}

#[test]
fn ambiguous_measurement_rules_reject_at_the_registry_boundary() {
    let genesis_hash = B256::repeat_byte(0x1a);
    let mut active_policy = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
    let mut overlapping = active_policy.measurement_rules[0].clone();
    overlapping.minimum_isv_svn = 2;
    active_policy.measurement_rules.insert(0, overlapping);
    active_policy.encode_canonical().unwrap();

    let node_signer = OutbeEvmSigner::from_secret_bytes([0x3a; 32]).unwrap();
    let enclave_signer = ed25519_dalek::SigningKey::from_bytes(&[0x3b; 32]);
    let intent = registration_intent(
        &active_policy,
        &node_signer,
        CONSENSUS_KEY,
        &enclave_signer,
        0x58,
        0x68,
    );
    let (node_signature, enclave_signature) = signatures(&intent, &node_signer, &enclave_signer);
    let mut provider = storage(genesis_hash);

    StorageHandle::enter(&mut provider, |storage| {
        register_validator(storage.clone(), &node_signer, CONSENSUS_KEY);
        let mut registry = TeeRegistry::new(storage);
        registry.install_initial_policy_v1(&active_policy).unwrap();
        assert!(revert_message(
            registry
                .register_enclave_after_verifier_for_test(
                    &intent,
                    &node_signature,
                    &enclave_signature,
                    PostVerifierDcapCapabilityV1::new(verdict(DcapPlatformTcbStatusV1::UpToDate,)),
                )
                .unwrap_err()
        )
        .contains("exactly one"));
    });
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
    let old_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x35; 32]);
    let new_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x36; 32]);
    let initial = full_node_registration_intent(&current, &node_signer, &old_enclave, 0x53, 0x63);
    let transition =
        measurement_transition_intent(&initial, &successor, &new_enclave, 0x54, 0x64, NOW + 3_600);
    let (initial_node, initial_enclave) =
        full_node_signatures(&initial, &node_signer, &old_enclave);
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
            .register_enclave_after_verifier_for_test(
                &initial,
                &initial_node,
                &initial_enclave,
                PostVerifierDcapCapabilityV1::new(verdict(DcapPlatformTcbStatusV1::UpToDate)),
            )
            .unwrap();
        registry
            .stage_successor_policy_v1(U256::from(9), &successor)
            .unwrap();
        dispatch_transition_after_verifier_for_test(
            storage.clone(),
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

#[test]
fn an_existing_lease_is_not_retroactively_filtered_by_the_policy_anchor() {
    let genesis_hash = B256::repeat_byte(0x26);
    let active_policy = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
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
fn renewal_opens_in_final_third_accepts_expired_current_and_is_exactly_idempotent() {
    let genesis_hash = B256::repeat_byte(0x21);
    let active_policy = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
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
    let renewal = renewal_intent(&initial, NOW + 6_000);
    let (renewal_node, renewal_enclave) = signatures(&renewal, &node_signer, &enclave_signer);
    let mut fresh_verdict = verdict(DcapPlatformTcbStatusV1::UpToDate);
    fresh_verdict.collateral_valid_until = NOW + 12_000;
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
            .set_block_timestamp(U256::from(NOW + 2_399))
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
        .contains("final third"));

        storage
            .set_block_timestamp(U256::from(NOW + 2_400))
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
        assert_eq!(binding.lease_started_at, NOW + 2_400);
        assert_eq!(binding.valid_until, NOW + 6_000);

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

    let mut expired_provider = storage(genesis_hash);
    StorageHandle::enter(&mut expired_provider, |storage| {
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
        let expired_renewal = renewal_intent(&initial, NOW + 7_200);
        let (node_signature, enclave_signature) =
            signatures(&expired_renewal, &node_signer, &enclave_signer);
        assert_eq!(
            registry
                .renew_enclave_after_verifier_for_test(
                    &expired_renewal,
                    &node_signature,
                    &enclave_signature,
                    PostVerifierDcapCapabilityV1::new(fresh_verdict),
                )
                .unwrap(),
            V1RegistrationOutcome::Created
        );
        assert!(registry
            .is_validator_enclave_ready_v1(node_signer.address())
            .unwrap());
    });
}

#[test]
fn renewal_rejects_collateral_margin_underflow_without_extending_state() {
    let genesis_hash = B256::repeat_byte(0x22);
    let active_policy = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
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
    let renewal = renewal_intent(&initial, NOW + 6_000);
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

#[test]
fn candidate_generated_quote_intent_reaches_registry_replacement_exactly() {
    use std::{
        os::unix::net::UnixListener,
        sync::{Arc, OnceLock},
    };

    use outbe_primitives::tee_attestation_v1::{
        AttestationEvidenceV1, DcapCollateralComponentV1, DcapCollateralKind, DcapEvidenceV1,
    };
    use outbe_tee::{
        connect_or_initialize_node_host_enclave, load_replacement_candidate_submission,
        persist_replacement_candidate_submission, prepare_node_host_enclave_replacement_candidate,
        NodeHostIdentityV1,
    };
    use outbe_tee_enclave::{
        initialization::InitializationState,
        keys::EnclaveKeys,
        seal::EnclaveBootConfig,
        transport::{
            serve_connection_with, serve_connection_with_synthetic_dcap, SharedTributeOfferKey,
        },
    };

    let genesis_hash = B256::repeat_byte(0x23);
    let active_policy = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
    let node_signer = OutbeEvmSigner::from_secret_bytes([0x75; 32]).unwrap();
    let identity = NodeHostIdentityV1 {
        chain_id: CHAIN_ID,
        genesis_hash,
        reth_p2p_public: reth_p2p_public_for_evm_signer(&node_signer),
    };
    let sign = |hash: B256| {
        node_signer
            .sign_hash(&hash)
            .map_err(|error| error.to_string())
    };

    let root = tempfile::tempdir().unwrap();
    let socket_a = root.path().join("active-enclave.sock");
    let socket_b = root.path().join("candidate-enclave.sock");
    let endpoint_a = socket_a.to_str().unwrap().to_owned();
    let endpoint_b = socket_b.to_str().unwrap().to_owned();
    let boot_a = Arc::new(EnclaveBootConfig::new(
        active_policy.chain_id,
        root.path().join("active-enclave-state"),
        0,
    ));
    let boot_b = Arc::new(EnclaveBootConfig::new(
        active_policy.chain_id,
        root.path().join("candidate-enclave-state"),
        0,
    ));
    std::fs::create_dir(&boot_a.tee_dir).unwrap();
    std::fs::create_dir(&boot_b.tee_dir).unwrap();
    let keys_a = Arc::new(EnclaveKeys::new([0x76; 32], Some([0x76; 32])).unwrap());
    let keys_b = Arc::new(EnclaveKeys::new([0x77; 32], Some([0x77; 32])).unwrap());
    let initialization_a =
        Arc::new(InitializationState::production(boot_a.clone(), &keys_a).unwrap());
    let initialization_b =
        Arc::new(InitializationState::production(boot_b.clone(), &keys_b).unwrap());

    let listener_a = UnixListener::bind(&socket_a).unwrap();
    let server_keys_a = keys_a.clone();
    let server_a = std::thread::spawn(move || {
        let offer_key: SharedTributeOfferKey = Arc::new(OnceLock::new());
        for _ in 0..2 {
            let (stream, _) = listener_a.accept().unwrap();
            serve_connection_with(
                stream,
                &server_keys_a,
                &offer_key,
                Some(&boot_a),
                &initialization_a,
            )
            .unwrap();
        }
    });
    let listener_b = UnixListener::bind(&socket_b).unwrap();
    let server_keys_b = keys_b.clone();
    let server_b = std::thread::spawn(move || {
        let offer_key: SharedTributeOfferKey = Arc::new(OnceLock::new());
        for _ in 0..2 {
            let (stream, _) = listener_b.accept().unwrap();
            serve_connection_with_synthetic_dcap(
                stream,
                &server_keys_b,
                &offer_key,
                Some(&boot_b),
                &initialization_b,
            )
            .unwrap();
        }
    });

    let node_data_dir = root.path().join("node-data");
    std::fs::create_dir(&node_data_dir).unwrap();
    drop(
        connect_or_initialize_node_host_enclave(&endpoint_a, &node_data_dir, identity, sign)
            .unwrap(),
    );
    let active_manifest_bytes = std::fs::read(
        node_data_dir
            .join(outbe_tee::node_host::NODE_HOST_DIRECTORY_V1)
            .join(outbe_tee::node_host::NODE_HOST_MANIFEST_V1),
    )
    .unwrap();
    let active_manifest =
        EnclaveInitializationManifestV1::decode_canonical(&active_manifest_bytes).unwrap();
    let initial = RegistrationIntentV1 {
        chain_id: active_manifest.chain_id,
        genesis_hash: active_manifest.genesis_hash,
        operation: AttestationOperationV1::RegisterEnclave,
        attestation_mode: AttestationMode::DcapRequired,
        policy_hash: active_policy.policy_hash().unwrap(),
        node_id: active_manifest.node_id.clone(),
        enclave_id: active_manifest.enclave_id().unwrap(),
        binding_id: B256::repeat_byte(0x66),
        binding_version: 1,
        registration_version: 0,
        renewal_nonce: 0,
        transition_nonce: 0,
        requested_valid_until: NOW + 3_600,
        recipient_x25519: active_manifest.recipient_x25519,
        attestation_ed25519: active_manifest.attestation_ed25519,
        noise_responder_x25519: active_manifest.noise_responder_x25519,
        node_host_authorization_hash: active_manifest.node_host_authorization_hash().unwrap(),
    };
    active_manifest.validate_intent_binding(&initial).unwrap();
    let initial_hash = initial.intent_hash().unwrap();
    let initial_node_signature = node_signer.sign_hash(&initial_hash).unwrap();
    let initial_enclave_signature = keys_a.sign_attestation(initial_hash.as_slice());

    let mut candidate = prepare_node_host_enclave_replacement_candidate(
        &endpoint_b,
        &node_data_dir,
        identity,
        sign,
    )
    .unwrap();
    let candidate_manifest = candidate.manifest().clone();
    let mut replacement = initial.clone();
    replacement.operation = AttestationOperationV1::ReplaceEnclaveBinding;
    replacement.enclave_id = candidate_manifest.enclave_id().unwrap();
    replacement.binding_id = B256::repeat_byte(0x68);
    replacement.binding_version = 2;
    replacement.registration_version = 1;
    replacement.requested_valid_until = NOW + 3_600;
    replacement.recipient_x25519 = candidate_manifest.recipient_x25519;
    replacement.attestation_ed25519 = candidate_manifest.attestation_ed25519;
    replacement.noise_responder_x25519 = candidate_manifest.noise_responder_x25519;
    replacement.node_host_authorization_hash =
        candidate_manifest.node_host_authorization_hash().unwrap();
    candidate_manifest
        .validate_intent_binding(&replacement)
        .unwrap();
    assert_eq!(
        replacement.node_host_authorization_hash,
        initial.node_host_authorization_hash
    );

    let generated = candidate.generate_dcap_quote(&replacement).unwrap();
    let replacement_hash = replacement.intent_hash().unwrap();
    let replacement_node_signature = node_signer.sign_hash(&replacement_hash).unwrap();
    let evidence = AttestationEvidenceV1::Dcap(DcapEvidenceV1 {
        intent: replacement.clone(),
        quote: generated.quote_body.clone(),
        components: (1_u8..=8)
            .map(|kind| DcapCollateralComponentV1 {
                kind: DcapCollateralKind::try_from(kind).unwrap(),
                bytes: vec![kind],
            })
            .collect(),
        transition_key_ready_proof: None,
    });
    let exact_evidence = evidence.encode_canonical().unwrap();
    let submission = persist_replacement_candidate_submission(
        &node_data_dir,
        &evidence,
        &replacement_node_signature,
        &generated.enclave_signature,
    )
    .unwrap();
    assert_eq!(submission.evidence(), exact_evidence);
    assert_eq!(
        load_replacement_candidate_submission(&node_data_dir)
            .unwrap()
            .unwrap(),
        submission
    );
    let AttestationEvidenceV1::Dcap(submitted) =
        AttestationEvidenceV1::decode_canonical(submission.evidence()).unwrap()
    else {
        unreachable!();
    };
    assert_eq!(
        submitted.intent.encode_canonical().unwrap(),
        replacement.encode_canonical().unwrap()
    );
    assert_eq!(submitted.quote, generated.quote_body);

    let mut accepted = verdict(DcapPlatformTcbStatusV1::UpToDate);
    accepted.collateral_valid_until = NOW + 12_000;
    let mut provider = storage(genesis_hash);
    StorageHandle::enter(&mut provider, |storage| {
        register_validator(storage.clone(), &node_signer, CONSENSUS_KEY);
        let mut registry = TeeRegistry::new(storage.clone());
        registry.install_initial_policy_v1(&active_policy).unwrap();
        register_same_key_node_for_lifecycle_test(
            &mut registry,
            &initial,
            &node_signer,
            &initial_node_signature,
            &initial_enclave_signature,
            PostVerifierDcapCapabilityV1::new(accepted.clone()),
        )
        .unwrap();
        assert_eq!(
            registry
                .replace_enclave_binding_after_verifier_for_test(
                    &submitted.intent,
                    submission.node_signature(),
                    submission.enclave_signature(),
                    PostVerifierDcapCapabilityV1::new(accepted),
                )
                .unwrap(),
            V1RegistrationOutcome::Created
        );
        let binding = registry
            .validator_enclave_binding_v1(node_signer.address())
            .unwrap()
            .unwrap();
        assert_eq!(binding.enclave_id, candidate_manifest.enclave_id().unwrap());
        assert_eq!(binding.binding_id, replacement.binding_id);
        assert_eq!(binding.binding_version, replacement.binding_version);
        assert_eq!(
            binding.registration_version,
            replacement.registration_version
        );
    });

    drop(candidate);
    server_a.join().unwrap();
    server_b.join().unwrap();
}

#[test]
fn replacement_candidate_intent_reaches_registry_unchanged_and_never_reuses_consumed_ids() {
    let genesis_hash = B256::repeat_byte(0x23);
    let active_policy = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
    let node_signer = OutbeEvmSigner::from_secret_bytes([0x75; 32]).unwrap();
    let old_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x76; 32]);
    let new_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x77; 32]);
    let initial = registration_intent(
        &active_policy,
        &node_signer,
        CONSENSUS_KEY,
        &old_enclave,
        0x65,
        0x66,
    );
    let replacement = replacement_intent(&initial, &new_enclave, 0x67, 0x68, NOW + 3_600);
    let active_manifest = initialization_manifest_for_intent(&initial, [0xa6; 32]);
    let candidate_manifest = initialization_manifest_for_intent(&replacement, [0xa7; 32]);
    assert_ne!(
        active_manifest.authorization_hash().unwrap(),
        candidate_manifest.authorization_hash().unwrap()
    );
    assert_eq!(
        active_manifest.node_host_authorization_hash().unwrap(),
        candidate_manifest.node_host_authorization_hash().unwrap()
    );
    candidate_manifest
        .validate_intent_binding(&replacement)
        .unwrap();
    let candidate_intent_bytes = replacement.encode_canonical().unwrap();
    let (initial_node, initial_enclave) = signatures(&initial, &node_signer, &old_enclave);
    let (replacement_node, replacement_enclave) =
        signatures(&replacement, &node_signer, &new_enclave);
    let mut accepted = verdict(DcapPlatformTcbStatusV1::UpToDate);
    accepted.collateral_valid_until = NOW + 12_000;
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
            PostVerifierDcapCapabilityV1::new(accepted.clone()),
        )
        .unwrap();
        assert_eq!(
            registry
                .replace_enclave_binding_after_verifier_for_test(
                    &replacement,
                    &replacement_node,
                    &replacement_enclave,
                    PostVerifierDcapCapabilityV1::new(accepted.clone()),
                )
                .unwrap(),
            V1RegistrationOutcome::Created
        );
        assert_eq!(
            registry
                .replace_enclave_binding_after_verifier_for_test(
                    &replacement,
                    &replacement_node,
                    &replacement_enclave,
                    PostVerifierDcapCapabilityV1::new(accepted.clone()),
                )
                .unwrap(),
            V1RegistrationOutcome::Idempotent
        );
        let binding = registry
            .validator_enclave_binding_v1(node_signer.address())
            .unwrap()
            .unwrap();
        assert_eq!(binding.enclave_id, replacement.enclave_id);
        assert_eq!(binding.binding_id, replacement.binding_id);
        assert_eq!(binding.binding_version, 2);
        assert_eq!(binding.registration_version, 1);
        assert_eq!(
            replacement.encode_canonical().unwrap(),
            candidate_intent_bytes
        );

        let old_renewal = renewal_intent(&initial, NOW + 6_000);
        let (old_node, old_enclave_signature) =
            signatures(&old_renewal, &node_signer, &old_enclave);
        storage
            .set_block_timestamp(U256::from(NOW + 2_400))
            .unwrap();
        assert!(revert_message(
            registry
                .renew_enclave_after_verifier_for_test(
                    &old_renewal,
                    &old_node,
                    &old_enclave_signature,
                    PostVerifierDcapCapabilityV1::new(accepted.clone()),
                )
                .unwrap_err()
        )
        .contains("superseded"));

        let current = replacement.clone();
        let attempted_reuse = replacement_intent(&current, &old_enclave, 0x65, 0x66, NOW + 6_000);
        let (reuse_node, reuse_enclave) = signatures(&attempted_reuse, &node_signer, &old_enclave);
        assert!(revert_message(
            registry
                .replace_enclave_binding_after_verifier_for_test(
                    &attempted_reuse,
                    &reuse_node,
                    &reuse_enclave,
                    PostVerifierDcapCapabilityV1::new(accepted),
                )
                .unwrap_err()
        )
        .contains("already been used"));
    });
}

#[test]
fn renew_and_replace_abi_are_replica_deterministic_and_fit_normative_gas() {
    let genesis_hash = B256::repeat_byte(0x24);
    let active_policy = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    );
    let node_signer = OutbeEvmSigner::from_secret_bytes([0x78; 32]).unwrap();
    let old_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x79; 32]);
    let new_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x7A; 32]);
    let initial = registration_intent(
        &active_policy,
        &node_signer,
        CONSENSUS_KEY,
        &old_enclave,
        0x69,
        0x6A,
    );
    let renewal = renewal_intent(&initial, NOW + 6_000);
    let replacement = replacement_intent(&renewal, &new_enclave, 0x6B, 0x6C, NOW + 6_000);
    let (initial_node, initial_enclave) = signatures(&initial, &node_signer, &old_enclave);
    let (renewal_node, renewal_enclave) = signatures(&renewal, &node_signer, &old_enclave);
    let (replacement_node, replacement_enclave) =
        signatures(&replacement, &node_signer, &new_enclave);
    let evidence = vec![0xA7; 4_096];
    let renewal_call = IRegisterEnclaveV1Test::renewEnclaveCall {
        evidence: evidence.clone().into(),
        nodeSignature: renewal_node.to_vec().into(),
        enclaveSignature: renewal_enclave.to_vec().into(),
    }
    .abi_encode();
    let replacement_call = IRegisterEnclaveV1Test::replaceEnclaveBindingCall {
        evidence: evidence.clone().into(),
        nodeSignature: replacement_node.to_vec().into(),
        enclaveSignature: replacement_enclave.to_vec().into(),
    }
    .abi_encode();
    let mut accepted = verdict(DcapPlatformTcbStatusV1::UpToDate);
    accepted.collateral_valid_until = NOW + 12_000;

    let execute = || {
        let mut provider = storage(genesis_hash);
        StorageHandle::enter(&mut provider, |storage| {
            register_validator(storage.clone(), &node_signer, CONSENSUS_KEY);
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
            storage
                .set_block_timestamp(U256::from(NOW + 2_400))
                .unwrap();
        });
        provider.enable_production_storage_gas_metering();
        provider.set_gas_limit(u64::MAX);
        StorageHandle::enter(&mut provider, |storage| {
            dispatch_renew_after_verifier_for_test(
                storage.clone(),
                &renewal_call,
                &renewal,
                PostVerifierDcapCapabilityV1::new(accepted.clone()),
            )
            .unwrap();
            dispatch_replace_after_verifier_for_test(
                storage,
                &replacement_call,
                &replacement,
                PostVerifierDcapCapabilityV1::new(accepted.clone()),
            )
            .unwrap();
        });
        provider
    };
    let proposer = execute();
    let validator = execute();
    let follower = execute();
    for replica in [&validator, &follower] {
        assert_eq!(replica.storage, proposer.storage);
        assert_eq!(replica.get_ordered_events(), proposer.get_ordered_events());
        assert_eq!(
            replica.metered_storage_operations(),
            proposer.metered_storage_operations()
        );
        assert_eq!(replica.gas_used(), proposer.gas_used());
    }

    let schedule = TeeRegistryGasScheduleV1::normative();
    let mut maximum_total = 0_u64;
    let mut intrinsic_total = 0_u64;
    let mut allowance_total = 0_u64;
    for (kind, call) in [
        (RegistryMutatorV1::RenewEnclave, &renewal_call),
        (RegistryMutatorV1::ReplaceEnclaveBinding, &replacement_call),
    ] {
        let maximum = schedule
            .maximum_transaction_gas(
                kind,
                call.len(),
                evidence.len(),
                active_policy.measurement_rules.len(),
                AttestationMode::DcapRequired,
            )
            .unwrap();
        assert!(maximum < 30_000_000);
        maximum_total += maximum;
        intrinsic_total += schedule.maximum_calldata_intrinsic_gas(call.len()).unwrap();
        allowance_total += schedule.mutator_storage_gas_allowance(kind);
    }
    let (reads, writes) = proposer.metered_storage_operations();
    let storage_gas = reads * 100 + writes * 5_000;
    assert!(storage_gas <= allowance_total);
    assert_eq!(
        intrinsic_total + 2 * 200 + proposer.gas_used(),
        maximum_total - allowance_total + storage_gas
    );
    assert!(intrinsic_total + 2 * 200 + proposer.gas_used() <= maximum_total);
}
