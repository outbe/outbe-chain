use super::*;
use crate::snapshot::validation::ocomp::ReferenceMembership;
use outbe_snapshot::layout::ProtectedPaths;

fn populate(root: &Path, f: &Fixture, expected: &Expected, members: &ReferenceMembership) {
    let counts = run(root, expected, None, &mut |reference| {
        members.insert(f.job_id, reference)
    })
    .unwrap();
    assert_eq!(counts.0, counts.1);
    assert!(counts.2 > 0);
}

fn rejected(result: eyre::Result<()>, incomplete: bool) {
    let error = result.unwrap_err();
    assert_eq!(
        error.downcast_ref::<Incomplete>().is_some(),
        incomplete,
        "{error:#}"
    );
}

#[test]
fn native_artifact_and_result_members_verify_without_source_writes_and_cleanup_on_drop() {
    let root = tempfile::tempdir().unwrap();
    let f = fixture(root.path(), 0x30, WorldwideDay::new(20_260_725), 10);
    let expected = expected(root.path(), &f);
    let before = fingerprint(root.path());
    let scratch = tempfile::tempdir().unwrap();
    let scratch_before = fingerprint(scratch.path());
    {
        let members = ReferenceMembership::create(
            scratch.path(),
            &ProtectedPaths(vec![root.path().to_path_buf()]),
        )
        .unwrap();
        populate(root.path(), &f, &expected, &members);
        let cas = FilesystemCasReader::open(&f.cas_root, CAS_LIMITS).unwrap();
        for reference in references(&expected.records) {
            members.verify(f.job_id, &reference, &cas, true).unwrap();
            members.verify(f.job_id, &reference, &cas, false).unwrap();
        }
        // Explicitly exercise the independently stored result object too.
        members
            .verify(f.job_id, &f.result_chunk_refs[0], &cas, true)
            .unwrap();
        assert_eq!(fingerprint(root.path()), before);
    }
    assert_eq!(fingerprint(scratch.path()), scratch_before);
    assert_eq!(fingerprint(root.path()), before);
}

#[test]
fn valid_cas_foreign_job_and_foreign_artifact_need_exact_membership() {
    let root = tempfile::tempdir().unwrap();
    let f = fixture(root.path(), 0x30, WorldwideDay::new(20_260_725), 10);
    let foreign = fixture(root.path(), 0x40, WorldwideDay::new(20_260_726), 10);
    let expected = expected(root.path(), &f);
    let foreign_expected = super::expected(root.path(), &foreign);
    let own_ref = &expected.records[0].artifact_ref;
    let foreign_ref = &foreign_expected.records[0].artifact_ref;
    assert_ne!(own_ref.transport_digest, foreign_ref.transport_digest);
    let before = fingerprint(root.path());
    let scratch = tempfile::tempdir().unwrap();
    let members = ReferenceMembership::create(
        scratch.path(),
        &ProtectedPaths(vec![root.path().to_path_buf()]),
    )
    .unwrap();
    populate(root.path(), &f, &expected, &members);
    let cas = FilesystemCasReader::open(&f.cas_root, CAS_LIMITS).unwrap();
    for complete in [false, true] {
        rejected(
            members.verify(foreign.job_id, own_ref, &cas, complete),
            !complete,
        );
        rejected(
            members.verify(f.job_id, foreign_ref, &cas, complete),
            !complete,
        );
    }
    // The second job's native callback establishes its own membership.
    populate(root.path(), &foreign, &foreign_expected, &members);
    members
        .verify(foreign.job_id, foreign_ref, &cas, true)
        .unwrap();
    members.verify(f.job_id, own_ref, &cas, true).unwrap();
    rejected(members.verify(foreign.job_id, own_ref, &cas, true), false);
    rejected(members.verify(f.job_id, foreign_ref, &cas, true), false);
    assert_eq!(fingerprint(root.path()), before);
}

#[test]
fn changed_length_and_wrong_kind_fail_even_when_evidence_is_partial() {
    let root = tempfile::tempdir().unwrap();
    let f = fixture(root.path(), 0x30, WorldwideDay::new(20_260_725), 10);
    let expected = expected(root.path(), &f);
    let before = fingerprint(root.path());
    let scratch = tempfile::tempdir().unwrap();
    let members = ReferenceMembership::create(
        scratch.path(),
        &ProtectedPaths(vec![root.path().to_path_buf()]),
    )
    .unwrap();
    populate(root.path(), &f, &expected, &members);
    let cas = FilesystemCasReader::open(&f.cas_root, CAS_LIMITS).unwrap();
    let original = &expected.records[0].artifact_ref;
    let mut wrong_length = original.clone();
    wrong_length.encoded_bytes += 1;
    let mut wrong_kind = original.clone();
    wrong_kind.expected_ocb1_kind = f.result_chunk_refs[0].expected_ocb1_kind;
    assert_ne!(wrong_kind.expected_ocb1_kind, original.expected_ocb1_kind);
    for complete in [false, true] {
        rejected(
            members.verify(f.job_id, &wrong_length, &cas, complete),
            false,
        );
        rejected(members.verify(f.job_id, &wrong_kind, &cas, complete), false);
    }
    assert_eq!(fingerprint(root.path()), before);
}

#[test]
fn untyped_reference_does_not_alias_a_typed_member() {
    let root = tempfile::tempdir().unwrap();
    let f = fixture(root.path(), 0x30, WorldwideDay::new(20_260_725), 10);
    let expected = expected(root.path(), &f);
    let before = fingerprint(root.path());
    let scratch = tempfile::tempdir().unwrap();
    let members = ReferenceMembership::create(
        scratch.path(),
        &ProtectedPaths(vec![root.path().to_path_buf()]),
    )
    .unwrap();
    populate(root.path(), &f, &expected, &members);
    let cas = FilesystemCasReader::open(&f.cas_root, CAS_LIMITS).unwrap();
    let mut untyped = expected.records[0].artifact_ref.clone();
    assert!(untyped.expected_ocb1_kind.is_some());
    untyped.expected_ocb1_kind = None;
    rejected(members.verify(f.job_id, &untyped, &cas, true), false);
    rejected(members.verify(f.job_id, &untyped, &cas, false), true);
    assert_eq!(fingerprint(root.path()), before);
}

#[test]
fn partial_native_admissions_allow_present_members_but_leave_valid_unmatched_refs_incomplete() {
    let root = tempfile::tempdir().unwrap();
    let f = fixture(root.path(), 0x30, WorldwideDay::new(20_260_725), 10);
    let expected = expected(root.path(), &f);
    let first = expected
        .topology
        .plan_ordinal_of(PlannedUnitPositionV1::Primary {
            phase: UnitPhase::Enumerate,
            ordinal: 0,
        })
        .unwrap();
    keep_only(root.path(), &f, &expected, &[first]);
    let absent = expected
        .records
        .iter()
        .find(|record| record.artifact_ref != expected.records[first as usize].artifact_ref)
        .unwrap()
        .artifact_ref
        .clone();
    let before = fingerprint(root.path());
    let scratch = tempfile::tempdir().unwrap();
    let members = ReferenceMembership::create(
        scratch.path(),
        &ProtectedPaths(vec![root.path().to_path_buf()]),
    )
    .unwrap();
    let counts = run(root.path(), &expected, None, &mut |reference| {
        members.insert(f.job_id, reference)
    })
    .unwrap();
    assert_eq!(counts, (expected.topology.total_unit_count(), 1, 0));
    let cas = FilesystemCasReader::open(&f.cas_root, CAS_LIMITS).unwrap();
    members
        .verify(
            f.job_id,
            &expected.records[first as usize].artifact_ref,
            &cas,
            false,
        )
        .unwrap();
    rejected(members.verify(f.job_id, &absent, &cas, false), true);
    rejected(
        members.verify(f.job_id, &f.result_chunk_refs[0], &cas, false),
        true,
    );
    assert_eq!(fingerprint(root.path()), before);
}

#[test]
fn membership_does_not_substitute_for_current_cas_bytes_and_error_paths_cleanup() {
    for missing in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let f = fixture(root.path(), 0x30, WorldwideDay::new(20_260_725), 10);
        let expected = expected(root.path(), &f);
        let scratch = tempfile::tempdir().unwrap();
        let scratch_before = fingerprint(scratch.path());
        let members = ReferenceMembership::create(
            scratch.path(),
            &ProtectedPaths(vec![root.path().to_path_buf()]),
        )
        .unwrap();
        populate(root.path(), &f, &expected, &members);
        let reference = &expected.records[0].artifact_ref;
        let path = cas_path(&f, reference);
        if missing {
            fs::remove_file(path).unwrap();
        } else {
            let mut bytes = fs::read(&path).unwrap();
            *bytes.last_mut().unwrap() ^= 1;
            fs::write(path, bytes).unwrap();
        }
        // Mutation is test setup. Verification must not repair or alter it.
        let before = fingerprint(root.path());
        let cas = FilesystemCasReader::open(&f.cas_root, CAS_LIMITS).unwrap();
        for complete in [false, true] {
            rejected(members.verify(f.job_id, reference, &cas, complete), missing);
        }
        drop(members);
        assert_eq!(fingerprint(scratch.path()), scratch_before);
        assert_eq!(fingerprint(root.path()), before);
    }
}

#[test]
fn protected_scratch_equal_nested_or_ancestor_is_rejected_before_writes() {
    let root = tempfile::tempdir().unwrap();
    let f = fixture(root.path(), 0x30, WorldwideDay::new(20_260_725), 10);
    let before = fingerprint(root.path());
    let protected = ProtectedPaths(vec![root.path().to_path_buf()]);
    assert!(ReferenceMembership::create(root.path(), &protected).is_err());
    let absent = root.path().join("must-not-be-created");
    assert!(ReferenceMembership::create(&absent, &protected).is_err());
    assert!(!absent.exists());
    // Protect a contained native CAS directory, so the candidate parent
    // overlaps it in the opposite direction as well.
    assert!(ReferenceMembership::create(root.path(), &ProtectedPaths(vec![f.cas_root]),).is_err());
    assert_eq!(fingerprint(root.path()), before);
}
