use alloy_primitives::{Address, B256, U256};
use outbe_ocomp_protocol::{
    codec::CodecLimits,
    common::EntityId36,
    control::CasObjectRefV1,
    registry::ObjectKind,
    result::ContributorActionV1,
    shuffle::{
        ShuffleBucketRecordV1, ShufflePageSpanV1, ShuffleRunArtifactV1, ShuffleRunChildV1,
        ShuffleRunKindV1, ShuffleRunPayloadV1, MAX_SHUFFLE_LEAF_RECORDS,
    },
    unit::CanonicalRunSpan,
    ProtocolError, SchemaLimits,
};

const LIMITS: SchemaLimits = SchemaLimits {
    codec: CodecLimits::new(1_048_576, 4_096, 2_097_152),
    max_bounded_bytes: 262_144,
    max_proof_bytes: 262_144,
    max_opening_bytes: 262_144,
    max_collection_items: 4_096,
    max_action_items: 4_096,
    max_chunk_items: 4_096,
    max_unit_inputs: 64,
    max_control_body_bytes: 262_144,
};

fn hash(byte: u8) -> B256 {
    B256::repeat_byte(byte)
}

fn entity(index: u32) -> EntityId36 {
    let mut bytes = [0_u8; 36];
    bytes[32..].copy_from_slice(&index.to_be_bytes());
    EntityId36(bytes)
}

fn owner(index: u32) -> Address {
    let mut bytes = [0_u8; 20];
    bytes[16..].copy_from_slice(&index.to_be_bytes());
    Address::from(bytes)
}

fn contributor(index: u32) -> ContributorActionV1 {
    ContributorActionV1 {
        owner: owner(index + 1),
        source_tribute_id: entity(index),
        nominal_amount_minor: U256::from(index + 1),
    }
}

fn owner_leaf(record_count: usize) -> ShuffleRunArtifactV1 {
    ShuffleRunArtifactV1 {
        protocol_bundle_hash: hash(1),
        job_id: hash(2),
        attempt: 1,
        unit_id: hash(3),
        kind: ShuffleRunKindV1::Owner,
        run_span: CanonicalRunSpan {
            start_run: 0,
            end_run: 1,
        },
        page_span: ShufflePageSpanV1 {
            start_page: 0,
            end_page: 1,
        },
        first_record_ordinal: 0,
        record_count: u32::try_from(record_count).unwrap(),
        source_coverage_root: hash(4),
        source_coverage_count: 300,
        ordered_record_root: hash(5),
        payload: ShuffleRunPayloadV1::OwnerLeaf(
            (0..u32::try_from(record_count).unwrap())
                .map(contributor)
                .collect(),
        ),
    }
}

fn child(
    digest: u8,
    start_page: u32,
    end_page: u32,
    first_record_ordinal: u32,
    record_count: u32,
) -> ShuffleRunChildV1 {
    ShuffleRunChildV1 {
        artifact_ref: CasObjectRefV1 {
            transport_digest: hash(digest),
            encoded_bytes: 128,
            expected_ocb1_kind: Some(ObjectKind::ShuffleRunArtifactV1.tag()),
        },
        page_span: ShufflePageSpanV1 {
            start_page,
            end_page,
        },
        first_record_ordinal,
        record_count,
        ordered_record_root: hash(digest + 20),
    }
}

#[test]
fn owner_leaf_round_trips_at_the_exact_256_record_boundary() {
    let artifact = owner_leaf(MAX_SHUFFLE_LEAF_RECORDS);
    let encoded = artifact.encode_canonical(&LIMITS).unwrap();
    assert_eq!(
        ShuffleRunArtifactV1::decode_canonical(&encoded, &LIMITS).unwrap(),
        artifact
    );
}

#[test]
fn a_257th_record_requires_another_leaf_instead_of_growing_one_object() {
    let artifact = owner_leaf(MAX_SHUFFLE_LEAF_RECORDS + 1);
    assert_eq!(
        artifact.encode_canonical(&LIMITS),
        Err(ProtocolError::InvalidInvariant("shuffle leaf record cap"))
    );
}

#[test]
fn leaf_kind_order_and_count_are_validated_from_values() {
    let mut artifact = owner_leaf(2);
    artifact.kind = ShuffleRunKindV1::Bucket;
    assert!(matches!(
        artifact.encode_canonical(&LIMITS),
        Err(ProtocolError::InvalidInvariant(
            "shuffle payload kind binding"
        ))
    ));

    let mut artifact = owner_leaf(2);
    if let ShuffleRunPayloadV1::OwnerLeaf(records) = &mut artifact.payload {
        records.swap(0, 1);
    }
    assert!(matches!(
        artifact.encode_canonical(&LIMITS),
        Err(ProtocolError::InvalidInvariant(
            "owner shuffle records strictly ordered"
        ))
    ));

    let mut artifact = owner_leaf(2);
    artifact.record_count = 1;
    assert!(matches!(
        artifact.encode_canonical(&LIMITS),
        Err(ProtocolError::InvalidInvariant("shuffle leaf record count"))
    ));
}

#[test]
fn bucket_leaf_rejects_duplicate_or_reordered_raw_records() {
    let mut artifact = owner_leaf(0);
    artifact.kind = ShuffleRunKindV1::Bucket;
    artifact.record_count = 2;
    artifact.payload = ShuffleRunPayloadV1::BucketLeaf(vec![
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
    ]);
    assert!(matches!(
        artifact.encode_canonical(&LIMITS),
        Err(ProtocolError::InvalidInvariant(
            "bucket shuffle records strictly ordered"
        ))
    ));
}

#[test]
fn node_requires_the_canonical_adjacent_page_and_record_split() {
    let valid = ShuffleRunArtifactV1 {
        protocol_bundle_hash: hash(1),
        job_id: hash(2),
        attempt: 1,
        unit_id: hash(3),
        kind: ShuffleRunKindV1::Owner,
        run_span: CanonicalRunSpan {
            start_run: 0,
            end_run: 4,
        },
        page_span: ShufflePageSpanV1 {
            start_page: 0,
            end_page: 3,
        },
        first_record_ordinal: 0,
        record_count: 600,
        source_coverage_root: hash(4),
        source_coverage_count: 1_024,
        ordered_record_root: hash(5),
        payload: ShuffleRunPayloadV1::Node {
            left: child(10, 0, 2, 0, 512),
            right: child(11, 2, 3, 512, 88),
        },
    };
    let encoded = valid.encode_canonical(&LIMITS).unwrap();
    assert_eq!(
        ShuffleRunArtifactV1::decode_canonical(&encoded, &LIMITS).unwrap(),
        valid
    );

    let mut wrong_split = valid.clone();
    if let ShuffleRunPayloadV1::Node { left, right } = &mut wrong_split.payload {
        left.page_span.end_page = 1;
        right.page_span.start_page = 1;
    }
    assert!(matches!(
        wrong_split.encode_canonical(&LIMITS),
        Err(ProtocolError::InvalidInvariant(
            "shuffle node canonical page split"
        ))
    ));

    let mut gap = valid;
    if let ShuffleRunPayloadV1::Node { right, .. } = &mut gap.payload {
        right.first_record_ordinal += 1;
    }
    assert!(matches!(
        gap.encode_canonical(&LIMITS),
        Err(ProtocolError::InvalidInvariant(
            "shuffle node record adjacency"
        ))
    ));
}

#[test]
fn node_references_must_be_distinct_typed_nonempty_cas_objects() {
    let mut artifact = owner_leaf(0);
    artifact.page_span.end_page = 2;
    artifact.record_count = 2;
    artifact.payload = ShuffleRunPayloadV1::Node {
        left: child(10, 0, 1, 0, 1),
        right: child(10, 1, 2, 1, 1),
    };
    assert!(matches!(
        artifact.encode_canonical(&LIMITS),
        Err(ProtocolError::InvalidInvariant(
            "shuffle node distinct child objects"
        ))
    ));
}

#[test]
fn owner_tree_can_prove_an_empty_contributor_stream_without_dropping_source_coverage() {
    let artifact = ShuffleRunArtifactV1 {
        protocol_bundle_hash: hash(1),
        job_id: hash(2),
        attempt: 1,
        unit_id: hash(3),
        kind: ShuffleRunKindV1::Owner,
        run_span: CanonicalRunSpan {
            start_run: 0,
            end_run: 2,
        },
        page_span: ShufflePageSpanV1 {
            start_page: 0,
            end_page: 2,
        },
        first_record_ordinal: 0,
        record_count: 0,
        source_coverage_root: hash(4),
        source_coverage_count: 512,
        ordered_record_root: hash(5),
        payload: ShuffleRunPayloadV1::Node {
            left: child(10, 0, 1, 0, 0),
            right: child(11, 1, 2, 0, 0),
        },
    };
    let encoded = artifact.encode_canonical(&LIMITS).unwrap();
    assert_eq!(
        ShuffleRunArtifactV1::decode_canonical(&encoded, &LIMITS).unwrap(),
        artifact
    );
}

#[test]
fn decoder_rejects_trailing_bytes_instead_of_accepting_an_ambiguous_artifact() {
    let mut encoded = owner_leaf(1).encode_canonical(&LIMITS).unwrap();
    encoded.push(0);
    assert!(matches!(
        ShuffleRunArtifactV1::decode_canonical(&encoded, &LIMITS),
        Err(ProtocolError::BodyLengthMismatch { .. })
            | Err(ProtocolError::TrailingBytes { .. })
            | Err(ProtocolError::NonCanonicalEncoding)
    ));
}
