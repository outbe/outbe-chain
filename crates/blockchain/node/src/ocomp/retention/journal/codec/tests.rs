use super::*;

fn candidate() -> CandidatePinV1 {
    CandidatePinV1 {
        block_number: 19,
        block_hash: B256::repeat_byte(1),
        state_root: B256::repeat_byte(2),
        intent_id: B256::repeat_byte(3),
        wwd: 7,
        protocol_bundle_hash: B256::repeat_byte(5),
        input_lease_id: B256::repeat_byte(6),
    }
}
fn export() -> ExportAuthorityV1 {
    ExportAuthorityV1 {
        source_generation: 4,
        lease_generation: 5,
        manifest_hash: B256::repeat_byte(8),
    }
}
fn finalized(tag: u8) -> PinStateV1 {
    let candidate = candidate();
    let job_id = B256::repeat_byte(7);
    let finality_recorded_height = 20;
    let open_height = 20 + outbe_ocomp_protocol::state::RESULT_VOTE_MIN_FINALITY_DEPTH;
    let deadline_height = open_height + 10;
    match tag {
        2 => PinStateV1::Finalized {
            candidate,
            job_id,
            finality_recorded_height,
            open_height,
            deadline_height,
        },
        3 => PinStateV1::Exported {
            candidate,
            job_id,
            finality_recorded_height,
            open_height,
            deadline_height,
            export: export(),
        },
        _ => panic!("fixture requires finalized or exported tag"),
    }
}
fn terminal(tag: u8, export: Option<ExportAuthorityV1>) -> PinStateV1 {
    let candidate = candidate();
    let job_id = B256::repeat_byte(7);
    let finality_recorded_height = 20;
    let open_height = 20 + outbe_ocomp_protocol::state::RESULT_VOTE_MIN_FINALITY_DEPTH;
    let deadline_height = open_height + 10;
    let source_generation = 4;
    let terminal_height = 100;
    let release_height = terminal_height + RETAINED_EVIDENCE_WINDOW_BLOCKS;
    match tag {
        4 => PinStateV1::Terminal {
            candidate,
            job_id,
            finality_recorded_height,
            open_height,
            deadline_height,
            source_generation,
            export,
            terminal_height,
            release_height,
        },
        6 => PinStateV1::GcPending {
            candidate,
            job_id,
            finality_recorded_height,
            open_height,
            deadline_height,
            source_generation,
            export,
            terminal_height,
            release_height,
        },
        _ => panic!("fixture requires terminal or GC tag"),
    }
}
fn released(export: Option<ExportAuthorityV1>) -> PinStateV1 {
    PinStateV1::Released {
        candidate: candidate(),
        job_id: B256::repeat_byte(7),
        source_generation: 4,
        observed_height: 500,
        export,
    }
}
fn records() -> Vec<(u8, PinRecordV1)> {
    let states = [
        (
            1,
            PinStateV1::AwaitingJobFinalization {
                candidate: candidate(),
            },
        ),
        (2, finalized(2)),
        (3, finalized(3)),
        (4, terminal(4, None)),
        (4, terminal(4, Some(export()))),
        (5, released(None)),
        (5, released(Some(export()))),
        (6, terminal(6, None)),
        (6, terminal(6, Some(export()))),
    ];
    states
        .into_iter()
        .map(|(tag, state)| {
            (
                tag,
                PinRecordV1 {
                    generation: 9,
                    state,
                },
            )
        })
        .collect()
}
fn registry(record: PinRecordV1) -> JobRegistryV1 {
    let key = record_candidate(record).block_hash;
    JobRegistryV1 {
        generation: record.generation,
        last_updated: key,
        records: BTreeMap::from([(key, record)]),
    }
}
fn malformed<T: std::fmt::Debug>(result: Result<T, RetentionError>, expected: &'static str) {
    assert!(matches!(result, Err(RetentionError::MalformedJournal(actual)) if actual == expected));
}
fn rechecksum(encoded: &mut [u8]) {
    let body_length = encoded.len() - 32;
    let checksum = keccak256(&encoded[..body_length]);
    encoded[body_length..].copy_from_slice(checksum.as_slice());
}

#[test]
fn record_header_precedence_and_unknown_tag_candidate_read_are_stable() {
    let record = records()[0].1;
    let mut encoded = encode_record(record);
    encoded[0] ^= 1;
    encoded[9] = 5;
    malformed(decode_record(&encoded), "checksum mismatch");
    rechecksum(&mut encoded);
    malformed(decode_record(&encoded), "wrong magic");
    let mut encoded = encode_record(record);
    encoded[18] = 255;
    encoded.drain(19..encoded.len() - 32);
    rechecksum(&mut encoded);
    malformed(decode_record(&encoded), "truncated field");
}
#[test]
fn registry_version_precedes_checksum_and_magic() {
    let mut encoded = encode_registry(&registry(records()[0].1));
    encoded[0] ^= 1;
    encoded[9] = 5;
    assert!(matches!(
        decode_registry(&encoded),
        Err(RetentionError::UnsupportedJournalVersion { actual: 5 })
    ));
    encoded[9] = 6;
    malformed(decode_registry(&encoded), "checksum mismatch");
    rechecksum(&mut encoded);
    malformed(decode_registry(&encoded), "wrong magic");
}

#[test]
fn terminal_and_released_authority_errors_keep_state_specific_diagnostics() {
    let cases = [
        (
            4,
            terminal(4, Some(export())),
            247,
            "zero terminal source generation",
            "invalid terminal export-authority flag",
            "terminal export authority has a conflicting source generation",
        ),
        (
            5,
            released(Some(export())),
            223,
            "zero released source generation",
            "invalid export-authority flag",
            "released record carries inconsistent authority",
        ),
        (
            6,
            terminal(6, Some(export())),
            247,
            "zero GC source generation",
            "invalid GC export-authority flag",
            "GC export authority has a conflicting source generation",
        ),
    ];
    for (tag, state, source_offset, zero_error, flag_error, conflict_error) in cases {
        let original = encode_record(PinRecordV1 {
            generation: 9,
            state,
        });
        assert_eq!(original[18], tag);
        let mut bytes = original.clone();
        bytes[source_offset..source_offset + 8].fill(0);
        rechecksum(&mut bytes);
        malformed(decode_record(&bytes), zero_error);
        let mut bytes = original.clone();
        bytes[source_offset + 8] = 2;
        rechecksum(&mut bytes);
        malformed(decode_record(&bytes), flag_error);
        let mut bytes = original;
        bytes[source_offset + 16] = 7;
        rechecksum(&mut bytes);
        malformed(decode_record(&bytes), conflict_error);
    }
}

#[test]
fn registry_rejects_duplicate_keys_and_zero_record_count() {
    let mut bytes = encode_registry(&registry(records()[0].1));
    bytes.truncate(bytes.len() - 32);
    let entry = bytes[52..].to_vec();
    bytes.extend_from_slice(&entry);
    bytes[50..52].copy_from_slice(&2u16.to_be_bytes());
    bytes = append_checksum(bytes);
    malformed(
        decode_registry(&bytes),
        "duplicate or mismatched registry key",
    );
    let mut bytes = encode_registry(&registry(records()[0].1));
    bytes[50..52].fill(0);
    rechecksum(&mut bytes);
    malformed(
        decode_registry(&bytes),
        "registry must use an absent file for zero records",
    );
}

// Derived from the field layout with an independent byte writer and Keccak.
// The native-state candidate omits the former 32-byte CE root.
const GOLDEN_RECORDS: [(u8, usize, &str); 9] = [
    (
        1,
        223,
        "0xfe35fe296d0b0c36f149a96c16b8602bf3303bdd2f24b9c9bfb94dd2a6454d84",
    ),
    (
        2,
        279,
        "0x9260f013862c8f5a20277eb6049cf9803f4283a769dfb6c33eea5da4d5476883",
    ),
    (
        3,
        327,
        "0xcd766dc17e18adefce44e87071341d6a077b445449754c65f226efbfde53a457",
    ),
    (
        4,
        304,
        "0x6796b5bd97c16b0d0199f0739f8b18d395cf8ea966346609bc82eb85aeeaacc3",
    ),
    (
        4,
        352,
        "0xdad7c0205cdb69193cb5532b1c550bf25a74efdb1df9c75ad93cacc484bf0a9e",
    ),
    (
        5,
        272,
        "0xe848bca22c396896cdc216ab6d4930dfac5363732c3ec5d304b7ea2093910bea",
    ),
    (
        5,
        320,
        "0x39bbb9841b5bee8ffca0a1346f0df99736e2ba2e84fae6db7e32233d6e9517df",
    ),
    (
        6,
        304,
        "0x1df28ce6c06f848c897d405c878bc2c08bad062b458dd7f29a291e243ca72971",
    ),
    (
        6,
        352,
        "0xa9a5c85a176ab4af87a30157b0bb53df25ab31018d2a1f9eedcfdc718e110387",
    ),
];

#[test]
fn all_six_states_and_optional_authority_preserve_v6_golden_bytes() {
    for ((tag, record), (expected_tag, length, checksum)) in
        records().into_iter().zip(GOLDEN_RECORDS)
    {
        let encoded = encode_record(record);
        assert_eq!(tag, expected_tag);
        assert_eq!(encoded[18], expected_tag);
        assert_eq!(encoded.len(), length);
        assert_eq!(&encoded[..8], b"OUTBPIN1");
        assert_eq!(&encoded[8..10], &[0, 6]);
        assert_eq!(format!("{}", keccak256(&encoded[..length - 32])), checksum);
        assert_eq!(
            format!("{}", B256::from_slice(&encoded[length - 32..])),
            checksum
        );
        assert_eq!(
            decode_record(&encoded).expect("valid independent golden record"),
            record
        );
    }
}
