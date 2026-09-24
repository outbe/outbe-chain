use super::*;

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

const CHAIN_ID: u64 = TESTNET_CHAIN_ID;

pub(super) const NOW: u64 = 10_000;

const MRENCLAVE: B256 = B256::repeat_byte(0x81);

pub(super) const MRSIGNER: B256 = B256::repeat_byte(0x82);

pub(super) const CONSENSUS_KEY: [u8; 48] = [0x32; 48];

const NODE_HOST_NOISE_X25519: [u8; 32] = [0xa5; 32];

pub(super) const OFFER_PUBLIC: [u8; 32] = [0xb1; 32];

pub(super) fn policy(genesis_hash: B256, statuses: PlatformTcbStatusSetV1) -> TeePolicyV1 {
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

pub(super) fn storage(genesis_hash: B256) -> HashMapStorageProvider {
    storage_for_chain(CHAIN_ID, genesis_hash)
}

pub(super) fn storage_for_chain(chain_id: u64, genesis_hash: B256) -> HashMapStorageProvider {
    let mut storage = HashMapStorageProvider::new_with_chain_identity(chain_id, genesis_hash);
    storage.set_block_number(10);
    storage.set_timestamp(U256::from(NOW));
    storage
}

pub(super) fn register_validator(
    storage: StorageHandle<'_>,
    signer: &OutbeEvmSigner,
    consensus_key: [u8; 48],
) {
    ValidatorSet::new(storage)
        .register_validator(Address::ZERO, signer.address(), &consensus_key)
        .expect("genesis-owner validator registration");
}

pub(super) fn reth_p2p_public_for_evm_signer(node_signer: &OutbeEvmSigner) -> [u8; 33] {
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

pub(super) fn initialization_manifest_for_intent(
    intent: &RegistrationIntentV1,
    challenge: [u8; 32],
) -> EnclaveInitializationManifestV1 {
    EnclaveInitializationManifestV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        attestation_mode: intent.attestation_mode,
        node_id: intent.node_id.clone(),
        initialization_challenge: challenge,
        node_host_noise_x25519: NODE_HOST_NOISE_X25519,
        recipient_x25519: intent.recipient_x25519,
        attestation_ed25519: intent.attestation_ed25519,
        noise_responder_x25519: intent.noise_responder_x25519,
    }
}

pub(super) fn bind_reachable_node_host_authorization(intent: &mut RegistrationIntentV1, challenge: [u8; 32]) {
    let manifest = initialization_manifest_for_intent(intent, challenge);
    intent.node_host_authorization_hash = manifest.node_host_authorization_hash().unwrap();
    manifest.validate_intent_binding(intent).unwrap();
}

pub(super) fn registration_intent(
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

pub(super) fn full_node_registration_intent(
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

pub(super) fn full_node_signatures(
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

pub(super) fn validator_node_binding_authorization_for_p2p_node(
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

pub(super) fn validator_node_binding_authorization_for_evm_node(
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

pub(super) fn full_node_public(intent: &RegistrationIntentV1) -> [u8; 33] {
    intent.node_id.reth_p2p_public
}

pub(super) fn signatures(
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

pub(super) fn register_same_key_node_for_lifecycle_test(
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

pub(super) fn verdict(status: DcapPlatformTcbStatusV1) -> DcapVerdictV1 {
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

pub(super) fn renewal_intent(
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

pub(super) fn replacement_intent(
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

pub(super) fn measurement_transition_intent(
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

pub(super) fn revert_message(error: PrecompileError) -> String {
    match error {
        PrecompileError::Revert(message) => message,
        other => panic!("expected deterministic revert, got {other:?}"),
    }
}
