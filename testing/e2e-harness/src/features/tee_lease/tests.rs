// Exact production message/target/severity spellings. Every negative test
// starts from this independently asserted affirmative fixture.
fn shareless_fixture() -> String {
    "2026-09-05T05:06:12.322609Z INFO outbe_engine::stack: local validator is absent from the finalized DKG boundary; restoring shareless verifier mode dkg_output_hash=0x00\n\
     2026-09-05T05:06:12.323719Z INFO outbe_engine::stack: shareless verifier is not yet in the canonical reshare target; retaining no proposer identity epoch=7\n\
     2026-09-05T05:06:12.330995Z INFO outbe_engine::stack: no threshold share for this epoch - running consensus engine in VERIFIER mode epoch=7".to_owned()
}

fn activation_fixture() -> &'static str {
    "INFO outbe_consensus::dkg_actor::actor::ceremony::completion: DKG ceremony complete - threshold material obtained\n\
     INFO outbe_engine::stack: VRF/DKG material activated dkg_cycle=9 activation_height=540 vrf_material_version=9"
}

#[test]
fn certified_follower_startup_probe_fails_closed_on_early_exit() {
    assert_eq!(
        super::classify_recovery_follower_startup(true, super::FOLLOWER_ENGINE_STARTED_MARKER),
        super::RecoveryFollowerStartupProbeV1::Ready
    );
    assert_eq!(
        super::classify_recovery_follower_startup(true, "still reconstructing committees"),
        super::RecoveryFollowerStartupProbeV1::Waiting
    );
    assert_eq!(
        super::classify_recovery_follower_startup(false, "fatal startup error"),
        super::RecoveryFollowerStartupProbeV1::Exited
    );
    assert_eq!(
        super::classify_recovery_follower_startup(false, super::FOLLOWER_ENGINE_STARTED_MARKER),
        super::RecoveryFollowerStartupProbeV1::Exited,
        "a dead follower must not satisfy the running-and-marker gate"
    );
}

#[test]
fn certified_follower_recovery_evidence_requires_all_operator_guidance() {
    let complete = "validator recovery requires certified follower catch-up; \
        omit --validator and use --upstream <healthy-certified-rpc>";
    assert!(super::requires_certified_follower_recovery(complete));
    assert!(!super::requires_certified_follower_recovery(
        "validator recovery requires certified follower catch-up; omit --validator"
    ));
}

#[test]
fn interrupted_recovery_requires_started_but_not_yet_admitted_follower() {
    let partial = format!(
        "INFO {marker}\nINFO finalized block processed",
        marker = super::FOLLOWER_ENGINE_STARTED_MARKER
    );
    assert!(super::validate_interrupted_recovery_log(&partial).is_ok());
    assert!(super::validate_interrupted_recovery_log("").is_err());
    assert!(super::validate_interrupted_recovery_log(&format!(
        "{partial}\nINFO local TEE lease guard armed at authenticated catch-up anchor"
    ))
    .is_err());
}

#[test]
fn pre_readiness_recovery_requires_explicit_shareless_runtime_evidence() {
    let log = shareless_fixture();
    assert!(super::pre_readiness_recovery_is_authority_free(
        super::STATUS_PENDING,
        false,
        &log,
    ));
    assert!(!super::pre_readiness_recovery_is_authority_free(
        super::STATUS_PENDING,
        true,
        &log,
    ));
}

#[test]
fn pre_readiness_recovery_rejects_validator_authority_markers() {
    let baseline = shareless_fixture();
    assert!(super::pre_readiness_recovery_is_authority_free(
        super::STATUS_PENDING,
        false,
        &baseline
    ));
    for marker in super::AUTHORITY_MARKERS {
        assert!(
            !super::pre_readiness_recovery_is_authority_free(
                super::STATUS_PENDING,
                false,
                &format!("{baseline}\nINFO outbe_engine::stack: {marker}")
            ),
            "missed {marker}"
        );
    }
}

#[test]
fn fresh_dkg_activation_requires_share_version_advance_and_runtime_evidence() {
    let log = activation_fixture();
    assert!(super::fresh_dkg_activation_is_complete(
        super::STATUS_ACTIVE,
        true,
        super::MaterialVersions {
            previous: 7,
            primary: 9,
            restarted: 9
        },
        log
    ));
    assert!(!super::fresh_dkg_activation_is_complete(
        super::STATUS_ACTIVE,
        true,
        super::MaterialVersions {
            previous: 7,
            primary: 7,
            restarted: 7
        },
        log
    ));
}

#[test]
fn recovery_follower_crash_requires_strict_post_start_progress() {
    assert!(!super::recovery_follower_has_post_start_progress(357, None));
    assert!(!super::recovery_follower_has_post_start_progress(
        357,
        Some(357)
    ));
    assert!(super::recovery_follower_has_post_start_progress(
        357,
        Some(358)
    ));
}

#[test]
fn lease_shareless_proof_rejects_each_missing_or_spoofed_positive() {
    let baseline = shareless_fixture();
    assert!(super::pre_readiness_recovery_is_authority_free(
        super::STATUS_PENDING,
        false,
        &baseline
    ));
    for line in baseline.lines() {
        let missing = baseline
            .lines()
            .filter(|other| *other != line)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!super::pre_readiness_recovery_is_authority_free(
            super::STATUS_PENDING,
            false,
            &missing
        ));
    }
    for changed in [
        baseline.replace("INFO", "DEBUG"),
        baseline.replace("outbe_engine::stack", "other"),
        baseline.replace("epoch - running", "epoch — running"),
    ] {
        assert!(!super::pre_readiness_recovery_is_authority_free(
            super::STATUS_PENDING,
            false,
            &changed
        ));
    }
    assert!(!super::pre_readiness_recovery_is_authority_free(
        super::STATUS_ACTIVE,
        false,
        &baseline
    ));
    let public = format!("{baseline}\nINFO outbe_engine::stack: VRF material active vrf_material_version=8\nINFO outbe_engine::stack: authenticated public DKG handoff");
    assert!(super::pre_readiness_recovery_is_authority_free(
        super::STATUS_PENDING,
        false,
        &public
    ));
}

#[test]
fn lease_activation_requires_order_exact_target_version_and_private_state() {
    let baseline = activation_fixture();
    assert!(super::fresh_dkg_activation_is_complete(
        super::STATUS_ACTIVE,
        true,
        super::MaterialVersions {
            previous: 7,
            primary: 9,
            restarted: 9
        },
        baseline
    ));
    for changed in [
        baseline.lines().rev().collect::<Vec<_>>().join("\n"),
        baseline.replace("complete - threshold", "complete — threshold"),
        baseline.replace(
            "outbe_consensus::dkg_actor::actor::ceremony::completion",
            "outbe_engine::stack",
        ),
        baseline.replace(
            "outbe_consensus::dkg_actor::actor::ceremony::completion",
            "outbe_consensus::dkg_actor::actor",
        ),
        baseline.replace("INFO", "DEBUG"),
        baseline.replace("vrf_material_version=9", "vrf_material_version=8"),
        baseline.replace("activation_height=540", "activation_height=bad"),
        baseline.replace("dkg_cycle=9", "dkg_cycle=9 dkg_cycle=10"),
    ] {
        assert!(
            !super::fresh_dkg_activation_is_complete(
                super::STATUS_ACTIVE,
                true,
                super::MaterialVersions {
                    previous: 7,
                    primary: 9,
                    restarted: 9
                },
                &changed
            ),
            "{changed}"
        );
    }
    for (status, share, primary, local) in [
        (super::STATUS_PENDING, true, 9, 9),
        (super::STATUS_ACTIVE, false, 9, 9),
        (super::STATUS_ACTIVE, true, 9, 8),
    ] {
        assert!(!super::fresh_dkg_activation_is_complete(
            status,
            share,
            super::MaterialVersions {
                previous: 7,
                primary: primary,
                restarted: local
            },
            baseline
        ));
    }
    assert!(super::activation_record(
        "INFO outbe_engine::stack: VRF material active vrf_material_version=9",
        9
    )
    .is_none());
}

#[test]
fn lease_activation_accepts_current_dkg_completion_and_activation_logs() {
    let captured = "2026-09-13T17:18:01.654150Z INFO outbe_consensus::dkg_actor::actor::ceremony::completion: DKG ceremony complete - threshold material obtained\n\
        2026-09-13T17:18:44.198768Z INFO outbe_engine::stack: VRF/DKG material activated dkg_cycle=9 activation_height=540 vrf_material_version=9";
    assert_eq!(super::activation_record(captured, 9), Some((9, 540)));
}

#[test]
fn lease_local_material_requires_explicit_boolean_in_same_response() {
    use serde_json::json;
    let observation =
        super::local_material(json!({"vrfMaterialVersion": "9", "lastDkgActivationHeight": 540, "hasThresholdShares": false}))
            .unwrap();
    assert_eq!(observation.version, 9);
    assert!(!observation.has_threshold_shares);
    for missing in [
        json!({"vrfMaterialVersion": 9}),
        json!({"hasThresholdShares": true}),
        json!({"vrfMaterialVersion": 9, "hasThresholdShares": "false"}),
        json!({"vrfMaterialVersion": 9, "hasThresholdShares": false}),
    ] {
        assert!(super::local_material(missing).is_err());
    }
}

#[test]
fn lease_material_comparison_rejects_rotation_boundary_and_local_share_disagreement() {
    let primary = super::LocalMaterialObservation {
        version: 9,
        last_activation_height: 540,
        has_threshold_shares: true,
    };
    let local = super::LocalMaterialObservation {
        has_threshold_shares: false,
        ..primary.clone()
    };
    assert!(super::material_observations_agree(
        &primary, &local, &primary, &local
    ));
    for changed in [
        super::LocalMaterialObservation {
            version: 10,
            ..local.clone()
        },
        super::LocalMaterialObservation {
            last_activation_height: 600,
            ..local.clone()
        },
        super::LocalMaterialObservation {
            has_threshold_shares: true,
            ..local.clone()
        },
    ] {
        assert!(!super::material_observations_agree(
            &primary, &local, &primary, &changed
        ));
    }
    let different_boundary = super::LocalMaterialObservation {
        last_activation_height: 600,
        ..local.clone()
    };
    assert!(!super::material_observations_agree(
        &primary,
        &different_boundary,
        &primary,
        &different_boundary
    ));
}

#[test]
fn lease_log_intervals_exclude_replacements_and_reject_truncation() {
    use std::io::Write as _;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("node.log");
    std::fs::write(&path, "earlier process\n").unwrap();
    let capture = super::LeaseLogCapture::arm(&path).unwrap();
    let mut writer = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    writeln!(writer, "current process").unwrap();
    let interval = capture.finish();
    writeln!(writer, "replacement process").unwrap();
    assert_eq!(interval.content, "current process\n");
    assert_eq!(interval.end - interval.start, interval.content.len() as u64);
    let mut capture = super::LeaseLogCapture::arm(&path).unwrap();
    std::fs::write(&path, "").unwrap();
    assert!(capture.log.read().is_err());
}

#[test]
fn finalized_clock_wait_refreshes_only_on_monotonic_progress() {
    let now = std::time::Instant::now();
    let progress_deadline = now + std::time::Duration::from_secs(180);

    assert_eq!(
        super::classify_finalized_clock_wait((10, 200), 600, (9, 100), now, progress_deadline,),
        super::FinalizedClockWaitDecisionV1::Progressed
    );
    assert_eq!(
        super::classify_finalized_clock_wait((10, 200), 600, (10, 200), now, progress_deadline,),
        super::FinalizedClockWaitDecisionV1::Waiting
    );
}

#[test]
fn finalized_clock_wait_reaches_target_and_rejects_stall_or_regression() {
    let now = std::time::Instant::now();

    assert_eq!(
        super::classify_finalized_clock_wait((11, 600), 600, (10, 200), now, now),
        super::FinalizedClockWaitDecisionV1::Reached
    );
    assert_eq!(
        super::classify_finalized_clock_wait((10, 599), 600, (10, 599), now, now),
        super::FinalizedClockWaitDecisionV1::Stalled
    );
    assert_eq!(
        super::classify_finalized_clock_wait((9, 201), 600, (10, 200), now, now),
        super::FinalizedClockWaitDecisionV1::Regressed
    );
    assert_eq!(
        super::classify_finalized_clock_wait((11, 199), 600, (10, 200), now, now),
        super::FinalizedClockWaitDecisionV1::Regressed
    );
}

#[test]
fn full_node_sync_probe_fails_immediately_when_the_process_exits() {
    assert_eq!(
        super::classify_full_node_sync_probe(Some(6), 7, Some(true)),
        super::FullNodeSyncProbeV1::Exited
    );
}

#[test]
fn full_node_sync_probe_never_treats_a_missing_handle_as_exit_evidence() {
    assert_eq!(
        super::classify_full_node_sync_probe(None, 7, None),
        super::FullNodeSyncProbeV1::NotTracked
    );
    assert_eq!(
        crate::world::localnet::Localnet::joiner_full_node_name(4),
        "joiner-full-node-4"
    );
}

#[test]
fn full_node_sync_probe_requires_a_live_node_at_the_checkpoint() {
    assert_eq!(
        super::classify_full_node_sync_probe(Some(6), 7, Some(false)),
        super::FullNodeSyncProbeV1::Pending
    );
    assert_eq!(
        super::classify_full_node_sync_probe(Some(7), 7, Some(false)),
        super::FullNodeSyncProbeV1::Reached(7)
    );
    assert_eq!(
        super::classify_full_node_sync_probe(Some(9), 7, Some(false)),
        super::FullNodeSyncProbeV1::Reached(9)
    );
}
