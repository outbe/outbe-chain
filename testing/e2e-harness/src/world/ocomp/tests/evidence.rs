use super::*;

#[cfg(feature = "ocomp-integration")]
#[test]
fn completed_job_artifacts_prove_four_isolated_deterministic_footprints() {
    let topology = completed_job_topology();
    let job_id = B256::repeat_byte(0x42);
    stage_completed_job_footprint(&topology, job_id);

    topology.verify_completed_job_artifacts(job_id).unwrap();
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn canonical_artifacts_accept_four_voters_and_both_successful_late_dispositions() {
    for disposition in [None, Some("checkpoint_pruned"), Some("protocol_owned")] {
        let (mut topology, mut proof, pids) = canonical_artifact_fixture();
        topology
            .arm_completed_artifact_phase(
                proof.bundle_hash,
                proof.result.job_id,
                pids.clone(),
                Duration::from_secs(60),
            )
            .unwrap();
        if let Some(disposition) = disposition {
            fs::remove_file(artifact_fixture_vote_path(
                &topology,
                3,
                proof.result.job_id,
            ))
            .unwrap();
            let line = if disposition == "checkpoint_pruned" {
                format!(
                    "ignored late OCOMP result before local persistence job_id={:#x} reason=\"checkpoint_pruned\"",
                    proof.result.job_id
                )
            } else {
                format!(
                    "embedded OCOMP local result arrived after canonical settlement; protocol owns the job job_id={:#x} result_digest={:#x}",
                    proof.result.job_id,
                    proof.result.result_digest(&poc_schema_limits()).unwrap()
                )
            };
            append_artifact_fixture_log(&topology, 3, &line);
        } else {
            proof.voters.push(3);
        }
        let evidence = topology
            .verify_completed_artifacts_canonical(&proof, &pids)
            .unwrap()
            .unwrap();
        assert_eq!(evidence["nodes"].as_array().unwrap().len(), 4);
        assert_eq!(
            evidence["canonical_voters"],
            serde_json::json!(proof.voters)
        );
    }
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn canonical_artifacts_never_exempt_a_missing_voter_journal() {
    let (mut topology, proof, pids) = canonical_artifact_fixture();
    topology
        .arm_completed_artifact_phase(
            proof.bundle_hash,
            proof.result.job_id,
            pids.clone(),
            Duration::from_secs(60),
        )
        .unwrap();
    let path = artifact_fixture_vote_path(&topology, 0, proof.result.job_id);
    fs::remove_file(&path).unwrap();
    append_artifact_fixture_log(
        &topology,
        0,
        &format!(
            "ignored late OCOMP result before local persistence job_id={:#x} reason=\"checkpoint_pruned\"",
            proof.result.job_id
        ),
    );
    assert!(topology
        .verify_completed_artifacts_canonical(&proof, &pids)
        .unwrap()
        .is_none());
    fs::write(path, b"restored-voter-journal").unwrap();
    assert!(topology
        .verify_completed_artifacts_canonical(&proof, &pids)
        .unwrap()
        .is_some());
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn canonical_artifacts_reject_stale_wrong_job_wrong_digest_and_failure_logs() {
    let (mut topology, proof, pids) = canonical_artifact_fixture();
    fs::remove_file(artifact_fixture_vote_path(
        &topology,
        3,
        proof.result.job_id,
    ))
    .unwrap();
    let valid = format!(
        "ignored late OCOMP result before local persistence job_id={:#x} reason=\"checkpoint_pruned\"",
        proof.result.job_id
    );
    append_artifact_fixture_log(&topology, 3, &valid);
    topology
        .arm_completed_artifact_phase(
            proof.bundle_hash,
            proof.result.job_id,
            pids.clone(),
            Duration::from_secs(60),
        )
        .unwrap();
    assert!(topology
        .verify_completed_artifacts_canonical(&proof, &pids)
        .unwrap()
        .is_none());
    for invalid in [
        valid.replace(
            &format!("{:#x}", proof.result.job_id),
            &format!("{:#x}", B256::repeat_byte(99)),
        ),
        format!(
            "ignored checkpoint-pruned OCOMP computation failure job_id={:#x}",
            proof.result.job_id
        ),
        format!(
            "embedded OCOMP local result arrived after canonical settlement; protocol owns the job job_id={:#x} result_digest={:#x}",
            proof.result.job_id,
            B256::ZERO
        ),
    ] {
        append_artifact_fixture_log(&topology, 3, &invalid);
        assert!(
            topology
                .verify_completed_artifacts_canonical(&proof, &pids)
                .unwrap()
                .is_none()
        );
    }
    append_artifact_fixture_log(&topology, 3, &valid);
    assert!(topology
        .verify_completed_artifacts_canonical(&proof, &pids)
        .unwrap()
        .is_some());
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn canonical_artifacts_wait_for_missing_cas_but_fail_on_corruption() {
    let (mut topology, proof, pids) = canonical_artifact_fixture();
    topology
        .arm_completed_artifact_phase(
            proof.bundle_hash,
            proof.result.job_id,
            pids.clone(),
            Duration::from_secs(60),
        )
        .unwrap();
    let bytes = proof.result.encode_canonical(&poc_schema_limits()).unwrap();
    let digest = hex::encode(keccak256(&bytes));
    let path = topology
        .domain_root(3)
        .unwrap()
        .join("cas-v1/objects")
        .join(&digest[..2])
        .join(&digest[2..]);
    fs::remove_file(&path).unwrap();
    assert!(topology
        .verify_completed_artifacts_canonical(&proof, &pids)
        .unwrap()
        .is_none());
    fs::write(&path, &bytes).unwrap();
    assert!(topology
        .verify_completed_artifacts_canonical(&proof, &pids)
        .unwrap()
        .is_some());
    let mut corrupt = bytes;
    *corrupt.last_mut().unwrap() ^= 1;
    fs::write(&path, corrupt).unwrap();
    assert!(topology
        .verify_completed_artifacts_canonical(&proof, &pids)
        .unwrap_err()
        .to_string()
        .contains("digest mismatch"));
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn canonical_artifacts_preserve_incarnation_job_quorum_and_deadline_guards() {
    let (mut topology, mut proof, pids) = canonical_artifact_fixture();
    assert!(topology
        .verify_completed_artifacts_canonical(&proof, &pids)
        .is_err());
    topology
        .arm_completed_artifact_phase(
            proof.bundle_hash,
            proof.result.job_id,
            pids.clone(),
            Duration::from_secs(60),
        )
        .unwrap();
    assert!(topology
        .arm_completed_artifact_phase(
            proof.bundle_hash,
            proof.result.job_id,
            pids.clone(),
            Duration::from_secs(60)
        )
        .is_err());
    let mut replacement = pids.clone();
    replacement.insert(3, 2_003);
    assert!(topology
        .verify_completed_artifacts_canonical(&proof, &replacement)
        .is_err());
    for voters in [vec![0, 1], vec![0, 1, 1], vec![0, 1, 4]] {
        proof.voters = voters;
        assert!(topology
            .verify_completed_artifacts_canonical(&proof, &pids)
            .is_err());
    }
    proof.voters = vec![0, 1, 2];
    assert!(topology
        .verify_completed_artifacts_canonical(&proof, &pids)
        .unwrap()
        .is_some());
    proof.result.job_id = B256::repeat_byte(99);
    assert!(topology
        .verify_completed_artifacts_canonical(&proof, &pids)
        .is_err());

    let (mut expired, proof, pids) = canonical_artifact_fixture();
    expired
        .arm_completed_artifact_phase(
            proof.bundle_hash,
            proof.result.job_id,
            pids.clone(),
            Duration::ZERO,
        )
        .unwrap();
    assert!(expired
        .verify_completed_artifacts_canonical(&proof, &pids)
        .unwrap_err()
        .to_string()
        .contains("deadline elapsed"));
    assert!(expired
        .arm_completed_artifact_phase(
            proof.bundle_hash,
            proof.result.job_id,
            pids,
            Duration::from_secs(60)
        )
        .is_err());
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn canonical_artifacts_reject_replaced_or_truncated_phase_logs() {
    for replace in [false, true] {
        let (mut topology, proof, pids) = canonical_artifact_fixture();
        append_artifact_fixture_log(&topology, 3, "old launch prefix");
        topology
            .arm_completed_artifact_phase(
                proof.bundle_hash,
                proof.result.job_id,
                pids.clone(),
                Duration::from_secs(60),
            )
            .unwrap();
        let path = topology.cfg.validator_dir(3).join("node.log");
        if replace {
            fs::rename(&path, path.with_extension("previous")).unwrap();
        }
        fs::write(&path, b"").unwrap();
        assert!(topology
            .verify_completed_artifacts_canonical(&proof, &pids)
            .is_err());
    }
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn completed_job_artifacts_reject_one_domain_with_different_worker_output() {
    let topology = completed_job_topology();
    let job_id = B256::repeat_byte(0x43);
    stage_completed_job_footprint(&topology, job_id);
    let bundle_hash = topology
        .launch_identity
        .expect("completed-job fixture has a launch identity")
        .protocol_bundle_hash;
    let changed = topology
        .domain_root(3)
        .unwrap()
        .join("worker-inbox-v1")
        .join(hex::encode(bundle_hash))
        .join("artifacts")
        .join("unit.ocb1");
    fs::write(changed, b"different-worker-output").unwrap();

    let error = topology.verify_completed_job_artifacts(job_id).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("retained different deterministic worker outputs"),
        "{error:#}"
    );
}

#[test]
fn topology_evidence_covers_every_configured_validator() {
    let topology = topology_with_validators(5);

    let evidence = topology.evidence_snapshot().unwrap();

    assert_eq!(evidence.domain_roots.len(), 5);
}

#[test]
fn fork_restart_evidence_requires_recovery_on_each_side_of_h() {
    let mut topology = topology();
    topology.launch_identity_evidence = Some(launch_identity_evidence(32, B256::repeat_byte(1)));
    topology
        .record_fork_restart_evidence(OcompForkRestartEvidenceV1 {
            validator_index: 0,
            activation_height: 32,
            pre_fork_restart_from_height: 2,
            pre_fork_rejoined_height: 4,
            down_across_fork_from_height: 30,
            finalized_while_down_height: 33,
            replayed_through_height: 34,
            post_fork_restart_from_height: 35,
            post_fork_rejoined_height: 36,
        })
        .unwrap();

    let snapshot = topology.evidence_snapshot().unwrap();
    snapshot.validate().unwrap();
    assert_eq!(snapshot.fork_restart.unwrap().replayed_through_height, 34);
}

#[test]
fn fork_restart_evidence_requires_the_exact_launch_identity() {
    let mut topology = topology();
    let error = topology
        .record_fork_restart_evidence(OcompForkRestartEvidenceV1 {
            validator_index: 0,
            activation_height: 32,
            pre_fork_restart_from_height: 2,
            pre_fork_rejoined_height: 4,
            down_across_fork_from_height: 30,
            finalized_while_down_height: 33,
            replayed_through_height: 34,
            post_fork_restart_from_height: 35,
            post_fork_rejoined_height: 36,
        })
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("requires the exact OCOMP launch identity"));
}

#[test]
fn fork_restart_evidence_rejects_a_validator_that_did_not_replay_h() {
    let mut topology = topology();
    topology.launch_identity_evidence = Some(launch_identity_evidence(32, B256::repeat_byte(1)));
    let error = topology
        .record_fork_restart_evidence(OcompForkRestartEvidenceV1 {
            validator_index: 0,
            activation_height: 32,
            pre_fork_restart_from_height: 2,
            pre_fork_rejoined_height: 4,
            down_across_fork_from_height: 30,
            finalized_while_down_height: 33,
            replayed_through_height: 31,
            post_fork_restart_from_height: 35,
            post_fork_rejoined_height: 36,
        })
        .unwrap_err();

    assert!(error.to_string().contains("does not span H-1/H/H+1 safely"));
    assert!(topology.evidence_snapshot().unwrap().fork_restart.is_none());
}

#[test]
fn fork_mismatch_evidence_requires_canonical_progress_and_isolated_head() {
    let mut topology = topology();
    topology.launch_identity_evidence = Some(launch_identity_evidence(32, B256::repeat_byte(1)));
    topology
        .record_fork_mismatch_evidence(OcompForkMismatchEvidenceV1 {
            validator_index: 0,
            canonical_install_hash: format!("{:#x}", B256::repeat_byte(1)),
            mismatched_install_hash: format!("{:#x}", B256::repeat_byte(2)),
            canonical_activation_height: 32,
            mismatched_activation_height: 32,
            canonical_head_before_restart: 7,
            mismatched_head_after_fork: 31,
            canonical_finalized_after_fork: 33,
        })
        .unwrap();

    let snapshot = topology.evidence_snapshot().unwrap();
    snapshot.validate().unwrap();
    assert_eq!(
        snapshot
            .fork_mismatch
            .unwrap()
            .canonical_finalized_after_fork,
        33
    );
}

#[test]
fn fork_mismatch_evidence_rejects_a_different_canonical_launch_identity() {
    let mut topology = topology();
    topology.launch_identity_evidence = Some(launch_identity_evidence(32, B256::repeat_byte(3)));
    let error = topology
        .record_fork_mismatch_evidence(OcompForkMismatchEvidenceV1 {
            validator_index: 0,
            canonical_install_hash: format!("{:#x}", B256::repeat_byte(1)),
            mismatched_install_hash: format!("{:#x}", B256::repeat_byte(2)),
            canonical_activation_height: 32,
            mismatched_activation_height: 32,
            canonical_head_before_restart: 7,
            mismatched_head_after_fork: 31,
            canonical_finalized_after_fork: 33,
        })
        .unwrap_err();

    assert!(error
        .to_string()
        .contains("does not match the launch identity"));
}

#[test]
fn fork_mismatch_evidence_rejects_a_node_that_imported_h() {
    let mut topology = topology();
    topology.launch_identity_evidence = Some(launch_identity_evidence(32, B256::repeat_byte(1)));
    let error = topology
        .record_fork_mismatch_evidence(OcompForkMismatchEvidenceV1 {
            validator_index: 0,
            canonical_install_hash: format!("{:#x}", B256::repeat_byte(1)),
            mismatched_install_hash: format!("{:#x}", B256::repeat_byte(2)),
            canonical_activation_height: 32,
            mismatched_activation_height: 32,
            canonical_head_before_restart: 7,
            mismatched_head_after_fork: 32,
            canonical_finalized_after_fork: 33,
        })
        .unwrap_err();

    assert!(error
        .to_string()
        .contains("does not prove fail-closed isolation"));
    assert!(topology
        .evidence_snapshot()
        .unwrap()
        .fork_mismatch
        .is_none());
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn canonical_artifact_phases_retain_distinct_jobs_using_the_same_bundle() {
    let (mut topology, mut proof, pids) = canonical_artifact_fixture();
    proof.voters.push(3);
    topology
        .arm_completed_artifact_phase(
            proof.bundle_hash,
            proof.result.job_id,
            pids.clone(),
            Duration::from_secs(60),
        )
        .unwrap();
    let second_job = B256::repeat_byte(0xfe);
    assert_ne!(second_job, proof.result.job_id);
    topology
        .arm_completed_artifact_phase(
            proof.bundle_hash,
            second_job,
            pids.clone(),
            Duration::from_secs(60),
        )
        .unwrap();
    assert_eq!(topology.artifact_phases.len(), 2);
    let original_job = proof.result.job_id;
    proof.result.job_id = second_job;
    assert!(
        !matches!(
            topology.verify_completed_artifacts_canonical(&proof, &pids),
            Ok(Some(_))
        ),
        "another job must not inherit the first job's artifacts"
    );
    stage_completed_job_footprint(&topology, second_job);
    let bytes = proof.result.encode_canonical(&poc_schema_limits()).unwrap();
    for &index in pids.keys() {
        use outbe_ocomp::cas::{CasLimits, CasWriterRole, FilesystemCas};
        let cas = FilesystemCas::open(
            topology.domain_root(index).unwrap().join("cas-v1"),
            CasWriterRole::Supervisor,
            CasLimits {
                max_object_bytes: bytes.len() as u64,
                max_total_bytes: u64::MAX,
            },
        )
        .unwrap();
        cas.publish_bytes(&bytes).unwrap();
    }
    assert!(topology
        .verify_completed_artifacts_canonical(&proof, &pids)
        .unwrap()
        .is_some());
    proof.result.job_id = original_job;
    assert!(topology
        .verify_completed_artifacts_canonical(&proof, &pids)
        .unwrap()
        .is_some());
    assert!(
        topology
            .arm_completed_artifact_phase(
                proof.bundle_hash,
                proof.result.job_id,
                pids,
                Duration::from_secs(60)
            )
            .is_err(),
        "re-arming must not erase earlier job evidence"
    );
}
