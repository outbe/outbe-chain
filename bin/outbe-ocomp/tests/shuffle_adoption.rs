use alloy_primitives::{Address, B256, U256};
use outbe_ocomp::{
    cas::{CasLimits, CasWriterRole, FilesystemCas, FilesystemCasReader},
    control::poc_schema_limits,
    inbox::{WorkerInbox, WorkerInboxLimits},
    lysis_shuffle_adoption::adopt_lysis_shuffle_descendants,
};
use outbe_ocomp_protocol::{
    common::EntityId36,
    result::ContributorActionV1,
    shuffle::{build_owner_shuffle_run, verified_shuffle_run_records, ShuffleRunBuildContextV1},
    unit::CanonicalRunSpan,
    ProtocolError,
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

fn contributor(index: u32) -> ContributorActionV1 {
    let mut owner = [0_u8; 20];
    owner[16..].copy_from_slice(&(index + 1).to_be_bytes());
    let mut tribute = [0_u8; 36];
    tribute[32..].copy_from_slice(&index.to_be_bytes());
    ContributorActionV1 {
        owner: Address::from(owner),
        source_tribute_id: EntityId36(tribute),
        nominal_amount_minor: U256::from(index + 1),
    }
}

#[test]
fn supervisor_adopts_the_complete_verified_descendant_closure_into_cas() {
    let directory = tempdir().unwrap();
    let limits = poc_schema_limits();
    let inbox = WorkerInbox::open(directory.path().join("inbox"), INBOX_LIMITS).unwrap();
    let cas = FilesystemCas::open(
        directory.path().join("cas"),
        CasWriterRole::Supervisor,
        CAS_LIMITS,
    )
    .unwrap();
    let root = build_owner_shuffle_run(
        ShuffleRunBuildContextV1 {
            protocol_bundle_hash: B256::repeat_byte(1),
            job_id: B256::repeat_byte(2),
            attempt: 1,
            unit_id: B256::repeat_byte(3),
            run_span: CanonicalRunSpan {
                start_run: 0,
                end_run: 3,
            },
            source_coverage_root: B256::repeat_byte(4),
            source_coverage_count: 513,
        },
        (0..513_u32).map(|index| Ok(contributor(index))),
        &limits,
        |bytes| {
            inbox
                .stage_shuffle_object(bytes, &limits)
                .map_err(|_| ProtocolError::InvalidInvariant("test inbox stage"))
        },
    )
    .unwrap();
    assert_eq!(inbox.object_count().unwrap(), 5);
    assert_eq!(cas.object_count().unwrap(), 0);

    let adopted = adopt_lysis_shuffle_descendants(root.clone(), &inbox, &cas, &limits).unwrap();
    assert_eq!(adopted.descendant_object_count, 4);
    assert_eq!(adopted.verified_record_count, 513);
    assert_eq!(cas.object_count().unwrap(), 4);

    let reader = FilesystemCasReader::open(directory.path().join("cas"), CAS_LIMITS).unwrap();
    let records = verified_shuffle_run_records(root, &limits, |reference| {
        reader
            .read_verified(reference)
            .map(|object| object.bytes().to_vec())
            .map_err(|_| ProtocolError::InvalidInvariant("test CAS read"))
    })
    .unwrap()
    .collect::<Result<Vec<_>, _>>()
    .unwrap();
    assert_eq!(records.len(), 513);
}

#[test]
fn adoption_fails_closed_when_a_referenced_worker_object_is_missing() {
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
    let root = build_owner_shuffle_run(
        ShuffleRunBuildContextV1 {
            protocol_bundle_hash: B256::repeat_byte(1),
            job_id: B256::repeat_byte(2),
            attempt: 1,
            unit_id: B256::repeat_byte(3),
            run_span: CanonicalRunSpan {
                start_run: 0,
                end_run: 2,
            },
            source_coverage_root: B256::repeat_byte(4),
            source_coverage_count: 257,
        },
        (0..257_u32).map(|index| Ok(contributor(index))),
        &limits,
        |bytes| {
            source_inbox
                .stage_shuffle_object(bytes, &limits)
                .map_err(|_| ProtocolError::InvalidInvariant("test inbox stage"))
        },
    )
    .unwrap();

    assert!(adopt_lysis_shuffle_descendants(root, &empty_inbox, &cas, &limits).is_err());
    assert_eq!(cas.object_count().unwrap(), 0);
}
