use super::*;
use outbe_ocomp_protocol::local_control::poc_schema_limits;

fn hash(byte: u8) -> B256 {
    B256::repeat_byte(byte)
}

fn record(plan_ordinal: u32) -> VerifiedAdmissionRecordV1 {
    VerifiedAdmissionRecordV1 {
        protocol_bundle_hash: hash(1),
        job_id: hash(2),
        attempt: 0,
        plan_hash: hash(4),
        plan_ordinal,
        unit_id: hash(u8::try_from(plan_ordinal + 5).unwrap()),
        artifact_ref: CasObjectRefV1 {
            transport_digest: hash(6),
            encoded_bytes: 100,
            expected_ocb1_kind: Some(ObjectKind::UnitArtifactV1.tag()),
        },
        result_chunk: None,
    }
}

fn open_catalog(
    root: &Path,
    limits: SchemaLimits,
    exact_plan_unit_count: u32,
) -> VerifiedAdmissionCatalog {
    create_private_directory(root).unwrap();
    let lock = ADMISSION_STORAGE_PROFILE.acquire_lock(root).unwrap();
    reject_orphaned_temps(root).unwrap();
    let header = AdmissionCatalogHeaderV1 {
        protocol_bundle_hash: hash(1),
        job_id: hash(2),
        attempt: 0,
        input_manifest_ref: CasObjectRefV1 {
            transport_digest: hash(20),
            encoded_bytes: 200,
            expected_ocb1_kind: Some(ObjectKind::InputManifestV1.tag()),
        },
        input_manifest_hash: hash(21),
        plan_ref: CasObjectRefV1 {
            transport_digest: hash(22),
            encoded_bytes: 100,
            expected_ocb1_kind: None,
        },
        plan_hash: hash(4),
        tribute_count: 1,
        primary_work_unit_count: 1,
        primary_work_unit_root: hash(23),
        exact_plan_unit_count,
    };
    let header_path = root.join(HEADER_FILE);
    if path_exists(&header_path).unwrap() {
        assert_eq!(
            decode_header(
                &ADMISSION_STORAGE_PROFILE
                    .read_bounded(&header_path, catalog_byte_cap(&limits))
                    .unwrap(),
                &limits
            )
            .unwrap(),
            header
        );
    } else {
        ADMISSION_STORAGE_PROFILE
            .persist_atomic(root, &header_path, &encode_header(&header, &limits).unwrap())
            .unwrap();
    }
    let abstained = path_exists(&root.join(CONFLICT_FILE)).unwrap();
    VerifiedAdmissionCatalog {
        root: root.to_path_buf(),
        limits,
        header,
        abstained,
        _lock: lock,
    }
}

#[test]
fn exact_replay_and_restart_preserve_one_admission() {
    let root = tempfile::tempdir().unwrap();
    let limits = poc_schema_limits();
    let admitted = record(0);
    {
        let mut catalog = open_catalog(root.path(), limits, 1);
        assert_eq!(
            catalog.admit(&admitted).unwrap(),
            AdmissionOutcome::NewlyAdmitted
        );
        assert_eq!(
            catalog.admit(&admitted).unwrap(),
            AdmissionOutcome::ExactReplay
        );
    }

    let catalog = open_catalog(root.path(), limits, 1);
    assert_eq!(catalog.read(0).unwrap(), admitted);
    assert_eq!(
        catalog
            .exact_plan_cursor()
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap(),
        vec![admitted]
    );
}

#[test]
fn conflicting_valid_admission_permanently_abstains_without_overwrite() {
    let root = tempfile::tempdir().unwrap();
    let limits = poc_schema_limits();
    let first = record(0);
    let mut second = first.clone();
    second.artifact_ref.transport_digest = hash(99);
    {
        let mut catalog = open_catalog(root.path(), limits, 1);
        catalog.admit(&first).unwrap();
        assert!(matches!(
            catalog.admit(&second),
            Err(AdmissionCatalogError::ConflictingAdmission { plan_ordinal: 0 })
        ));
        assert!(catalog.is_abstained());
        assert!(matches!(
            catalog.read(0),
            Err(AdmissionCatalogError::Abstained)
        ));
    }

    let catalog = open_catalog(root.path(), limits, 1);
    assert!(catalog.is_abstained());
    assert!(matches!(
        catalog.exact_plan_cursor(),
        Err(AdmissionCatalogError::Abstained)
    ));
}

#[test]
fn exact_plan_cursor_rejects_a_missing_plan_ordinal_before_iteration() {
    let root = tempfile::tempdir().unwrap();
    let limits = poc_schema_limits();
    let mut catalog = open_catalog(root.path(), limits, 3);
    catalog.admit(&record(0)).unwrap();
    catalog.admit(&record(2)).unwrap();

    assert!(matches!(
        catalog.exact_plan_cursor(),
        Err(AdmissionCatalogError::MissingAdmission { plan_ordinal: 1 })
    ));
}

#[test]
fn exact_plan_cursor_rejects_an_admission_outside_the_closed_plan() {
    let root = tempfile::tempdir().unwrap();
    let limits = poc_schema_limits();
    let mut catalog = open_catalog(root.path(), limits, 2);
    catalog.admit(&record(0)).unwrap();
    catalog.admit(&record(1)).unwrap();
    catalog.admit(&record(7)).unwrap();

    assert!(matches!(
        catalog.exact_plan_cursor(),
        Err(AdmissionCatalogError::UnexpectedAdmission { plan_ordinal: 7 })
    ));
}

#[test]
fn exact_plan_cursor_rejects_a_record_from_another_plan() {
    let root = tempfile::tempdir().unwrap();
    let limits = poc_schema_limits();
    let mut catalog = open_catalog(root.path(), limits, 1);
    let mut wrong_plan = record(0);
    wrong_plan.plan_hash = hash(99);
    catalog.admit(&wrong_plan).unwrap();

    let mut cursor = catalog.exact_plan_cursor().unwrap();
    assert!(matches!(
        cursor.next().unwrap(),
        Err(AdmissionCatalogError::PlanBinding {
            plan_ordinal: 0,
            expected,
            actual,
        }) if expected == hash(4) && actual == hash(99)
    ));
}

#[test]
fn exact_plan_cursor_rejects_an_uncommitted_side_index() {
    let root = tempfile::tempdir().unwrap();
    let limits = poc_schema_limits();
    let mut catalog = open_catalog(root.path(), limits, 1);
    catalog.admit(&record(0)).unwrap();
    let side_index = root.path().join("completion-order.index");
    fs::write(&side_index, b"0").unwrap();

    assert!(matches!(
        catalog.exact_plan_cursor(),
        Err(AdmissionCatalogError::UnexpectedCatalogEntry(path)) if path == side_index
    ));
}

#[test]
fn admission_read_rejects_a_symlink_substitution() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let limits = poc_schema_limits();
    {
        let mut catalog = open_catalog(root.path(), limits, 1);
        catalog.admit(&record(0)).unwrap();
    }
    let entry = root.path().join(format!("{:010}{ENTRY_SUFFIX}", 0));
    let substitute = root.path().join("substitute");
    fs::write(&substitute, b"not an admission").unwrap();
    fs::remove_file(&entry).unwrap();
    symlink(&substitute, &entry).unwrap();

    let catalog = open_catalog(root.path(), limits, 1);
    assert!(matches!(
        catalog.read(0),
        Err(AdmissionCatalogError::InvalidEnvelope)
    ));
}
