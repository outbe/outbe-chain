use super::*;
use ed25519_dalek::Signer as _;

#[test]
fn network_binding_is_canonical_and_every_field_changes_its_hash() {
    let binding = NetworkBindingV1 {
        chain_id: [0x10; 32],
        genesis_hash: B256::repeat_byte(0x11),
        attestation_mode: AttestationMode::DcapRequired,
    };
    let encoded = binding.encode_canonical().unwrap();
    assert_eq!(
        NetworkBindingV1::decode_canonical(&encoded).unwrap(),
        binding
    );

    let expected_hash = binding.binding_hash().unwrap();
    let mut changed_chain = binding;
    changed_chain.chain_id[31] ^= 1;
    assert_ne!(changed_chain.binding_hash().unwrap(), expected_hash);

    let mut changed_genesis = binding;
    changed_genesis.genesis_hash = B256::repeat_byte(0x12);
    assert_ne!(changed_genesis.binding_hash().unwrap(), expected_hash);

    let mut changed_mode = binding;
    changed_mode.attestation_mode = AttestationMode::GramineDirectDev;
    assert_ne!(changed_mode.binding_hash().unwrap(), expected_hash);

    let mut trailing = encoded;
    trailing.push(0);
    assert_eq!(
        NetworkBindingV1::decode_canonical(&trailing).unwrap_err(),
        CodecError::TrailingBytes(1)
    );
}

#[test]
fn trusted_network_descriptor_is_canonical_and_enforces_network_attestation_policy() {
    let descriptor = TrustedNetworkDescriptorV1 {
        network_binding: NetworkBindingV1 {
            chain_id: alloy_primitives::U256::from(54322345_u64).to_be_bytes(),
            genesis_hash: B256::repeat_byte(0x31),
            attestation_mode: AttestationMode::DcapRequired,
        },
        genesis_consensus_keys: vec![[0x51; 48], [0x52; 48]],
    };
    let encoded = descriptor.encode_canonical().unwrap();
    assert_eq!(
        encoded.len(),
        TrustedNetworkDescriptorV1::FIXED_CANONICAL_LEN + 2 * 48
    );
    assert_eq!(
        TrustedNetworkDescriptorV1::decode_canonical(&encoded).unwrap(),
        descriptor
    );

    let mut changed = descriptor.clone();
    changed.network_binding.genesis_hash = B256::repeat_byte(0x42);
    assert_ne!(
        descriptor.descriptor_hash().unwrap(),
        changed.descriptor_hash().unwrap()
    );

    let mut direct = descriptor.clone();
    direct.network_binding.attestation_mode = AttestationMode::GramineDirectDev;
    let direct_bytes = direct.encode_canonical().unwrap();
    assert_eq!(
        TrustedNetworkDescriptorV1::decode_canonical(&direct_bytes).unwrap(),
        direct
    );
    assert_ne!(
        direct.descriptor_hash().unwrap(),
        descriptor.descriptor_hash().unwrap()
    );

    // Direct mode is an explicit devnet/testnet profile, never a mainnet fallback.
    direct.network_binding.chain_id =
        alloy_primitives::U256::from(outbe_primitives::chain::MAINNET_CHAIN_ID).to_be_bytes();
    assert_eq!(
        direct.encode_canonical().unwrap_err(),
        CodecError::NonCanonical("descriptor attestation mode is forbidden for this network")
    );

    let mut unsorted = descriptor;
    unsorted.genesis_consensus_keys.reverse();
    assert_eq!(
        unsorted.encode_canonical().unwrap_err(),
        CodecError::NonCanonical("trusted network descriptor genesis committee order")
    );
}

#[test]
fn dkg_announcement_binding_covers_network_ceremony_round_set_and_recipient() {
    use outbe_primitives::tee_attestation_v1::{
        dkg_ceremony_id_v1, dkg_participant_announce_hash_v1, dkg_participant_set_hash_v1,
    };

    let binding = NetworkBindingV1 {
        chain_id: [0x10; 32],
        genesis_hash: B256::repeat_byte(0x20),
        attestation_mode: AttestationMode::DcapRequired,
    };
    let participants = vec![vec![0x01; 48], vec![0x02; 48], vec![0x03; 48]];
    let set_hash = dkg_participant_set_hash_v1(&participants).unwrap();
    assert_eq!(
        set_hash,
        dkg_participant_set_hash_v1(&[
            participants[2].clone(),
            participants[0].clone(),
            participants[1].clone(),
        ])
        .unwrap()
    );
    assert!(dkg_participant_set_hash_v1(&[]).is_err());
    assert!(
        dkg_participant_set_hash_v1(&[participants[0].clone(), participants[0].clone(),]).is_err()
    );

    let ceremony_id = dkg_ceremony_id_v1(&binding, 7, set_hash).unwrap();
    let baseline =
        dkg_participant_announce_hash_v1(&binding, ceremony_id, 7, set_hash, &[0x30; 32]).unwrap();
    let mut other_binding = binding;
    other_binding.genesis_hash = B256::repeat_byte(0x21);
    let other_set =
        dkg_participant_set_hash_v1(&[participants[0].clone(), participants[1].clone()]).unwrap();

    assert_ne!(
        baseline,
        dkg_participant_announce_hash_v1(
            &other_binding,
            dkg_ceremony_id_v1(&other_binding, 7, set_hash).unwrap(),
            7,
            set_hash,
            &[0x30; 32],
        )
        .unwrap()
    );
    assert_ne!(
        ceremony_id,
        dkg_ceremony_id_v1(&binding, 8, set_hash).unwrap()
    );
    assert_ne!(
        ceremony_id,
        dkg_ceremony_id_v1(&binding, 7, other_set).unwrap()
    );
    assert_ne!(
        baseline,
        dkg_participant_announce_hash_v1(&binding, ceremony_id, 7, set_hash, &[0x31; 32]).unwrap()
    );
    assert!(dkg_participant_announce_hash_v1(
        &binding,
        B256::repeat_byte(0x99),
        7,
        set_hash,
        &[0x30; 32],
    )
    .is_err());
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
        b256!("0fb70b436e5ca523c45c8ffa91c39521d48c83dc0981c4d1279cc5fb05e3cdca")
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
    assert_eq!(original.network_binding(), intent.network_binding());
}

#[test]
fn canonical_node_host_witness_opens_the_exact_manifest_authorization() {
    let validator_key = SigningKey::from_bytes((&[0x61; 32]).into()).unwrap();
    let manifest = validator_initialization_manifest(&validator_key);
    let witness = NodeHostAuthorizationWitnessV1::from_manifest(&manifest).unwrap();

    let encoded = witness.encode_canonical().unwrap();
    assert_eq!(encoded.len(), MAX_NODE_HOST_AUTHORIZATION_WITNESS_BYTES);
    assert_eq!(
        NodeHostAuthorizationWitnessV1::decode_canonical(&encoded).unwrap(),
        witness
    );
    assert_eq!(
        witness.authorization_hash().unwrap(),
        manifest.node_host_authorization_hash().unwrap()
    );
}

#[test]
fn initialization_manifest_rejects_wrong_signer_and_intent_keys() {
    let validator_key = SigningKey::from_bytes((&[0x61; 32]).into()).unwrap();
    let other_key = SigningKey::from_bytes((&[0x62; 32]).into()).unwrap();
    let manifest = validator_initialization_manifest(&validator_key);
    let wrong_signature = recoverable_signature(&other_key, manifest.authorization_hash().unwrap());
    assert!(!manifest.verify_node_signature(&wrong_signature));

    let mut wrong_intent = intent_for_manifest(&manifest);
    wrong_intent.noise_responder_x25519[0] ^= 1;
    assert_eq!(
        manifest.validate_intent_binding(&wrong_intent).unwrap_err(),
        CodecError::NonCanonical("registration intent does not match initialized enclave")
    );

    let mut wrong_mode = intent_for_manifest(&manifest);
    wrong_mode.attestation_mode = AttestationMode::GramineDirectDev;
    assert_eq!(
        manifest.validate_intent_binding(&wrong_mode).unwrap_err(),
        CodecError::NonCanonical("registration intent does not match initialized enclave")
    );
}

#[test]
fn node_host_initialization_signature_uses_the_exact_reth_p2p_key() {
    let node_host_key = SigningKey::from_bytes((&[0x71; 32]).into()).unwrap();
    let manifest = validator_initialization_manifest(&node_host_key);
    let signature = recoverable_signature(&node_host_key, manifest.authorization_hash().unwrap());
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
    let mut intent = validator_intent(B256::repeat_byte(0x11));
    intent.node_id = node_id(&node_key);

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

    // version + chain id + genesis + operation + mode + policy hash = 99 bytes;
    // the nested NodeId version starts at byte 99.
    unknown_kind[99] = 0xff;
    assert!(matches!(
        RegistrationIntentV1::decode_canonical(&unknown_kind),
        Err(CodecError::UnsupportedVersion {
            field: "NodeIdV1",
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
    let node_host = node_id(&SigningKey::from_bytes((&[0x44; 32]).into()).unwrap());
    let encoded = node_host.encode_canonical().unwrap();
    assert_eq!(NodeIdV1::decode_canonical(&encoded).unwrap(), node_host);
    assert_ne!(node_host.node_id_hash().unwrap(), B256::ZERO);

    let mut unknown = encoded.clone();
    unknown[0] = 0xff;
    assert!(matches!(
        NodeIdV1::decode_canonical(&unknown),
        Err(CodecError::UnsupportedVersion {
            field: "NodeIdV1",
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
        NodeIdV1 {
            reth_p2p_public: [0; 33]
        }
        .encode_canonical()
        .unwrap_err(),
        CodecError::NonCanonical("node id is not canonical compressed secp256k1")
    );
    assert_eq!(
        NodeIdV1 {
            reth_p2p_public: {
                let mut invalid = [0xff; 33];
                invalid[0] = 0x02;
                invalid
            }
        }
        .encode_canonical()
        .unwrap_err(),
        CodecError::NonCanonical("node id is not canonical compressed secp256k1")
    );
}
