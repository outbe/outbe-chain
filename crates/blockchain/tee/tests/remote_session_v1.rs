use alloy_primitives::B256;
use outbe_primitives::tee_attestation_v1::{
    AttestationMode, NodeHostAuthorizationWitnessV1, NodeIdV1,
};
use outbe_tee::{
    admit_remote_session_v1, admit_rpc_trusted_remote_session_v1, FinalizedRegistryBindingV1,
    FinalizedRegistryViewV1, RemoteSessionAdmissionError, RemoteSessionExpectationV1,
};

fn node(seed: u8) -> NodeIdV1 {
    let signing = k256::ecdsa::SigningKey::from_bytes((&[seed; 32]).into()).unwrap();
    NodeIdV1 {
        reth_p2p_public: signing
            .verifying_key()
            .to_encoded_point(true)
            .as_bytes()
            .try_into()
            .unwrap(),
    }
}

// Two independent success paths use the same field layout with distinct
// deterministic byte families. The seeds remain visible in each assertion.
fn seeded_success_case(base: u8, block_number: u64, consensus_timestamp: u64) -> AdmissionCase {
    let byte = |offset| base.checked_add(offset).expect("fixture byte overflow");
    let source_node = node(byte(0x01));
    let target_node = node(byte(0x11));
    let chain_id = [byte(0x21); 32];
    let genesis_hash = B256::repeat_byte(byte(0x22));
    let view = FinalizedRegistryViewV1 {
        chain_id,
        genesis_hash,
        block_number,
        block_hash: B256::repeat_byte(byte(0x23)),
        state_root: B256::repeat_byte(byte(0x24)),
        consensus_timestamp,
    };
    let witness = NodeHostAuthorizationWitnessV1 {
        chain_id,
        genesis_hash,
        attestation_mode: AttestationMode::DcapRequired,
        node_id: source_node.clone(),
        node_host_noise_x25519: [byte(0x31); 32],
    };
    let source = FinalizedRegistryBindingV1 {
        view,
        node_id_hash: source_node.node_id_hash().unwrap(),
        enclave_id: B256::repeat_byte(byte(0x32)),
        binding_id: B256::repeat_byte(byte(0x33)),
        intent_hash: B256::repeat_byte(byte(0x34)),
        valid_until: consensus_timestamp + 500,
        noise_responder_x25519: [byte(0x35); 32],
        node_host_authorization_hash: witness.authorization_hash().unwrap(),
    };
    let target = FinalizedRegistryBindingV1 {
        view,
        node_id_hash: target_node.node_id_hash().unwrap(),
        enclave_id: B256::repeat_byte(byte(0x42)),
        binding_id: B256::repeat_byte(byte(0x43)),
        intent_hash: B256::repeat_byte(byte(0x44)),
        valid_until: consensus_timestamp + 400,
        noise_responder_x25519: [byte(0x45); 32],
        node_host_authorization_hash: B256::repeat_byte(byte(0x46)),
    };
    AdmissionCase {
        expected: RemoteSessionExpectationV1 {
            chain_id,
            genesis_hash,
            source_node_id_hash: source.node_id_hash,
            target_node_id_hash: target.node_id_hash,
        },
        witness,
        source,
        target,
    }
}

#[test]
fn exact_finalized_source_and_target_admit_one_bounded_session() {
    let case = seeded_success_case(0x10, 90, 1_000);
    let admitted =
        admit_remote_session_v1(case.expected, &case.witness, case.source, case.target).unwrap();
    assert_eq!(admitted.initiator_static_x25519(), [0x41; 32]);
    assert_eq!(admitted.responder_static_x25519(), [0x55; 32]);
    assert_eq!(admitted.deadline(), 1_400);
    assert_eq!(admitted.finalized_view(), case.source.view);
}

#[test]
fn rpc_trust_is_an_explicit_non_finality_verified_result_type() {
    let case = seeded_success_case(0x60, 100, 2_000);
    let rpc_trusted =
        admit_rpc_trusted_remote_session_v1(case.expected, &case.witness, case.source, case.target)
            .unwrap();
    assert_eq!(rpc_trusted.initiator_static_x25519(), [0x91; 32]);
    assert_eq!(rpc_trusted.responder_static_x25519(), [0xA5; 32]);
    assert_eq!(rpc_trusted.deadline(), 2_400);
    assert_eq!(rpc_trusted.rpc_claimed_view(), case.source.view);
}

#[test]
fn stale_mixed_chain_genesis_and_nodehost_key_substitutions_reject() {
    let (expected, source_witness, source, target) = admission_fixture();

    let mut stale_source = source;
    stale_source.valid_until = source.view.consensus_timestamp;
    assert_eq!(
        admit_remote_session_v1(expected, &source_witness, stale_source, target),
        Err(RemoteSessionAdmissionError::SourceExpired)
    );
    let mut stale_target = target;
    stale_target.valid_until = target.view.consensus_timestamp;
    assert_eq!(
        admit_remote_session_v1(expected, &source_witness, source, stale_target),
        Err(RemoteSessionAdmissionError::TargetExpired)
    );

    let mut mixed = target;
    mixed.view.block_hash = B256::repeat_byte(0xE1);
    assert_eq!(
        admit_remote_session_v1(expected, &source_witness, source, mixed),
        Err(RemoteSessionAdmissionError::MixedFinalizedViews)
    );

    let mut wrong_chain = expected;
    wrong_chain.chain_id = [0xE2; 32];
    assert_eq!(
        admit_remote_session_v1(wrong_chain, &source_witness, source, target),
        Err(RemoteSessionAdmissionError::WrongChain)
    );
    let mut wrong_genesis = expected;
    wrong_genesis.genesis_hash = B256::repeat_byte(0xE3);
    assert_eq!(
        admit_remote_session_v1(wrong_genesis, &source_witness, source, target),
        Err(RemoteSessionAdmissionError::WrongGenesis)
    );

    let mut wrong_nodehost = source_witness;
    wrong_nodehost.node_host_noise_x25519 = [0xE4; 32];
    assert_eq!(
        admit_remote_session_v1(expected, &wrong_nodehost, source, target),
        Err(RemoteSessionAdmissionError::WrongSourceWitness)
    );
}

fn admission_fixture() -> (
    RemoteSessionExpectationV1,
    NodeHostAuthorizationWitnessV1,
    FinalizedRegistryBindingV1,
    FinalizedRegistryBindingV1,
) {
    let source_node = node(0xB1);
    let target_node = node(0xC1);
    let chain_id = [0xD1; 32];
    let genesis_hash = B256::repeat_byte(0xD2);
    let view = FinalizedRegistryViewV1 {
        chain_id,
        genesis_hash,
        block_number: 110,
        block_hash: B256::repeat_byte(0xD3),
        state_root: B256::repeat_byte(0xD4),
        consensus_timestamp: 3_000,
    };
    let source_witness = NodeHostAuthorizationWitnessV1 {
        chain_id,
        genesis_hash,
        attestation_mode: AttestationMode::DcapRequired,
        node_id: source_node.clone(),
        node_host_noise_x25519: [0xD5; 32],
    };
    let source = FinalizedRegistryBindingV1 {
        view,
        node_id_hash: source_node.node_id_hash().unwrap(),
        enclave_id: B256::repeat_byte(0xD6),
        binding_id: B256::repeat_byte(0xD7),
        intent_hash: B256::repeat_byte(0xD8),
        valid_until: 3_500,
        noise_responder_x25519: [0xD9; 32],
        node_host_authorization_hash: source_witness.authorization_hash().unwrap(),
    };
    let target = FinalizedRegistryBindingV1 {
        view,
        node_id_hash: target_node.node_id_hash().unwrap(),
        enclave_id: B256::repeat_byte(0xDA),
        binding_id: B256::repeat_byte(0xDB),
        intent_hash: B256::repeat_byte(0xDC),
        valid_until: 3_400,
        noise_responder_x25519: [0xDD; 32],
        node_host_authorization_hash: B256::repeat_byte(0xDE),
    };
    (
        RemoteSessionExpectationV1 {
            chain_id,
            genesis_hash,
            source_node_id_hash: source.node_id_hash,
            target_node_id_hash: target.node_id_hash,
        },
        source_witness,
        source,
        target,
    )
}

#[derive(Clone)]
struct AdmissionCase {
    expected: RemoteSessionExpectationV1,
    witness: NodeHostAuthorizationWitnessV1,
    source: FinalizedRegistryBindingV1,
    target: FinalizedRegistryBindingV1,
}

impl AdmissionCase {
    fn valid() -> Self {
        let (expected, witness, source, target) = admission_fixture();
        Self {
            expected,
            witness,
            source,
            target,
        }
    }

    fn assert_error(&self, expected_error: RemoteSessionAdmissionError) {
        assert_eq!(
            admit_remote_session_v1(self.expected, &self.witness, self.source, self.target),
            Err(expected_error)
        );
    }
}

fn malformed_bindings(binding: FinalizedRegistryBindingV1) -> [FinalizedRegistryBindingV1; 7] {
    [
        FinalizedRegistryBindingV1 {
            node_id_hash: B256::ZERO,
            ..binding
        },
        FinalizedRegistryBindingV1 {
            enclave_id: B256::ZERO,
            ..binding
        },
        FinalizedRegistryBindingV1 {
            binding_id: B256::ZERO,
            ..binding
        },
        FinalizedRegistryBindingV1 {
            intent_hash: B256::ZERO,
            ..binding
        },
        FinalizedRegistryBindingV1 {
            valid_until: 0,
            ..binding
        },
        FinalizedRegistryBindingV1 {
            noise_responder_x25519: [0; 32],
            ..binding
        },
        FinalizedRegistryBindingV1 {
            node_host_authorization_hash: B256::ZERO,
            ..binding
        },
    ]
}

#[test]
fn finalized_view_and_both_bindings_reject_each_missing_field() {
    let valid = AdmissionCase::valid();
    let view = valid.source.view;
    let malformed_views = [
        FinalizedRegistryViewV1 {
            chain_id: [0; 32],
            ..view
        },
        FinalizedRegistryViewV1 {
            genesis_hash: B256::ZERO,
            ..view
        },
        FinalizedRegistryViewV1 {
            block_number: 0,
            ..view
        },
        FinalizedRegistryViewV1 {
            block_hash: B256::ZERO,
            ..view
        },
        FinalizedRegistryViewV1 {
            state_root: B256::ZERO,
            ..view
        },
        FinalizedRegistryViewV1 {
            consensus_timestamp: 0,
            ..view
        },
    ];
    for invalid in malformed_views {
        let mut case = valid.clone();
        case.source.view = invalid;
        case.assert_error(RemoteSessionAdmissionError::MalformedFinalizedView);
    }
    for invalid in malformed_bindings(valid.source) {
        let mut case = valid.clone();
        case.source = invalid;
        case.assert_error(RemoteSessionAdmissionError::MalformedBinding);
    }
    for invalid in malformed_bindings(valid.target) {
        let mut case = valid.clone();
        case.target = invalid;
        case.assert_error(RemoteSessionAdmissionError::MalformedBinding);
    }
}

#[test]
fn remote_admission_preserves_first_error_across_validation_phases() {
    let valid = AdmissionCase::valid();

    let mut case = valid.clone();
    case.source.view.chain_id = [0; 32];
    case.target.view.block_hash = B256::repeat_byte(0xe1);
    case.assert_error(RemoteSessionAdmissionError::MalformedFinalizedView);

    let mut case = valid.clone();
    case.target.view.block_hash = B256::repeat_byte(0xe1);
    case.source.enclave_id = B256::ZERO;
    case.assert_error(RemoteSessionAdmissionError::MixedFinalizedViews);

    let mut case = valid.clone();
    case.target.enclave_id = B256::ZERO;
    case.expected.chain_id = [0xe2; 32];
    case.assert_error(RemoteSessionAdmissionError::MalformedBinding);

    let mut case = valid.clone();
    case.expected.chain_id = [0xe2; 32];
    case.expected.genesis_hash = B256::repeat_byte(0xe3);
    case.assert_error(RemoteSessionAdmissionError::WrongChain);

    let mut case = valid.clone();
    case.expected.genesis_hash = B256::repeat_byte(0xe3);
    case.expected.source_node_id_hash = B256::repeat_byte(0xe4);
    case.assert_error(RemoteSessionAdmissionError::WrongGenesis);

    let mut case = valid.clone();
    case.expected.source_node_id_hash = B256::repeat_byte(0xe4);
    case.expected.target_node_id_hash = B256::repeat_byte(0xe5);
    case.assert_error(RemoteSessionAdmissionError::WrongSourceNode);

    let mut case = valid.clone();
    case.expected.target_node_id_hash = B256::repeat_byte(0xe5);
    case.witness.node_id.reth_p2p_public = [0; 33];
    case.assert_error(RemoteSessionAdmissionError::WrongTargetNode);

    let mut case = valid.clone();
    case.witness.node_id.reth_p2p_public = [0; 33];
    case.witness.chain_id = [0xe6; 32];
    case.assert_error(RemoteSessionAdmissionError::MalformedSourceWitness);

    let mut case = valid.clone();
    case.witness.node_host_noise_x25519 = [0xe7; 32];
    case.source.valid_until = case.source.view.consensus_timestamp;
    case.assert_error(RemoteSessionAdmissionError::WrongSourceWitness);

    let mut case = valid;
    case.source.valid_until = case.source.view.consensus_timestamp;
    case.target.valid_until = case.target.view.consensus_timestamp;
    case.assert_error(RemoteSessionAdmissionError::SourceExpired);
}

#[test]
fn malformed_witness_hash_sites_and_exclusive_lease_boundary_are_distinct() {
    let valid = AdmissionCase::valid();

    let mut invalid_node = valid.clone();
    invalid_node.witness.node_id.reth_p2p_public = [0; 33];
    invalid_node.assert_error(RemoteSessionAdmissionError::MalformedSourceWitness);

    let mut invalid_authorization = valid.clone();
    invalid_authorization.witness.chain_id = [0; 32];
    invalid_authorization.assert_error(RemoteSessionAdmissionError::MalformedSourceWitness);

    let mut source_boundary = valid.clone();
    source_boundary.source.valid_until = source_boundary.source.view.consensus_timestamp;
    source_boundary.assert_error(RemoteSessionAdmissionError::SourceExpired);

    let mut target_boundary = valid.clone();
    target_boundary.target.valid_until = target_boundary.source.view.consensus_timestamp;
    target_boundary.assert_error(RemoteSessionAdmissionError::TargetExpired);

    let mut one_second_lease = valid;
    let next_second = one_second_lease.source.view.consensus_timestamp + 1;
    one_second_lease.source.valid_until = next_second;
    one_second_lease.target.valid_until = next_second + 1;
    let admitted = admit_remote_session_v1(
        one_second_lease.expected,
        &one_second_lease.witness,
        one_second_lease.source,
        one_second_lease.target,
    )
    .unwrap();
    assert_eq!(admitted.deadline(), next_second);
    assert_eq!(admitted.retirement_height(), 0);
}
