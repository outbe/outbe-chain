use super::*;
use crate::snapshot::{
    tests::headers::fingerprint,
    validation::{ocomp::verify_present_discovery, Incomplete},
};
use outbe_ocomp::discovery_spool::{DiscoverySpoolRecordV1, DiscoverySpoolV1};
use outbe_ocomp_protocol::{
    common::BoundedBytes,
    control::{FinalizedJobSpecV1, FinalizedJobSummaryV1},
};
use std::{
    fs,
    path::{Path, PathBuf},
};

fn spec(job: &OcompJobRecordV1) -> FinalizedJobSpecV1 {
    let finality = job.finalized.as_ref().unwrap();
    FinalizedJobSpecV1 {
        summary: FinalizedJobSummaryV1 {
            cursor: job.intent_height,
            job_id: finality.job_id,
            intent_id: job.intent.intent_id(&poc_schema_limits()).unwrap(),
            finalized_block_hash: finality.finalized_request_block_hash,
            finalized_state_root: finality.finalized_request_state_root,
            protocol_bundle_hash: job.intent.protocol_bundle_hash,
            open_height: finality.open_height,
            deadline_height: finality.deadline_height,
        },
        canonical_job_intent: BoundedBytes(
            job.intent.encode_canonical(&poc_schema_limits()).unwrap(),
        ),
    }
}

fn write(root: &Path, view: &RethReadOnlyView, spec: &FinalizedJobSpecV1) -> PathBuf {
    let parent = root.join("exporter-v1/discovery");
    fs::create_dir_all(&parent).unwrap();
    let path = parent.join(hex::encode(spec.summary.protocol_bundle_hash));
    let writer = DiscoverySpoolV1::open(
        &path,
        view.chain.chain().id(),
        view.chain.genesis_hash(),
        poc_schema_limits(),
    )
    .unwrap();
    writer.put_offer(1, spec).unwrap();
    drop(writer);
    path
}

#[test]
fn all_present_offers_bind_current_canonical_job_without_export_prerequisite() {
    for version in [1, 2] {
        with_job(version, true, true, |state, view, job| {
            let root = tempfile::tempdir().unwrap();
            write(root.path(), view, &spec(&job));
            let before = fingerprint(root.path());
            let mut offers = 0;
            let mut pending = 0;
            let audit = verify_present_discovery(
                state,
                view,
                root.path(),
                None,
                &mut |record, authority| {
                    match record {
                        DiscoverySpoolRecordV1::Offer(_) => {
                            assert_eq!(authority.as_ref(), Some(&job));
                            offers += 1;
                        }
                        DiscoverySpoolRecordV1::Pending { .. } => {
                            assert!(authority.is_none());
                            pending += 1;
                        }
                        _ => panic!("unexpected fixture record"),
                    }
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!((audit.spools, audit.records, audit.offers), (1, 2, 1));
            assert_eq!((offers, pending), (1, 1));
            assert_eq!(fingerprint(root.path()), before);
            assert!(!root.path().join("supervisor-v1").exists());
        });
    }
}

#[test]
fn absent_retired_spools_are_optional_and_closure_is_not_a_bundle_spool() {
    with_job(1, true, true, |state, view, _| {
        let root = tempfile::tempdir().unwrap();
        for present_closure in [false, true] {
            if present_closure {
                fs::create_dir_all(
                    root.path()
                        .join("exporter-v1/discovery/closure-checkpoint-v1"),
                )
                .unwrap();
            }
            let before = fingerprint(root.path());
            let audit = verify_present_discovery(state, view, root.path(), Some(0), &mut |_, _| {
                panic!("no records")
            })
            .unwrap();
            assert_eq!((audit.spools, audit.records, audit.offers), (0, 0, 0));
            assert_eq!(fingerprint(root.path()), before);
        }
    });
}

#[test]
fn native_valid_offer_cannot_replace_canonical_cursor_or_voting_window() {
    with_job(1, true, true, |state, view, job| {
        for field in 0..3 {
            let root = tempfile::tempdir().unwrap();
            let mut offered = spec(&job);
            match field {
                0 => offered.summary.cursor += 1,
                1 => offered.summary.open_height += 1,
                _ => offered.summary.deadline_height += 1,
            };
            write(root.path(), view, &offered);
            let before = fingerprint(root.path());
            let error =
                verify_present_discovery(state, view, root.path(), None, &mut |_, _| Ok(()))
                    .unwrap_err();
            assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
            assert_eq!(fingerprint(root.path()), before);
        }
    });
}

#[test]
fn complete_spool_walk_checks_bundle_location_and_later_native_records() {
    with_job(2, true, true, |state, view, job| {
        for fault in 0..2 {
            let root = tempfile::tempdir().unwrap();
            let path = write(root.path(), view, &spec(&job));
            if fault == 0 {
                fs::rename(
                    &path,
                    path.parent()
                        .unwrap()
                        .join(hex::encode(B256::repeat_byte(0xee))),
                )
                .unwrap();
            } else {
                let pending = fs::read_dir(path.join("pending"))
                    .unwrap()
                    .next()
                    .unwrap()
                    .unwrap()
                    .path();
                fs::write(pending, b"corrupt").unwrap();
            }
            let before = fingerprint(root.path());
            assert!(
                verify_present_discovery(state, view, root.path(), None, &mut |_, _| Ok(()))
                    .is_err()
            );
            assert_eq!(fingerprint(root.path()), before);
        }
    });
}

#[test]
fn incomplete_budget_or_callback_cannot_be_replaced_by_partial_spool_success() {
    with_job(1, true, true, |state, view, job| {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), view, &spec(&job));
        let before = fingerprint(root.path());
        for budget in [0, 1] {
            let error = verify_present_discovery(
                state,
                view,
                root.path(),
                Some(budget),
                &mut |_, _| Ok(()),
            )
            .unwrap_err();
            assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
        }
        let error = verify_present_discovery(state, view, root.path(), None, &mut |_, _| {
            Err(Incomplete("callback evidence unavailable".into()).into())
        })
        .unwrap_err();
        assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
        assert_eq!(fingerprint(root.path()), before);
    });
}
