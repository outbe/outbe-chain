//! CHARACTERIZATION: consensus bytes frozen before the in-process TEE migration.
#![cfg(feature = "tee-attestation-v1")]
use alloy_primitives::{hex, keccak256, B256};
use alloy_sol_types::{SolCall, SolEvent};
use outbe_primitives::{
    chain::{DEVNET_CHAIN_ID, MAINNET_CHAIN_ID, TESTNET_CHAIN_ID},
    tee_genesis_v1::{
        initial_tee_policy_v1, is_attestation_mode_allowed_for_chain_id, InitialTeeProfileV1,
    },
};
use outbe_primitives::{tee_attestation_v1::*, tee_registry_abi_v1::ITeeRegistryV1};

fn node() -> NodeIdV1 {
    NodeIdV1 {
        reth_p2p_public: hex!("0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798"),
    }
}
fn assert_hash(bytes: &[u8], expected: &str) {
    assert_eq!(hex::encode(keccak256(bytes)), expected);
}

#[test]
fn network_and_stable_node_authorization_vectors() {
    assert_eq!(AttestationMode::DcapRequired as u8, 1);
    assert_eq!(AttestationMode::GramineDirectDev as u8, 2);
    assert_eq!(NetworkBindingV1::CANONICAL_LEN, 66);
    let binding = NetworkBindingV1 {
        chain_id: [0x11; 32],
        genesis_hash: B256::repeat_byte(0x22),
        attestation_mode: AttestationMode::GramineDirectDev,
    };
    let expected = hex::decode(concat!(
    "011111111111111111111111111111111111111111111111111111111111111111222222222222222222222222222222",
    "222222222222222222222222222222222202",
)).unwrap();
    assert_eq!(binding.encode_canonical().unwrap(), expected);
    assert_eq!(
        NetworkBindingV1::decode_canonical(&expected).unwrap(),
        binding
    );
    assert_eq!(
        hex::encode(binding.binding_hash().unwrap()),
        "f97a977c7b981bdee73e47d1ecfc25502b268e10eab37c778cbc8ffead96f6d6"
    );
    let authorization = NodeHostAuthorizationWitnessV1 {
        chain_id: binding.chain_id,
        genesis_hash: binding.genesis_hash,
        attestation_mode: binding.attestation_mode,
        node_id: node(),
        node_host_noise_x25519: [0x33; 32],
    };
    let expected = hex::decode(concat!(
    "011111111111111111111111111111111111111111111111111111111111111111222222222222222222222222222222",
    "22222222222222222222222222222222220201000000210279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d9",
    "59f2815b16f817983333333333333333333333333333333333333333333333333333333333333333",
)).unwrap();
    assert_eq!(expected.len(), 136);
    assert_eq!(authorization.encode_canonical().unwrap(), expected);
    assert_eq!(
        NodeHostAuthorizationWitnessV1::decode_canonical(&expected).unwrap(),
        authorization
    );
    assert_eq!(
        hex::encode(authorization.authorization_hash().unwrap()),
        "704154d1a9b6007f01b25f107ff0f489496b3794813146f07ef4d31c731331f8"
    );
    let mut manifest = EnclaveInitializationManifestV1 {
        chain_id: binding.chain_id,
        genesis_hash: binding.genesis_hash,
        attestation_mode: binding.attestation_mode,
        node_id: node(),
        node_host_noise_x25519: [0x33; 32],
        initialization_challenge: [0x44; 32],
        recipient_x25519: [0x55; 32],
        attestation_ed25519: [0x66; 32],
        noise_responder_x25519: [0x77; 32],
    };
    assert_eq!(
        NodeHostAuthorizationWitnessV1::from_manifest(&manifest).unwrap(),
        authorization
    );
    manifest.initialization_challenge = [0x88; 32];
    manifest.recipient_x25519 = [0x99; 32];
    manifest.attestation_ed25519 = [0xaa; 32];
    manifest.noise_responder_x25519 = [0xbb; 32];
    assert_eq!(
        NodeHostAuthorizationWitnessV1::from_manifest(&manifest).unwrap(),
        authorization
    );
    assert_eq!(
        manifest.node_host_authorization_hash().unwrap(),
        authorization.authorization_hash().unwrap()
    );
}

#[test]
fn consensus_domain_literals() {
    for (actual, literal) in [
        (POLICY_DOMAIN_V1, b"outbe/tee/policy/v1".as_slice()),
        (
            POLICY_SCHEDULE_DOMAIN_V1,
            b"outbe/tee/policy-schedule/v1".as_slice(),
        ),
        (
            TEE_REGISTRY_GAS_SCHEDULE_DOMAIN_V1,
            b"outbe/tee-registry-gas-schedule/v1".as_slice(),
        ),
        (
            SYSTEM_GAS_SCHEDULE_DOMAIN_V1,
            b"outbe/system-gas-schedule/v1".as_slice(),
        ),
        (
            RESOURCE_SCHEDULE_DOMAIN_V1,
            b"outbe/resource-schedule/v1".as_slice(),
        ),
        (NODE_ID_DOMAIN_V1, b"outbe/tee/node-id/v1".as_slice()),
        (
            VALIDATOR_NODE_BINDING_DOMAIN_V1,
            b"outbe/tee/validator-node-binding/v1".as_slice(),
        ),
        (
            REGISTRATION_INTENT_DOMAIN_V1,
            b"outbe/tee/registration-intent/v1".as_slice(),
        ),
        (
            ATTESTATION_EVIDENCE_DOMAIN_V1,
            b"outbe/tee/attestation-evidence/v1".as_slice(),
        ),
        (ENCLAVE_ID_DOMAIN_V1, b"outbe/tee/enclave-id/v1".as_slice()),
        (
            INITIALIZATION_MANIFEST_DOMAIN_V1,
            b"outbe/tee/initialization-manifest/v1".as_slice(),
        ),
        (
            NODE_HOST_AUTHORIZATION_DOMAIN_V1,
            b"outbe/tee/node-host-authorization/v1".as_slice(),
        ),
        (
            NETWORK_BINDING_DOMAIN_V1,
            b"outbe/tee/network-binding/v1".as_slice(),
        ),
        (
            TRUSTED_NETWORK_DESCRIPTOR_DOMAIN_V1,
            b"outbe/tee/trusted-network-descriptor/v1".as_slice(),
        ),
        (
            DKG_PARTICIPANT_SET_DOMAIN_V1,
            b"outbe/tee/dkg-participant-set/v1".as_slice(),
        ),
        (
            DKG_CEREMONY_DOMAIN_V1,
            b"outbe/tee/dkg-ceremony/v1".as_slice(),
        ),
        (
            DKG_PARTICIPANT_ANNOUNCE_DOMAIN_V1,
            b"outbe/tee/dkg-participant-announce/v1".as_slice(),
        ),
        (
            TRANSITION_KEY_READY_PROOF_DOMAIN_V1,
            b"outbe/tee/transition-key-ready-proof/v1".as_slice(),
        ),
        (
            REPORT_POLICY_DOMAIN_V1,
            b"outbe/tee/report-policy/v1".as_slice(),
        ),
    ] {
        assert_eq!(actual, literal);
    }
}

#[test]
fn genesis_measurements_policy_and_mode_admission() {
    for (chain_id, allowed) in [
        (DEVNET_CHAIN_ID, true),
        (TESTNET_CHAIN_ID, true),
        (MAINNET_CHAIN_ID, false),
        (0, false),
        (u64::MAX, false),
    ] {
        assert_eq!(
            is_attestation_mode_allowed_for_chain_id(chain_id, AttestationMode::GramineDirectDev),
            allowed
        );
    }
    for chain_id in [DEVNET_CHAIN_ID, TESTNET_CHAIN_ID, MAINNET_CHAIN_ID] {
        assert!(is_attestation_mode_allowed_for_chain_id(
            chain_id,
            AttestationMode::DcapRequired
        ));
    }
    assert!(!is_attestation_mode_allowed_for_chain_id(
        u64::MAX,
        AttestationMode::DcapRequired
    ));
    let policy = initial_tee_policy_v1(
        InitialTeeProfileV1::GramineDirectDev,
        DEVNET_CHAIN_ID,
        B256::repeat_byte(0x22),
    )
    .unwrap();
    let rule = &policy.measurement_rules[0];
    assert_eq!(
        hex::encode(rule.mrenclave),
        "432059db8927087e8e999f65d1888b2cfb3de4a8ae84b5272c1b42ca20ffd7ca"
    );
    assert_eq!(
        hex::encode(rule.mrsigner),
        "53d827e693b032ad9feb018e8c73aa9a890299adbab1c0a1760f9731026cbba5"
    );
    assert_hash(
        b"outbe/tee/gramine-direct-dev/mrenclave/v1",
        "432059db8927087e8e999f65d1888b2cfb3de4a8ae84b5272c1b42ca20ffd7ca",
    );
    assert_hash(
        b"outbe/tee/gramine-direct-dev/mrsigner/v1",
        "53d827e693b032ad9feb018e8c73aa9a890299adbab1c0a1760f9731026cbba5",
    );
    assert_eq!(
        hex::encode(policy.policy_hash().unwrap()),
        "7dccb73d5ee5d08cf12d83c4a68ad6753119dfbf1e82c5262bf8537b85f1f4cd"
    );
}

#[test]
fn registry_abi_signatures() {
    for (actual, signature, selector) in [
        (
            ITeeRegistryV1::registerEnclaveCall::SELECTOR,
            "registerEnclave(bytes,bytes,bytes,bytes,bytes,bytes)",
            "05c09143",
        ),
        (
            ITeeRegistryV1::renewEnclaveCall::SELECTOR,
            "renewEnclave(bytes,bytes,bytes)",
            "d5e59650",
        ),
        (
            ITeeRegistryV1::prepareEnclaveUpgradeCall::SELECTOR,
            "prepareEnclaveUpgrade(bytes,bytes,bytes)",
            "60c1dfb8",
        ),
        (
            ITeeRegistryV1::transitionEnclaveMeasurementCall::SELECTOR,
            "transitionEnclaveMeasurement(bytes,bytes,bytes)",
            "a796ccea",
        ),
        (
            ITeeRegistryV1::cancelEnclaveUpgradeCall::SELECTOR,
            "cancelEnclaveUpgrade(bytes32,bytes32)",
            "fc2360c5",
        ),
    ] {
        assert_eq!(actual, keccak256(signature.as_bytes())[..4]);
        assert_eq!(hex::encode(actual), selector);
    }
    let signature = "OfferKeySealedForRegistryV1(bytes32,bytes)";
    assert_eq!(
        ITeeRegistryV1::OfferKeySealedForRegistryV1::SIGNATURE,
        signature
    );
    assert_eq!(
        ITeeRegistryV1::OfferKeySealedForRegistryV1::SIGNATURE_HASH,
        keccak256(signature)
    );
    assert_eq!(
        hex::encode(ITeeRegistryV1::OfferKeySealedForRegistryV1::SIGNATURE_HASH),
        "ad6a7c5f7c80db47a3d7c41111425cc794bbeba88fccfb1a80449656c763a999"
    );
}

#[test]
fn evidence_canonical_framing_vector() {
    let intent = RegistrationIntentV1 {
        chain_id: [0x11; 32],
        genesis_hash: B256::repeat_byte(0x22),
        operation: AttestationOperationV1::RegisterEnclave,
        attestation_mode: AttestationMode::GramineDirectDev,
        policy_hash: B256::repeat_byte(0x33),
        node_id: node(),
        enclave_id: B256::repeat_byte(0x44),
        binding_id: B256::repeat_byte(0x55),
        binding_version: 1,
        registration_version: 2,
        renewal_nonce: 3,
        transition_nonce: 4,
        requested_valid_until: 7200,
        recipient_x25519: [0x66; 32],
        attestation_ed25519: [0x77; 32],
        noise_responder_x25519: [0x88; 32],
        node_host_authorization_hash: B256::repeat_byte(0x99),
    };
    let evidence = AttestationEvidenceV1::GramineDirectDev(GramineDirectEvidenceV1 {
        intent: intent.clone(),
        dev_attestation_public: [0xaa; 32],
        dev_signature: [0xbb; 64],
        transition_key_ready_proof: None,
    });
    let expected = hex::decode(concat!(
    "0102000001d6010000017101111111111111111111111111111111111111111111111111111111111111111122222222",
    "222222222222222222222222222222222222222222222222222222220102333333333333333333333333333333333333",
    "333333333333333333333333333301000000210279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b",
    "16f817984444444444444444444444444444444444444444444444444444444444444444555555555555555555555555",
    "555555555555555555555555555555555555555500000000000000010000000000000002000000000000000300000000",
    "000000040000000000001c20666666666666666666666666666666666666666666666666666666666666666677777777",
    "777777777777777777777777777777777777777777777777777777778888888888888888888888888888888888888888",
    "8888888888888888888888889999999999999999999999999999999999999999999999999999999999999999aaaaaaaa",
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaabbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
)).unwrap();
    assert_eq!(expected.len(), 476);
    assert_eq!(evidence.encode_canonical().unwrap(), expected);
    assert_eq!(
        AttestationEvidenceV1::decode_canonical(&expected).unwrap(),
        evidence
    );
    assert_eq!(
        hex::encode(evidence.evidence_hash().unwrap()),
        "9cc3c0080c0437e749f4b1d8c56a8f2b5e0d5eedf26120eea55656c777e71f93"
    );
    let mut intent = intent;
    intent.attestation_mode = AttestationMode::DcapRequired;
    let kinds = [
        DcapCollateralKind::PckCertificateChain,
        DcapCollateralKind::PckCrl,
        DcapCollateralKind::PckCrlIssuerChain,
        DcapCollateralKind::RootCaCrl,
        DcapCollateralKind::TcbInfo,
        DcapCollateralKind::TcbInfoIssuerChain,
        DcapCollateralKind::QeIdentity,
        DcapCollateralKind::QeIdentityIssuerChain,
    ];
    let dcap = AttestationEvidenceV1::Dcap(DcapEvidenceV1 {
        intent,
        quote: vec![0xdd, 0xee],
        components: kinds
            .into_iter()
            .map(|kind| DcapCollateralComponentV1 {
                kind,
                bytes: vec![0xc0 + kind as u8],
            })
            .collect(),
        transition_key_ready_proof: None,
    });
    let expected_dcap = hex::decode(concat!(
    "0101000001af010000017101111111111111111111111111111111111111111111111111111111111111111122222222",
    "222222222222222222222222222222222222222222222222222222220101333333333333333333333333333333333333",
    "333333333333333333333333333301000000210279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b",
    "16f817984444444444444444444444444444444444444444444444444444444444444444555555555555555555555555",
    "555555555555555555555555555555555555555500000000000000010000000000000002000000000000000300000000",
    "000000040000000000001c20666666666666666666666666666666666666666666666666666666666666666677777777",
    "777777777777777777777777777777777777777777777777777777778888888888888888888888888888888888888888",
    "888888888888888888888888999999999999999999999999999999999999999999999999999999999999999900000002",
    "ddee00080100000001c10200000001c20300000001c30400000001c40500000001c50600000001c60700000001c70800",
    "000001c800",
)).unwrap();
    assert_eq!(expected_dcap.len(), 437);
    assert_eq!(dcap.encode_canonical().unwrap(), expected_dcap);
    assert_eq!(
        AttestationEvidenceV1::decode_canonical(&expected_dcap).unwrap(),
        dcap
    );
    assert_eq!(
        hex::encode(dcap.evidence_hash().unwrap()),
        "064b1f4ffea3a33ff545b1bbd1ff39fc38486bc27e4b846d3112902997224900"
    );
    let mut trailing = expected.clone();
    trailing.push(0);
    assert!(matches!(
        AttestationEvidenceV1::decode_canonical(&trailing),
        Err(CodecError::TrailingBytes(1))
    ));
    assert!(AttestationEvidenceV1::decode_canonical(&expected[..expected.len() - 1]).is_err());
}

#[test]
fn registry_additional_event_topics() {
    for (actual, signature, expected) in [
        (
            ITeeRegistryV1::EnclaveRegisteredV1::SIGNATURE_HASH,
            "EnclaveRegisteredV1(bytes32,bytes32,bytes32,uint64,uint64)",
            "fe2acba50c9a5b9e997a700234ebc94aefa6a229d7d624428ccb7fa60ffbfe6c",
        ),
        (
            ITeeRegistryV1::EnclaveBindingReplacedV1::SIGNATURE_HASH,
            "EnclaveBindingReplacedV1(bytes32,bytes32,bytes32,uint64,uint64)",
            "4aa9738a7463ff72a706f278af076738cf4e09f953cedff472dd15c344659d7b",
        ),
        (
            ITeeRegistryV1::EnclaveBindingRevokedV1::SIGNATURE_HASH,
            "EnclaveBindingRevokedV1(bytes32,bytes32,uint256,uint64)",
            "3e5308ad2ba49d3a0c5b29cb593e5744f72b6dca86c61914ad7f345a8058bed4",
        ),
    ] {
        assert_eq!(actual, keccak256(signature));
        assert_eq!(hex::encode(actual), expected);
    }
}
