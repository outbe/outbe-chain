use super::*;
use crate::snapshot::validation::{
    ocomp::verify_ocomp_relations,
    report::{CheckName, ValidationReport},
};
use outbe_ocomp::cas::{CasLimits, CasWriterRole, FilesystemCas};
use std::fs;

const LIMITS: CasLimits = CasLimits {
    max_object_bytes: 1_048_576,
    max_total_bytes: u64::MAX,
};

#[test]
fn empty_native_join_preserves_independent_frontiers_and_creates_no_job_stores() {
    for version in [1, 2] {
        with_canonical_frontiers(
            version,
            |_| {},
            |_| queued_owner(0),
            |state, source, layout, scratch| {
                let before = crate::snapshot::tests::headers::fingerprint(&layout.ocomp_root);
                let mut report = ValidationReport::new([CheckName::Ocomp]);
                verify_ocomp_relations(state, source, layout, scratch, &mut report).unwrap();
                assert_eq!(report.observed.p.as_ref().unwrap().number, 100);
                assert_eq!(report.observed.c_current.as_ref().unwrap().number, 100);
                assert!(report.required_missing.is_empty());
                assert!(report.active_ocomp.is_empty());
                assert_eq!(
                    crate::snapshot::tests::headers::fingerprint(&layout.ocomp_root),
                    before
                );
                for absent in [
                    "cas-v1",
                    "supervisor-v1/jobs",
                    "exporter-v1/receipts",
                    "exporter-v1/input-refs",
                    "supervisor-v1/export-bindings",
                ] {
                    assert!(!layout.ocomp_root.join(absent).exists());
                }
            },
        );
    }
}

#[test]
fn exporter_work_directory_is_not_a_public_input_catalog() {
    for populated in [false, true] {
        with_canonical_frontiers(
            2,
            |layout| {
                let root = layout.ocomp_root.join("exporter-v1/input-refs");
                let job = hex::encode(B256::repeat_byte(71));
                fs::create_dir_all(root.join(&job)).unwrap();
                fs::create_dir_all(root.join(".work")).unwrap();
                if populated {
                    // The exporter retains its working inventory/openings after export.
                    let working = root.join(".work").join(&job);
                    fs::create_dir_all(working.join("inventory")).unwrap();
                    fs::create_dir_all(working.join("openings")).unwrap();
                    fs::write(working.join("inventory/tributes.spool"), b"working bytes").unwrap();
                    fs::write(
                        working.join("openings/opening-stage.complete"),
                        b"stage bytes",
                    )
                    .unwrap();
                }
            },
            |_| queued_owner(0),
            |state, source, layout, scratch| {
                let before = crate::snapshot::tests::headers::fingerprint(&layout.ocomp_root);
                let mut report = ValidationReport::new([CheckName::Ocomp]);
                verify_ocomp_relations(state, source, layout, scratch, &mut report).unwrap();
                let inputs = report
                    .inventory_bounds
                    .iter()
                    .find(|bound| bound.name == "input_job_directories")
                    .unwrap();
                assert_eq!(inputs.visited, 1, ".work is not a public JobId");
                assert_eq!(
                    crate::snapshot::tests::headers::fingerprint(&layout.ocomp_root),
                    before
                );
            },
        );
    }
}

#[test]
fn every_present_population_is_enumerated_even_without_live_canonical_jobs() {
    for prefix in [
        "exporter-v1/receipts",
        "supervisor-v1/export-bindings",
        "exporter-v1/input-refs",
        "supervisor-v1/jobs",
        "supervisor-v1/materialization-references",
    ] {
        with_canonical_frontiers(
            2,
            |layout| {
                fs::create_dir_all(layout.ocomp_root.join(prefix).join("not-a-native-job"))
                    .unwrap();
            },
            |_| queued_owner(0),
            |state, source, layout, scratch| {
                let mut report = ValidationReport::new([CheckName::Ocomp]);
                let error = verify_ocomp_relations(state, source, layout, scratch, &mut report)
                    .unwrap_err();
                assert!(
                    error.downcast_ref::<Incomplete>().is_none(),
                    "{prefix}: {error:#}"
                );
                assert_eq!(report.observed.p.as_ref().unwrap().number, 100);
            },
        );
    }
}

#[test]
fn empty_historical_job_and_nested_reference_directories_do_not_require_old_evidence() {
    with_canonical_frontiers(
        2,
        |layout| {
            let job = hex::encode(B256::repeat_byte(71));
            for prefix in [
                "exporter-v1/receipts",
                "supervisor-v1/export-bindings",
                "exporter-v1/input-refs",
                "supervisor-v1/jobs",
            ] {
                fs::create_dir_all(layout.ocomp_root.join(prefix).join(&job)).unwrap();
            }
            fs::create_dir_all(
                layout
                    .ocomp_root
                    .join("supervisor-v1/materialization-references")
                    .join(&job)
                    .join("0"),
            )
            .unwrap();
        },
        |_| queued_owner(0),
        |state, source, layout, scratch| {
            let mut report = ValidationReport::new([CheckName::Ocomp]);
            verify_ocomp_relations(state, source, layout, scratch, &mut report).unwrap();
        },
    );
}

#[test]
fn full_join_runs_orphan_cas_check_after_canonical_success() {
    with_canonical_frontiers(
        2,
        |layout| {
            let cas = FilesystemCas::open(
                layout.ocomp_root.join("cas-v1"),
                CasWriterRole::Supervisor,
                LIMITS,
            )
            .unwrap();
            let reference = cas.publish_bytes(b"native orphan bytes").unwrap();
            drop(cas);
            let digest = hex::encode(reference.transport_digest);
            fs::write(
                layout
                    .ocomp_root
                    .join("cas-v1/objects")
                    .join(&digest[..2])
                    .join(&digest[2..]),
                b"native broken bytes",
            )
            .unwrap();
        },
        |_| queued_owner(0),
        |state, source, layout, scratch| {
            let mut report = ValidationReport::new([CheckName::Ocomp]);
            let error =
                verify_ocomp_relations(state, source, layout, scratch, &mut report).unwrap_err();
            assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
            assert_eq!(report.observed.c_current.as_ref().unwrap().number, 100);
        },
    );
}

#[test]
fn nested_materialization_reference_with_unavailable_job_evidence_is_incomplete() {
    use outbe_ocomp::nod_materialization::MaterializationReferenceStoreV1;
    with_canonical_frontiers(
        2,
        |layout| {
            let cas = FilesystemCas::open(
                layout.ocomp_root.join("cas-v1"),
                CasWriterRole::Supervisor,
                LIMITS,
            )
            .unwrap();
            let reference = cas.publish_bytes(b"retained dependency").unwrap();
            let job = B256::repeat_byte(72);
            let path = layout
                .ocomp_root
                .join("supervisor-v1/materialization-references")
                .join(hex::encode(job))
                .join("9");
            MaterializationReferenceStoreV1::open(path)
                .unwrap()
                .pin_exact(job, &[reference])
                .unwrap();
        },
        |_| queued_owner(0),
        |state, source, layout, scratch| {
            let mut report = ValidationReport::new([CheckName::Ocomp]);
            let error =
                verify_ocomp_relations(state, source, layout, scratch, &mut report).unwrap_err();
            assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
            assert!(format!("{error:#}").contains("job"));
        },
    );
}
