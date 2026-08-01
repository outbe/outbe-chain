#![cfg(feature = "tee-attestation-v1")]

use alloy_primitives::{b256, B256};
use ed25519_dalek::Signer as _;
use k256::ecdsa::{signature::hazmat::PrehashSigner as _, SigningKey};
use outbe_primitives::tee_attestation_v1::{
    AttestationEvidenceV1, AttestationMode, AttestationOperationV1, CodecError,
    DcapCollateralComponentV1, DcapCollateralKind, DcapEvidenceV1, EnclaveInitializationManifestV1,
    EnclaveProfile, NodeIdV1, PlatformTcbStatusSetV1, QvlTcbStatusV1, RegistrationIntentV1,
    RegistryMutatorV1, ResourceScheduleV1, TeeMeasurementRuleV1, TeePolicyScheduleEntryV1,
    TeePolicyScheduleV1, TeePolicyV1, TeeRegistryGasScheduleV1, ACTIVE_TEE_ATTESTATION_V1_MANIFEST,
    MAX_ACTIVE_MEASUREMENT_RULES, MAX_ATTESTATION_EVIDENCE_BYTES, MAX_COLLATERAL_COMPONENT_BYTES,
    MAX_EVIDENCE_CALL_FRAMING_BYTES, MAX_QUOTE_BYTES,
};

fn validator_intent(genesis_hash: B256) -> RegistrationIntentV1 {
    RegistrationIntentV1 {
        chain_id: [0; 32],
        genesis_hash,
        operation: AttestationOperationV1::RegisterEnclave,
        attestation_mode: AttestationMode::DcapRequired,
        policy_hash: B256::repeat_byte(0x21),
        enclave_profile: EnclaveProfile::Validator,
        node_id: NodeIdV1::Validator {
            address: [0x31; 20],
            bls_minpk_public: [0x32; 48],
        },
        enclave_id: B256::repeat_byte(0x41),
        binding_id: B256::repeat_byte(0x42),
        binding_version: 1,
        registration_version: 0,
        renewal_nonce: 0,
        transition_nonce: 0,
        requested_valid_until: 7_200,
        recipient_x25519: [0x51; 32],
        attestation_ed25519: [0x52; 32],
        noise_responder_x25519: [0x53; 32],
        node_host_authorization_hash: B256::repeat_byte(0x54),
    }
}

fn recoverable_signature(key: &SigningKey, prehash: B256) -> [u8; 65] {
    let (signature, recovery_id) = key.sign_prehash(prehash.as_slice()).unwrap();
    let mut out = [0u8; 65];
    out[..64].copy_from_slice(signature.to_bytes().as_slice());
    out[64] = recovery_id.to_byte();
    out
}

fn validator_initialization_manifest(key: &SigningKey) -> EnclaveInitializationManifestV1 {
    let public = key.verifying_key().to_encoded_point(false);
    let address_hash = alloy_primitives::keccak256(&public.as_bytes()[1..]);
    let mut address = [0u8; 20];
    address.copy_from_slice(&address_hash.as_slice()[12..]);
    EnclaveInitializationManifestV1 {
        chain_id: [0x10; 32],
        genesis_hash: B256::repeat_byte(0x11),
        enclave_profile: EnclaveProfile::Validator,
        node_id: NodeIdV1::Validator {
            address,
            bls_minpk_public: [0x32; 48],
        },
        initialization_challenge: [0x41; 32],
        node_host_noise_x25519: [0x42; 32],
        recipient_x25519: [0x51; 32],
        attestation_ed25519: [0x52; 32],
        noise_responder_x25519: [0x53; 32],
    }
}

fn intent_for_manifest(manifest: &EnclaveInitializationManifestV1) -> RegistrationIntentV1 {
    RegistrationIntentV1 {
        chain_id: manifest.chain_id,
        genesis_hash: manifest.genesis_hash,
        operation: AttestationOperationV1::RegisterEnclave,
        attestation_mode: AttestationMode::DcapRequired,
        policy_hash: B256::repeat_byte(0x21),
        enclave_profile: manifest.enclave_profile,
        node_id: manifest.node_id.clone(),
        enclave_id: manifest.enclave_id().unwrap(),
        binding_id: B256::repeat_byte(0x42),
        binding_version: 1,
        registration_version: 0,
        renewal_nonce: 0,
        transition_nonce: 0,
        requested_valid_until: 7_200,
        recipient_x25519: manifest.recipient_x25519,
        attestation_ed25519: manifest.attestation_ed25519,
        noise_responder_x25519: manifest.noise_responder_x25519,
        node_host_authorization_hash: manifest.node_host_authorization_hash().unwrap(),
    }
}

#[test]
fn initialization_manifest_is_canonical_node_signed_and_intent_bound() {
    let validator_key = SigningKey::from_bytes((&[0x61; 32]).into()).unwrap();
    let manifest = validator_initialization_manifest(&validator_key);
    let encoded = manifest.encode_canonical().unwrap();
    assert_eq!(
        EnclaveInitializationManifestV1::decode_canonical(&encoded).unwrap(),
        manifest
    );
    let signature = recoverable_signature(&validator_key, manifest.authorization_hash().unwrap());
    assert!(manifest.verify_node_signature(&signature));
    assert!(manifest
        .validate_intent_binding(&intent_for_manifest(&manifest))
        .is_ok());

    let mut trailing = encoded;
    trailing.push(0);
    assert_eq!(
        EnclaveInitializationManifestV1::decode_canonical(&trailing).unwrap_err(),
        CodecError::TrailingBytes(1)
    );
}

#[test]
fn node_host_authorization_survives_fresh_enclave_initialization() {
    let validator_key = SigningKey::from_bytes((&[0x61; 32]).into()).unwrap();
    let original = validator_initialization_manifest(&validator_key);
    let mut replacement = original.clone();
    replacement.initialization_challenge = [0x43; 32];
    replacement.recipient_x25519 = [0x61; 32];
    replacement.attestation_ed25519 = [0x62; 32];
    replacement.noise_responder_x25519 = [0x63; 32];

    assert_ne!(
        original.authorization_hash().unwrap(),
        replacement.authorization_hash().unwrap()
    );
    assert_eq!(
        original.node_host_authorization_hash().unwrap(),
        replacement.node_host_authorization_hash().unwrap()
    );
    assert_eq!(
        original.node_host_authorization_hash().unwrap(),
        b256!("7e9282a26adfb0a997e6c8afbfc453204b98ff499250dec01dc8097164fddb94")
    );
    let mut another_node_host = original.clone();
    another_node_host.node_host_noise_x25519 = [0x44; 32];
    assert_ne!(
        original.node_host_authorization_hash().unwrap(),
        another_node_host.node_host_authorization_hash().unwrap()
    );

    let mut intent = intent_for_manifest(&replacement);
    intent.operation = AttestationOperationV1::ReplaceEnclaveBinding;
    intent.node_host_authorization_hash = original.node_host_authorization_hash().unwrap();
    replacement.validate_intent_binding(&intent).unwrap();
}

#[test]
fn initialization_manifest_rejects_wrong_signer_profile_and_intent_keys() {
    let validator_key = SigningKey::from_bytes((&[0x61; 32]).into()).unwrap();
    let other_key = SigningKey::from_bytes((&[0x62; 32]).into()).unwrap();
    let manifest = validator_initialization_manifest(&validator_key);
    let wrong_signature = recoverable_signature(&other_key, manifest.authorization_hash().unwrap());
    assert!(!manifest.verify_node_signature(&wrong_signature));

    let mut wrong_profile = manifest.clone();
    wrong_profile.enclave_profile = EnclaveProfile::FullNode;
    assert_eq!(
        wrong_profile.encode_canonical().unwrap_err(),
        CodecError::NonCanonical("node kind does not match enclave profile")
    );

    let mut wrong_intent = intent_for_manifest(&manifest);
    wrong_intent.noise_responder_x25519[0] ^= 1;
    assert_eq!(
        manifest.validate_intent_binding(&wrong_intent).unwrap_err(),
        CodecError::NonCanonical("registration intent does not match initialized enclave")
    );
}

#[test]
fn full_node_initialization_signature_uses_the_exact_reth_p2p_key() {
    let full_node_key = SigningKey::from_bytes((&[0x71; 32]).into()).unwrap();
    let compressed: [u8; 33] = full_node_key
        .verifying_key()
        .to_encoded_point(true)
        .as_bytes()
        .try_into()
        .unwrap();
    let mut manifest = validator_initialization_manifest(&full_node_key);
    manifest.enclave_profile = EnclaveProfile::FullNode;
    manifest.node_id = NodeIdV1::FullNode {
        reth_p2p_public: compressed,
    };
    let signature = recoverable_signature(&full_node_key, manifest.authorization_hash().unwrap());
    assert!(manifest.verify_node_signature(&signature));

    let other_key = SigningKey::from_bytes((&[0x72; 32]).into()).unwrap();
    let wrong_signature = recoverable_signature(&other_key, manifest.authorization_hash().unwrap());
    assert!(!manifest.verify_node_signature(&wrong_signature));
}

#[test]
fn v1_manifest_is_compiled_for_direct_harnesses_but_inactive() {
    assert!(ACTIVE_TEE_ATTESTATION_V1_MANIFEST.is_none());
}

#[test]
fn registration_intent_rejects_same_chain_id_with_another_genesis() {
    let expected_genesis = B256::repeat_byte(0x11);
    let other_genesis = B256::repeat_byte(0x12);
    let intent = validator_intent(expected_genesis);

    intent
        .validate_chain_identity([0; 32], expected_genesis)
        .unwrap();
    assert_eq!(
        intent
            .validate_chain_identity([0; 32], other_genesis)
            .unwrap_err(),
        CodecError::ChainIdentityMismatch
    );
}

#[test]
fn registration_intent_requires_node_and_enclave_pop_over_the_same_hash() {
    let node_key = SigningKey::from_bytes((&[0x63; 32]).into()).unwrap();
    let public = node_key.verifying_key().to_encoded_point(false);
    let address_hash = alloy_primitives::keccak256(&public.as_bytes()[1..]);
    let mut intent = validator_intent(B256::repeat_byte(0x11));
    intent.node_id = NodeIdV1::Validator {
        address: address_hash.as_slice()[12..].try_into().unwrap(),
        bls_minpk_public: [0x32; 48],
    };

    let enclave_key = ed25519_dalek::SigningKey::from_bytes(&[0x64; 32]);
    intent.attestation_ed25519 = enclave_key.verifying_key().to_bytes();
    let intent_hash = intent.intent_hash().unwrap();
    let node_signature = recoverable_signature(&node_key, intent_hash);
    let enclave_signature = enclave_key.sign(intent_hash.as_slice()).to_bytes();

    assert!(intent.verify_node_signature(&node_signature));
    assert!(intent.verify_enclave_signature(&enclave_signature));

    let mut conflicting = intent;
    conflicting.binding_id = B256::repeat_byte(0x43);
    assert!(!conflicting.verify_node_signature(&node_signature));
    assert!(!conflicting.verify_enclave_signature(&enclave_signature));
}

#[test]
fn registration_intent_roundtrips_and_rejects_unknown_or_trailing_data() {
    let intent = validator_intent(B256::repeat_byte(0x11));
    let encoded = intent.encode_canonical().unwrap();
    assert_eq!(
        RegistrationIntentV1::decode_canonical(&encoded).unwrap(),
        intent
    );

    let mut unknown_kind = encoded.clone();
    let mut unknown_operation = encoded.clone();
    unknown_operation[65] = 0xff;
    assert!(matches!(
        RegistrationIntentV1::decode_canonical(&unknown_operation),
        Err(CodecError::UnknownDiscriminant {
            field: "attestation operation",
            value: 0xff
        })
    ));

    // version + chain id + genesis + operation + mode + policy hash + profile +
    // NodeId version = byte 100, NodeId kind = byte 101.
    unknown_kind[101] = 0xff;
    assert!(matches!(
        RegistrationIntentV1::decode_canonical(&unknown_kind),
        Err(CodecError::UnknownDiscriminant {
            field: "node kind",
            value: 0xff
        })
    ));

    let mut trailing = encoded;
    trailing.push(0);
    assert_eq!(
        RegistrationIntentV1::decode_canonical(&trailing).unwrap_err(),
        CodecError::TrailingBytes(1)
    );
}

#[test]
fn node_id_codec_rejects_trailing_unknown_and_noncanonical_keys() {
    let full_node = NodeIdV1::FullNode {
        reth_p2p_public: {
            let mut key = [0x44; 33];
            key[0] = 0x02;
            key
        },
    };
    let encoded = full_node.encode_canonical().unwrap();
    assert_eq!(NodeIdV1::decode_canonical(&encoded).unwrap(), full_node);
    assert_ne!(full_node.node_id_hash().unwrap(), B256::ZERO);

    let mut unknown = encoded.clone();
    unknown[1] = 0xff;
    assert!(matches!(
        NodeIdV1::decode_canonical(&unknown),
        Err(CodecError::UnknownDiscriminant {
            field: "node kind",
            value: 0xff
        })
    ));

    let mut trailing = encoded;
    trailing.push(0);
    assert_eq!(
        NodeIdV1::decode_canonical(&trailing).unwrap_err(),
        CodecError::TrailingBytes(1)
    );
    assert_eq!(
        NodeIdV1::FullNode {
            reth_p2p_public: [0; 33]
        }
        .encode_canonical()
        .unwrap_err(),
        CodecError::NonCanonical("full-node node id is not canonical compressed secp256k1")
    );
    assert_eq!(
        NodeIdV1::FullNode {
            reth_p2p_public: {
                let mut invalid = [0xff; 33];
                invalid[0] = 0x02;
                invalid
            }
        }
        .encode_canonical()
        .unwrap_err(),
        CodecError::NonCanonical("full-node node id is not canonical compressed secp256k1")
    );
}

#[test]
fn resource_schedule_has_a_fixed_golden_vector() {
    let schedule = ResourceScheduleV1::new(B256::repeat_byte(0xaa), B256::repeat_byte(0xbb));
    let encoded = schedule.encode_canonical().unwrap();
    let expected = format!(
        "01{}{}000000001dcd65000000000001c9c380",
        "aa".repeat(32),
        "bb".repeat(32)
    );

    assert_eq!(hex::encode(&encoded), expected);
    assert_eq!(
        ResourceScheduleV1::decode_canonical(&encoded).unwrap(),
        schedule
    );

    let mut trailing = encoded;
    trailing.push(0);
    assert_eq!(
        ResourceScheduleV1::decode_canonical(&trailing).unwrap_err(),
        CodecError::TrailingBytes(1)
    );

    let mut non_normative = schedule;
    non_normative.steady_block_gas_limit += 1;
    assert_eq!(
        non_normative.encode_canonical().unwrap_err(),
        CodecError::NonCanonical("non-normative block gas limits")
    );
}

#[test]
fn normative_qvl_and_registry_gas_match_engineering_gate_vectors() {
    let gas = TeeRegistryGasScheduleV1::normative();
    assert_eq!(
        hex::encode(gas.schedule_hash().unwrap()),
        "11edf34f5614ee89ceb28c4597c309ad055cc67a752e335affa69fbc177c3da8"
    );
    assert_eq!(
        hex::encode(
            validator_intent(B256::repeat_byte(0x11))
                .intent_hash()
                .unwrap()
        ),
        "7866748eebd89640c998bfaf64d5c5a44b4849a44c7b0e1ac32d118560a85e13"
    );
    let encoded = gas.encode_canonical().unwrap();
    assert_eq!(
        TeeRegistryGasScheduleV1::decode_canonical(&encoded).unwrap(),
        gas
    );
    let mut trailing = encoded;
    trailing.push(0);
    assert_eq!(
        TeeRegistryGasScheduleV1::decode_canonical(&trailing).unwrap_err(),
        CodecError::TrailingBytes(1)
    );
    let mut non_normative = gas;
    non_normative.input_byte += 1;
    assert_eq!(
        non_normative.encode_canonical().unwrap_err(),
        CodecError::NonCanonical("non-normative TeeRegistry gas schedule")
    );
    let evidence_len = MAX_ATTESTATION_EVIDENCE_BYTES;
    let input_len = evidence_len + MAX_EVIDENCE_CALL_FRAMING_BYTES;

    assert!(
        gas.register_storage_gas_allowance() <= gas.register_fixed,
        "storage allowance must remain inside the normative fixed registration term"
    );
    for kind in [
        RegistryMutatorV1::RegisterEnclave,
        RegistryMutatorV1::RenewEnclave,
        RegistryMutatorV1::TransitionEnclaveMeasurement,
        RegistryMutatorV1::ReplaceEnclaveBinding,
    ] {
        assert!(gas.mutator_storage_gas_allowance(kind) <= 450_000);
    }
    assert_eq!(
        gas.qvl_dcap(evidence_len, MAX_ACTIVE_MEASUREMENT_RULES)
            .unwrap(),
        9_405_024
    );
    assert_eq!(
        gas.maximum_transaction_gas(
            RegistryMutatorV1::RegisterEnclave,
            input_len,
            evidence_len,
            MAX_ACTIVE_MEASUREMENT_RULES,
            AttestationMode::DcapRequired,
        )
        .unwrap(),
        28_768_784
    );
    assert_eq!(
        gas.maximum_transaction_gas(
            RegistryMutatorV1::RenewEnclave,
            input_len,
            evidence_len,
            MAX_ACTIVE_MEASUREMENT_RULES,
            AttestationMode::DcapRequired,
        )
        .unwrap(),
        28_668_784
    );
    assert_eq!(
        gas.maximum_transaction_gas(
            RegistryMutatorV1::ReplaceEnclaveBinding,
            input_len,
            evidence_len,
            MAX_ACTIVE_MEASUREMENT_RULES,
            AttestationMode::DcapRequired,
        )
        .unwrap(),
        29_133_784
    );
}

#[test]
fn report_data_has_fixed_intent_and_node_host_policy_commitments() {
    let intent = validator_intent(B256::repeat_byte(0x11));
    let report_data = intent.report_data().unwrap();

    assert_eq!(
        hex::encode(&report_data[..32]),
        "7866748eebd89640c998bfaf64d5c5a44b4849a44c7b0e1ac32d118560a85e13"
    );
    assert_eq!(
        hex::encode(&report_data[32..]),
        "378c9bbee1671eeb2d8447ba76919a81f2175148800244ff0ca20c2e907d5216"
    );
}

#[test]
fn gas_calculators_reject_cap_plus_one_and_checked_overflow() {
    let gas = TeeRegistryGasScheduleV1::normative();
    assert!(matches!(
        gas.qvl_dcap(MAX_ATTESTATION_EVIDENCE_BYTES + 1, 1),
        Err(CodecError::LimitExceeded {
            field: "attestation evidence",
            ..
        })
    ));
    assert!(matches!(
        gas.qvl_dcap(1, MAX_ACTIVE_MEASUREMENT_RULES + 1),
        Err(CodecError::LimitExceeded {
            field: "active measurement rules",
            ..
        })
    ));
    assert_eq!(
        gas.maximum_calldata_intrinsic_gas(usize::MAX).unwrap_err(),
        CodecError::ArithmeticOverflow
    );
}

fn dcap_evidence_with_component_bytes(last_component_len: usize) -> AttestationEvidenceV1 {
    let intent = validator_intent(B256::repeat_byte(0x11));
    let components = (1u8..=8)
        .map(|value| DcapCollateralComponentV1 {
            kind: DcapCollateralKind::try_from(value).unwrap(),
            bytes: vec![value; if value == 8 { last_component_len } else { 1 }],
        })
        .collect();
    AttestationEvidenceV1::Dcap(DcapEvidenceV1 {
        intent,
        quote: vec![0x61],
        components,
    })
}

#[test]
fn dcap_evidence_roundtrips_and_rejects_duplicate_unknown_and_trailing_components() {
    let evidence = dcap_evidence_with_component_bytes(1);
    let encoded = evidence.encode_canonical().unwrap();
    assert_eq!(
        AttestationEvidenceV1::decode_canonical(&encoded).unwrap(),
        evidence
    );

    let mut duplicate = match evidence.clone() {
        AttestationEvidenceV1::Dcap(value) => value,
        AttestationEvidenceV1::GramineDirectDev(_) => unreachable!(),
    };
    duplicate.components[7].kind = DcapCollateralKind::QeIdentity;
    assert_eq!(
        AttestationEvidenceV1::Dcap(duplicate)
            .encode_canonical()
            .unwrap_err(),
        CodecError::NonCanonical("DCAP component kinds must be exactly 0x01..=0x08")
    );

    let intent_len = validator_intent(B256::repeat_byte(0x11))
        .encode_canonical()
        .unwrap()
        .len();
    let first_component_kind = 6 + 1 + 4 + intent_len + 4 + 1 + 2;
    let mut unknown = encoded.clone();
    unknown[first_component_kind] = 0xff;
    assert!(matches!(
        AttestationEvidenceV1::decode_canonical(&unknown),
        Err(CodecError::UnknownDiscriminant {
            field: "DCAP collateral component",
            value: 0xff
        })
    ));

    let mut trailing = encoded;
    trailing.push(0);
    assert_eq!(
        AttestationEvidenceV1::decode_canonical(&trailing).unwrap_err(),
        CodecError::TrailingBytes(1)
    );
}

#[test]
fn evidence_variant_rejects_an_intent_for_another_attestation_mode() {
    let mut dcap = dcap_evidence_with_component_bytes(1);
    let AttestationEvidenceV1::Dcap(value) = &mut dcap else {
        unreachable!()
    };
    value.intent.attestation_mode = AttestationMode::GramineDirectDev;

    assert_eq!(
        dcap.encode_canonical().unwrap_err(),
        CodecError::NonCanonical("DCAP evidence intent mode mismatch")
    );
}

#[test]
fn evidence_codec_enforces_cap_minus_one_cap_and_cap_plus_one() {
    let base = dcap_evidence_with_component_bytes(1);
    let base_len = base.encode_canonical().unwrap().len();
    let exact_last_len = 1 + (MAX_ATTESTATION_EVIDENCE_BYTES - base_len);

    let cap_minus_one = dcap_evidence_with_component_bytes(exact_last_len - 1);
    assert_eq!(
        cap_minus_one.encode_canonical().unwrap().len(),
        MAX_ATTESTATION_EVIDENCE_BYTES - 1
    );

    let at_cap = dcap_evidence_with_component_bytes(exact_last_len);
    let at_cap_bytes = at_cap.encode_canonical().unwrap();
    assert_eq!(at_cap_bytes.len(), MAX_ATTESTATION_EVIDENCE_BYTES);
    assert_eq!(
        AttestationEvidenceV1::decode_canonical(&at_cap_bytes).unwrap(),
        at_cap
    );

    let cap_plus_one = dcap_evidence_with_component_bytes(exact_last_len + 1);
    assert!(matches!(
        cap_plus_one.encode_canonical(),
        Err(CodecError::LimitExceeded {
            field: "attestation evidence",
            actual,
            ..
        }) if actual == MAX_ATTESTATION_EVIDENCE_BYTES + 1
    ));
}

#[test]
fn evidence_codec_checks_declared_caps_before_payload_allocation() {
    let mut oversized_declared_payload = vec![1, AttestationMode::DcapRequired as u8];
    oversized_declared_payload.extend_from_slice(
        &u32::try_from(MAX_ATTESTATION_EVIDENCE_BYTES + 1)
            .unwrap()
            .to_be_bytes(),
    );
    assert!(matches!(
        AttestationEvidenceV1::decode_canonical(&oversized_declared_payload),
        Err(CodecError::LimitExceeded {
            field: "attestation evidence payload",
            ..
        })
    ));

    let mut oversized_quote = dcap_evidence_with_component_bytes(1);
    let AttestationEvidenceV1::Dcap(value) = &mut oversized_quote else {
        unreachable!()
    };
    value.quote = vec![0x61; MAX_QUOTE_BYTES + 1];
    assert!(matches!(
        oversized_quote.encode_canonical(),
        Err(CodecError::LimitExceeded {
            field: "SGX quote",
            ..
        })
    ));

    let mut oversized_component = dcap_evidence_with_component_bytes(1);
    let AttestationEvidenceV1::Dcap(value) = &mut oversized_component else {
        unreachable!()
    };
    value.components[7].bytes = vec![0x62; MAX_COLLATERAL_COMPONENT_BYTES + 1];
    assert!(matches!(
        oversized_component.encode_canonical(),
        Err(CodecError::LimitExceeded {
            field: "DCAP collateral component",
            ..
        })
    ));
}

fn measurement_rule(profile: EnclaveProfile, marker: u8) -> TeeMeasurementRuleV1 {
    TeeMeasurementRuleV1 {
        enclave_profile: profile,
        mrenclave: B256::repeat_byte(marker),
        mrsigner: B256::repeat_byte(marker + 1),
        isv_prod_id: u16::from(marker),
        minimum_isv_svn: 2,
        admit_from_height: 1,
        admit_until_height_exclusive: 1_000,
    }
}

fn policy(
    policy_version: u64,
    activation_height: u64,
    predecessor_policy_hash: B256,
) -> TeePolicyV1 {
    let gas = TeeRegistryGasScheduleV1::normative();
    let resources = ResourceScheduleV1::new(B256::repeat_byte(0x71), gas.schedule_hash().unwrap());
    TeePolicyV1 {
        policy_version,
        chain_id: [0; 32],
        genesis_hash: B256::repeat_byte(0x11),
        activation_height,
        predecessor_policy_hash,
        attestation_mode: AttestationMode::DcapRequired,
        intel_root_der_hash: B256::repeat_byte(0x72),
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
        accepted_platform_tcb_statuses: PlatformTcbStatusSetV1::UpToDateOrSWHardeningNeeded,
        accepted_qe_tcb_status: QvlTcbStatusV1::UpToDate,
        minimum_lease: 3_600,
        maximum_lease: 604_800,
        collateral_margin: 3_600,
        resource_schedule_hash: resources.schedule_hash().unwrap(),
        measurement_rules: vec![
            measurement_rule(EnclaveProfile::Validator, 1),
            measurement_rule(EnclaveProfile::FullNode, 3),
        ],
    }
}

#[test]
fn policy_and_schedule_roundtrip_with_height_selection() {
    let first = policy(1, 1, B256::ZERO);
    let first_hash = first.policy_hash().unwrap();
    assert_eq!(
        hex::encode(first_hash),
        "4dbc61a63c5c3107b56a75eb3a38f640e22650a34ad25cb52060726b1f7baacd"
    );
    let mut second = policy(2, 100, first_hash);
    second.accepted_platform_tcb_statuses = PlatformTcbStatusSetV1::UpToDateOnly;
    let schedule = TeePolicyScheduleV1 {
        chain_id: [0; 32],
        genesis_hash: B256::repeat_byte(0x11),
        entries: vec![
            TeePolicyScheduleEntryV1 {
                activation_height: 1,
                policy: first.clone(),
            },
            TeePolicyScheduleEntryV1 {
                activation_height: 100,
                policy: second.clone(),
            },
        ],
    };

    let encoded = schedule.encode_canonical().unwrap();
    assert_eq!(
        hex::encode(schedule.schedule_hash().unwrap()),
        "ff3de6da76820b4a84e27f68d0de81b28278b1244c62cf3962707e409660786c"
    );
    assert_eq!(
        TeePolicyScheduleV1::decode_canonical(&encoded).unwrap(),
        schedule
    );
    assert_eq!(schedule.active_policy(1).unwrap(), &first);
    assert_eq!(schedule.active_policy(99).unwrap(), &first);
    assert_eq!(schedule.active_policy(100).unwrap(), &second);
    assert_eq!(
        schedule
            .active_policy(1)
            .unwrap()
            .accepted_platform_tcb_statuses,
        PlatformTcbStatusSetV1::UpToDateOrSWHardeningNeeded
    );
    assert_eq!(
        schedule
            .active_policy(100)
            .unwrap()
            .accepted_platform_tcb_statuses,
        PlatformTcbStatusSetV1::UpToDateOnly
    );
    assert!(schedule.schedule_hash().is_ok());

    // Fixed V1 layout through minimum_tcb_evaluation_data_number is 178 bytes.
    let mut unknown_platform_status_set = first.encode_canonical().unwrap();
    assert_eq!(
        unknown_platform_status_set[178],
        PlatformTcbStatusSetV1::UpToDateOrSWHardeningNeeded as u8
    );
    unknown_platform_status_set[178] = 0xff;
    assert!(matches!(
        TeePolicyV1::decode_canonical(&unknown_platform_status_set),
        Err(CodecError::UnknownDiscriminant {
            field: "accepted Platform TCB status set",
            value: 0xff
        })
    ));

    let mut trailing = encoded;
    trailing.push(0);
    assert_eq!(
        TeePolicyScheduleV1::decode_canonical(&trailing).unwrap_err(),
        CodecError::TrailingBytes(1)
    );
}

#[test]
fn policy_schedule_rejects_duplicate_rules_and_broken_predecessor_chain() {
    let mut duplicate_rule_policy = policy(1, 1, B256::ZERO);
    duplicate_rule_policy.measurement_rules[1] = duplicate_rule_policy.measurement_rules[0].clone();
    assert_eq!(
        duplicate_rule_policy.encode_canonical().unwrap_err(),
        CodecError::NonCanonical("measurement rules must be strictly sorted and unique")
    );

    let first = policy(1, 1, B256::ZERO);
    let broken = TeePolicyScheduleV1 {
        chain_id: [0; 32],
        genesis_hash: B256::repeat_byte(0x11),
        entries: vec![
            TeePolicyScheduleEntryV1 {
                activation_height: 1,
                policy: first,
            },
            TeePolicyScheduleEntryV1 {
                activation_height: 100,
                policy: policy(2, 100, B256::repeat_byte(0xff)),
            },
        ],
    };
    assert_eq!(
        broken.encode_canonical().unwrap_err(),
        CodecError::NonCanonical("policy predecessor hash mismatch")
    );
}
