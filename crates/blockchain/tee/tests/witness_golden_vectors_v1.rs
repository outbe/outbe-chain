//! CHARACTERIZATION: witness variant order is independent of transport response order.
use alloy_primitives::{hex, keccak256, B256};
use outbe_tee::{tee_call::canonical_attestation_preimage, witness::EnclaveWitnessEntryV1 as W};

fn entries(tag: u8) -> [W; 4] {
    let request_hash = B256::repeat_byte(0x11);
    [
        W::DcapVerificationFinishedV1 {
            request_hash,
            outcome: vec![0x21, 0x22],
            attestation_tag: vec![tag],
        },
        W::DcapOnboardingVerificationFinishedV1 {
            request_hash,
            outcome: vec![0x21, 0x22],
            onboarding_artifact: vec![0x31],
            attestation_tag: vec![tag],
        },
        W::NoAttestationDevOnboardingArtifactPreparedV2 {
            request_hash,
            onboarding_artifact: vec![0x31],
            attestation_tag: vec![tag],
        },
        W::Payload {
            payload: vec![0x41, 0x42],
            inputs_canonical_hash: request_hash,
            attestation_tag: vec![tag],
        },
    ]
}

#[test]
fn postcard_variant_vectors_and_tag_semantics() {
    let vectors = [
        "002011111111111111111111111111111111111111111111111111111111111111110221220151",
        "0120111111111111111111111111111111111111111111111111111111111111111102212201310151",
        "0220111111111111111111111111111111111111111111111111111111111111111101310151",
        "030241422011111111111111111111111111111111111111111111111111111111111111110151",
    ];
    for (index, ((entry, retagged), expected)) in entries(0x51)
        .into_iter()
        .zip(entries(0x61))
        .zip(vectors)
        .enumerate()
    {
        let bytes = hex::decode(expected).unwrap();
        assert_eq!(usize::from(bytes[0]), index);
        assert_eq!(postcard::to_allocvec(&entry).unwrap(), bytes);
        assert_eq!(postcard::from_bytes::<W>(&bytes).unwrap(), entry);
        assert_ne!(entry, retagged);
        assert!(entry.semantic_eq_ignoring_attestation_tag(&retagged));
        assert!(retagged.semantic_eq_ignoring_attestation_tag(&entry));
        for other in entries(0x51) {
            assert_eq!(
                entry.semantic_eq_ignoring_attestation_tag(&other),
                entry == other
            );
        }
        let mut changed = entry.clone();
        match &mut changed {
            W::DcapVerificationFinishedV1 { request_hash, .. }
            | W::DcapOnboardingVerificationFinishedV1 { request_hash, .. }
            | W::NoAttestationDevOnboardingArtifactPreparedV2 { request_hash, .. } => {
                *request_hash = B256::repeat_byte(0x12)
            }
            W::Payload {
                inputs_canonical_hash,
                ..
            } => *inputs_canonical_hash = B256::repeat_byte(0x12),
        }
        assert!(!entry.semantic_eq_ignoring_attestation_tag(&changed));
        let mut changed = entry.clone();
        match &mut changed {
            W::DcapVerificationFinishedV1 { outcome, .. }
            | W::DcapOnboardingVerificationFinishedV1 { outcome, .. } => outcome.push(0x23),
            W::NoAttestationDevOnboardingArtifactPreparedV2 {
                onboarding_artifact,
                ..
            } => onboarding_artifact.push(0x32),
            W::Payload { payload, .. } => payload.push(0x43),
        }
        assert!(!entry.semantic_eq_ignoring_attestation_tag(&changed));
        if let W::DcapOnboardingVerificationFinishedV1 {
            onboarding_artifact,
            outcome,
            ..
        } = &mut changed
        {
            *onboarding_artifact = vec![0x32];
            *outcome = vec![0x21, 0x22];
            assert!(!entry.semantic_eq_ignoring_attestation_tag(&changed));
        }
    }
}

#[test]
fn dev_onboarding_signed_preimage() {
    let actual = canonical_attestation_preimage(
        b"outbe/tee/gramine-direct-dev-onboarding-result/v2",
        B256::repeat_byte(0x11),
        &vec![0x31_u8, 0x32],
    )
    .unwrap();
    let expected = hex::decode(concat!(
    "6f757462652f7465652f6772616d696e652d6469726563742d6465762d6f6e626f617264696e672d726573756c742f76",
    "3211111111111111111111111111111111111111111111111111111111111111115b34392c35305d",
)).unwrap();
    assert_eq!(actual, expected);
    assert_eq!(
        hex::encode(keccak256(actual)),
        "52ea807674272b11e66ee1994cdf0f2050d656a4f15716d8143bf9e4d50d0ce7"
    );
}

#[test]
fn onboarding_verifier_accepts_only_the_frozen_signed_domain() {
    use ed25519_dalek::{Signer as _, SigningKey};
    use outbe_tee::{
        dcap_protocol::{DcapOnboardingArtifactV1, DcapOnboardingContextV1},
        prepare_no_attestation_dev_onboarding_artifact_v1,
        witness::{
            EnclaveWitnessTapeV1, WitnessExecutionMode, WitnessExecutionScope, WitnessState,
        },
    };
    use std::sync::{Arc, Mutex};

    let context = DcapOnboardingContextV1 {
        chain_id: [0x11; 32],
        genesis_hash: B256::repeat_byte(0x22),
        intent_hash: B256::repeat_byte(0x33),
        node_id_hash: B256::repeat_byte(0x44),
        enclave_id: B256::repeat_byte(0x55),
        binding_id: B256::repeat_byte(0x66),
        policy_hash: B256::repeat_byte(0x77),
        recipient_x25519: [0x88; 32],
        network_encryption_public: [0x99; 32],
        key_epoch: 1,
        network_encryption_epoch: 2,
    };
    let artifact = DcapOnboardingArtifactV1 {
        context,
        nonce: [0xaa; 12],
        ciphertext: vec![0xbb; 16],
    };
    let encoded = artifact.encode_canonical().unwrap();
    let request_hash = context.context_hash();
    assert_eq!(
        hex::encode(request_hash),
        "923e0670d89de1a4fe5d10a8765ffe4ae8d67ab1f4be2f1860ece44faea758bc"
    );
    // Independent JSON byte-array framing, not the production preimage builder.
    let json = format!(
        "[{}]",
        encoded
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join(",")
    );
    let signing = SigningKey::from_bytes(&[0xcc; 32]);
    for (domain, accepted) in [
        (
            b"outbe/tee/gramine-direct-dev-onboarding-result/v2".as_slice(),
            true,
        ),
        (
            b"outbe/tee/gramine-direct-dev-onboarding-result/v1".as_slice(),
            false,
        ),
    ] {
        let preimage = [domain, request_hash.as_slice(), json.as_bytes()].concat();
        let entry = W::NoAttestationDevOnboardingArtifactPreparedV2 {
            request_hash,
            onboarding_artifact: encoded.clone(),
            attestation_tag: signing.sign(&preimage).to_bytes().to_vec(),
        };
        let tape = EnclaveWitnessTapeV1 {
            prefix: vec![entry],
            txs: vec![],
        };
        let state = Arc::new(Mutex::new(WitnessState::new(
            WitnessExecutionMode::VerifierReplay,
            Some(signing.verifying_key().to_bytes()),
            tape,
        )));
        let _scope = WitnessExecutionScope::enter(state);
        let result = prepare_no_attestation_dev_onboarding_artifact_v1(context);
        if accepted {
            assert_eq!(result.unwrap(), artifact);
        } else {
            assert!(result.is_err());
        }
    }
}

#[test]
fn complete_witness_tape_framing_vector() {
    use outbe_tee::witness::{
        decode_witness_payload_v1, encode_witness_payload_v1, EnclaveWitnessTapeV1, TxWitnessV1,
    };
    let [first, _, prefix, payload] = entries(0x51);
    let tape = EnclaveWitnessTapeV1 {
        prefix: vec![prefix],
        txs: vec![
            TxWitnessV1 {
                tx_index: 1,
                entries: vec![payload],
            },
            TxWitnessV1 {
                tx_index: 129,
                entries: vec![first],
            },
        ],
    };
    // Independent Postcard layout: prefix count, entries, tx count, each index/count/entries.
    let raw = hex::decode("010220111111111111111111111111111111111111111111111111111111111111111101310151020101030241422011111111111111111111111111111111111111111111111111111111111111110151810101002011111111111111111111111111111111111111111111111111111111111111110221220151").unwrap();
    assert_eq!(postcard::to_allocvec(&tape).unwrap(), raw);
    assert_eq!(
        postcard::from_bytes::<EnclaveWitnessTapeV1>(&raw).unwrap(),
        tape
    );
    // Independent zstd CLI -3 --no-check fixture; 5-byte header is version + u32 LE raw length.
    let expected = hex::decode("017b00000028b52ffd207b250100c80102201101310151020101030241425181010100022122015103004a8dfa29c7378008").unwrap();
    assert_eq!(&expected[..5], &[1, 123, 0, 0, 0]);
    assert_eq!(&expected[5..9], &[0x28, 0xb5, 0x2f, 0xfd]);
    let encoded = encode_witness_payload_v1(&tape).unwrap();
    assert_eq!(&encoded[..5], &[1, 123, 0, 0, 0]);
    assert_eq!(decode_witness_payload_v1(&encoded).unwrap(), tape);
    assert_eq!(decode_witness_payload_v1(&expected).unwrap(), tape);
    let mut wrong_length = expected;
    wrong_length[1] += 1;
    assert!(decode_witness_payload_v1(&wrong_length).is_err());
}

#[test]
fn dcap_request_context_and_signed_preimage_vectors() {
    use outbe_tee::dcap_protocol::{
        dcap_evidence_hash_v1, dcap_onboarding_attestation_preimage, dcap_onboarding_request_hash,
        dcap_verification_attestation_preimage, dcap_verification_request_hash,
        DcapOnboardingContextV1, RegistrationVerificationRequest,
    };
    let context = DcapOnboardingContextV1 {
        chain_id: [0x11; 32],
        genesis_hash: B256::repeat_byte(0x22),
        intent_hash: B256::repeat_byte(0x33),
        node_id_hash: B256::repeat_byte(0x44),
        enclave_id: B256::repeat_byte(0x55),
        binding_id: B256::repeat_byte(0x66),
        policy_hash: B256::repeat_byte(0x77),
        recipient_x25519: [0x88; 32],
        network_encryption_public: [0x99; 32],
        key_epoch: 1,
        network_encryption_epoch: 2,
    };
    assert_eq!(hex::encode(context.encode_canonical()), "0111111111111111111111111111111111111111111111111111111111111111112222222222222222222222222222222222222222222222222222222222222222333333333333333333333333333333333333333333333333333333333333333344444444444444444444444444444444444444444444444444444444444444445555555555555555555555555555555555555555555555555555555555555555666666666666666666666666666666666666666666666666666666666666666677777777777777777777777777777777777777777777777777777777777777778888888888888888888888888888888888888888888888888888888888888888999999999999999999999999999999999999999999999999999999999999999900000000000000010000000000000002");
    assert_eq!(
        hex::encode(context.context_hash()),
        "923e0670d89de1a4fe5d10a8765ffe4ae8d67ab1f4be2f1860ece44faea758bc"
    );
    assert_eq!(
        hex::encode(dcap_verification_request_hash(&[1, 2, 3], &[4, 5], 123).unwrap()),
        "bc22a45d047b8bb17558d861c313a1457d08d6c67b23e73a5025b4dbc064a668"
    );
    let request = RegistrationVerificationRequest {
        evidence: &[1, 2, 3],
        policy: &[4, 5],
        block_timestamp: 123,
        node_signature: &[6; 65],
        enclave_signature: &[7; 64],
        expected_network_encryption_public: [8; 32],
        key_epoch: 1,
        network_encryption_epoch: 2,
    };
    assert_eq!(
        hex::encode(dcap_onboarding_request_hash(request).unwrap()),
        "b4879591d9bce913ca63d9f0fa4f47be0843480766d16e6037047bc4e80ca9aa"
    );
    assert_eq!(
        hex::encode(dcap_evidence_hash_v1(&[1, 2, 3]).unwrap()),
        "997644d89adf65ef6b8d0a4a42b3d81ca2673ea07b29d87c36ae5d5f43693736"
    );
    let actual =
        dcap_verification_attestation_preimage(B256::repeat_byte(0x11), &[0x21, 0x22]).unwrap();
    assert_eq!(hex::encode(&actual), "6f757462652f7465652f646361702d766572696669636174696f6e2d6174746573746174696f6e2f76311111111111111111111111111111111111111111111111111111111111111111000000022122");
    assert_eq!(
        hex::encode(keccak256(&actual)),
        "f61f624731affd6e77aee71e5b4f2a5a49da4a259b57d1ac5a6d4af4b25bdfcd"
    );
    let actual = dcap_onboarding_attestation_preimage(
        B256::repeat_byte(0x11),
        &[0x21, 0x22],
        &[0x31, 0x32, 0x33],
    )
    .unwrap();
    assert_eq!(hex::encode(&actual), "6f757462652f7465652f646361702d6f6e626f617264696e672d6174746573746174696f6e2f7631111111111111111111111111111111111111111111111111111111111111111100000002212200000003313233");
    assert_eq!(
        hex::encode(keccak256(&actual)),
        "26b07632d37ccf40db89ef4e6a90dbce2c09cbed639a89063ebd005ec749ed2b"
    );
}

// Independent Python canonical artifact encoding and Keccak commitments.
#[test]
fn finalized_ingest_and_upgrade_key_transfer_vectors() {
    use outbe_tee::{
        dcap_protocol::{DcapOnboardingArtifactV1, DcapOnboardingContextV1},
        finalized_admission::{
            onboarding_artifact_ingest_request_hash_v1, upgrade_key_transfer_request_hash_v1,
            FinalizedAdmissionBeginInputV1,
        },
    };
    let context = DcapOnboardingContextV1 {
        chain_id: [0x11; 32],
        genesis_hash: B256::repeat_byte(0x22),
        intent_hash: B256::repeat_byte(0x33),
        node_id_hash: B256::repeat_byte(0x44),
        enclave_id: B256::repeat_byte(0x55),
        binding_id: B256::repeat_byte(0x66),
        policy_hash: B256::repeat_byte(0x77),
        recipient_x25519: [0x88; 32],
        network_encryption_public: [0x99; 32],
        key_epoch: 1,
        network_encryption_epoch: 2,
    };
    let artifact = DcapOnboardingArtifactV1 {
        context,
        nonce: [0xaa; 12],
        ciphertext: vec![0xbb; 16],
    };
    let encoded = artifact.encode_canonical().unwrap();
    assert_eq!(
        hex::encode(keccak256(&encoded)),
        "7856d0faf22c6cf1ccba2a9a7bcab69cd9dbc5bc49d5e021bb43dd0bb4de6fea"
    );
    let input = FinalizedAdmissionBeginInputV1 {
        artifact: &encoded,
        anchor_outcome: &[0x41, 0x42, 0x43],
        expected_intent_hash: context.intent_hash,
        expected_network_encryption_public: context.network_encryption_public,
        expected_key_epoch: 1,
        expected_network_encryption_epoch: 2,
    };
    assert_eq!(
        hex::encode(onboarding_artifact_ingest_request_hash_v1(input).unwrap()),
        "1b03e956474713ffe2af2f798292f8bc3656dddf13d89ff3999fd71e1ae2da6e"
    );
    assert_eq!(
        hex::encode(
            upgrade_key_transfer_request_hash_v1(&encoded, input.anchor_outcome, false).unwrap()
        ),
        "44b5825f56015a4a86b9d3e9b064af86165a1787ce2ff072a57c98db9c747a2e"
    );
    assert_eq!(
        hex::encode(
            upgrade_key_transfer_request_hash_v1(&encoded, input.anchor_outcome, true).unwrap()
        ),
        "b763ae0e06c86f9d4daae4c1f53b5dd1cd581eeca0512f8654c2b2a8b24d2bb2"
    );
}
