use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::{sol, SolCall};
use ed25519_dalek::Signer as _;
use outbe_primitives::{
    error::PrecompileError,
    signer::OutbeEvmSigner,
    storage::{hashmap::HashMapStorageProvider, PrecompileStorageProvider, StorageHandle},
    tee_attestation_v1::{
        AttestationMode, AttestationOperationV1, EnclaveProfile, NodeIdV1, PlatformTcbStatusSetV1,
        QvlTcbStatusV1, RegistrationIntentV1, RegistryMutatorV1, TeeMeasurementRuleV1, TeePolicyV1,
        TeeRegistryGasScheduleV1,
    },
};
use outbe_tee::dcap_protocol::{DcapPckCaV1, DcapPlatformTcbStatusV1, DcapVerdictV1};
use outbe_validatorset::contract::ValidatorSet;

use crate::{
    schema::TeeRegistry,
    v1::{PostVerifierDcapCapabilityV1, V1RegistrationOutcome},
    v1_precompile::dispatch_register_after_verifier_for_test,
};

sol! {
    interface IRegisterEnclaveV1Test {
        function registerEnclave(
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
            enclave_profile: EnclaveProfile::Validator,
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

fn register_validator(
    storage: StorageHandle<'_>,
    signer: &OutbeEvmSigner,
    consensus_key: [u8; 48],
) {
    ValidatorSet::new(storage)
        .register_validator(Address::ZERO, signer.address(), &consensus_key)
        .expect("genesis-owner validator registration");
}

fn registration_intent(
    policy: &TeePolicyV1,
    node_signer: &OutbeEvmSigner,
    consensus_key: [u8; 48],
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
        enclave_profile: EnclaveProfile::Validator,
        node_id: NodeIdV1::Validator {
            address: node_signer.address().into_array(),
            bls_minpk_public: consensus_key,
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
        node_host_authorization_hash: B256::repeat_byte(key_seed.wrapping_add(2)),
    };
    intent.enclave_id = intent.derived_enclave_id().unwrap();
    intent
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

fn revert_message(error: PrecompileError) -> String {
    match error {
        PrecompileError::Revert(message) => message,
        other => panic!("expected deterministic revert, got {other:?}"),
    }
}

#[test]
fn validator_binding_is_active_idempotent_and_expires_without_relay_authority() {
    let genesis_hash = B256::repeat_byte(0x11);
    let active_policy = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrSWHardeningNeeded,
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
                .register_validator_enclave_after_verifier_for_test(
                    &intent,
                    &node_signature,
                    &enclave_signature,
                    PostVerifierDcapCapabilityV1::new(accepted_verdict.clone()),
                )
                .unwrap(),
            V1RegistrationOutcome::Created
        );
        assert!(registry
            .is_validator_enclave_ready_v1(node_signer.address())
            .unwrap());
        let binding = registry
            .validator_enclave_binding_v1(node_signer.address())
            .unwrap()
            .unwrap();
        assert_eq!(binding.enclave_id, intent.enclave_id);
        assert_eq!(binding.binding_id, intent.binding_id);
        assert_eq!(binding.intent_hash, intent.intent_hash().unwrap());
        assert_eq!(binding.evidence_hash, B256::repeat_byte(0xEC));
        assert_eq!(binding.valid_until, intent.requested_valid_until);
        assert_ne!(binding.verdict_hash, B256::ZERO);
        assert_eq!(registry.registered_count.read().unwrap(), 1);

        assert_eq!(
            registry
                .register_validator_enclave_after_verifier_for_test(
                    &intent,
                    &node_signature,
                    &enclave_signature,
                    PostVerifierDcapCapabilityV1::new(accepted_verdict.clone()),
                )
                .unwrap(),
            V1RegistrationOutcome::Idempotent
        );
        assert_eq!(registry.registered_count.read().unwrap(), 1);

        let conflict = registry
            .register_validator_enclave_after_verifier_for_test(
                &intent,
                &node_signature,
                &enclave_signature,
                PostVerifierDcapCapabilityV1::with_evidence_hash(
                    accepted_verdict,
                    B256::repeat_byte(0xED),
                ),
            )
            .unwrap_err();
        assert!(revert_message(conflict).contains("not an exact evidence replay"));
        assert_eq!(registry.registered_count.read().unwrap(), 1);

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
        1,
        "idempotent replay must not emit a second event"
    );
}

#[test]
fn proposer_validator_and_follower_apply_identical_full_state_verdict_and_gas() {
    let genesis_hash = B256::repeat_byte(0x18);
    let active_policy = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrSWHardeningNeeded,
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
    let accepted_verdict = verdict(DcapPlatformTcbStatusV1::SWHardeningNeeded);
    let evidence = vec![0xA5; 4_096];
    let call = IRegisterEnclaveV1Test::registerEnclaveCall {
        evidence: evidence.clone().into(),
        nodeSignature: node_signature.to_vec().into(),
        enclaveSignature: enclave_signature.to_vec().into(),
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
    assert_eq!(writes, 32, "fresh V1 binding storage schema drifted");
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
fn validator_registration_rejects_full_node_profile_after_verifier() {
    let genesis_hash = B256::repeat_byte(0x19);
    let active_policy = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrSWHardeningNeeded,
    );
    let node_signer = OutbeEvmSigner::from_secret_bytes([0x6A; 32]).unwrap();
    let enclave_signer = ed25519_dalek::SigningKey::from_bytes(&[0x6B; 32]);
    let mut intent = registration_intent(
        &active_policy,
        &node_signer,
        CONSENSUS_KEY,
        &enclave_signer,
        0x49,
        0x59,
    );
    intent.enclave_profile = EnclaveProfile::FullNode;
    intent.node_id = NodeIdV1::FullNode {
        reth_p2p_public: [
            0x02, 0x79, 0xBE, 0x66, 0x7E, 0xF9, 0xDC, 0xBB, 0xAC, 0x55, 0xA0, 0x62, 0x95, 0xCE,
            0x87, 0x0B, 0x07, 0x02, 0x9B, 0xFC, 0xDB, 0x2D, 0xCE, 0x28, 0xD9, 0x59, 0xF2, 0x81,
            0x5B, 0x16, 0xF8, 0x17, 0x98,
        ],
    };
    intent.enclave_id = intent.derived_enclave_id().unwrap();
    let (node_signature, enclave_signature) = signatures(&intent, &node_signer, &enclave_signer);
    let mut provider = storage(genesis_hash);

    StorageHandle::enter(&mut provider, |storage| {
        register_validator(storage.clone(), &node_signer, CONSENSUS_KEY);
        let mut registry = TeeRegistry::new(storage);
        registry.install_initial_policy_v1(&active_policy).unwrap();
        let error = registry
            .register_validator_enclave_after_verifier_for_test(
                &intent,
                &node_signature,
                &enclave_signature,
                PostVerifierDcapCapabilityV1::new(verdict(DcapPlatformTcbStatusV1::UpToDate)),
            )
            .unwrap_err();
        assert!(revert_message(error).contains("not a validator DCAP registration"));
    });
}

#[test]
fn validator_registration_rejects_pop_nonce_measurement_and_consensus_key_errors() {
    let genesis_hash = B256::repeat_byte(0x12);
    let active_policy = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrSWHardeningNeeded,
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
                .register_validator_enclave_after_verifier_for_test(
                    &intent,
                    &wrong_node_signature,
                    &enclave_signature,
                    PostVerifierDcapCapabilityV1::new(verdict(DcapPlatformTcbStatusV1::UpToDate,)),
                )
                .unwrap_err()
        )
        .contains("node proof"));

        let wrong_enclave_signature = other_enclave
            .sign(intent.intent_hash().unwrap().as_slice())
            .to_bytes();
        assert!(revert_message(
            registry
                .register_validator_enclave_after_verifier_for_test(
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
                .register_validator_enclave_after_verifier_for_test(
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
                .register_validator_enclave_after_verifier_for_test(
                    &intent,
                    &node_signature,
                    &enclave_signature,
                    PostVerifierDcapCapabilityV1::new(wrong_measurement),
                )
                .unwrap_err()
        )
        .contains("measurement rule"));

        let wrong_bls_intent = registration_intent(
            &active_policy,
            &node_signer,
            [0x33; 48],
            &enclave_signer,
            0x43,
            0x53,
        );
        let (wrong_bls_node, wrong_bls_enclave) =
            signatures(&wrong_bls_intent, &node_signer, &enclave_signer);
        assert!(revert_message(
            registry
                .register_validator_enclave_after_verifier_for_test(
                    &wrong_bls_intent,
                    &wrong_bls_node,
                    &wrong_bls_enclave,
                    PostVerifierDcapCapabilityV1::new(verdict(DcapPlatformTcbStatusV1::UpToDate,)),
                )
                .unwrap_err()
        )
        .contains("consensus public key mismatch"));

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
        PlatformTcbStatusSetV1::UpToDateOrSWHardeningNeeded,
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
            .register_validator_enclave_after_verifier_for_test(
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
                .register_validator_enclave_after_verifier_for_test(
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
                .register_validator_enclave_after_verifier_for_test(
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
        assert!(revert_message(
            registry
                .register_validator_enclave_after_verifier_for_test(
                    &strict_intent,
                    &strict_node_signature,
                    &strict_enclave_signature,
                    PostVerifierDcapCapabilityV1::new(verdict(
                        DcapPlatformTcbStatusV1::SWHardeningNeeded,
                    )),
                )
                .unwrap_err()
        )
        .contains("stricter than active policy"));
    });
}

#[test]
fn initial_policy_is_state_authority_and_is_write_once() {
    let genesis_hash = B256::repeat_byte(0x14);
    let first = policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrSWHardeningNeeded,
    );
    let mut provider = storage(genesis_hash);
    let legacy_policy_hash = B256::repeat_byte(0xD1);
    StorageHandle::enter(&mut provider, |storage| {
        let mut registry = TeeRegistry::new(storage);
        registry.policy_hash.write(legacy_policy_hash).unwrap();
        registry.install_initial_policy_v1(&first).unwrap();
        registry.install_initial_policy_v1(&first).unwrap();
        assert_eq!(registry.active_policy_v1().unwrap(), first);
        assert_eq!(registry.policy_hash.read().unwrap(), legacy_policy_hash);
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
