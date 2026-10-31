use super::*;
use crate::{consensus::ReshareResult, error::PrecompileError};

fn mixed_artifacts(consensus: ConsensusHeaderArtifact) -> OutbeBlockArtifacts {
    OutbeBlockArtifacts {
        radicle_publication_enabled: false,
        radicle_publication: None,
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
        enclave_witness: None,
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
        tee_expired_target_exclusions_hash: tee_expired_target_exclusions_hash(&exclusions)
            .unwrap(),
        tee_expired_target_exclusions: exclusions,
    }
}

fn envelope(records: &[(u8, &[u8])]) -> Vec<u8> {
    let mut bytes = b"OART\x01\x00".to_vec();
    bytes.push(records.len() as u8);
    for (tag, payload) in records {
        bytes.push(*tag);
        bytes.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        bytes.extend_from_slice(payload);
    }
    bytes
}

/// Returns the record tags of a well-formed envelope, in wire order.
fn record_tags(bytes: &[u8]) -> Vec<u8> {
    let mut tags = Vec::new();
    let mut offset = HEADER_LEN;
    for _ in 0..bytes[6] {
        tags.push(bytes[offset]);
        let len = u16::from_be_bytes([bytes[offset + 1], bytes[offset + 2]]) as usize;
        offset += 3 + len;
    }
    assert_eq!(offset, bytes.len());
    tags
}

fn assert_fatal(bytes: &[u8], expected: &str) {
    match decode_outbe_block_artifacts(bytes).unwrap_err() {
        PrecompileError::Revert { 0: message } => assert_eq!(message, expected),
        other => panic!("expected revert codec error, got {other:?}"),
    }
}

#[test]
fn mixed_record_wire_vectors_are_pinned() {
    // Pinned wire vectors omit the retired entity-root record. The hashes bind
    // every remaining field, length, endian convention, version and record order.
    let expected = [
        (
            521,
            alloy_primitives::b256!(
                "f5195dfa12becfc0d73f13f1d4576d946f2b1a0dba180623dd8706eff3fb316e"
            ),
        ),
        (
            263,
            alloy_primitives::b256!(
                "844c9f8d27fb7e9f5670d13b3b5158ab75f49d7a6b73dabdf02035b2e15f8842"
            ),
        ),
        (
            276,
            alloy_primitives::b256!(
                "0f8d3b1b06993422ed2cba6db78ae255420b496ece6a129b528f76f16efca22f"
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
        let tags = record_tags(&bytes);
        assert!(
            tags.is_sorted_by(|a, b| a < b),
            "tags {tags:?} not ascending"
        );
        assert_eq!(bytes.len(), expected[index].0);
        assert_eq!(alloy_primitives::keccak256(&bytes), expected[index].1);
        assert_eq!(decode_outbe_block_artifacts(&bytes).unwrap(), artifacts);
    }
}

#[test]
fn order_and_duplicate_checks_precede_record_payload_validation() {
    for (tag, payload) in [
        (1, vec![0; 32]),
        (5, 7_u64.to_be_bytes().to_vec()),
        (0x0A, vec![0x01]),
    ] {
        assert_fatal(
            &envelope(&[(tag, &payload), (tag, &[])]),
            &format!("block artifact record tags out of order: {tag} after {tag}"),
        );
    }
    // All consensus tags share one slot. The decoder never inspects the second payload.
    assert_fatal(
        &envelope(&[(3, b"dealer"), (7, &[])]),
        "duplicate consensus header artifact",
    );
    for tag in [2, 3] {
        assert_fatal(
            &envelope(&[(3, b"dealer"), (tag, &[])]),
            &format!("block artifact record tags out of order: {tag} after 3"),
        );
    }
}

#[test]
fn zero_timestamp_record_is_rejected_and_structural_ranges_are_preserved() {
    let zero = 0_u64.to_be_bytes();
    let out_of_range = u64::MAX.to_be_bytes();
    assert_fatal(
        &envelope(&[(5, &zero)]),
        "block artifact timestamp_millis_part record is zero",
    );
    let decoded = decode_outbe_block_artifacts(&envelope(&[(5, &out_of_range)])).unwrap();
    assert_eq!(decoded.timestamp_millis_part, u64::MAX);
    assert_fatal(
        &envelope(&[(5, &out_of_range), (5, &zero)]),
        "block artifact record tags out of order: 5 after 5",
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
        (8, "unsupported block artifact tag: 8"),
        (0x0A, "empty enclave witness payload"),
        (255, "unsupported block artifact tag: 255"),
    ] {
        assert_fatal(&envelope(&[(tag, &[])]), message);
    }
}

#[test]
fn malformed_envelope_preserves_framing_error_precedence() {
    for (bytes, message) in [
        (&b"OART\x01"[..], "block artifacts too short"),
        (
            &b"NOPE\x01\x00\x00"[..],
            "unknown non-empty extra_data block artifact",
        ),
        (
            &b"OART\xff\x00\x00"[..],
            "unsupported block artifact version: 255",
        ),
        (
            &b"OART\x01\x00\x01"[..],
            "truncated block artifact record header",
        ),
        (
            &b"OART\x01\x00\x01\xFF\x00\x01"[..],
            "truncated block artifact record payload",
        ),
        (
            &b"OART\x01\x00\x00"[..],
            "block artifact envelope has no records and no publication flag",
        ),
        (
            &b"OART\x01\x01\x00\x00"[..],
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
    let mut payload = encoded[10..].to_vec();
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
    payload = encoded[10..].to_vec();
    payload[0..2].copy_from_slice(&2_u16.to_be_bytes());
    payload.extend_from_slice(&encoded[12..]);
    assert_fatal(
        &envelope(&[(6, &payload)]),
        "late finalize batches not in strictly ascending canonical order",
    );
}

#[test]
fn artifact_decode_requires_order() {
    let artifacts = mixed_artifacts(ConsensusHeaderArtifact::CommitteePreAnnounce {
        epoch: 9,
        outcome: Bytes::from_static(b"preannounce"),
    });
    let bytes = encode_outbe_block_artifacts(&artifacts).unwrap();
    assert_eq!(record_tags(&bytes), [0x01, 0x05, 0x06, 0x07]);

    // Swap the first two records: the decoder rejects the permutation.
    let summary_end = HEADER_LEN + 3 + EXECUTION_SUMMARY_LEN;
    let timestamp_end = summary_end + 3 + TIMESTAMP_MILLIS_PART_LEN;
    let mut permuted = bytes[..HEADER_LEN].to_vec();
    permuted.extend_from_slice(&bytes[summary_end..timestamp_end]);
    permuted.extend_from_slice(&bytes[HEADER_LEN..summary_end]);
    permuted.extend_from_slice(&bytes[timestamp_end..]);
    assert_eq!(permuted.len(), bytes.len());
    assert_fatal(
        &permuted,
        "block artifact record tags out of order: 1 after 5",
    );

    // Every encoder output re-encodes byte-identically after a decode.
    let mut publication_only = OutbeBlockArtifacts {
        radicle_publication_enabled: true,
        ..Default::default()
    };
    let empty_envelope = encode_outbe_block_artifacts(&publication_only).unwrap();
    assert_eq!(empty_envelope.as_ref(), b"OART\x01\x01\x00");
    publication_only.enclave_witness = Some(Bytes::from_static(b"witness"));
    let mut outputs = vec![bytes, empty_envelope];
    outputs.push(encode_outbe_block_artifacts(&publication_only).unwrap());
    outputs.push(
        encode_outbe_block_artifacts(&mixed_artifacts(ConsensusHeaderArtifact::BoundaryOutcome(
            boundary(),
        )))
        .unwrap(),
    );
    for encoded in outputs {
        let decoded = decode_outbe_block_artifacts(&encoded).unwrap();
        assert_eq!(encode_outbe_block_artifacts(&decoded).unwrap(), encoded);
    }

    // The encoder omits an empty late-finalize batch list, so the decoder
    // rejects an explicit empty record.
    assert_fatal(
        &envelope(&[(6, &[0, 0])]),
        "block artifact late finalize credits record has no batches",
    );
}
