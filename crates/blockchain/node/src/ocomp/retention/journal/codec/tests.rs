use super::*;

fn candidate() -> CandidatePinV1 {
    CandidatePinV1 {
        block_number: 19,
        block_hash: B256::repeat_byte(1),
        state_root: B256::repeat_byte(2),
        intent_id: B256::repeat_byte(3),
        wwd: 7,
        ce_sealed_root: B256::repeat_byte(4),
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
            279,
            "zero terminal source generation",
            "invalid terminal export-authority flag",
            "terminal export authority has a conflicting source generation",
        ),
        (
            5,
            released(Some(export())),
            255,
            "zero released source generation",
            "invalid export-authority flag",
            "released record carries inconsistent authority",
        ),
        (
            6,
            terminal(6, Some(export())),
            279,
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

// Captured from the pre-refactor v6 codec, independently of the current encoder.
const GOLDEN_RECORDS: [(u8, usize, &str); 9] = [
    (
        1,
        255,
        "0xc36054213686577638bdb7b1b4b3922b5a3060e4fb938317c3621f9198eafb2d",
    ),
    (
        2,
        311,
        "0x41d4dbd10fb50cbc706d15e1d23d65b3c0dbbeccd3a71f231a991422b4673d8b",
    ),
    (
        3,
        359,
        "0x2a1369498dd11a71fba6c94ede929c4dfa10699b42c7ce230aea86b0160a7668",
    ),
    (
        4,
        336,
        "0x715cc5d94c145bbf82b1dde2f7f7ff7316a5648eab452d7910949ad06744e822",
    ),
    (
        4,
        384,
        "0x4f420d591abc16b731706131cb0afdeb02349743892f9a39202b5acfe4597c14",
    ),
    (
        5,
        304,
        "0x5bad530cb6343e730068af313972b6f8764df2bdf2f88b8970311ac79002e269",
    ),
    (
        5,
        352,
        "0x00ab2e2c52e7950d0765dcda30d6b4a32bca133da4744d08527e569379bb973c",
    ),
    (
        6,
        336,
        "0x34a0257ba62a73eaf69dbfc211d69d9510a723f7b47330d3ab30ac6cc3162d28",
    ),
    (
        6,
        384,
        "0xd912406c5d7e366b42633f578a192d1cefeb72472957eede7b3e50af4e664188",
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
