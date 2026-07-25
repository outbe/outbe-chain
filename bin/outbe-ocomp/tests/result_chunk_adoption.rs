use alloy_primitives::{Address, B256, U256};
use outbe_lysis::program_v1::result::{
    encode_root_reduce_output, LysisListSubtreeCarrierV1, RootReduceOutputV1, RootReduceSummaryV1,
};
use outbe_ocomp::{
    cas::{CasLimits, CasWriterRole, FilesystemCas},
    control::poc_schema_limits,
    inbox::{WorkerInbox, WorkerInboxLimits},
    lysis_result_adoption::adopt_lysis_result_chunk,
};
use outbe_ocomp_protocol::{
    common::{BoundedBytes, EntityId36},
    result::{ContributorActionV1, NodActionV1, OutputManifestEntryV1, ResultChunkV1},
    unit::{
        BinaryReducerNode, CanonicalInputRefV1, InputPurpose, InputSourceKind, UnitArtifactV1,
        UnitInterval, UnitPhase, UnitSpecV1, WorkOutputHeaderV1,
    },
    ListKind, ObjectKind, SchemaLimits,
};
use tempfile::tempdir;

const CAS_LIMITS: CasLimits = CasLimits {
    max_object_bytes: 1_048_576,
    max_total_bytes: 16_777_216,
};
const INBOX_LIMITS: WorkerInboxLimits = WorkerInboxLimits {
    max_artifact_bytes: 1_048_576,
    max_total_bytes: 16_777_216,
};

fn entity(marker: u8) -> EntityId36 {
    EntityId36([marker; 36])
}

fn leaf_spec(chunk_ordinal: u32) -> UnitSpecV1 {
    UnitSpecV1 {
        protocol_bundle_hash: B256::repeat_byte(1),
        job_id: B256::repeat_byte(2),
        attempt: 3,
        phase: UnitPhase::RootReduce,
        interval: UnitInterval::BinaryReducerNode(BinaryReducerNode {
            level: 0,
            index: chunk_ordinal,
        }),
        canonical_ordered_inputs: vec![
            CanonicalInputRefV1 {
                purpose: InputPurpose::InputManifest,
                source_kind: InputSourceKind::AuthenticatedRoot,
                source_id: B256::repeat_byte(4),
                record_count_limit: 1,
                max_encoded_bytes: 1024,
                max_decoded_bytes: 1024,
            },
            CanonicalInputRefV1 {
                purpose: InputPurpose::FinalizedOutputRecords,
                source_kind: InputSourceKind::UnitOutput,
                source_id: B256::repeat_byte(5),
                record_count_limit: 256,
                max_encoded_bytes: 1_048_576,
                max_decoded_bytes: 1_048_576,
            },
            CanonicalInputRefV1 {
                purpose: InputPurpose::OwnerOrderedRecords,
                source_kind: InputSourceKind::UnitOutput,
                source_id: B256::repeat_byte(6),
                record_count_limit: 256,
                max_encoded_bytes: 1_048_576,
                max_decoded_bytes: 1_048_576,
            },
            CanonicalInputRefV1 {
                purpose: InputPurpose::BucketOrderedRecords,
                source_kind: InputSourceKind::UnitOutput,
                source_id: B256::repeat_byte(7),
                record_count_limit: 256,
                max_encoded_bytes: 1_048_576,
                max_decoded_bytes: 1_048_576,
            },
        ],
        lysis_program_semantics_hash: B256::repeat_byte(8),
        planner_spec_version: 1,
        reducer_spec_version: 1,
    }
}

fn chunk(spec: &UnitSpecV1, chunk_ordinal: u32) -> ResultChunkV1 {
    ResultChunkV1 {
        protocol_bundle_hash: spec.protocol_bundle_hash,
        job_id: spec.job_id,
        attempt: spec.attempt,
        chunk_ordinal,
        first_nod_ordinal: chunk_ordinal * 256,
        ordered_nod_actions: vec![NodActionV1 {
            raw_ordinal: chunk_ordinal * 256,
            tribute_id: entity(9),
            nod_id: entity(10),
            owner: Address::repeat_byte(11),
            wwd: 20_260_725,
            league_id: 1,
            floor_price_minor: U256::from(2),
            gratis_load_minor: U256::from(3),
            entry_price_minor: U256::from(4),
            cost_amount_minor: U256::from(5),
            issuance_currency: 840,
            reference_currency: 978,
            issued_at: 2_026_072_500,
            bucket_key: B256::repeat_byte(12),
        }],
        ordered_eligible_contributors: vec![ContributorActionV1 {
            owner: Address::repeat_byte(11),
            source_tribute_id: entity(9),
            nominal_amount_minor: U256::from(6),
        }],
    }
}

fn root_leaf_artifact(
    spec: &UnitSpecV1,
    entry: OutputManifestEntryV1,
    limits: &SchemaLimits,
) -> UnitArtifactV1 {
    let action_records = vec![vec![0x31]];
    let manifest_records = vec![entry.encode_canonical_record(limits).unwrap()];
    let result_hashes = vec![entry.result_chunk_hash.as_slice().to_vec()];
    let summary = RootReduceSummaryV1 {
        protocol_bundle_hash: spec.protocol_bundle_hash,
        job_id: spec.job_id,
        attempt: spec.attempt,
        plan_hash: B256::repeat_byte(0x32),
        covered_primary_start: entry.chunk_ordinal,
        covered_primary_count: 1,
        nod_actions: LysisListSubtreeCarrierV1::from_primary_page(
            ListKind::NodActions,
            entry.chunk_ordinal,
            &action_records,
            1,
        )
        .unwrap(),
        bucket_records: LysisListSubtreeCarrierV1::from_primary_page(
            ListKind::BucketRecords,
            entry.chunk_ordinal,
            &action_records,
            1,
        )
        .unwrap(),
        contributor_actions: LysisListSubtreeCarrierV1::from_primary_page(
            ListKind::ContributorActions,
            entry.chunk_ordinal,
            &action_records,
            1,
        )
        .unwrap(),
        output_manifest_entries: LysisListSubtreeCarrierV1::from_primary_page(
            ListKind::CompleteOutputManifest,
            entry.chunk_ordinal,
            &manifest_records,
            limits.max_bounded_bytes,
        )
        .unwrap(),
        result_chunk_hashes: LysisListSubtreeCarrierV1::from_primary_page(
            ListKind::ResultChunkHashes,
            entry.chunk_ordinal,
            &result_hashes,
            32,
        )
        .unwrap(),
        tribute_count: 1,
        nod_count: 1,
        bucket_count: 1,
        contributor_count: 1,
        tribute_nominal_total: U256::from(6),
        eligible_nominal_total: U256::from(6),
        nod_gratis_consumed: U256::from(3),
        nod_cost_total: U256::from(5),
        first_error_ordinal: None,
    };
    let coverage_root = summary.result_chunk_hashes.tree_root;
    UnitArtifactV1::from_canonical_output(
        spec,
        WorkOutputHeaderV1 {
            source_coverage_root: coverage_root,
            output_coverage_root: coverage_root,
            source_coverage_count: 1,
            output_coverage_count: 1,
        },
        BoundedBytes(
            encode_root_reduce_output(
                &RootReduceOutputV1::Leaf {
                    summary,
                    output_manifest_entry: entry,
                },
                limits,
            )
            .unwrap(),
        ),
        limits,
    )
    .unwrap()
}

#[test]
fn supervisor_adopts_only_the_exact_manifest_bound_result_chunk() {
    let directory = tempdir().unwrap();
    let limits = poc_schema_limits();
    let inbox = WorkerInbox::open(directory.path().join("inbox"), INBOX_LIMITS).unwrap();
    let cas = FilesystemCas::open(
        directory.path().join("cas"),
        CasWriterRole::Supervisor,
        CAS_LIMITS,
    )
    .unwrap();
    let spec = leaf_spec(0);
    let chunk = chunk(&spec, 0);
    let bytes = chunk.encode_canonical(&limits).unwrap();
    let reference = inbox.stage_result_chunk(&bytes, &limits).unwrap();
    let entry = OutputManifestEntryV1 {
        chunk_ordinal: 0,
        result_chunk_hash: chunk.result_chunk_hash(&limits).unwrap(),
        result_chunk_ref: reference,
    };
    let artifact = root_leaf_artifact(&spec, entry.clone(), &limits);

    let adopted = adopt_lysis_result_chunk(&spec, &artifact, &inbox, &cas, &limits).unwrap();
    assert_eq!(adopted.chunk_ordinal, 0);
    assert_eq!(adopted.nod_count, 1);
    assert_eq!(adopted.contributor_count, 1);
    assert_eq!(adopted.authoritative_ref, entry.result_chunk_ref);
    assert_eq!(
        cas.read_verified(&entry.result_chunk_ref).unwrap().bytes(),
        bytes
    );
}

#[test]
fn adoption_rejects_missing_or_semantically_substituted_result_chunk() {
    let directory = tempdir().unwrap();
    let limits = poc_schema_limits();
    let source_inbox =
        WorkerInbox::open(directory.path().join("source-inbox"), INBOX_LIMITS).unwrap();
    let empty_inbox =
        WorkerInbox::open(directory.path().join("empty-inbox"), INBOX_LIMITS).unwrap();
    let cas = FilesystemCas::open(
        directory.path().join("cas"),
        CasWriterRole::Supervisor,
        CAS_LIMITS,
    )
    .unwrap();
    let spec = leaf_spec(0);
    let chunk = chunk(&spec, 0);
    let bytes = chunk.encode_canonical(&limits).unwrap();
    let reference = source_inbox.stage_result_chunk(&bytes, &limits).unwrap();
    assert_eq!(
        reference.expected_ocb1_kind,
        Some(ObjectKind::ResultChunkV1.tag())
    );
    let entry = OutputManifestEntryV1 {
        chunk_ordinal: 0,
        result_chunk_hash: chunk.result_chunk_hash(&limits).unwrap(),
        result_chunk_ref: reference,
    };
    let artifact = root_leaf_artifact(&spec, entry.clone(), &limits);

    assert!(adopt_lysis_result_chunk(&spec, &artifact, &empty_inbox, &cas, &limits).is_err());
    let mut substituted = entry;
    substituted.result_chunk_hash = B256::repeat_byte(99);
    let substituted_artifact = root_leaf_artifact(&spec, substituted, &limits);
    assert!(
        adopt_lysis_result_chunk(&spec, &substituted_artifact, &source_inbox, &cas, &limits)
            .is_err()
    );
    assert_eq!(cas.object_count().unwrap(), 0);
}
