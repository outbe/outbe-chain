use super::*;

#[test]
fn activated_dkg_cleanup_removes_retry_and_pending_artifacts() {
    let dir = tempfile::tempdir().unwrap();
    for file in [
        DKG_PENDING_SHARE_FILE,
        DKG_PENDING_POLYNOMIAL_FILE,
        DKG_PENDING_OUTPUT_FILE,
        DKG_PENDING_BOUNDARY_FILE,
        DKG_PENDING_BOUNDARY_TMP_FILE,
        DKG_DEALER_RETRY_FILE,
        DKG_PLAYER_RETRY_FILE,
    ] {
        std::fs::write(dir.path().join(file), b"stale").unwrap();
    }

    retire_activated_dkg_retry_state(dir.path(), &bls::KeyBackend::Plaintext).unwrap();

    for file in [
        DKG_PENDING_SHARE_FILE,
        DKG_PENDING_POLYNOMIAL_FILE,
        DKG_PENDING_OUTPUT_FILE,
        DKG_PENDING_BOUNDARY_FILE,
        DKG_PENDING_BOUNDARY_TMP_FILE,
        DKG_DEALER_RETRY_FILE,
        DKG_PLAYER_RETRY_FILE,
    ] {
        assert!(!dir.path().join(file).exists(), "{file} was not retired");
    }
}

#[test]
fn existing_chain_without_current_threshold_material_fails_with_recovery_contract() {
    let error = missing_current_threshold_material_error(
        "saved DKG material is stale for the latest finalized boundary",
    );
    let message = error.to_string();

    assert!(message.contains("startup cannot recover threshold material before sync starts"));
    assert!(message.contains("--consensus.public-polynomial"));
    assert!(message.contains("--consensus.dkg-output"));
    assert!(message.contains("without --consensus.signing-share"));
    assert!(message.contains("saved DKG material is stale"));
}

#[test]
fn recovered_boundary_rejects_stale_threshold_material() {
    let (_, _, _output, _share, polynomial) = run_test_dkg_complete();
    let matching_hash = vrf_group_public_key_hash(&polynomial);

    assert!(vrf_material_matches_recovered_boundary(
        &polynomial,
        StartupDkgContext {
            last_execution_height: 100,
            last_consensus_finalized_height: 100,
            recovered_boundary_finalized: true,
            recovered_vrf_group_public_key: Some(matching_hash),
            recovered_dkg_output_hash: None,
            genesis_formation_proven: false,
        }
    ));
    assert!(vrf_material_matches_recovered_boundary(
        &polynomial,
        StartupDkgContext {
            last_execution_height: 0,
            last_consensus_finalized_height: 0,
            recovered_boundary_finalized: false,
            recovered_vrf_group_public_key: None,
            recovered_dkg_output_hash: None,
            genesis_formation_proven: true,
        }
    ));
    assert!(
        !vrf_material_matches_recovered_boundary(
            &polynomial,
            StartupDkgContext {
                last_execution_height: 100,
                last_consensus_finalized_height: 100,
                recovered_boundary_finalized: true,
                recovered_vrf_group_public_key: Some(B256::ZERO),
                recovered_dkg_output_hash: None,
                genesis_formation_proven: false,
            }
        ),
        "saved or CLI material from an older DKG boundary must not build a signer"
    );
}

#[test]
fn test_decode_boundary_output_round_trips_full_output() {
    let (keys, _participants, output, _share, _polynomial) = run_test_dkg_complete();

    let validator_set = validators::ValidatorSet {
        public_keys: keys.iter().map(|key| key.public_key()).collect(),
        addresses: vec![
            Address::with_last_byte(0x11),
            Address::with_last_byte(0x22),
            Address::with_last_byte(0x33),
        ],
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 3],
    };

    let artifact = dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
        epoch: Epoch::new(1),
        validator_set: &validator_set,
        output: &output,
        is_full_dkg: false,
        dkg_cycle: 1,
        freeze_height: 10,
        planned_activation_height: 20,
        vrf_material_version: 1,
        is_validator_set_change: true,
        tee_expired_target_exclusions: Vec::new(),
    })
    .unwrap();

    let decoded = decode_boundary_output(&artifact).unwrap();
    assert_eq!(decoded, output);
}

#[test]
fn test_decode_boundary_output_rejects_corrupted_outcome() {
    let (keys, _participants, output, _share, _polynomial) = run_test_dkg_complete();

    let validator_set = validators::ValidatorSet {
        public_keys: keys.iter().map(|key| key.public_key()).collect(),
        addresses: vec![
            Address::with_last_byte(0x11),
            Address::with_last_byte(0x22),
            Address::with_last_byte(0x33),
        ],
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 3],
    };

    let mut artifact = dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
        epoch: Epoch::new(1),
        validator_set: &validator_set,
        output: &output,
        is_full_dkg: false,
        dkg_cycle: 1,
        freeze_height: 10,
        planned_activation_height: 20,
        vrf_material_version: 1,
        is_validator_set_change: true,
        tee_expired_target_exclusions: Vec::new(),
    })
    .unwrap();

    let mut corrupted = artifact.outcome.to_vec();
    corrupted[0] = b'X';
    artifact.outcome = Bytes::from(corrupted);

    let error = decode_boundary_output(&artifact).unwrap_err().to_string();
    assert!(error.contains("invalid magic"));
}

#[test]
fn test_pending_dkg_boundary_snapshot_round_trips_and_rejects_corruption() {
    let (keys, _participants, output, _share, _polynomial) = run_test_dkg_complete();
    let validator_set = validators::ValidatorSet {
        public_keys: keys.iter().map(|key| key.public_key()).collect(),
        addresses: vec![
            Address::with_last_byte(0x11),
            Address::with_last_byte(0x22),
            Address::with_last_byte(0x33),
        ],
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 3],
    };
    let artifact = dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
        epoch: Epoch::new(7),
        validator_set: &validator_set,
        output: &output,
        is_full_dkg: false,
        dkg_cycle: 6,
        freeze_height: 10,
        planned_activation_height: 20,
        vrf_material_version: 2,
        is_validator_set_change: true,
        tee_expired_target_exclusions: Vec::new(),
    })
    .unwrap();
    let snapshot = PendingDkgBoundarySnapshot {
        artifact,
        completed_at_height: 17,
    };

    let encoded = encode_pending_dkg_boundary_snapshot(&snapshot).unwrap();
    let decoded = decode_pending_dkg_boundary_snapshot(&encoded).unwrap();
    assert_eq!(decoded, snapshot);

    let mut corrupted = encoded.clone();
    corrupted[0] = b'X';
    let error = decode_pending_dkg_boundary_snapshot(&corrupted)
        .unwrap_err()
        .to_string();
    assert!(error.contains("invalid pending DKG boundary snapshot magic"));

    let mut legacy = encoded;
    legacy[..8].copy_from_slice(b"ODKGPB01");
    let error = decode_pending_dkg_boundary_snapshot(&legacy)
        .unwrap_err()
        .to_string();
    assert!(error.contains("unsupported pending DKG boundary snapshot version"));
}

#[test]
fn test_save_load_and_clear_pending_dkg_boundary_snapshot() {
    let boundary = test_boundary_with_vrf_hash(B256::with_last_byte(0x55), 9);
    let snapshot = PendingDkgBoundarySnapshot {
        artifact: boundary,
        completed_at_height: 42,
    };
    let dir = tempfile::tempdir().unwrap();

    assert!(load_pending_dkg_boundary(dir.path()).unwrap().is_none());
    save_pending_dkg_boundary(dir.path(), &snapshot).unwrap();
    assert_eq!(
        load_pending_dkg_boundary(dir.path()).unwrap(),
        Some(snapshot)
    );
    clear_pending_dkg_boundary(dir.path());
    assert!(load_pending_dkg_boundary(dir.path()).unwrap().is_none());
}

#[test]
fn test_completed_dkg_is_durable_before_activation_boundary() {
    let (keys, participants, output, share, _polynomial) = run_test_dkg_complete();
    let validator_set = validators::ValidatorSet {
        public_keys: keys.iter().map(|key| key.public_key()).collect(),
        addresses: vec![
            Address::with_last_byte(0x11),
            Address::with_last_byte(0x22),
            Address::with_last_byte(0x33),
        ],
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 3],
    };
    let target = FrozenDkgTarget {
        dkg_cycle: 4,
        freeze_height: 90,
        planned_activation_height: 120,
        validator_set,
        participants: participants.clone(),
        tee_expired_target_exclusions: Vec::new(),
        is_validator_set_change: false,
    };
    let complete = dkg_actor::DkgComplete {
        output: output.clone(),
        share,
        participants: participants.clone(),
    };
    let dir = tempfile::tempdir().unwrap();
    let backend = bls::KeyBackend::Plaintext;

    let completed_boundary = persist_completed_dkg_before_activation(
        dir.path(),
        &backend,
        Epoch::new(3),
        3,
        &participants,
        &target,
        &complete,
        104,
    )
    .unwrap();

    let (_, _, recovered_output) = load_pending_dkg_state(dir.path(), &backend)
        .unwrap()
        .expect("completed DKG material must survive a pre-activation crash");
    assert_eq!(recovered_output, output);
    let snapshot = load_pending_dkg_boundary(dir.path())
        .unwrap()
        .expect("completed DKG boundary must survive a pre-activation crash");
    assert_eq!(
        snapshot.artifact, completed_boundary,
        "the object published for pre-announcement must be the exact durable boundary"
    );
    assert_eq!(snapshot.completed_at_height, 104);
    assert_eq!(snapshot.artifact.epoch, 4);
    assert_eq!(snapshot.artifact.dkg_cycle, 4);

    let manager = DkgManagerMailbox::new();
    manager.note_ceremony_completed(completed_boundary.clone());
    let planned = commonware_runtime::tokio::Runner::default().start(|_| async move {
        manager
            .plan_header_artifact(None, Epoch::new(3), 105, &NoAncestry)
            .await
    });
    assert_eq!(
        planned.map(|plan| plan.artifact),
        Ok(Some(ConsensusHeaderArtifact::CommitteePreAnnounce {
            epoch: completed_boundary.epoch,
            outcome: completed_boundary.outcome,
        })),
        "a durable completed boundary must be pre-announced before activation"
    );
}

/// Ancestry that is never consulted: with no parent and no current-epoch
/// boundary pending, the plan is decided from local DKG state alone.
struct NoAncestry;

impl dkg_manager::AncestryReader for NoAncestry {
    fn get_block_by_height<'a>(&'a self, _height: u64) -> dkg_manager::BlockLookupFuture<'a> {
        Box::pin(async { None })
    }

    fn get_block_by_hash<'a>(&'a self, _hash: B256) -> dkg_manager::BlockLookupFuture<'a> {
        Box::pin(async { None })
    }

    fn is_ready(&self) -> bool {
        true
    }
}

#[test]
fn test_pending_dkg_material_alone_does_not_restore_boundary() {
    let (_keys, _participants, output, share, polynomial) = run_test_dkg_complete();
    let dir = tempfile::tempdir().unwrap();
    let backend = bls::KeyBackend::Plaintext;

    // Crash cut point: pending DKG triplet reached disk, but the boundary
    // snapshot did not. Restart must not infer/activate a boundary from material
    // alone; the pending-boundary file remains absent and DkgManager has no
    // pending artifact to verify/drain.
    save_pending_dkg_state(dir.path(), &share, &polynomial, &output, &backend).unwrap();
    assert!(load_pending_dkg_state(dir.path(), &backend)
        .unwrap()
        .is_some());
    assert!(load_pending_dkg_boundary(dir.path()).unwrap().is_none());

    let manager = DkgManagerMailbox::new();
    assert!(commonware_runtime::tokio::Runner::default()
        .start(|_| async move { manager.pending_boundary_artifact(Epoch::new(7)).await })
        .is_none());
}

#[test]
fn test_pending_boundary_snapshot_restores_manager_before_commit() {
    let (keys, _participants, output, share, polynomial) = run_test_dkg_complete();
    let dir = tempfile::tempdir().unwrap();
    let backend = bls::KeyBackend::Plaintext;
    let validator_set = validators::ValidatorSet {
        public_keys: keys.iter().map(|key| key.public_key()).collect(),
        addresses: vec![
            Address::with_last_byte(0x11),
            Address::with_last_byte(0x22),
            Address::with_last_byte(0x33),
        ],
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 3],
    };
    let artifact = dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
        epoch: Epoch::new(7),
        validator_set: &validator_set,
        output: &output,
        is_full_dkg: false,
        dkg_cycle: 6,
        freeze_height: 10,
        planned_activation_height: 20,
        vrf_material_version: 2,
        is_validator_set_change: true,
        tee_expired_target_exclusions: Vec::new(),
    })
    .unwrap();
    let snapshot = PendingDkgBoundarySnapshot {
        artifact: artifact.clone(),
        completed_at_height: 20,
    };

    // Crash cut point: pending material + pending boundary snapshot exist, but
    // process memory was lost before/around note_ceremony_completed. Restart can
    // load both durable pieces and restore the boundary into DkgManager without
    // creating a committed marker.
    save_pending_dkg_state(dir.path(), &share, &polynomial, &output, &backend).unwrap();
    save_pending_dkg_boundary(dir.path(), &snapshot).unwrap();
    let loaded_state = load_pending_dkg_state(dir.path(), &backend)
        .unwrap()
        .expect("pending DKG state must survive restart");
    assert_eq!(loaded_state.2, output);
    let loaded_snapshot = load_pending_dkg_boundary(dir.path())
        .unwrap()
        .expect("pending boundary snapshot must survive restart");
    assert_eq!(loaded_snapshot, snapshot);

    let manager = DkgManagerMailbox::new();
    manager.note_recovered_pending_boundary(loaded_snapshot.artifact.clone());
    commonware_runtime::tokio::Runner::default().start(|_| async move {
        assert_eq!(
            manager.pending_boundary_artifact(Epoch::new(7)).await,
            Some(artifact.clone())
        );
        manager
            .verify_pending_boundary_artifact(Epoch::new(7), &artifact)
            .await
            .unwrap();
        assert_eq!(manager.take_committed_boundary_artifact().await, None);
    });
}

#[test]
fn test_pending_boundary_commit_requires_matching_finalized_artifact_then_clears() {
    let (keys, _participants, output, _share, _polynomial) = run_test_dkg_complete();
    let validator_set = validators::ValidatorSet {
        public_keys: keys.iter().map(|key| key.public_key()).collect(),
        addresses: vec![
            Address::with_last_byte(0x11),
            Address::with_last_byte(0x22),
            Address::with_last_byte(0x33),
        ],
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 3],
    };
    let artifact = dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
        epoch: Epoch::new(7),
        validator_set: &validator_set,
        output: &output,
        is_full_dkg: false,
        dkg_cycle: 6,
        freeze_height: 10,
        planned_activation_height: 20,
        vrf_material_version: 2,
        is_validator_set_change: true,
        tee_expired_target_exclusions: Vec::new(),
    })
    .unwrap();
    let mut different = artifact.clone();
    different.dkg_cycle = different.dkg_cycle.saturating_add(1);

    let manager = DkgManagerMailbox::new();
    manager.note_recovered_pending_boundary(artifact.clone());
    commonware_runtime::tokio::Runner::default().start(|_| async move {
        // Crash cut point: pending boundary exists before finalization. A different
        // finalized BoundaryOutcome must not drain/activate the pending artifact.
        manager.note_finalized_header_artifact(Some(&ConsensusHeaderArtifact::BoundaryOutcome(
            different,
        )));
        assert_eq!(manager.take_committed_boundary_artifact().await, None);
        assert_eq!(
            manager.pending_boundary_artifact(Epoch::new(7)).await,
            Some(artifact.clone())
        );

        // Once the matching boundary finalizes, activation drain returns it once
        // and clears pending state.
        manager.note_finalized_header_artifact(Some(&ConsensusHeaderArtifact::BoundaryOutcome(
            artifact.clone(),
        )));
        assert_eq!(
            manager.take_committed_boundary_artifact().await,
            Some(artifact.clone())
        );
        assert_eq!(manager.take_committed_boundary_artifact().await, None);
        assert!(manager
            .pending_boundary_artifact(Epoch::new(7))
            .await
            .is_none());
    });
}

#[test]
fn test_stale_pending_boundary_snapshot_predicate_covers_restart_cleanup() {
    let current = test_boundary_with_vrf_hash(B256::with_last_byte(0x55), 9);
    let snapshot = PendingDkgBoundarySnapshot {
        artifact: current.clone(),
        completed_at_height: 42,
    };
    assert!(!pending_boundary_is_finalized(&snapshot, None));
    assert!(pending_boundary_is_finalized(
        &snapshot,
        Some(&(41, current.clone()))
    ));
    assert!(pending_boundary_is_finalized(
        &snapshot,
        Some(&(42, current.clone()))
    ));

    let mut conflicting_same_cycle = current.clone();
    conflicting_same_cycle.outcome = Bytes::from_static(b"conflict");
    assert!(!pending_boundary_is_finalized(
        &snapshot,
        Some(&(42, conflicting_same_cycle))
    ));

    let mut newer_cycle = current.clone();
    newer_cycle.dkg_cycle = current.dkg_cycle.saturating_add(1);
    assert!(pending_boundary_is_finalized(
        &snapshot,
        Some(&(42, newer_cycle.clone()))
    ));
    assert!(pending_boundary_is_finalized(
        &snapshot,
        Some(&(142, newer_cycle))
    ));

    let mut older_cycle = current;
    older_cycle.dkg_cycle = older_cycle.dkg_cycle.saturating_sub(1);
    assert!(!pending_boundary_is_finalized(
        &snapshot,
        Some(&(42, older_cycle))
    ));
}

#[test]
fn test_save_and_load_dkg_state_preserves_output() {
    let (_keys, _participants, output, share, polynomial) = run_test_dkg_complete();
    let dir = tempfile::tempdir().unwrap();
    let backend = bls::KeyBackend::Plaintext;

    save_dkg_state(dir.path(), &share, &polynomial, &output, &backend).unwrap();

    let (loaded_share, loaded_polynomial, loaded_output) =
        load_saved_dkg_state(dir.path(), &backend).unwrap().unwrap();

    assert_eq!(loaded_share.index, share.index);
    assert_eq!(loaded_polynomial.encode(), polynomial.encode());
    assert_eq!(loaded_output, output);
}

#[test]
fn test_load_saved_dkg_state_rejects_incomplete_files() {
    let (_keys, _participants, _output, share, polynomial) = run_test_dkg_complete();
    let dir = tempfile::tempdir().unwrap();
    let backend = bls::KeyBackend::Plaintext;

    bls::save_signing_share(&dir.path().join(DKG_SHARE_FILE), &share, &backend).unwrap();
    bls::save_public_polynomial(&dir.path().join(DKG_POLYNOMIAL_FILE), &polynomial, &backend)
        .unwrap();

    let error = load_saved_dkg_state(dir.path(), &backend).unwrap_err();
    assert!(error.to_string().contains("saved DKG state is incomplete"));
}

/// A node that has already finalized (`Some(N>0)`) - or whose execution layer
/// recovered after a crash with consensus still durable - must classify as an
/// existing-chain join: it must NOT re-run the initial genesis DKG and the
/// genesis-formation gate must NOT (re)form genesis. An inverted height check
/// would compile clean but re-run genesis DKG on a restarted validator.
#[test]
fn restarted_finalized_node_does_not_refresh_genesis_dkg() {
    let fresh = StartupDkgContext {
        last_execution_height: 0,
        last_consensus_finalized_height: 0,
        recovered_boundary_finalized: false,
        recovered_vrf_group_public_key: None,
        recovered_dkg_output_hash: None,
        genesis_formation_proven: true,
    };
    // Genuinely fresh node (local key in set, no force) runs the genesis DKG.
    assert_eq!(
        startup_dkg_mode(fresh, true),
        StartupDkgMode::InitialGenesisDkg
    );

    // Restarted after finalizing 42 blocks (durable Some(42)) -> live join,
    // never a fresh genesis DKG.
    let finalized = StartupDkgContext {
        last_consensus_finalized_height: 42,
        ..fresh
    };
    assert_eq!(
        startup_dkg_mode(finalized, true),
        StartupDkgMode::LiveJoinRequired,
        "a node that already finalized blocks must NOT re-run the initial genesis DKG"
    );

    // The genesis-formation gate short-circuits to existing-chain on any prior
    // progress, regardless of peer evidence.
    let genesis = B256::repeat_byte(0x11);
    let evidence = RethGenesisPeerEvidence {
        connected_peers: 0,
        is_syncing: false,
        is_initially_syncing: false,
        peer_query_failed: false,
        peers: Vec::new(),
    };
    assert_eq!(
        genesis_formation_gate_decision(finalized, genesis, 3, &evidence),
        GenesisFormationGate::ExistingChainJoin,
        "durable consensus finalization must classify as existing-chain join"
    );
    // Crash recovery: execution lost (height 0) but consensus durable -> still
    // existing-chain (must not reset to genesis formation).
    let crash_recovery = StartupDkgContext {
        last_execution_height: 10,
        last_consensus_finalized_height: 0,
        ..fresh
    };
    assert_eq!(
        genesis_formation_gate_decision(crash_recovery, genesis, 3, &evidence),
        GenesisFormationGate::ExistingChainJoin
    );
}
