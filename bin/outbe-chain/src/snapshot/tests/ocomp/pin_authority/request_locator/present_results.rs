use super::super::local_result::{authority, result_for, write_result};
use super::*;
use crate::snapshot::{
    tests::headers::fingerprint, validation::ocomp::verify_present_local_results,
};
use std::{fs, path::Path};

fn fixture(
    version: u32,
    backend: Receipts,
    case: Case,
    completed: bool,
    inspect: impl FnOnce(
        &crate::snapshot::validation::canonical_state::CanonicalState<'_>,
        &crate::snapshot::native::RethReadOnlyView,
        OcompJobRecordV1,
    ),
) {
    with_prepared_owner_storage_setup(
        version,
        400,
        |layout| setup_frame(layout, backend, case),
        |request| {
            let job = authority(request, completed);
            let limits = poc_schema_limits();
            stored_job(
                job.intent.intent_id(&limits).unwrap(),
                &job.encode_canonical(&limits).unwrap(),
            )
        },
        |state, view| {
            inspect(
                state,
                view,
                authority(&view.header(B).unwrap().unwrap(), completed),
            );
        },
    );
}

fn inspect(
    state: &crate::snapshot::validation::canonical_state::CanonicalState<'_>,
    view: &crate::snapshot::native::RethReadOnlyView,
    root: &Path,
    maximum: Option<u64>,
) -> eyre::Result<(u64, u64)> {
    let before = fingerprint(root);
    let mut visited = 0;
    let result = verify_present_local_results(state, view, root, maximum, &mut |job, audit| {
        assert_eq!(job.finalized.as_ref().unwrap().job_id, audit.result.job_id);
        visited += 1;
        Ok(())
    });
    assert_eq!(fingerprint(root), before);
    result.map(|audit| {
        assert_eq!(audit.results, visited);
        (audit.results, audit.terminal_digests)
    })
}

#[test]
fn bare_results_use_exact_request_frames_without_retired_export_or_spool() {
    for version in [1, 2] {
        for backend in [Receipts::Mdbx, Receipts::Static] {
            for completed in [false, true] {
                fixture(
                    version,
                    backend,
                    Case::Valid,
                    completed,
                    |state, view, job| {
                        let root = tempfile::tempdir().unwrap();
                        write_result(root.path(), &result_for(&job));
                        assert_eq!(
                            inspect(state, view, root.path(), None).unwrap(),
                            (1, u64::from(completed))
                        );
                        assert!(!root.path().join("exporter-v1").exists());
                        assert!(!root.path().join("supervisor-v1").exists());
                    },
                );
            }
        }
    }
}

#[test]
fn absent_optional_results_do_not_create_a_store_or_spend_budget() {
    fixture(1, Receipts::Mdbx, Case::Valid, true, |state, view, _| {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(inspect(state, view, root.path(), Some(0)).unwrap(), (0, 0));
        assert!(!root.path().join("node-v1").exists());
        fs::create_dir(root.path().join("node-v1")).unwrap();
        drop(
            outbe_node::ocomp::local_result::LocalLysisResultStore::open(
                root.path().join("node-v1/local-results"),
                poc_schema_limits(),
            )
            .unwrap(),
        );
        assert_eq!(inspect(state, view, root.path(), Some(0)).unwrap(), (0, 0));
    });
}

#[test]
fn result_inventory_budget_and_missing_request_receipt_are_incomplete() {
    for (case, budget) in [(Case::Valid, Some(0)), (Case::MissingReceipt, None)] {
        fixture(2, Receipts::Mdbx, case, true, |state, view, job| {
            let root = tempfile::tempdir().unwrap();
            write_result(root.path(), &result_for(&job));
            let error = inspect(state, view, root.path(), budget).unwrap_err();
            assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
        });
    }
}

#[test]
fn semantically_valid_changed_result_still_fails_canonical_terminal_digest() {
    fixture(1, Receipts::Mdbx, Case::Valid, true, |state, view, job| {
        let root = tempfile::tempdir().unwrap();
        let mut result = result_for(&job);
        result.input_manifest_hash = B256::repeat_byte(0xea);
        super::super::local_result::refresh_arithmetic(&mut result);
        write_result(root.path(), &result);
        let error = inspect(state, view, root.path(), None).unwrap_err();
        assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
    });
}

#[test]
fn every_present_result_is_examined_even_without_canonical_active_jobs() {
    fixture(1, Receipts::Mdbx, Case::Valid, true, |state, view, job| {
        assert!(state.live_ocomp_jobs().unwrap().is_empty());
        let root = tempfile::tempdir().unwrap();
        write_result(root.path(), &result_for(&job));
        let mut foreign = result_for(&job);
        foreign.job_id = B256::repeat_byte(0xed);
        super::super::local_result::refresh_arithmetic(&mut foreign);
        write_result(root.path(), &foreign);
        let error = inspect(state, view, root.path(), None).unwrap_err();
        assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
    });
}
