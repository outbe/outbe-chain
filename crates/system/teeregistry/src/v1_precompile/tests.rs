use super::*;
use crate::v1_tests::{
    assert_metered_writes, assert_normative_gas, hardening_policy, meter_production_gas,
    normative_budget, successor_policy, MeteredCall,
};
use alloy_sol_types::{SolError, SolValue};
use outbe_primitives::{
    chain::TESTNET_CHAIN_ID,
    storage::{hashmap::HashMapStorageProvider, PrecompileStorageProvider},
    tee_attestation_v1::{
        AttestationEvidenceV1, AttestationMode, AttestationOperationV1, DcapEvidenceV1, NodeIdV1,
        RegistrationIntentV1, ResourceScheduleV1, TeeMeasurementRuleV1, TeePolicyV1,
        MAX_ATTESTATION_EVIDENCE_BYTES,
    },
};

const GENESIS: B256 = B256::repeat_byte(0x31);
const CHAIN_ID: u64 = TESTNET_CHAIN_ID;

fn policy() -> TeePolicyV1 {
    let resources = ResourceScheduleV1::normative().unwrap();
    TeePolicyV1 {
        intel_root_der_hash: B256::repeat_byte(0x43),
        resource_schedule_hash: resources.schedule_hash().unwrap(),
        measurement_rules: vec![TeeMeasurementRuleV1 {
            mrenclave: B256::repeat_byte(0x45),
            mrsigner: B256::repeat_byte(0x46),
            isv_prod_id: 7,
            minimum_isv_svn: 2,
            admit_from_height: 1,
            admit_until_height_exclusive: 1_000,
        }],
        ..hardening_policy(GENESIS)
    }
}

/// A new chain at `block_number` with `policy` installed.
fn installed_provider(policy: &TeePolicyV1, block_number: u64) -> HashMapStorageProvider {
    let mut provider = HashMapStorageProvider::new_with_chain_identity(CHAIN_ID, GENESIS);
    provider.set_block_number(block_number);
    provider
        .enter(|storage| TeeRegistry::new(storage).install_initial_policy_v1(policy))
        .unwrap();
    provider
}

/// Dispatches `input` from the caller `caller_byte` on `provider` and asserts
/// a revert (`message` on failure).
fn assert_dispatch_reverts(
    provider: &mut HashMapStorageProvider,
    input: &[u8],
    caller_byte: u8,
    message: &str,
) {
    let result = provider.enter(|storage| {
        dispatch(
            storage,
            input,
            Address::repeat_byte(caller_byte),
            U256::ZERO,
        )
    });
    assert!(
        matches!(
            result,
            Err(PrecompileError::Revert(_) | PrecompileError::RevertBytes(_))
        ),
        "{message}: {result:?}"
    );
}

/// The metered charge of the first reverted register dispatch.
struct PrechargedRevert {
    /// The metered policy reads of the dispatch.
    policy_reads: u64,
    /// The precharge plus the gas of the policy reads.
    exact_dispatch_gas: u64,
}

impl PrechargedRevert {
    /// Asserts that the reverted register dispatch on `provider` metered
    /// policy reads and no write, and that it charged `dispatch_charge` plus
    /// the gas of those reads.
    fn assert_metered(provider: &HashMapStorageProvider, dispatch_charge: u64) -> Self {
        let policy_reads = assert_metered_writes(
            provider,
            0,
            "a rejected register must not write registry state",
        );
        let exact_dispatch_gas = dispatch_charge + policy_reads * 100;
        assert_eq!(provider.gas_used(), exact_dispatch_gas);
        Self {
            policy_reads,
            exact_dispatch_gas,
        }
    }

    /// Sets the gas limit of `provider` to the exact dispatch gas and repeats
    /// the reverted dispatch of `input` from `caller_byte`. Asserts the same
    /// gas and the same metered reads.
    fn assert_exact_gas_reverts(
        &self,
        provider: &mut HashMapStorageProvider,
        input: &[u8],
        caller_byte: u8,
        message: &str,
    ) {
        provider.set_gas_limit(self.exact_dispatch_gas);
        assert_dispatch_reverts(provider, input, caller_byte, message);
        assert_eq!(provider.gas_used(), self.exact_dispatch_gas);
        assert_eq!(
            provider.metered_storage_operations(),
            (self.policy_reads, 0)
        );
    }
}

#[test]
fn staged_successor_view_is_canonical_and_distinguishes_absence() {
    let current = policy();
    let mut provider = installed_provider(&current, 10);
    let call = ITeeRegistryV1::stagedSuccessorPolicyV1Call {};
    let empty = provider
        .enter(|storage| dispatch(storage, &call.abi_encode(), Address::ZERO, U256::ZERO))
        .unwrap();
    let empty = ITeeRegistryV1::stagedSuccessorPolicyV1Call::abi_decode_returns(&empty).unwrap();
    assert!(!empty.exists);
    assert!(empty.proposalId.is_zero());
    assert!(empty.policy.is_empty());

    let successor = successor_policy(&current);
    provider
        .enter(|storage| {
            TeeRegistry::new(storage).stage_successor_policy_v1(U256::from(7), &successor)
        })
        .unwrap();
    let encoded = provider
        .enter(|storage| dispatch(storage, &call.abi_encode(), Address::ZERO, U256::ZERO))
        .unwrap();
    let staged = ITeeRegistryV1::stagedSuccessorPolicyV1Call::abi_decode_returns(&encoded).unwrap();
    assert!(staged.exists);
    assert_eq!(staged.proposalId, U256::from(7));
    assert_eq!(
        TeePolicyV1::decode_canonical(&staged.policy).unwrap(),
        successor
    );
}

fn call(evidence: Vec<u8>, node_len: usize, enclave_len: usize) -> Vec<u8> {
    mutator_call(
        RegistryMutatorV1::RegisterEnclave,
        evidence,
        node_len,
        enclave_len,
    )
}

fn mutator_call(
    kind: RegistryMutatorV1,
    evidence: Vec<u8>,
    node_len: usize,
    enclave_len: usize,
) -> Vec<u8> {
    let node_signature = Bytes::from(vec![0x51; node_len]);
    let enclave_signature = Bytes::from(vec![0x52; enclave_len]);
    match kind {
        RegistryMutatorV1::PrepareEnclaveUpgrade => ITeeRegistryV1::prepareEnclaveUpgradeCall {
            evidence: Bytes::from(evidence),
            nodeSignature: node_signature,
            enclaveSignature: enclave_signature,
        }
        .abi_encode(),
        RegistryMutatorV1::RegisterEnclave => ITeeRegistryV1::registerEnclaveCall {
            evidence: Bytes::from(evidence),
            nodeSignature: node_signature,
            enclaveSignature: enclave_signature,
            validatorNodeBinding: Bytes::from(vec![0x53; ValidatorNodeBindingV1::CANONICAL_LEN]),
            validatorSignature: Bytes::from(vec![0x54; 65]),
            nodeBindingSignature: Bytes::from(vec![0x55; 65]),
        }
        .abi_encode(),
        RegistryMutatorV1::RenewEnclave => ITeeRegistryV1::renewEnclaveCall {
            evidence: Bytes::from(evidence),
            nodeSignature: node_signature,
            enclaveSignature: enclave_signature,
        }
        .abi_encode(),
        RegistryMutatorV1::ReplaceEnclaveBinding => ITeeRegistryV1::replaceEnclaveBindingCall {
            evidence: Bytes::from(evidence),
            nodeSignature: node_signature,
            enclaveSignature: enclave_signature,
        }
        .abi_encode(),
        RegistryMutatorV1::TransitionEnclaveMeasurement => {
            ITeeRegistryV1::transitionEnclaveMeasurementCall {
                evidence: Bytes::from(evidence),
                nodeSignature: node_signature,
                enclaveSignature: enclave_signature,
            }
            .abi_encode()
        }
    }
}

fn canonical_dcap_evidence(kind: RegistryMutatorV1) -> Vec<u8> {
    let policy = policy();
    let node_key = k256::ecdsa::SigningKey::from_bytes((&[0x61; 32]).into()).unwrap();
    let reth_p2p_public = node_key.verifying_key().to_encoded_point(true);
    let mut intent = RegistrationIntentV1 {
        chain_id: policy.chain_id,
        genesis_hash: policy.genesis_hash,
        operation: match kind {
            RegistryMutatorV1::PrepareEnclaveUpgrade => {
                AttestationOperationV1::PrepareEnclaveUpgrade
            }
            RegistryMutatorV1::RegisterEnclave => AttestationOperationV1::RegisterEnclave,
            RegistryMutatorV1::RenewEnclave => AttestationOperationV1::RenewEnclave,
            RegistryMutatorV1::TransitionEnclaveMeasurement => {
                AttestationOperationV1::TransitionEnclaveMeasurement
            }
            RegistryMutatorV1::ReplaceEnclaveBinding => {
                AttestationOperationV1::ReplaceEnclaveBinding
            }
        },
        attestation_mode: AttestationMode::DcapRequired,
        policy_hash: policy.policy_hash().unwrap(),
        node_id: NodeIdV1 {
            reth_p2p_public: reth_p2p_public.as_bytes().try_into().unwrap(),
        },
        enclave_id: B256::repeat_byte(1),
        binding_id: B256::repeat_byte(2),
        binding_version: 1,
        registration_version: 0,
        renewal_nonce: 0,
        transition_nonce: 0,
        requested_valid_until: 7_200,
        recipient_x25519: [3; 32],
        attestation_ed25519: [4; 32],
        noise_responder_x25519: [5; 32],
        node_host_authorization_hash: B256::repeat_byte(6),
    };
    intent.enclave_id = intent.derived_enclave_id().unwrap();
    AttestationEvidenceV1::Dcap(DcapEvidenceV1 {
        intent,
        quote: vec![7],
        components: outbe_tee::test_utils::canonical_dcap_collateral_fixture(),
        transition_key_ready_proof: None,
    })
    .encode_canonical()
    .unwrap()
}

#[test]
fn canonical_register_preflight_is_borrowed_and_rejects_noncanonical_framing() {
    let encoded = call(vec![1, 2, 3], 65, 64);
    assert_eq!(
        preflight_evidence_mutator_call(&encoded).unwrap().evidence,
        [1, 2, 3]
    );

    let mut wrong_offset = encoded.clone();
    wrong_offset[35] = 0x80;
    assert!(preflight_evidence_mutator_call(&wrong_offset).is_err());

    let mut nonzero_padding = encoded;
    let evidence_value_start = 4 + 96 + 32;
    nonzero_padding[evidence_value_start + 3] = 1;
    assert!(preflight_evidence_mutator_call(&nonzero_padding).is_err());
    assert!(preflight_evidence_mutator_call(&call(vec![1], 64, 64)).is_err());
    assert!(preflight_evidence_mutator_call(&call(vec![1], 65, 63)).is_err());
}

#[test]
fn register_precharges_exact_protocol_gas_before_evidence_decode() {
    let mut provider = installed_provider(&policy(), 1);
    meter_production_gas(&mut provider);

    // Canonical outer ABI, deliberately malformed canonical evidence. The
    // decoder rejection must occur only after the complete QVL/register
    // protocol charge is reserved.
    let input = call(vec![1, 1, 0, 0, 0, 0], 65, 64);
    let budget = normative_budget(
        RegistryMutatorV1::RegisterEnclave,
        input.len(),
        6,
        &policy(),
    );
    let dispatch_charge =
        budget.maximum - budget.intrinsic - PRECOMPILE_BASE_GAS - budget.allowance;

    // Actual production-shaped storage gas consumes the allowance already
    // included in `register_fixed`. It must never sit above the normative
    // maximum. The malformed canonical evidence is rejected before verifier
    // invocation. No state write is reachable.
    assert_dispatch_reverts(
        &mut provider,
        &input,
        0x77,
        "malformed canonical evidence framing must revert",
    );
    let precharged = PrechargedRevert::assert_metered(&provider, dispatch_charge);
    assert_normative_gas(
        &provider,
        &[MeteredCall {
            kind: RegistryMutatorV1::RegisterEnclave,
            calldata: &input,
        }],
        6,
        &policy(),
    );

    precharged.assert_exact_gas_reverts(
        &mut provider,
        &input,
        0x88,
        "exact-gas malformed evidence must still revert",
    );

    provider.set_gas_limit(precharged.exact_dispatch_gas - 1);
    let result =
        provider.enter(|storage| dispatch(storage, &input, Address::repeat_byte(0x99), U256::ZERO));
    assert!(matches!(result, Err(PrecompileError::OutOfGas)));
    assert_eq!(provider.gas_used(), precharged.policy_reads * 100);
    assert_eq!(
        provider.metered_storage_operations(),
        (precharged.policy_reads, 0)
    );
}

#[test]
fn gramine_direct_dev_register_precharge_uses_active_policy_mode() {
    let mut active_policy = policy();
    active_policy.attestation_mode = AttestationMode::GramineDirectDev;
    let mut provider = installed_provider(&active_policy, 1);
    meter_production_gas(&mut provider);

    // Canonical outer ABI with malformed development evidence reaches the
    // mode-selected precharge and then reverts during evidence decoding.
    // The exact GramineDirectDev maximum must be sufficient; charging the
    // production DCAP schedule here makes the reachable dev transaction
    // consume its entire signed gas limit before validation.
    let input = call(vec![1, 1, 0, 0, 0, 0], 65, 64);
    let budget = normative_budget(
        RegistryMutatorV1::RegisterEnclave,
        input.len(),
        6,
        &active_policy,
    );
    let dispatch_charge =
        budget.maximum - budget.intrinsic - PRECOMPILE_BASE_GAS - budget.allowance;

    assert_dispatch_reverts(
        &mut provider,
        &input,
        0xA7,
        "malformed development evidence must revert after its mode-selected precharge",
    );
    let precharged = PrechargedRevert::assert_metered(&provider, dispatch_charge);

    precharged.assert_exact_gas_reverts(
        &mut provider,
        &input,
        0xA8,
        "exact GramineDirectDev gas must reach canonical evidence rejection",
    );
}

#[test]
fn unavailable_verifier_never_mutates_or_fails_open_for_renewal_or_replacement() {
    for kind in [
        RegistryMutatorV1::RenewEnclave,
        RegistryMutatorV1::ReplaceEnclaveBinding,
    ] {
        let mut provider = installed_provider(&policy(), 1);
        meter_production_gas(&mut provider);

        let evidence = canonical_dcap_evidence(kind);
        let evidence_len = evidence.len();
        let input = mutator_call(kind, evidence, 65, 64);
        let budget = normative_budget(kind, input.len(), evidence_len, &policy());

        let result = provider
            .enter(|storage| dispatch(storage, &input, Address::repeat_byte(0xA1), U256::ZERO));
        assert!(
            matches!(result, Err(PrecompileError::EnclaveUnavailable(_))),
            "unexpected pre-verifier result for {kind:?}: {result:?}"
        );
        let reads = assert_metered_writes(
            &provider,
            0,
            "verifier outage must not extend registry state",
        );
        let storage_gas = reads * 100;
        assert_eq!(
            budget.intrinsic + PRECOMPILE_BASE_GAS + provider.gas_used(),
            budget.maximum - budget.allowance + storage_gas
        );
        assert!(storage_gas <= budget.allowance);
        assert!(budget.intrinsic + PRECOMPILE_BASE_GAS + provider.gas_used() <= budget.maximum);
    }
}

#[test]
fn cap_plus_one_rejects_before_policy_read_or_gas_charge() {
    let input = call(vec![0; MAX_ATTESTATION_EVIDENCE_BYTES + 1], 65, 64);
    let mut provider = HashMapStorageProvider::new_with_chain_identity(CHAIN_ID, GENESIS);
    provider.set_gas_limit(u64::MAX);
    let result =
        provider.enter(|storage| dispatch(storage, &input, Address::repeat_byte(0x99), U256::ZERO));
    match result {
        Err(PrecompileError::RevertBytes(bytes)) => {
            let decoded = ITeeRegistryV1::AttestationEvidenceExceedsMaxBytes::abi_decode(&bytes)
                .expect("evidence cap");
            assert_eq!(
                decoded.value,
                u64::try_from(MAX_ATTESTATION_EVIDENCE_BYTES).unwrap()
            );
        }
        other => panic!("expected typed evidence-cap revert, got {other:?}"),
    }
    assert_eq!(provider.gas_used(), 0);
}

#[test]
fn empty_binding_views_are_fixed_and_not_ready() {
    let validator = Address::repeat_byte(0x61);
    let mut provider = HashMapStorageProvider::new_with_chain_identity(CHAIN_ID, GENESIS);
    let ready_call = ITeeRegistryV1::isValidatorEnclaveReadyCall { validator };
    let ready = provider
        .enter(|storage| dispatch(storage, &ready_call.abi_encode(), Address::ZERO, U256::ZERO))
        .unwrap();
    assert!(!bool::abi_decode(&ready).unwrap());

    let binding_call = ITeeRegistryV1::validatorEnclaveBindingCall { validator };
    let encoded = provider
        .enter(|storage| {
            dispatch(
                storage,
                &binding_call.abi_encode(),
                Address::ZERO,
                U256::ZERO,
            )
        })
        .unwrap();
    let binding = NodeEnclaveBindingV1View::abi_decode(&encoded).unwrap();
    assert!(!binding.exists);
    assert_eq!(binding.nodeIdHash, B256::ZERO);
    assert_eq!(encoded.len(), 24 * 32);

    let p2p_signing = k256::ecdsa::SigningKey::from_bytes((&[0x62; 32]).into()).unwrap();
    let encoded_public = p2p_signing.verifying_key().to_encoded_point(true);
    let reth_p2p_prefix = encoded_public.as_bytes()[0];
    let reth_p2p_x = B256::from_slice(&encoded_public.as_bytes()[1..]);
    let ready_call = ITeeRegistryV1::isNodeHostEnclaveReadyCall {
        rethP2pPrefix: reth_p2p_prefix,
        rethP2pX: reth_p2p_x,
    };
    let ready = provider
        .enter(|storage| dispatch(storage, &ready_call.abi_encode(), Address::ZERO, U256::ZERO))
        .unwrap();
    assert!(!bool::abi_decode(&ready).unwrap());

    let binding_call = ITeeRegistryV1::nodeHostEnclaveBindingCall {
        rethP2pPrefix: reth_p2p_prefix,
        rethP2pX: reth_p2p_x,
    };
    let encoded = provider
        .enter(|storage| {
            dispatch(
                storage,
                &binding_call.abi_encode(),
                Address::ZERO,
                U256::ZERO,
            )
        })
        .unwrap();
    assert!(
        !NodeEnclaveBindingV1View::abi_decode(&encoded)
            .unwrap()
            .exists
    );

    let invalid = ITeeRegistryV1::isNodeHostEnclaveReadyCall {
        rethP2pPrefix: 0x04,
        rethP2pX: reth_p2p_x,
    };
    assert!(provider
        .enter(|storage| dispatch(storage, &invalid.abi_encode(), Address::ZERO, U256::ZERO))
        .is_err());
}
