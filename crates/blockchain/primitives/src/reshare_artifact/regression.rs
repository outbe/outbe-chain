use super::*;
use crate::consensus::ReshareResult;

fn mixed_artifacts(consensus: ConsensusHeaderArtifact) -> OutbeBlockArtifacts {
    OutbeBlockArtifacts {
        execution_summary: Some(ExecutionSummaryArtifact {
            validator_fee_sum: U256::from(0x01020304_u64),
        }),
        consensus_header_artifact: Some(consensus),
        timestamp_millis_part: 1_234, // Structural codec deliberately accepts >= 1000.
        late_finalize_credits: Some(LateFinalizeCreditsArtifact {
            batches: vec![PerBlockCredit {
                fb_number: 17,
                fb_hash: B256::repeat_byte(0x11),
                epoch: 3,
                view: 19,
                parent_view: 18,
                committee_set_hash: B256::repeat_byte(0x22),
                signer_bitmap: vec![0x07, 0x01],
                aggregate_signature: [0x33; 96],
            }],
        }),
        compressed_entities_root: Some(CompressedEntitiesRootArtifact {
            commitment_scheme_version: 0x01020304,
            r_sealed: B256::repeat_byte(0x44),
        }),
    }
}

fn boundary() -> DkgBoundaryArtifact {
    let exclusions = vec![Address::repeat_byte(0x88)];
    DkgBoundaryArtifact {
        epoch: 1,
        dkg_cycle: 2,
        freeze_height: 3,
        planned_activation_height: 4,
        target_set_hash: B256::repeat_byte(0x11),
        vrf_material_version: 5,
        vrf_group_public_key: B256::repeat_byte(0x22),
        vrf_group_public_key_bytes: Bytes::from_static(b"vrf"),
        committee_set_hash: B256::repeat_byte(0x33),
        is_validator_set_change: true,
        outcome: Bytes::from_static(b"outcome"),
        is_full_dkg: false,
        reshare: ReshareResult {
            new_active_set: vec![Address::repeat_byte(0x44)],
            active_set_hash: B256::repeat_byte(0x55),
        },
        tee_recipient_pubkeys: vec![(Address::repeat_byte(0x66), B256::repeat_byte(0x77))],
        tee_expired_target_exclusions_hash: tee_expired_target_exclusions_hash(&exclusions)
            .unwrap(),
        tee_expired_target_exclusions: exclusions,
    }
}

fn envelope(records: &[(u8, &[u8])]) -> Vec<u8> {
    let mut bytes = b"OART\x0B".to_vec();
    bytes.push(records.len() as u8);
    for (tag, payload) in records {
        bytes.push(*tag);
        bytes.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        bytes.extend_from_slice(payload);
    }
    bytes
}

fn assert_fatal(bytes: &[u8], expected: &str) {
    match decode_outbe_block_artifacts(bytes).unwrap_err() {
        PrecompileError::Fatal(message) => assert_eq!(message, expected),
        other => panic!("expected fatal codec error, got {other:?}"),
    }
}

#[test]
fn mixed_record_wire_vectors_are_pinned() {
    // Captured from the pre-refactor encoder; hashes bind every field, length,
    // endian convention, version and record order without duplicating the codec.
    let expected = [
        (
            613,
            alloy_primitives::b256!(
                "5243cd6624127a9fdf8332f75c74dac235dda705acf5da281ce4b7ab1e9aac98"
            ),
        ),
        (
            301,
            alloy_primitives::b256!(
                "18f30aab3b466960cc6e31fdc651c76798310ddcf0101cce5cabab04319f7e98"
            ),
        ),
        (
            314,
            alloy_primitives::b256!(
                "5ac4c2d8589782c0d27e515d0b4b46fa5ee90dc0f5c3652296589cfb2da79c4d"
            ),
        ),
    ];
    let consensus = [
        ConsensusHeaderArtifact::BoundaryOutcome(boundary()),
        ConsensusHeaderArtifact::DealerLog(Bytes::from_static(b"dealer")),
        ConsensusHeaderArtifact::CommitteePreAnnounce {
            epoch: 0x0102030405060708,
            outcome: Bytes::from_static(b"preannounce"),
        },
    ];
    for (index, consensus) in consensus.into_iter().enumerate() {
        let artifacts = mixed_artifacts(consensus);
        let bytes = encode_outbe_block_artifacts(&artifacts).unwrap();
        assert_eq!(bytes.len(), expected[index].0);
        assert_eq!(alloy_primitives::keccak256(&bytes), expected[index].1);
        assert_eq!(decode_outbe_block_artifacts(&bytes).unwrap(), artifacts);
    }
}

#[test]
fn duplicate_checks_precede_record_payload_validation() {
    for (tag, payload, message) in [
        (1, vec![0; 32], "duplicate execution summary artifact"),
        (
            5,
            7_u64.to_be_bytes().to_vec(),
            "duplicate timestamp_millis_part",
        ),
        (6, vec![0, 0], "duplicate late finalize credits artifact"),
        (
            8,
            vec![0; 36],
            "duplicate compressed-entities root artifact",
        ),
    ] {
        assert_fatal(&envelope(&[(tag, &payload), (tag, &[])]), message);
    }
    // All consensus tags share one slot; the second payload is never inspected.
    for tag in [2, 3, 7] {
        assert_fatal(
            &envelope(&[(3, b"dealer"), (tag, &[])]),
            "duplicate consensus header artifact",
        );
    }
}

#[test]
fn zero_timestamp_sentinel_and_structural_ranges_are_preserved() {
    let zero = 0_u64.to_be_bytes();
    let out_of_range = u64::MAX.to_be_bytes();
    let decoded =
        decode_outbe_block_artifacts(&envelope(&[(5, &zero), (5, &zero), (5, &out_of_range)]))
            .unwrap();
    assert_eq!(decoded.timestamp_millis_part, u64::MAX);
    assert_fatal(
        &envelope(&[(5, &out_of_range), (5, &zero)]),
        "duplicate timestamp_millis_part",
    );
}

#[test]
fn malformed_record_lengths_and_unknown_tags_keep_exact_errors() {
    for (tag, message) in [
        (1, "invalid execution summary artifact length: 0"),
        (2, "boundary header artifact payload too short"),
        (4, "unsupported block artifact tag: 4"),
        (5, "timestamp_millis_part payload length: 0 (expected 8)"),
        (6, "late finalize credits payload too short"),
        (7, "committee pre-announce payload too short for epoch"),
        (
            8,
            "compressed-entities root payload length: 0 (expected 36)",
        ),
        (255, "unsupported block artifact tag: 255"),
    ] {
        assert_fatal(&envelope(&[(tag, &[])]), message);
    }
}

#[test]
fn malformed_envelope_preserves_framing_error_precedence() {
    for (bytes, message) in [
        (&b"OART\x0B"[..], "block artifacts too short"),
        (
            &b"NOPE\x01\x00"[..],
            "unknown non-empty extra_data block artifact",
        ),
        (
            &b"OART\x01\x00"[..],
            "unsupported block artifact version: 1",
        ),
        (
            &b"OART\x0B\x01"[..],
            "truncated block artifact record header",
        ),
        (
            &b"OART\x0B\x01\xFF\x00\x01"[..],
            "truncated block artifact record payload",
        ),
        (
            &b"OART\x0B\x00\x00"[..],
            "trailing bytes in block artifacts",
        ),
    ] {
        assert_fatal(bytes, message);
    }
    let mut bytes = envelope(&[(255, &[])]);
    bytes.push(0);
    assert_fatal(&bytes, "unsupported block artifact tag: 255");
}

#[test]
fn late_credit_malformed_payloads_keep_caps_order_and_trailing_errors() {
    assert_fatal(
        &envelope(&[(6, &[0, 4])]),
        "too many late finalize batches: 4 > 3",
    );
    assert_fatal(
        &envelope(&[(6, &[0, 1])]),
        "truncated late finalize credit prefix",
    );
    assert_fatal(
        &envelope(&[(6, &[0, 0, 0])]),
        "trailing bytes in late finalize credits payload",
    );

    let artifacts = mixed_artifacts(ConsensusHeaderArtifact::DealerLog(Bytes::new()));
    let credits = artifacts.late_finalize_credits.unwrap();
    let encoded = encode_late_finalize_credits_artifact(&credits).unwrap();
    let mut payload = encoded[9..].to_vec();
    // Count + fixed binding prefix precede the bitmap length.
    payload[98..100].copy_from_slice(&33_u16.to_be_bytes());
    assert_fatal(
        &envelope(&[(6, &payload)]),
        "late finalize bitmap too long: 33 > 32",
    );
    payload[98..100].copy_from_slice(&32_u16.to_be_bytes());
    assert_fatal(
        &envelope(&[(6, &payload)]),
        "truncated late finalize credit body",
    );
    payload = encoded[9..].to_vec();
    payload[0..2].copy_from_slice(&2_u16.to_be_bytes());
    payload.extend_from_slice(&encoded[11..]);
    assert_fatal(
        &envelope(&[(6, &payload)]),
        "late finalize batches not in strictly ascending canonical order",
    );
}
