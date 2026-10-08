mod support {
    include!("../support/mod.rs");
}
use alloy_primitives::B256;
use outbe_lysis::program_v1::planner::{LysisPlanTopologyV1, PlannedUnitPositionV1};
use outbe_ocomp::{
    admission_catalog::{
        AdmissionCatalogError, AdmissionCatalogReader, AdmissionPositionV1,
        VerifiedAdmissionCatalog,
    },
    bundle::PinnedProtocolBundle,
    cas::{CasLimits, CasWriterRole, FilesystemCas, FilesystemCasReader},
    control::poc_schema_limits,
    input_artifacts::poc_input_list_limits,
    input_ref_catalog::{InputRefCatalogError, VerifiedInputChunkRefCatalog},
    lysis_plan_audit::LysisPlanAuditStepV1,
    lysis_result_catalog::{ExactLysisResultCatalogCursorV1, LysisResultCatalogStepV1},
};
use outbe_ocomp_protocol::{
    common::BoundedBytes,
    registry::ObjectKind,
    result::{OutputManifestEntryV1, ResultChunkV1},
    unit::{UnitArtifactV1, UnitPhase, WorkOutputHeaderV1},
};
use outbe_primitives::time::WorldwideDay;
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::{symlink, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};
const CAS_LIMITS: CasLimits = CasLimits {
    max_object_bytes: 1_048_576,
    max_total_bytes: 64 * 1_048_576,
};
fn hash(byte: u8) -> B256 {
    B256::repeat_byte(byte)
}
struct Fixture {
    _directory: tempfile::TempDir,
    cas_root: PathBuf,
    input_ref_root: PathBuf,
    admission_root: PathBuf,
    limits: outbe_ocomp_protocol::SchemaLimits,
    bundle: PinnedProtocolBundle,
    expected_count: u32,
}
// Protocol-shaped fixture using native codecs/planner. Non-root phase payloads
// are minimal: this checks stored evidence, not real worker execution.
fn fixture(job_seed: u8) -> Fixture {
    let limits = poc_schema_limits();
    let list_limits = poc_input_list_limits();
    let bundle = crate::test_support::protocol_bundle_fixture();
    let bundle_hash = bundle.protocol_bundle_hash(&limits).unwrap();
    let pinned_bundle = PinnedProtocolBundle::decode(
        &bundle.encode_canonical(&limits).unwrap(),
        bundle_hash,
        &limits,
    )
    .unwrap();
    let job_id = hash(job_seed);
    let tribute_count = 1_u32;
    let day = WorldwideDay::new(20_260_725);

    let tributes = crate::test_support::tribute_population(day, tribute_count);
    let contributors_by_owner = crate::test_support::contributor_population(&tributes);
    let nod_action_tributes = tributes.clone();
    let openings = crate::test_support::fixture_openings(&bundle, job_id, day, &tributes, &limits);
    let directory = support::tempdir().unwrap();
    let cas_root = directory.path().join("cas");
    let input_ref_root = directory.path().join("input-refs");
    let admission_root = directory.path().join("admissions");
    let cas = FilesystemCas::open(&cas_root, CasWriterRole::Supervisor, CAS_LIMITS).unwrap();
    let crate::snapshot_test_support::PublishedFixturePlan {
        plan,
        plan_ref,
        manifest_ref,
    } = crate::snapshot_test_support::publish_fixture_plan(
        &cas,
        &input_ref_root,
        crate::snapshot_test_support::FixturePlanInputs {
            bundle: &bundle,
            job_id,
            day,
            tributes: &tributes,
            fidelity_openings: openings.fidelity,
            oracle_opening: openings.oracle,
        },
    );

    let reader = FilesystemCasReader::open(&cas_root, CAS_LIMITS).unwrap();
    let input_refs =
        VerifiedInputChunkRefCatalog::reopen(&input_ref_root, &reader, limits, list_limits)
            .unwrap();
    let mut admissions =
        VerifiedAdmissionCatalog::open(&admission_root, &cas, &plan_ref, &manifest_ref, limits)
            .unwrap();
    let topology = LysisPlanTopologyV1::new(plan.primary_work_unit_count).unwrap();
    let plan_hash = plan.plan_hash(&limits).unwrap();

    let page_inputs = RootPageInputs {
        bundle_hash,
        job_id,
        plan_hash,
        tributes: &tributes,
        nod_action_tributes: &nod_action_tributes,
        contributors_by_owner: &contributors_by_owner,
        limits: &limits,
    };
    for plan_ordinal in 0..topology.total_unit_count() {
        let spec = {
            let audit = outbe_ocomp::lysis_plan_audit::open_local_plan_audit(
                &admissions,
                &input_refs,
                &reader,
                &pinned_bundle,
                &limits,
            )
            .unwrap();
            audit.candidate_spec_at(plan_ordinal).unwrap()
        };
        let (artifact, result_entry) = match topology.plan_position_at(plan_ordinal).unwrap() {
            PlannedUnitPositionV1::TreeNode {
                phase: UnitPhase::RootReduce,
                level: 0,
                index,
            } => root_page_artifact(&spec, index, &page_inputs, &cas),
            _ => (
                UnitArtifactV1::from_canonical_output(
                    &spec,
                    WorkOutputHeaderV1 {
                        source_coverage_root: hash(0xa1),
                        output_coverage_root: hash(0xa2),
                        source_coverage_count: 1,
                        output_coverage_count: 1,
                    },
                    BoundedBytes(vec![0x42]),
                    &limits,
                )
                .unwrap(),
                None,
            ),
        };
        let mut artifact_ref = cas
            .publish_bytes(&artifact.encode_canonical(&limits).unwrap())
            .unwrap();
        artifact_ref.expected_ocb1_kind = Some(ObjectKind::UnitArtifactV1.tag());
        admissions
            .admit_verified_unit(
                AdmissionPositionV1 { plan_ordinal },
                &spec,
                artifact_ref,
                result_entry,
            )
            .unwrap();
    }
    drop(admissions);
    drop(input_refs);
    drop(reader);
    drop(cas);

    Fixture {
        _directory: directory,
        cas_root,
        input_ref_root,
        admission_root,
        limits,
        bundle: pinned_bundle,
        expected_count: topology.total_unit_count(),
    }
}

struct RootPageInputs<'a> {
    bundle_hash: B256,
    job_id: B256,
    plan_hash: B256,
    tributes: &'a [outbe_compressed_entities::TributeBodyV1],
    nod_action_tributes: &'a [outbe_compressed_entities::TributeBodyV1],
    contributors_by_owner: &'a [outbe_ocomp_protocol::result::ContributorActionV1],
    limits: &'a outbe_ocomp_protocol::SchemaLimits,
}
fn root_page_artifact(
    spec: &outbe_ocomp_protocol::unit::UnitSpecV1,
    index: u32,
    inputs: &RootPageInputs<'_>,
    cas: &FilesystemCas,
) -> (UnitArtifactV1, Option<OutputManifestEntryV1>) {
    let RootPageInputs {
        bundle_hash,
        job_id,
        plan_hash,
        tributes,
        nod_action_tributes,
        contributors_by_owner,
        limits,
    } = *inputs;
    let start = usize::try_from(index * 256).unwrap();
    let end = (start + 256).min(tributes.len());
    let actions = crate::test_support::nod_actions(
        &nod_action_tributes[start..end],
        u32::try_from(start).unwrap(),
    );
    let contributors = contributors_by_owner[start..end].to_vec();
    let chunk = ResultChunkV1 {
        protocol_bundle_hash: bundle_hash,
        job_id,
        attempt: 0,
        chunk_ordinal: index,
        first_nod_ordinal: u32::try_from(start).unwrap(),
        ordered_nod_actions: actions.clone(),
        ordered_eligible_contributors: contributors.clone(),
    };
    let chunk_hash = chunk.result_chunk_hash(limits).unwrap();
    let mut chunk_ref = cas
        .publish_bytes(&chunk.encode_canonical(limits).unwrap())
        .unwrap();
    chunk_ref.expected_ocb1_kind = Some(ObjectKind::ResultChunkV1.tag());
    let entry = OutputManifestEntryV1 {
        chunk_ordinal: index,
        result_chunk_hash: chunk_hash,
        result_chunk_ref: chunk_ref,
    };
    let summary = crate::test_support::root_summary_fixture(
        plan_hash,
        &chunk,
        &tributes[start..end],
        &entry,
        limits,
    );
    let coverage_root = summary.result_chunk_hashes.tree_root;
    let output_coverage_root = coverage_root;
    (
        crate::test_support::root_leaf_artifact(
            spec,
            summary,
            &entry,
            output_coverage_root,
            limits,
        ),
        Some(entry),
    )
}

#[derive(Debug, PartialEq, Eq)]
struct SnapshotEntry {
    mode: u32,
    len: u64,
    digest: Option<B256>,
    link: Option<PathBuf>,
}
fn snapshot(root: &Path) -> BTreeMap<PathBuf, SnapshotEntry> {
    fn visit(root: &Path, path: &Path, entries: &mut BTreeMap<PathBuf, SnapshotEntry>) {
        let m = fs::symlink_metadata(path).unwrap();
        entries.insert(
            path.strip_prefix(root).unwrap().to_path_buf(),
            SnapshotEntry {
                mode: m.mode(),
                len: m.len(),
                digest: if m.is_file() {
                    Some(alloy_primitives::keccak256(fs::read(path).unwrap()))
                } else {
                    None
                },
                link: if m.file_type().is_symlink() {
                    Some(fs::read_link(path).unwrap())
                } else {
                    None
                },
            },
        );
        if m.is_dir() {
            for child in fs::read_dir(path).unwrap() {
                visit(root, &child.unwrap().path(), entries);
            }
        }
    }
    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries);
    entries
}

fn audit_read_only(f: &Fixture) -> Result<(u32, u32), String> {
    let cas = FilesystemCasReader::open(&f.cas_root, CAS_LIMITS).map_err(|e| e.to_string())?;
    let inputs = VerifiedInputChunkRefCatalog::reopen(
        &f.input_ref_root,
        &cas,
        f.limits,
        poc_input_list_limits(),
    )
    .map_err(|e| e.to_string())?;
    let admissions = AdmissionCatalogReader::open_existing(&f.admission_root, &cas, f.limits)
        .map_err(|e| e.to_string())?;
    let audit = outbe_ocomp::lysis_plan_audit::open_read_only_local_plan_audit(
        &admissions,
        &inputs,
        &cas,
        &f.bundle,
        &f.limits,
    )
    .map_err(|e| e.to_string())?;
    let mut plan_complete = 0;
    for step in audit.audit_cursor().map_err(|e| e.to_string())? {
        if matches!(
            step.map_err(|e| e.to_string())?,
            LysisPlanAuditStepV1::Complete
        ) {
            plan_complete += 1;
        }
    }
    assert_eq!(plan_complete, 1);
    let mut chunks = 0;
    let mut results_complete = 0;
    for step in ExactLysisResultCatalogCursorV1::open(&audit).map_err(|e| e.to_string())? {
        match step.map_err(|e| e.to_string())? {
            LysisResultCatalogStepV1::Chunk(_) => chunks += 1,
            LysisResultCatalogStepV1::Complete => results_complete += 1,
            _ => {}
        }
    }
    Ok((chunks, results_complete))
}

#[test]
fn shared_admission_readers_hold_existing_lock_and_preserve_source() {
    let f = fixture(0x30);
    let cas = FilesystemCasReader::open(&f.cas_root, CAS_LIMITS).unwrap();
    let lock = f.admission_root.join("catalog.lock");
    fs::set_permissions(&lock, fs::Permissions::from_mode(0o400)).unwrap();
    let before = snapshot(f._directory.path());
    let first = AdmissionCatalogReader::open_existing(&f.admission_root, &cas, f.limits).unwrap();
    let second = AdmissionCatalogReader::open_existing(&f.admission_root, &cas, f.limits).unwrap();
    assert!(!first.is_abstained());
    let records = first
        .exact_plan_cursor()
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(records.len(), f.expected_count as usize);
    assert_eq!(first.read(0).unwrap(), records[0]);
    assert_eq!(second.read(0).unwrap(), records[0]);
    // Restore write access solely for the native writer's exclusion check.
    assert_eq!(snapshot(f._directory.path()), before);
    fs::set_permissions(&lock, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(matches!(
        VerifiedAdmissionCatalog::reopen(&f.admission_root, &cas, f.limits),
        Err(AdmissionCatalogError::LockHeld(_))
    ));
    drop(first);
    assert!(matches!(
        VerifiedAdmissionCatalog::reopen(&f.admission_root, &cas, f.limits),
        Err(AdmissionCatalogError::LockHeld(_))
    ));
    drop(second);
    let writer = VerifiedAdmissionCatalog::reopen(&f.admission_root, &cas, f.limits).unwrap();
    assert!(matches!(
        AdmissionCatalogReader::open_existing(&f.admission_root, &cas, f.limits),
        Err(AdmissionCatalogError::LockHeld(_))
    ));
    drop(writer);
    AdmissionCatalogReader::open_existing(&f.admission_root, &cas, f.limits).unwrap();
}

#[test]
fn concrete_read_only_bridge_completes_plan_and_results_without_mutation() {
    let f = fixture(0x30);
    let before = snapshot(f._directory.path());
    assert_eq!(audit_read_only(&f).unwrap(), (1, 1));
    assert_eq!(snapshot(f._directory.path()), before);
    let lock = f.input_ref_root.join("catalog.lock");
    fs::remove_file(&lock).unwrap();
    let before = snapshot(f._directory.path());
    assert!(audit_read_only(&f).is_err());
    assert!(!lock.exists());
    assert_eq!(snapshot(f._directory.path()), before);
}

#[test]
#[allow(unsafe_code)]
fn input_ref_reader_rejects_fifo_lock_with_bounded_wait() {
    const CHILD_ROOT: &str = "OUTBE_INPUT_REF_FIFO_TEST_ROOT";
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        let root = PathBuf::from(root);
        let cas = FilesystemCasReader::open(root.join("cas"), CAS_LIMITS).unwrap();
        assert!(matches!(
            VerifiedInputChunkRefCatalog::reopen(
                root.join("input-refs"),
                &cas,
                poc_schema_limits(),
                poc_input_list_limits(),
            ),
            Err(InputRefCatalogError::InvalidEnvelope)
        ));
        return;
    }

    let f = fixture(0x30);
    let lock = f.input_ref_root.join("catalog.lock");
    fs::remove_file(&lock).unwrap();
    let fifo_path = std::ffi::CString::new(lock.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: the NUL-terminated path remains alive for the entire call.
    assert_eq!(unsafe { libc::mkfifo(fifo_path.as_ptr(), 0o600) }, 0);
    let before = snapshot(f._directory.path());
    // A blocking open must fail this test without hanging the test runner.
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "admission::input_ref_reader_rejects_fifo_lock_with_bounded_wait",
            "--test-threads=1",
        ])
        .env(CHILD_ROOT, f._directory.path())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            break None;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert_eq!(snapshot(f._directory.path()), before);
    assert!(
        status.is_some_and(|status| status.success()),
        "read-only input-ref open blocked on FIFO or failed to reject it"
    );
}

#[test]
fn input_ref_reader_rejects_directory_lock_without_mutation() {
    let f = fixture(0x30);
    let cas = FilesystemCasReader::open(&f.cas_root, CAS_LIMITS).unwrap();
    let lock = f.input_ref_root.join("catalog.lock");
    fs::remove_file(&lock).unwrap();
    fs::create_dir(&lock).unwrap();
    fs::set_permissions(&lock, fs::Permissions::from_mode(0o500)).unwrap();
    let before = snapshot(f._directory.path());
    assert!(matches!(
        VerifiedInputChunkRefCatalog::reopen(
            &f.input_ref_root,
            &cas,
            f.limits,
            poc_input_list_limits(),
        ),
        Err(InputRefCatalogError::InvalidEnvelope)
    ));
    assert_eq!(snapshot(f._directory.path()), before);
}

#[test]
fn input_ref_reader_preserves_shared_and_exclusive_lock_behavior() {
    let f = fixture(0x30);
    let cas = FilesystemCasReader::open(&f.cas_root, CAS_LIMITS).unwrap();
    let lock = f.input_ref_root.join("catalog.lock");
    fs::set_permissions(&lock, fs::Permissions::from_mode(0o400)).unwrap();
    let before = snapshot(f._directory.path());
    let reopen = || {
        VerifiedInputChunkRefCatalog::reopen(
            &f.input_ref_root,
            &cas,
            f.limits,
            poc_input_list_limits(),
        )
    };
    let first = reopen().unwrap();
    let second = reopen().unwrap();
    let exclusive = fs::File::open(&lock).unwrap();
    assert!(exclusive.try_lock().is_err());
    drop(first);
    assert!(exclusive.try_lock().is_err());
    drop(second);
    exclusive.try_lock().unwrap();
    assert!(matches!(reopen(), Err(InputRefCatalogError::LockHeld(path)) if path == lock));
    exclusive.unlock().unwrap();
    drop(reopen().unwrap());
    assert_eq!(snapshot(f._directory.path()), before);
}

#[test]
fn admission_reader_missing_and_unsafe_inputs_fail_without_repair() {
    let f = fixture(0x30);
    let cas = FilesystemCasReader::open(&f.cas_root, CAS_LIMITS).unwrap();
    let missing = f._directory.path().join("absent");
    let before = snapshot(f._directory.path());
    assert!(AdmissionCatalogReader::open_existing(&missing, &cas, f.limits).is_err());
    assert_eq!(snapshot(f._directory.path()), before);
    let alias = f._directory.path().join("alias");
    symlink(&f.admission_root, &alias).unwrap();
    let before = snapshot(f._directory.path());
    assert!(AdmissionCatalogReader::open_existing(&alias, &cas, f.limits).is_err());
    assert_eq!(snapshot(f._directory.path()), before);
    fs::remove_file(alias).unwrap();
    for name in ["catalog.lock", "catalog.header"] {
        let path = f.admission_root.join(name);
        let backup = f._directory.path().join("backup");
        fs::rename(&path, &backup).unwrap();
        let before = snapshot(f._directory.path());
        assert!(AdmissionCatalogReader::open_existing(&f.admission_root, &cas, f.limits).is_err());
        assert!(!path.exists());
        assert_eq!(snapshot(f._directory.path()), before);
        symlink(&backup, &path).unwrap();
        let before = snapshot(f._directory.path());
        assert!(AdmissionCatalogReader::open_existing(&f.admission_root, &cas, f.limits).is_err());
        assert_eq!(snapshot(f._directory.path()), before);
        fs::remove_file(&path).unwrap();
        fs::rename(backup, path).unwrap();
    }
    let header = f.admission_root.join("catalog.header");
    let bytes = fs::read(&header).unwrap();
    fs::write(&header, b"corrupt").unwrap();
    let before = snapshot(f._directory.path());
    assert!(AdmissionCatalogReader::open_existing(&f.admission_root, &cas, f.limits).is_err());
    assert_eq!(snapshot(f._directory.path()), before);
    fs::write(header, bytes).unwrap();
    let temp = f.admission_root.join("0000000000.admission.tmp");
    fs::write(&temp, b"interrupted").unwrap();
    let before = snapshot(f._directory.path());
    assert!(matches!(
        AdmissionCatalogReader::open_existing(&f.admission_root, &cas, f.limits),
        Err(AdmissionCatalogError::AmbiguousTemporary(_))
    ));
    assert_eq!(snapshot(f._directory.path()), before);
    fs::remove_file(temp).unwrap();
    fs::write(f.admission_root.join("catalog.abstained"), b"latched").unwrap();
    let before = snapshot(f._directory.path());
    let reader = AdmissionCatalogReader::open_existing(&f.admission_root, &cas, f.limits).unwrap();
    assert!(reader.is_abstained());
    assert!(reader.read(0).is_err());
    assert!(reader.exact_plan_cursor().is_err());
    drop(reader);
    assert!(audit_read_only(&f).is_err());
    assert_eq!(snapshot(f._directory.path()), before);
}

#[test]
fn concrete_read_only_bridge_rejects_missing_duplicate_and_foreign_admissions() {
    let f = fixture(0x30);
    let foreign = fixture(0x40);
    let path = f.admission_root.join("0000000000.admission");
    let original = fs::read(&path).unwrap();
    fs::remove_file(&path).unwrap();
    let before = snapshot(f._directory.path());
    let cas = FilesystemCasReader::open(&f.cas_root, CAS_LIMITS).unwrap();
    let reader = AdmissionCatalogReader::open_existing(&f.admission_root, &cas, f.limits).unwrap();
    assert!(reader.read(0).is_err());
    assert!(reader.exact_plan_cursor().is_err());
    drop(reader);
    assert!(audit_read_only(&f).is_err());
    assert_eq!(snapshot(f._directory.path()), before);
    fs::write(&path, &original).unwrap();
    let duplicate = f
        .admission_root
        .join(format!("{:010}.admission", f.expected_count));
    fs::write(&duplicate, &original).unwrap();
    let before = snapshot(f._directory.path());
    let reader = AdmissionCatalogReader::open_existing(&f.admission_root, &cas, f.limits).unwrap();
    assert!(reader.exact_plan_cursor().is_err());
    drop(reader);
    assert!(audit_read_only(&f).is_err());
    assert_eq!(snapshot(f._directory.path()), before);
    fs::remove_file(duplicate).unwrap();
    fs::copy(foreign.admission_root.join("0000000000.admission"), &path).unwrap();
    let before = snapshot(f._directory.path());
    let reader = AdmissionCatalogReader::open_existing(&f.admission_root, &cas, f.limits).unwrap();
    assert!(reader.exact_plan_cursor().unwrap().next().unwrap().is_err());
    drop(reader);
    assert!(audit_read_only(&f).is_err());
    assert_eq!(snapshot(f._directory.path()), before);
}

#[test]
#[allow(unsafe_code)]
fn admission_reader_rejects_fifo_lock_with_bounded_wait() {
    const CHILD_ROOT: &str = "OUTBE_ADMISSION_FIFO_TEST_ROOT";
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        let root = PathBuf::from(root);
        let cas = FilesystemCasReader::open(root.join("cas"), CAS_LIMITS).unwrap();
        assert!(matches!(
            AdmissionCatalogReader::open_existing(
                root.join("admissions"),
                &cas,
                poc_schema_limits(),
            ),
            Err(AdmissionCatalogError::InvalidEnvelope)
        ));
        return;
    }

    let f = fixture(0x30);
    let lock = f.admission_root.join("catalog.lock");
    fs::remove_file(&lock).unwrap();
    let fifo_path = std::ffi::CString::new(lock.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: the NUL-terminated path remains alive for the entire call.
    assert_eq!(unsafe { libc::mkfifo(fifo_path.as_ptr(), 0o600) }, 0);
    let before = snapshot(f._directory.path());
    // Isolate the potentially blocking open so a regression cannot hang the
    // test suite. The parent always kills and reaps a timed-out child.
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "admission::admission_reader_rejects_fifo_lock_with_bounded_wait",
            "--test-threads=1",
        ])
        .env(CHILD_ROOT, f._directory.path())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            break None;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert_eq!(snapshot(f._directory.path()), before);
    assert!(
        status.is_some_and(|status| status.success()),
        "read-only admission open blocked on FIFO or failed to reject it"
    );
}

#[test]
fn admission_reader_rejects_directory_lock_without_mutation() {
    let f = fixture(0x30);
    let cas = FilesystemCasReader::open(&f.cas_root, CAS_LIMITS).unwrap();
    let lock = f.admission_root.join("catalog.lock");
    fs::remove_file(&lock).unwrap();
    fs::create_dir(&lock).unwrap();
    fs::set_permissions(&lock, fs::Permissions::from_mode(0o500)).unwrap();
    let before = snapshot(f._directory.path());
    assert!(matches!(
        AdmissionCatalogReader::open_existing(&f.admission_root, &cas, f.limits),
        Err(AdmissionCatalogError::InvalidEnvelope)
    ));
    assert_eq!(snapshot(f._directory.path()), before);
}
