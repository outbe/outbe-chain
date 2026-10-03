//! availability regression scenarios.
use super::*;

#[test]
fn canonical_two_job_queue_checks_every_remaining_batch_without_historical_exports() {
    let public = tempfile::tempdir().unwrap();
    let first = fixture(public.path(), 0x30, WorldwideDay::new(20_260_725), 10);
    let second = fixture(public.path(), 0x40, WorldwideDay::new(20_260_726), 10);
    assert!(!public.path().join("node-v1").exists());
    assert!(!public.path().join("exporter-v1/receipts").exists());
    assert!(!public.path().join("supervisor-v1/export-bindings").exists());
    let before = fingerprint(public.path());
    with_owner_storage(
        2,
        canonical_owner(&[(&first, 0), (&second, 0)], None),
        |state, source| {
            let scratch = tempfile::tempdir().unwrap();
            let inventory =
                CanonicalInventory::scan(state, scratch.path(), &source.protected, None).unwrap();
            let audit = inventory
                .verify_nod_inputs(public.path(), CAS_LIMITS, 3, None)
                .unwrap();
            assert_eq!((audit.jobs, audit.batches, audit.actions), (2, 4, 20));
        },
    );
    assert_eq!(fingerprint(public.path()), before);
}

#[test]
fn missing_entire_second_job_is_incomplete_even_when_head_files_are_complete() {
    let public = tempfile::tempdir().unwrap();
    let first = fixture(public.path(), 0x30, WorldwideDay::new(20_260_725), 10);
    let second = fixture(public.path(), 0x40, WorldwideDay::new(20_260_726), 10);
    fs::remove_dir_all(
        public
            .path()
            .join("supervisor-v1/jobs")
            .join(hex::encode(second.job_id)),
    )
    .unwrap();
    let before = fingerprint(public.path());
    with_owner_storage(
        2,
        canonical_owner(&[(&first, 0), (&second, 0)], None),
        |state, source| {
            let scratch = tempfile::tempdir().unwrap();
            let inventory =
                CanonicalInventory::scan(state, scratch.path(), &source.protected, None).unwrap();
            let error = inventory
                .verify_nod_inputs(public.path(), CAS_LIMITS, 3, None)
                .expect_err("later job cannot be skipped");
            assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
        },
    );
    assert_eq!(fingerprint(public.path()), before);
}

#[test]
fn first_batch_success_does_not_hide_missing_later_required_result_chunk() {
    let public = tempfile::tempdir().unwrap();
    let f = fixture(public.path(), 0x30, WorldwideDay::new(20_260_725), 257);
    fs::remove_file(object_path(&f, 1)).unwrap();
    // Addressable native first batch only needs the later producer artifact,
    // not the deleted later ResultChunk. Later traversal must find the gap.
    assert_eq!(first_native_batch(public.path(), &f), 8);
    let before = fingerprint(public.path());
    with_owner_storage(2, canonical_owner(&[(&f, 0)], None), |state, source| {
        let scratch = tempfile::tempdir().unwrap();
        let inventory =
            CanonicalInventory::scan(state, scratch.path(), &source.protected, None).unwrap();
        let error = inventory
            .verify_nod_inputs(public.path(), CAS_LIMITS, 3, None)
            .expect_err("all remaining batches required");
        assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
    });
    assert_eq!(fingerprint(public.path()), before);
}

#[test]
fn nonzero_start_uses_addressable_native_proofs_without_consumed_result_chunk() {
    let public = tempfile::tempdir().unwrap();
    let f = fixture(public.path(), 0x30, WorldwideDay::new(20_260_725), 257);
    fs::remove_file(object_path(&f, 0)).unwrap();
    let before = fingerprint(public.path());
    with_owner_storage(1, canonical_owner(&[(&f, 256)], None), |state, source| {
        let scratch = tempfile::tempdir().unwrap();
        let inventory =
            CanonicalInventory::scan(state, scratch.path(), &source.protected, None).unwrap();
        let audit = inventory
            .verify_nod_inputs(public.path(), CAS_LIMITS, 3, None)
            .unwrap();
        assert_eq!((audit.jobs, audit.batches, audit.actions), (1, 1, 1));
    });
    assert_eq!(fingerprint(public.path()), before);
}

#[test]
fn canonical_root_job_and_bundle_mismatches_are_failed_not_missing() {
    for damage in ["root", "job", "bundle"] {
        let public = tempfile::tempdir().unwrap();
        let f = fixture(public.path(), 0x30, WorldwideDay::new(20_260_725), 10);
        if damage == "job" {
            // Put the foreign native files at the required path: existence
            // cannot turn their different plan JobId into canonical authority.
            for base in ["supervisor-v1/jobs", "exporter-v1/input-refs"] {
                fs::rename(
                    public.path().join(base).join(hex::encode(f.job_id)),
                    public.path().join(base).join(hex::encode(hash(0x99))),
                )
                .unwrap();
            }
        } else if damage == "bundle" {
            fs::copy(
                public
                    .path()
                    .join("protocol-bundles-v1")
                    .join(format!("{}.ocb1", hex::encode(f.bundle.hash()))),
                public
                    .path()
                    .join("protocol-bundles-v1")
                    .join(format!("{}.ocb1", hex::encode(hash(0x99)))),
            )
            .unwrap();
        }
        let before = fingerprint(public.path());
        with_owner_storage(
            2,
            canonical_owner(&[(&f, 0)], Some(damage)),
            |state, source| {
                let scratch = tempfile::tempdir().unwrap();
                let inventory =
                    CanonicalInventory::scan(state, scratch.path(), &source.protected, None)
                        .unwrap();
                let error = inventory
                    .verify_nod_inputs(public.path(), CAS_LIMITS, 3, None)
                    .expect_err("canonical authority mismatch");
                assert!(
                    error.downcast_ref::<Incomplete>().is_none(),
                    "{damage}: {error:#}"
                );
            },
        );
        assert_eq!(fingerprint(public.path()), before);
    }
}

#[test]
fn corrupt_required_cas_bytes_fail_without_becoming_missing_input() {
    let public = tempfile::tempdir().unwrap();
    let f = fixture(public.path(), 0x30, WorldwideDay::new(20_260_725), 10);
    let path = object_path(&f, 0);
    let mut bytes = fs::read(&path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    fs::write(path, bytes).unwrap();
    let before = fingerprint(public.path());
    with_owner_storage(2, canonical_owner(&[(&f, 0)], None), |state, source| {
        let scratch = tempfile::tempdir().unwrap();
        let inventory =
            CanonicalInventory::scan(state, scratch.path(), &source.protected, None).unwrap();
        let error = inventory
            .verify_nod_inputs(public.path(), CAS_LIMITS, 3, None)
            .expect_err("CAS digest corruption");
        assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
    });
    assert_eq!(fingerprint(public.path()), before);
}

#[test]
fn missing_middle_native_admission_is_incomplete_through_transparent_plan_error() {
    use outbe_ocomp::{
        admission_catalog::AdmissionCatalogError, lysis_plan_audit::ExactLysisPlanError,
        nod_materialization::NodMaterializationBuildErrorV1,
    };

    let public = tempfile::tempdir().unwrap();
    let f = fixture(public.path(), 0x30, WorldwideDay::new(20_260_725), 257);
    let topology = LysisPlanTopologyV1::new(2).unwrap();
    let missing_ordinal = topology
        .plan_ordinal_of(PlannedUnitPositionV1::TreeNode {
            phase: UnitPhase::RootReduce,
            level: 0,
            index: 1,
        })
        .unwrap();
    // The second leaf is a proof sibling of the first batch, with the final
    // reducer and all CAS objects still present. Only its admission is absent.
    assert!(missing_ordinal > 0 && missing_ordinal + 1 < topology.total_unit_count());
    fs::remove_file(
        public
            .path()
            .join("supervisor-v1/jobs")
            .join(hex::encode(f.job_id))
            .join("admissions")
            .join(format!("{missing_ordinal:010}.admission")),
    )
    .unwrap();
    let before = fingerprint(public.path());
    with_owner_storage(2, canonical_owner(&[(&f, 0)], None), |state, source| {
        let scratch = tempfile::tempdir().unwrap();
        let inventory =
            CanonicalInventory::scan(state, scratch.path(), &source.protected, None).unwrap();
        let error = inventory
            .verify_nod_inputs(public.path(), CAS_LIMITS, 3, None)
            .expect_err("missing proof-sibling admission must be incomplete");
        assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
        assert!(
            matches!(
                error.downcast_ref::<NodMaterializationBuildErrorV1>(),
                Some(NodMaterializationBuildErrorV1::Plan(
                    ExactLysisPlanError::Admission(AdmissionCatalogError::MissingAdmission {
                        plan_ordinal
                    })
                )) if *plan_ordinal == missing_ordinal
            ),
            "retain the native transparent missing-record error: {error:#}"
        );
    });
    assert_eq!(fingerprint(public.path()), before);
}

#[test]
fn absent_catalog_bundle_uses_valid_hash_pinned_fallback() {
    let public = tempfile::tempdir().unwrap();
    let f = fixture(public.path(), 0x30, WorldwideDay::new(20_260_725), 10);
    let catalog = public
        .path()
        .join("protocol-bundles-v1")
        .join(format!("{}.ocb1", hex::encode(f.bundle.hash())));
    fs::rename(&catalog, public.path().join("protocol-bundle-v1.ocb1")).unwrap();
    let before = fingerprint(public.path());
    with_owner_storage(2, canonical_owner(&[(&f, 0)], None), |state, source| {
        let scratch = tempfile::tempdir().unwrap();
        let inventory =
            CanonicalInventory::scan(state, scratch.path(), &source.protected, None).unwrap();
        let audit = inventory
            .verify_nod_inputs(public.path(), CAS_LIMITS, 3, None)
            .unwrap();
        assert_eq!((audit.jobs, audit.batches, audit.actions), (1, 2, 10));
    });
    assert!(!catalog.exists());
    assert_eq!(fingerprint(public.path()), before);
}
