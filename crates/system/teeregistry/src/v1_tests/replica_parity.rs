use super::*;

/// Runs the same registration on a proposer, a validator and a follower with
/// `run_on_three_replicas`. Asserts that all three replicas create the binding.
/// Returns the providers in that order.
fn register_on_three_replicas(
    new_chain: impl Fn() -> HashMapStorageProvider,
    register: impl Fn(StorageHandle<'_>) -> V1RegistrationOutcome,
) -> [HashMapStorageProvider; 3] {
    let [(proposer, outcome), (validator, validator_outcome), (follower, follower_outcome)] =
        run_on_three_replicas(new_chain, register);
    assert_eq!(outcome, V1RegistrationOutcome::Created);
    assert_eq!(validator_outcome, outcome);
    assert_eq!(follower_outcome, outcome);
    [proposer, validator, follower]
}

/// Asserts that the proposer of one fresh registration reads storage and writes
/// the 25 slots of a fresh binding (`schema_message` on failure). Then asserts
/// that the proposer fits the normative gas of `register` and that the
/// validator and the follower match the proposer.
fn assert_fresh_registration_replicas(
    replicas: &[HashMapStorageProvider; 3],
    register: MeteredCall<'_>,
    evidence_len: usize,
    policy: &TeePolicyV1,
    schema_message: &str,
) {
    let [proposer, validator, follower] = replicas;
    assert_metered_writes(proposer, 25, schema_message);
    assert_normative_gas(proposer, &[register], evidence_len, policy);
    assert_replicas_match(proposer, &[validator, follower]);
}

#[test]
fn proposer_validator_and_follower_apply_identical_full_state_verdict_and_gas() {
    let genesis_hash = B256::repeat_byte(0x18);
    let node = LifecycleValidator::new(
        hardening_policy(genesis_hash),
        0x68,
        0x69,
        EnclaveBindingSeeds::new(0x48, 0x58),
    );
    let accepted_verdict = verdict(DcapPlatformTcbStatusV1::SWHardeningNeeded);
    let evidence = [0xA5; 4_096];
    let call = node.register_call(&node.initial, &node.enclave_signer, &evidence);

    let replicas = register_on_three_replicas(
        || node.run_as_validator(|_, _| {}),
        |storage| {
            dispatch_register_after_verifier_for_test(
                storage,
                node.node_signer.address(),
                &call,
                &node.initial,
                PostVerifierDcapCapabilityV1::new(accepted_verdict.clone()),
            )
            .unwrap()
        },
    );
    assert_fresh_registration_replicas(
        &replicas,
        MeteredCall {
            kind: RegistryMutatorV1::RegisterEnclave,
            calldata: &call,
        },
        evidence.len(),
        &node.policy,
        "fresh V1 binding storage schema drifted",
    );
}

#[test]
fn full_node_proposer_validator_and_follower_apply_identical_abi_state_and_gas() {
    let genesis_hash = B256::repeat_byte(0x1A);
    let full_node = LifecycleFullNode::new(
        hardening_policy(genesis_hash),
        0x7A,
        0x7B,
        EnclaveBindingSeeds::new(0x4A, 0x5A),
    );
    let admission_signer = OutbeEvmSigner::from_secret_bytes([0x7C; 32]).unwrap();
    let accepted_verdict = verdict(DcapPlatformTcbStatusV1::SWHardeningNeeded);
    let evidence = [0xA6; 4_096];
    let call = full_node.register_call(&admission_signer, &evidence);

    let replicas = register_on_three_replicas(
        || full_node.run_installed(|_, _| {}),
        |storage| {
            dispatch_register_after_verifier_for_test(
                storage,
                admission_signer.address(),
                &call,
                &full_node.initial,
                PostVerifierDcapCapabilityV1::new(accepted_verdict.clone()),
            )
            .unwrap()
        },
    );
    assert_fresh_registration_replicas(
        &replicas,
        MeteredCall {
            kind: RegistryMutatorV1::RegisterEnclave,
            calldata: &call,
        },
        evidence.len(),
        &full_node.policy,
        "fresh FullNode V1 binding storage schema drifted",
    );
}
