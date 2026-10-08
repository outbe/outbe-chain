// Unit fixtures only: these bytes/process records are NOT runtime acceptance evidence.

use super::*;
use crate::world::state::*;
use std::collections::BTreeMap;

#[test]
fn snapshot_launch_requires_a_non_genesis_local_committee_anchor() {
    assert!(assert_local_committee_anchor(
        "certified follower startup recovery barrier completed",
        745_250
    )
    .is_err());
    assert!(assert_local_committee_anchor(
        "anchor_epoch=0 anchor_height=1 follower restored committee from local finalized history",
        745_250
    )
    .is_err());
    assert!(assert_local_committee_anchor("anchor_epoch=621 anchor_height=745205 follower restored committee from local finalized history", 745_250).is_ok());
    assert!(assert_local_committee_anchor("anchor_epoch=621 anchor_height=745205 follower restored committee from local finalized history", 745_204).is_err());
}

fn block(number: u64) -> SnapshotBlock {
    SnapshotBlock {
        number,
        hash: format!("{number:064x}"),
    }
}
fn native(h: u64) -> SnapshotNativeProgress {
    SnapshotNativeProgress {
        finalized: block(h),
        execution: block(h + 1),
        execution_stage: Some(h + 1),
        finish_stage: Some(h),
        partial_state_trie: Some(h),
        unwind: None,
        storage_version: 2,
        ce: block(h - 1),
        projection: block(h),
        ocomp_baseline: block(1),
        ocomp_previous: block(h - 1),
        ocomp_current: block(h),
    }
}
fn command(verb: &str, at: u64) -> SnapshotCommandObservation {
    SnapshotCommandObservation {
        argv: vec![
            "/release/outbe-chain".into(),
            "snapshot".into(),
            verb.into(),
            "--".into(),
            "--datadir".into(),
            "/fixture/receiver".into(),
        ],
        started: at,
        ended: at + 1,
        exit_code: Some(0),
        signal: None,
        stdout: Vec::new(),
        stderr: Vec::new(),
    }
}
fn metrics(started: u64, success: u64) -> Vec<u8> {
    format!("outbe_ocomp_worker_units_started_total {started}\noutbe_ocomp_worker_units_completed_total{{outcome=\"success\"}} {success}\n").into_bytes()
}
fn worker_fixture() -> SnapshotWorkerExecution {
    use alloy_primitives::B256;
    use outbe_ocomp_protocol::{
        common::BoundedBytes,
        profile::poc_schema_limits,
        unit::{
            BinaryReducerNode, UnitArtifactV1, UnitInterval, UnitPhase, UnitSpecV1,
            WorkOutputHeaderV1,
        },
    };
    let limits = poc_schema_limits();
    let spec = UnitSpecV1 {
        protocol_bundle_hash: B256::repeat_byte(0x77),
        job_id: B256::repeat_byte(0x55),
        attempt: 0,
        phase: UnitPhase::FixedReduce,
        interval: UnitInterval::BinaryReducerNode(BinaryReducerNode { level: 0, index: 0 }),
        canonical_ordered_inputs: vec![],
        lysis_program_semantics_hash: B256::repeat_byte(0x88),
        planner_spec_version: 1,
        reducer_spec_version: 1,
    };
    // Valid canonical unit fixture, not evidence of an executed production job.
    let artifact = UnitArtifactV1::from_canonical_output(
        &spec,
        WorkOutputHeaderV1 {
            source_coverage_root: B256::repeat_byte(0x81),
            output_coverage_root: B256::repeat_byte(0x82),
            source_coverage_count: 1,
            output_coverage_count: 1,
        },
        BoundedBytes(vec![1]),
        &limits,
    )
    .unwrap();
    let bytes = artifact.encode_canonical(&limits).unwrap();
    let path = std::path::PathBuf::from(format!(
        "/fixture/worker-inbox-v1/{}/artifacts/{}.ocb1",
        hex::encode(spec.protocol_bundle_hash),
        hex::encode(artifact.unit_id)
    ));
    let owner = SnapshotWorkerOwner {
        process: crate::world::ocomp::OcompProcessRecordV1 {
            validator_index: Some(4),
            role: crate::world::ocomp::OcompProcessRole::Worker,
            worker_ordinal: Some(0),
            pid: 2001,
            started_at_millis: 10,
            stopped_at_millis: None,
        },
        endpoint: "http://127.0.0.1:41000".into(),
        inbox_root: path.parent().unwrap().parent().unwrap().to_path_buf(),
        bundle_hash: spec.protocol_bundle_hash,
    };
    SnapshotWorkerExecution {
        before: SnapshotWorkerHttpObservation {
            owner: owner.clone(),
            observed: 11,
            body: metrics(0, 0),
        },
        after: SnapshotWorkerHttpObservation {
            owner: owner.clone(),
            observed: 17,
            body: metrics(1, 1),
        },
        attribution: SnapshotWorkerAttribution::SingleOwnedProducer {
            inventory_from: 7,
            inventory_through: 18,
            workers: vec![owner.clone()],
        },
        owner,
        artifact_before: SnapshotPriorFileObservation::DirectoryListing(SnapshotDirectoryListing {
            root: path.parent().unwrap().to_path_buf(),
            directory_identity: Some((1, 3)),
            started: 7,
            completed: 7,
            entries: vec![],
        }),
        admitted_artifact_len: bytes.len() as u64,
        admitted_artifact_keccak256: alloy_primitives::keccak256(&bytes),
        artifact_after: SnapshotFileRead {
            path,
            observed: 15,
            bytes: Some(bytes),
        },
        admission_catalog: "/fixture/new-job/admissions".into(),
        canonical_unit_spec: spec.encode_canonical(&limits).unwrap(),
        log: Some(SnapshotLogSlice {
            path: "/fixture/worker.log".into(),
            device: 1,
            inode: 2,
            start: 0,
            end: 0,
            bytes: vec![],
        }),
    }
}
fn local_result_fixture() -> (
    alloy_primitives::B256,
    outbe_ocomp_protocol::result::LysisResultV1,
    Vec<u8>,
) {
    use alloy_primitives::{B256, U256};
    use outbe_ocomp_protocol::{
        hash::hash_framed,
        profile::poc_schema_limits,
        registry::HashDomain,
        result::{
            lysis_v1_empty_semantic_event_root, CarryOverCreditActionV1, CarryOverReason,
            ConservationTotalsV1, ExactCountsV1, LysisArithmeticSummaryV1, LysisResultV1,
            ResultRootsV1,
        },
    };

    let limits = poc_schema_limits();
    let roots = ResultRootsV1 {
        nod_root: B256::repeat_byte(0x31),
        bucket_root: B256::repeat_byte(0x32),
        contributor_root: B256::repeat_byte(0x33),
        output_manifest_root: B256::repeat_byte(0x34),
    };
    let counts = ExactCountsV1 {
        tribute_count: 1,
        nod_count: 1,
        bucket_count: 0,
        contributor_count: 0,
        semantic_event_count: 0,
    };
    let conservation = ConservationTotalsV1 {
        tribute_nominal_total: U256::ZERO,
        eligible_nominal_total: U256::ZERO,
        day_limit: U256::ZERO,
        gratis_demand: U256::ZERO,
        day_gratis_limit_minor: U256::ZERO,
        lysis_limit_minor: U256::ZERO,
        desis_limit_minor: U256::ZERO,
        lysis_allocation_minor: U256::ZERO,
        unused_lysis_limit_minor: U256::ZERO,
        carry_over_credit: U256::ZERO,
        nod_cost_total: U256::ZERO,
    };
    let summary = LysisArithmeticSummaryV1 {
        input_manifest_hash: B256::repeat_byte(0x35),
        plan_hash: B256::repeat_byte(0x36),
        unit_artifact_root: B256::repeat_byte(0x37),
        fidelity_fraction_root: B256::repeat_byte(0x38),
        gratis_prefix_root: B256::repeat_byte(0x39),
        roots: roots.clone(),
        counts: counts.clone(),
        conservation: conservation.clone(),
        first_error_ordinal: None,
    };
    let job_id = B256::repeat_byte(0x55);
    let result = LysisResultV1 {
        protocol_bundle_hash: B256::repeat_byte(0x77),
        job_id,
        attempt: 0,
        input_manifest_hash: summary.input_manifest_hash,
        plan_hash: summary.plan_hash,
        unit_artifact_root: summary.unit_artifact_root,
        fidelity_fraction_root: summary.fidelity_fraction_root,
        gratis_prefix_root: summary.gratis_prefix_root,
        result_chunk_count: 1,
        result_chunk_list_root: B256::repeat_byte(0x3a),
        carry_over_credit: CarryOverCreditActionV1 {
            source_wwd: 1,
            reason: CarryOverReason::UnusedLysis,
            amount: U256::ZERO,
        },
        metadosis_completion_summary: outbe_ocomp::test_support::zero_completion_summary(
            1, 0, 1, 1,
        ),
        tribute_count: 1,
        tribute_nominal_total: U256::ZERO,
        unused_lysis_limit_minor: U256::ZERO,
        roots,
        counts,
        conservation,
        arithmetic_commitment: hash_framed(
            HashDomain::LysisArithmetic,
            &summary.encode_canonical(&limits).unwrap(),
        )
        .unwrap(),
        event_summary_hash: lysis_v1_empty_semantic_event_root().unwrap(),
    };
    let encoded = result
        .encode_canonical(&limits)
        .expect("fixture result encodes canonically");
    (job_id, result, encoded)
}

fn fixture() -> OfflineSnapshotEvidence {
    let (_, local, local_bytes) = local_result_fixture();
    let local_digest = hex::encode(
        local
            .result_digest(&outbe_ocomp_protocol::profile::poc_schema_limits())
            .unwrap(),
    );
    let progress = native(10);
    let manifest = serde_json::to_vec(
        &serde_json::json!({"version":1,"chain_id":54322345,"progress":progress}),
    )
    .unwrap();
    let archive = b"unit fixture archive, not a native snapshot";
    let mut create = command("create", 1);
    create.stdout = format!("snapshot=/fixture/cut.tar finalized_height=10 finalized_hash={} files=1 bytes=42 creator_public_key={} signature=verified\n", block(10).hash, "02".to_owned() + &"11".repeat(32)).into_bytes();
    let identity = BTreeMap::from([(
        "/fixture/receiver/own.key".into(),
        SnapshotFingerprint {
            sha256: "22".repeat(32),
            mode: 0o600,
        },
    )]);
    let launch = |at, pid, progress: SnapshotNativeProgress| {
        let anchor = block(progress.finalized.number + 1);
        let record = format!("certified follower startup recovery barrier completed marshal_processed={} recovery_height={} recovery_hash=0x{} ce_marker_height={} last_execution_height={}\n", anchor.number - 1, anchor.number, anchor.hash, anchor.number, anchor.number + 1).into_bytes();
        SnapshotLaunchObservation {
            slot: 4,
            started: at,
            pid,
            argv: vec![
                "/release/outbe-chain".into(),
                "node".into(),
                "--datadir".into(),
                "/fixture/receiver".into(),
            ],
            before_launch: SnapshotNativeObservation {
                progress,
                sources: vec!["/fixture/receiver/db".into()],
                observed: at - 1,
            },
            recovery: SnapshotRecoveryObservation {
                pid,
                incarnation_started: at,
                observed: at + 1,
                log: SnapshotLogSlice {
                    path: "/fixture/receiver/node.log".into(),
                    device: 1,
                    inode: 1,
                    start: 0,
                    end: record.len() as u64,
                    bytes: record,
                },
                marshal_processed: anchor.number - 1,
                ce_marker_height: anchor.number,
                last_execution_height: anchor.number + 1,
                canonical: anchor.clone(),
                anchor,
            },
        }
    };
    OfflineSnapshotEvidence {
        create: Some(create),
        manifest_bytes: manifest,
        archive_sha256: sha256(archive),
        transferred_archive_sha256: sha256(archive),
        transfer: Some(SnapshotCommandObservation {
            argv: vec!["cp".into(), "cut.tar".into(), "receiver.tar".into()],
            ..command("unused", 3)
        }),
        placement: Some(SnapshotPlacementObservation {
            completed: 6,
            native: SnapshotNativeObservation {
                progress: native(10),
                sources: vec!["/fixture/receiver/db".into()],
                observed: 5,
            },
        }),
        validation: SnapshotValidationObservation::NotRun,
        cut_canonical: block(10),
        identity_before: identity.clone(),
        identity_placed: identity.clone(),
        identity_at_k: identity.clone(),
        identity_restarted: identity,
        first_start: Some(launch(8, 1001, native(10))),
        copied_result: SnapshotResultObservation {
            job_id: "33".repeat(32),
            digest: "44".repeat(32),
        },
        new_job: Some(SnapshotNewJobObservation {
            requested: 12,
            request: block(12),
            job_id: "55".repeat(32),
            worker: worker_fixture(),
            local_before: SnapshotPriorFileObservation::DirectoryListing(
                SnapshotDirectoryListing {
                    root: "/fixture/new-job".into(),
                    directory_identity: Some((1, 4)),
                    started: 7,
                    completed: 7,
                    entries: vec![],
                },
            ),
            local_result_root: "/fixture/new-job".into(),
            local_after: SnapshotFileRead {
                path: std::path::PathBuf::from("/fixture/new-job")
                    .join(format!("{}.lysis-result-v1.ocb1", "55".repeat(32))),
                observed: 18,
                bytes: Some(local_bytes),
            },
            local_result: SnapshotResultObservation {
                job_id: "55".repeat(32),
                digest: local_digest.clone(),
            },
            canonical_result: SnapshotResultObservation {
                job_id: "55".repeat(32),
                digest: local_digest.clone(),
            },
            canonical_result_at: block(14),
        }),
        before_restart: Some(SnapshotNativeObservation {
            progress: native(15),
            sources: vec!["/fixture/receiver/db".into()],
            observed: 21,
        }),
        k_canonical: Some(block(15)),
        first_exit: Some(SnapshotExitObservation {
            pid: 1001,
            reaped: 20,
            code: Some(0),
            signal: None,
        }),
        second_start: Some(launch(23, 1002, native(15))),
    }
}
#[test]
fn accepts_distinct_native_frontiers_and_unvalidated_start_records() {
    let evidence = fixture();
    assert!(assert_snapshot_workflow(&evidence).is_ok());
    let decoded: SnapshotManifestObservation =
        serde_json::from_slice(&evidence.manifest_bytes).unwrap();
    assert_ne!(decoded.progress.execution, decoded.progress.finalized);
    assert_ne!(decoded.progress.ce, decoded.progress.projection);
    assert_eq!(decoded.progress.partial_state_trie, Some(10));
}
#[test]
fn rejects_missing_actual_cli_and_nonzero_exit() {
    let mut e = fixture();
    e.create = None;
    assert!(assert_snapshot_workflow(&e).is_err());
    let mut e = fixture();
    e.create.as_mut().unwrap().exit_code = Some(1);
    assert!(assert_snapshot_workflow(&e).is_err());
    let mut e = fixture();
    e.transfer = None;
    assert!(assert_snapshot_workflow(&e).is_err());
    let mut e = fixture();
    e.placement = None;
    assert!(assert_snapshot_workflow(&e).is_err());
}
#[test]
fn rejects_wrong_cut_height_hash_or_rpc_only_resume() {
    let mut e = fixture();
    e.cut_canonical.number += 1;
    assert!(assert_snapshot_workflow(&e).is_err());
    let mut e = fixture();
    e.cut_canonical.hash = "ab".repeat(32);
    assert!(assert_snapshot_workflow(&e).is_err());
    let mut e = fixture();
    e.first_start.as_mut().unwrap().before_launch.progress = native(1);
    assert!(assert_snapshot_workflow(&e).is_err());
}
#[test]
fn rejects_no_post_placement_progress_or_changed_current_k_hash() {
    let mut e = fixture();
    e.before_restart.as_mut().unwrap().progress = native(10);
    assert!(assert_snapshot_workflow(&e).is_err());
    let mut e = fixture();
    e.k_canonical.as_mut().unwrap().hash = "ab".repeat(32);
    assert!(assert_snapshot_workflow(&e).is_err());
}
#[test]
fn rejects_changed_own_identity_at_every_boundary() {
    for boundary in 0..3 {
        let mut e = fixture();
        let fingerprints = match boundary {
            0 => &mut e.identity_placed,
            1 => &mut e.identity_at_k,
            _ => &mut e.identity_restarted,
        };
        fingerprints.values_mut().next().unwrap().sha256 = "ff".repeat(32);
        assert!(assert_snapshot_workflow(&e).is_err());
    }
}
#[test]
fn rejects_copied_job_relabeling_and_pre_start_work() {
    let mut e = fixture();
    let old = e.copied_result.job_id.clone();
    let new = e.new_job.as_mut().unwrap();
    new.job_id = old.clone();
    new.local_result.job_id = old.clone();
    new.canonical_result.job_id = old;
    assert!(assert_snapshot_workflow(&e).is_err());
    let mut e = fixture();
    e.new_job.as_mut().unwrap().requested = 7;
    assert!(assert_snapshot_workflow(&e).is_err());
    let mut e = fixture();
    e.new_job.as_mut().unwrap().worker.after.body = metrics(0, 0);
    assert!(assert_snapshot_workflow(&e).is_err());
}
#[test]
fn rejects_missing_restart_reap_or_old_h_resume() {
    let mut e = fixture();
    e.second_start = None;
    assert!(assert_snapshot_workflow(&e).is_err());
    let mut e = fixture();
    e.first_exit = None;
    assert!(assert_snapshot_workflow(&e).is_err());
    let mut e = fixture();
    e.second_start.as_mut().unwrap().before_launch.progress = native(10);
    assert!(assert_snapshot_workflow(&e).is_err());
}
fn report(q: SnapshotBlock, p: SnapshotBlock, bodies: &str) -> Vec<u8> {
    let mut checks = serde_json::Map::new();
    for name in [
        "files",
        "provenance",
        "headers",
        "evm",
        "ce",
        "bodies",
        "ocomp",
    ] {
        checks.insert(name.into(), serde_json::json!({"selected":name == "bodies", "status":if name == "bodies" { bodies } else { "not_requested" }, "diagnostic":null}));
    }
    serde_json::to_vec(&serde_json::json!({"checks":checks,"observed":{"q":q,"p":p},"provenance":{},"required_missing":[],"retained_ranges":[],"inventory_bounds":[],"active_ocomp":[]})).unwrap()
}
#[test]
fn preserves_actual_incomplete_report_and_rejects_false_body_equality() {
    assert!(parse_snapshot_validation_report(&report(block(9), block(10), "incomplete")).is_ok());
    assert!(parse_snapshot_validation_report(&report(block(9), block(10), "passed")).is_err());
    let mut fork = block(10);
    fork.hash = "ef".repeat(32);
    assert!(parse_snapshot_validation_report(&report(block(10), fork, "passed")).is_err());
    assert!(parse_snapshot_validation_report(&report(block(10), block(10), "passed")).is_ok());
}
#[test]
fn rejects_unknown_status_and_missing_check_in_actual_json_shape() {
    assert!(parse_snapshot_validation_report(&report(block(10), block(10), "success")).is_err());
    let mut json: serde_json::Value =
        serde_json::from_slice(&report(block(10), block(10), "passed")).unwrap();
    json["checks"].as_object_mut().unwrap().remove("ce");
    assert!(parse_snapshot_validation_report(&serde_json::to_vec(&json).unwrap()).is_err());
}
#[test]
fn optional_audit_keeps_incomplete_exit_and_cannot_be_backfilled_after_start() {
    let mut e = fixture();
    let mut validation = command("validate", 6);
    validation.stdout = report(block(9), block(10), "incomplete");
    validation.exit_code = Some(1);
    e.validation = SnapshotValidationObservation::Run(validation.clone());
    assert!(assert_snapshot_workflow(&e).is_ok());
    validation.exit_code = Some(0);
    e.validation = SnapshotValidationObservation::Run(validation.clone());
    assert!(assert_snapshot_workflow(&e).is_err());
    validation.exit_code = Some(1);
    validation.started = 9;
    validation.ended = 10;
    e.validation = SnapshotValidationObservation::Run(validation);
    assert!(assert_snapshot_workflow(&e).is_err());
}

#[test]
fn digest_inequality_does_not_replace_new_job_execution_evidence() {
    let mut e = fixture();
    // Keep the new job's canonical bytes and digest bound. The copied result
    // observation is not used as a substitute for actual worker evidence.
    e.copied_result.digest = e.new_job.as_ref().unwrap().local_result.digest.clone();
    assert!(assert_snapshot_workflow(&e).is_ok());
}
#[test]
fn native_read_is_prelaunch_and_anchor_may_follow_a_legitimate_tail() {
    let e = fixture();
    let launch = e.first_start.as_ref().unwrap();
    assert!(launch.before_launch.observed < launch.started);
    assert_ne!(
        launch.recovery.anchor,
        launch.before_launch.progress.finalized
    );
    assert_ne!(
        launch.recovery.last_execution_height,
        launch.before_launch.progress.execution.number
    );
    assert!(assert_snapshot_workflow(&e).is_ok());
    let mut e = fixture();
    e.first_start.as_mut().unwrap().before_launch.observed = 9;
    assert!(assert_snapshot_workflow(&e).is_err());
}
#[test]
fn rejects_missing_wrong_or_old_incarnation_recovery_record() {
    let mut e = fixture();
    e.first_start.as_mut().unwrap().recovery.log.bytes.clear();
    assert!(assert_snapshot_workflow(&e).is_err());
    let mut e = fixture();
    e.first_start.as_mut().unwrap().recovery.pid += 1;
    assert!(assert_snapshot_workflow(&e).is_err());
    let mut e = fixture();
    e.first_start.as_mut().unwrap().recovery.incarnation_started -= 1;
    assert!(assert_snapshot_workflow(&e).is_err());
    let mut e = fixture();
    e.first_start.as_mut().unwrap().recovery.canonical.hash = "ee".repeat(32);
    assert!(assert_snapshot_workflow(&e).is_err());
}
#[test]
fn empty_worker_log_is_valid_with_sampled_status_native_artifact_and_counters() {
    let e = fixture();
    assert!(e
        .new_job
        .as_ref()
        .unwrap()
        .worker
        .log
        .as_ref()
        .unwrap()
        .bytes
        .is_empty());
    assert!(assert_snapshot_workflow(&e).is_ok());
}
#[test]
fn rejects_wrong_unit_path_tampered_artifact_or_no_success_counter() {
    let mut e = fixture();
    let worker = &mut e.new_job.as_mut().unwrap().worker;
    worker.artifact_after.path = worker.owner.inbox_root.join("artifacts/wrong-unit.ocb1");
    assert!(assert_snapshot_workflow(&e).is_err());
    let mut e = fixture();
    e.new_job
        .as_mut()
        .unwrap()
        .worker
        .artifact_after
        .bytes
        .as_mut()
        .unwrap()
        .push(0);
    assert!(assert_snapshot_workflow(&e).is_err());
    let mut e = fixture();
    e.new_job.as_mut().unwrap().worker.after.body = metrics(0, 0);
    assert!(assert_snapshot_workflow(&e).is_err());
}
#[test]
fn unique_owned_producer_requires_one_complete_matching_incarnation() {
    let mut e = fixture();
    assert!(assert_snapshot_workflow(&e).is_ok());
    let worker = &mut e.new_job.as_mut().unwrap().worker;
    let mut other = worker.owner.clone();
    other.process.pid += 1;
    other.process.worker_ordinal = Some(1);
    {
        let SnapshotWorkerAttribution::SingleOwnedProducer { workers, .. } =
            &mut worker.attribution;
        workers.push(other);
    }
    assert!(assert_snapshot_workflow(&e).is_err());
    let mut e = fixture();
    let SnapshotWorkerAttribution::SingleOwnedProducer { workers, .. } =
        &mut e.new_job.as_mut().unwrap().worker.attribution;
    workers.clear();
    assert!(assert_snapshot_workflow(&e).is_err());
    let mut e = fixture();
    let SnapshotWorkerAttribution::SingleOwnedProducer { workers, .. } =
        &mut e.new_job.as_mut().unwrap().worker.attribution;
    workers[0].process.pid += 1;
    assert!(assert_snapshot_workflow(&e).is_err());
}
#[test]
fn rejects_counter_from_restarted_worker_and_preexisting_new_job_artifact() {
    let mut e = fixture();
    e.new_job
        .as_mut()
        .unwrap()
        .worker
        .after
        .owner
        .process
        .started_at_millis += 1;
    assert!(assert_snapshot_workflow(&e).is_err());
    let mut e = fixture();
    {
        let worker = &mut e.new_job.as_mut().unwrap().worker;
        let SnapshotPriorFileObservation::DirectoryListing(listing) = &mut worker.artifact_before;
        listing.entries.push(
            worker
                .artifact_after
                .path
                .strip_prefix(&listing.root)
                .unwrap()
                .to_path_buf(),
        );
    }
    assert!(assert_snapshot_workflow(&e).is_err());
    let mut e = fixture();
    {
        let new = e.new_job.as_mut().unwrap();
        let SnapshotPriorFileObservation::DirectoryListing(listing) = &mut new.local_before;
        listing.entries.push(
            new.local_after
                .path
                .strip_prefix(&listing.root)
                .unwrap()
                .to_path_buf(),
        );
    }
    assert!(assert_snapshot_workflow(&e).is_err());
}
#[test]
fn job_must_follow_barrier_observation_and_second_native_read_is_independent() {
    let mut e = fixture();
    e.new_job.as_mut().unwrap().requested = 9;
    assert!(assert_snapshot_workflow(&e).is_err());
    let mut e = fixture();
    e.second_start.as_mut().unwrap().before_launch.progress.ce = block(1);
    assert!(assert_snapshot_workflow(&e).is_err());
}
#[test]
fn transfer_tampering_still_fails() {
    let mut e = fixture();
    e.transferred_archive_sha256 = "ff".repeat(32);
    assert!(assert_snapshot_workflow(&e).is_err());
}
#[test]
fn unknown_future_unit_path_can_be_proven_absent_by_prior_complete_listing() {
    let e = fixture();
    assert!(assert_snapshot_workflow(&e).is_ok());
    let mut e = fixture();
    let new = e.new_job.as_mut().unwrap();
    {
        let SnapshotPriorFileObservation::DirectoryListing(listing) =
            &mut new.worker.artifact_before;
        listing.entries.push(
            new.worker
                .artifact_after
                .path
                .strip_prefix(&listing.root)
                .unwrap()
                .to_path_buf(),
        );
    }
    assert!(assert_snapshot_workflow(&e).is_err());
}
#[test]
fn rejects_listing_after_request_wrong_root_and_incomplete_producer_interval() {
    let mut e = fixture();
    {
        let SnapshotPriorFileObservation::DirectoryListing(listing) =
            &mut e.new_job.as_mut().unwrap().worker.artifact_before;
        listing.completed = 13;
    }
    assert!(assert_snapshot_workflow(&e).is_err());
    let mut e = fixture();
    {
        let SnapshotPriorFileObservation::DirectoryListing(listing) =
            &mut e.new_job.as_mut().unwrap().worker.artifact_before;
        listing.root = "/unrelated".into();
    }
    assert!(assert_snapshot_workflow(&e).is_err());
    let mut e = fixture();
    let worker = &mut e.new_job.as_mut().unwrap().worker;
    worker.attribution = SnapshotWorkerAttribution::SingleOwnedProducer {
        inventory_from: 10,
        inventory_through: 18,
        workers: vec![worker.owner.clone()],
    };
    assert!(assert_snapshot_workflow(&e).is_err());
}
#[test]
fn serialized_observations_in_the_same_millisecond_are_valid() {
    let mut e = fixture();
    e.create.as_mut().unwrap().ended = 1;
    e.transfer.as_mut().unwrap().ended = 3;
    e.first_start.as_mut().unwrap().before_launch.observed = 8;
    let new = e.new_job.as_mut().unwrap();
    new.worker.before.observed = new.requested;
    {
        let SnapshotPriorFileObservation::DirectoryListing(listing) =
            &mut new.worker.artifact_before;
        listing.completed = new.requested;
    }
    {
        let SnapshotPriorFileObservation::DirectoryListing(listing) = &mut new.local_before;
        listing.completed = new.requested;
    }
    assert!(assert_snapshot_workflow(&e).is_ok());
}
#[test]
fn pending_cut_accepts_lysis_at_execution_after_finalized_height() {
    let mut progress = fixture().placement.unwrap().native.progress;
    progress.finalized.number = 105;
    progress.execution.number = 106;
    let mut queried = Vec::new();
    let observed = snapshot_pending_cut(&progress, |at| {
        queried.push(at.number);
        ensure!(at.number == 106, "Lysis has not activated at H=105");
        Ok(serde_json::json!({"block_number":106,"queue_sequence":1}))
    })
    .expect("copied E contains the completed Lysis even when H precedes it");
    assert_eq!(queried, vec![106]);
    assert_eq!(observed["execution"]["block_number"], 106);
    assert_eq!(observed["finalized_block"]["number"], 105);
    assert!(snapshot_pending_cut(&progress, |_| Err(eyre!("no pending work at E"))).is_err());
}
#[test]
fn local_result_requires_exact_job_filename_and_canonical_bytes() {
    let mut e = fixture();
    e.new_job.as_mut().unwrap().local_after.path = "/fixture/new-job/unrelated.ocb1".into();
    assert!(assert_snapshot_workflow(&e).is_err());
    let mut e = fixture();
    e.new_job.as_mut().unwrap().local_after.bytes = Some(vec![1]);
    assert!(assert_snapshot_workflow(&e).is_err());
    let mut e = fixture();
    e.new_job.as_mut().unwrap().local_result.digest = "ff".repeat(32);
    e.new_job.as_mut().unwrap().canonical_result.digest = "ff".repeat(32);
    assert!(assert_snapshot_workflow(&e).is_err());
}
