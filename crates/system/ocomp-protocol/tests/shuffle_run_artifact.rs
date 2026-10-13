use std::collections::BTreeMap;

use alloy_primitives::{B256, keccak256};
use outbe_ocomp_protocol::test_utils::FINALITY_INPUT_TEST_LIMITS as LIMITS;
use outbe_ocomp_protocol::{
    ProtocolError,
    control::CasObjectRefV1,
    registry::ObjectKind,
    shuffle::{
        MAX_SHUFFLE_LEAF_RECORDS, ShuffleBucketRecordV1, ShufflePageSpanV1, ShuffleRunArtifactV1,
        ShuffleRunBuildContextV1, ShuffleRunKindV1, ShuffleRunPayloadV1, ShuffleSourceCoverageV1,
        build_bucket_shuffle_run,
    },
    unit::CanonicalRunSpan,
};

fn hash(byte: u8) -> B256 {
    B256::repeat_byte(byte)
}

fn entity(index: u32) -> B256 {
    let mut bytes = [0_u8; 32];
    bytes[28..].copy_from_slice(&index.to_be_bytes());
    B256::from(bytes)
}

#[test]
fn source_coverage_composes_exact_adjacent_raw_runs_without_padding_units() {
    let run_0 = ShuffleSourceCoverageV1::leaf(
        CanonicalRunSpan {
            start_run: 0,
            end_run: 1,
        },
        hash(41),
        256,
        &LIMITS,
    )
    .unwrap();
    let run_1 = ShuffleSourceCoverageV1::leaf(
        CanonicalRunSpan {
            start_run: 1,
            end_run: 2,
        },
        hash(42),
        256,
        &LIMITS,
    )
    .unwrap();
    let run_2 = ShuffleSourceCoverageV1::leaf(
        CanonicalRunSpan {
            start_run: 2,
            end_run: 3,
        },
        hash(43),
        1,
        &LIMITS,
    )
    .unwrap();

    let first_pair = ShuffleSourceCoverageV1::merge(&run_0, &run_1, &LIMITS).unwrap();
    let complete = ShuffleSourceCoverageV1::merge(&first_pair, &run_2, &LIMITS).unwrap();
    assert_eq!(
        complete.run_span,
        CanonicalRunSpan {
            start_run: 0,
            end_run: 3
        }
    );
    assert_eq!(complete.count, 513);
    assert!(!complete.root.is_zero());

    let changed_last_raw_root = ShuffleSourceCoverageV1::leaf(
        CanonicalRunSpan {
            start_run: 2,
            end_run: 3,
        },
        hash(44),
        1,
        &LIMITS,
    )
    .unwrap();
    assert_ne!(
        complete.root,
        ShuffleSourceCoverageV1::merge(&first_pair, &changed_last_raw_root, &LIMITS)
            .unwrap()
            .root
    );
}

#[test]
fn source_coverage_rejects_gap_overlap_and_reordered_producers() {
    let left = ShuffleSourceCoverageV1::leaf(
        CanonicalRunSpan {
            start_run: 0,
            end_run: 1,
        },
        hash(41),
        256,
        &LIMITS,
    )
    .unwrap();
    let adjacent = ShuffleSourceCoverageV1::leaf(
        CanonicalRunSpan {
            start_run: 1,
            end_run: 2,
        },
        hash(42),
        1,
        &LIMITS,
    )
    .unwrap();
    let gap = ShuffleSourceCoverageV1::leaf(
        CanonicalRunSpan {
            start_run: 2,
            end_run: 3,
        },
        hash(43),
        1,
        &LIMITS,
    )
    .unwrap();

    assert!(ShuffleSourceCoverageV1::merge(&left, &adjacent, &LIMITS).is_ok());
    assert!(ShuffleSourceCoverageV1::merge(&left, &gap, &LIMITS).is_err());
    assert!(ShuffleSourceCoverageV1::merge(&adjacent, &left, &LIMITS).is_err());
}

fn build_context(source_coverage_count: u32) -> ShuffleRunBuildContextV1 {
    ShuffleRunBuildContextV1 {
        protocol_bundle_hash: hash(1),
        job_id: hash(2),
        attempt: 1,
        unit_id: hash(3),
        run_span: CanonicalRunSpan {
            start_run: 0,
            end_run: source_coverage_count.div_ceil(MAX_SHUFFLE_LEAF_RECORDS as u32),
        },
        source_coverage_root: hash(4),
        source_coverage_count,
    }
}

fn memory_stage(
    objects: &mut BTreeMap<B256, Vec<u8>>,
    bytes: &[u8],
) -> Result<CasObjectRefV1, ProtocolError> {
    let digest = keccak256(bytes);
    objects.insert(digest, bytes.to_vec());
    Ok(CasObjectRefV1 {
        transport_digest: digest,
        encoded_bytes: u64::try_from(bytes.len()).unwrap(),
        expected_ocb1_kind: Some(ObjectKind::ShuffleRunArtifactV1.tag()),
    })
}

#[test]
fn builder_emits_one_empty_owner_leaf_but_rejects_an_empty_bucket_stream() {
    let mut bucket_objects = BTreeMap::new();
    assert_eq!(
        build_bucket_shuffle_run(build_context(1), std::iter::empty(), &LIMITS, |bytes| {
            memory_stage(&mut bucket_objects, bytes)
        },),
        Err(ProtocolError::InvalidInvariant(
            "non-empty bucket shuffle run"
        ))
    );
    assert!(bucket_objects.is_empty());
}

#[test]
fn bucket_leaf_rejects_duplicate_or_reordered_raw_records() {
    let artifact = ShuffleRunArtifactV1 {
        protocol_bundle_hash: hash(1),
        job_id: hash(2),
        attempt: 1,
        unit_id: hash(3),
        kind: ShuffleRunKindV1::Bucket,
        run_span: CanonicalRunSpan {
            start_run: 0,
            end_run: 1,
        },
        page_span: ShufflePageSpanV1 {
            start_page: 0,
            end_page: 1,
        },
        first_record_ordinal: 0,
        record_count: 2,
        source_coverage_root: hash(4),
        source_coverage_count: 300,
        ordered_record_root: hash(1),
        payload: ShuffleRunPayloadV1::BucketLeaf(vec![
            ShuffleBucketRecordV1 {
                bucket_key: hash(9),
                raw_ordinal: 2,
                tribute_id: entity(2),
                nod_id: entity(102),
            },
            ShuffleBucketRecordV1 {
                bucket_key: hash(9),
                raw_ordinal: 1,
                tribute_id: entity(1),
                nod_id: entity(101),
            },
        ]),
    };
    assert!(matches!(
        artifact.encode_canonical(&LIMITS),
        Err(ProtocolError::InvalidInvariant(
            "bucket shuffle records strictly ordered"
        ))
    ));
}
