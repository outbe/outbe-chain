use super::*;

#[test]
fn startup_dkg_round_zero_is_only_for_empty_genesis_formation() {
    let empty_without_boundary = StartupDkgContext {
        last_execution_height: 0,
        last_consensus_finalized_height: 0,
        recovered_boundary_finalized: false,
        recovered_vrf_group_public_key: None,
        recovered_dkg_output_hash: None,
        genesis_formation_proven: true,
    };
    assert_eq!(
        startup_dkg_mode(empty_without_boundary, true),
        StartupDkgMode::InitialGenesisDkg
    );

    assert_eq!(
        startup_dkg_mode(empty_without_boundary, false),
        StartupDkgMode::LiveJoinRequired,
        "a local key outside the current set must not start genesis DKG"
    );

    let nonzero_execution_history = StartupDkgContext {
        last_execution_height: 7,
        last_consensus_finalized_height: 0,
        recovered_boundary_finalized: false,
        recovered_vrf_group_public_key: None,
        recovered_dkg_output_hash: None,
        genesis_formation_proven: true,
    };
    assert_eq!(
        startup_dkg_mode(nonzero_execution_history, true),
        StartupDkgMode::LiveJoinRequired,
        "non-zero execution history must not start genesis DKG"
    );

    let recovered_boundary = StartupDkgContext {
        last_execution_height: 0,
        last_consensus_finalized_height: 0,
        recovered_boundary_finalized: false,
        recovered_vrf_group_public_key: Some(B256::with_last_byte(9)),
        recovered_dkg_output_hash: Some(B256::with_last_byte(10)),
        genesis_formation_proven: true,
    };
    assert_eq!(
        startup_dkg_mode(recovered_boundary, true),
        StartupDkgMode::LiveJoinRequired,
        "a recovered chain DKG boundary must force live-join semantics"
    );
}

#[test]
fn startup_dkg_round_zero_requires_genesis_formation_proof() {
    let unproven = StartupDkgContext {
        last_execution_height: 0,
        last_consensus_finalized_height: 0,
        recovered_boundary_finalized: false,
        recovered_vrf_group_public_key: None,
        recovered_dkg_output_hash: None,
        genesis_formation_proven: false,
    };
    assert_eq!(
        startup_dkg_mode(unproven, true),
        StartupDkgMode::LiveJoinRequired,
        "local execution height 0 alone must not start DKG round 0"
    );

    let consensus_already_finalized = StartupDkgContext {
        last_execution_height: 0,
        last_consensus_finalized_height: 3,
        recovered_boundary_finalized: true,
        recovered_vrf_group_public_key: None,
        recovered_dkg_output_hash: None,
        genesis_formation_proven: true,
    };
    assert_eq!(
        startup_dkg_mode(consensus_already_finalized, true),
        StartupDkgMode::LiveJoinRequired,
        "marshal finalized height > 0 must block genesis DKG"
    );
}

#[test]
fn offer_key_gate_allows_only_proven_founding_identity_to_be_keyless() {
    let founding = StartupDkgContext {
        last_execution_height: 0,
        last_consensus_finalized_height: 0,
        recovered_boundary_finalized: false,
        recovered_vrf_group_public_key: None,
        recovered_dkg_output_hash: None,
        genesis_formation_proven: true,
    };
    validate_offer_key_before_threshold_work(founding, true, false, B256::ZERO, None).unwrap();

    let error = validate_offer_key_before_threshold_work(
        StartupDkgContext {
            genesis_formation_proven: false,
            ..founding
        },
        true,
        false,
        B256::ZERO,
        None,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("no permanent resident offer key"), "{error}");
    assert!(error.contains("no recovery or fallback"), "{error}");
}

#[test]
fn offer_key_gate_requires_exact_canonical_key_for_existing_state() {
    let existing = StartupDkgContext {
        last_execution_height: 12,
        last_consensus_finalized_height: 11,
        recovered_boundary_finalized: true,
        recovered_vrf_group_public_key: Some(B256::repeat_byte(0x41)),
        recovered_dkg_output_hash: Some(B256::repeat_byte(0x42)),
        genesis_formation_proven: false,
    };
    let canonical = B256::repeat_byte(0x51);
    validate_offer_key_before_threshold_work(existing, true, false, canonical, Some(canonical))
        .unwrap();

    for (resident, expected) in [
        (None, "no permanent resident offer key"),
        (Some(B256::ZERO), "zero permanent resident offer key"),
        (
            Some(B256::repeat_byte(0x52)),
            "does not hold the canonical permanent offer key",
        ),
    ] {
        let error =
            validate_offer_key_before_threshold_work(existing, true, false, canonical, resident)
                .unwrap_err()
                .to_string();
        assert!(error.contains(expected), "{error}");
        assert!(error.contains("no recovery or fallback"), "{error}");
    }

    let error = validate_offer_key_before_threshold_work(
        existing,
        true,
        false,
        B256::ZERO,
        Some(canonical),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("existing canonical state"), "{error}");
}

#[test]
fn offer_key_gate_defers_exact_comparison_only_for_ready_empty_db_verifier_join() {
    let empty_join = StartupDkgContext {
        last_execution_height: 0,
        last_consensus_finalized_height: 0,
        recovered_boundary_finalized: false,
        recovered_vrf_group_public_key: None,
        recovered_dkg_output_hash: None,
        genesis_formation_proven: false,
    };
    let resident = Some(B256::repeat_byte(0x61));
    validate_offer_key_before_threshold_work(empty_join, false, true, B256::ZERO, resident)
        .unwrap();

    let error =
        validate_offer_key_before_threshold_work(empty_join, false, false, B256::ZERO, resident)
            .unwrap_err()
            .to_string();
    assert!(error.contains("only an empty-DB verifier join"), "{error}");
}

#[test]
fn genesis_formation_gate_waits_without_expected_peers() {
    let genesis = B256::with_last_byte(1);
    let context = StartupDkgContext {
        last_execution_height: 0,
        last_consensus_finalized_height: 0,
        recovered_boundary_finalized: false,
        recovered_vrf_group_public_key: None,
        recovered_dkg_output_hash: None,
        genesis_formation_proven: false,
    };
    let evidence = RethGenesisPeerEvidence {
        connected_peers: 1,
        is_syncing: false,
        is_initially_syncing: false,
        peer_query_failed: false,
        peers: vec![RethGenesisPeerStatus {
            genesis,
            blockhash: genesis,
            latest_block: Some(0),
        }],
    };
    assert_eq!(
        genesis_formation_gate_decision(context, genesis, 3, &evidence),
        GenesisFormationGate::WaitForExecutionSync
    );
}

#[test]
fn tee_genesis_bootstrap_is_reserved_for_proven_founding_members() {
    let fresh = StartupDkgContext {
        last_execution_height: 0,
        last_consensus_finalized_height: 0,
        recovered_boundary_finalized: false,
        recovered_vrf_group_public_key: None,
        recovered_dkg_output_hash: None,
        genesis_formation_proven: true,
    };
    assert!(should_coordinate_genesis_tee_bootstrap(fresh, true, false));
    assert!(
        !should_coordinate_genesis_tee_bootstrap(fresh, true, true),
        "a shareless verifier join must not reproduce the block-1 OST3 ceremony"
    );
    assert!(
        !should_coordinate_genesis_tee_bootstrap(fresh, false, false),
        "a non-member cannot produce the founding OST3 payload"
    );

    let unsynced_join = StartupDkgContext {
        genesis_formation_proven: false,
        ..fresh
    };
    assert!(
        !should_coordinate_genesis_tee_bootstrap(unsynced_join, false, true),
        "an empty local database joining a running chain is not fresh genesis"
    );

    let existing = StartupDkgContext {
        last_execution_height: 12,
        genesis_formation_proven: false,
        ..fresh
    };
    assert!(!should_coordinate_genesis_tee_bootstrap(
        existing, true, false
    ));
}

#[test]
fn genesis_formation_gate_proves_peers_are_at_genesis() {
    let genesis = B256::with_last_byte(1);
    let context = StartupDkgContext {
        last_execution_height: 0,
        last_consensus_finalized_height: 0,
        recovered_boundary_finalized: false,
        recovered_vrf_group_public_key: None,
        recovered_dkg_output_hash: None,
        genesis_formation_proven: false,
    };
    let evidence = RethGenesisPeerEvidence {
        connected_peers: 2,
        is_syncing: true,
        is_initially_syncing: true,
        peer_query_failed: false,
        peers: vec![
            RethGenesisPeerStatus {
                genesis,
                blockhash: genesis,
                latest_block: Some(0),
            },
            RethGenesisPeerStatus {
                genesis,
                blockhash: genesis,
                latest_block: None,
            },
        ],
    };
    assert_eq!(
        genesis_formation_gate_decision(context, genesis, 2, &evidence),
        GenesisFormationGate::Proven
    );
}

#[test]
fn genesis_formation_gate_accepts_quorum_connected_non_mesh_topology() {
    let genesis = B256::with_last_byte(1);
    let context = StartupDkgContext {
        last_execution_height: 0,
        last_consensus_finalized_height: 0,
        recovered_boundary_finalized: false,
        recovered_vrf_group_public_key: None,
        recovered_dkg_output_hash: None,
        genesis_formation_proven: false,
    };
    let peer = RethGenesisPeerStatus {
        genesis,
        blockhash: genesis,
        latest_block: Some(0),
    };
    let evidence = RethGenesisPeerEvidence {
        connected_peers: 2,
        is_syncing: true,
        is_initially_syncing: true,
        peer_query_failed: false,
        peers: vec![peer; 2],
    };

    // Four validators need a 3-of-4 BFT quorum, hence two matching remote
    // witnesses per node. Requiring all three remote validators creates a split
    // startup gate on a healthy non-fully-meshed gossip topology: nodes seeing
    // 3/3 start all-member DKG while nodes seeing 2/3 never enter it.
    assert_eq!(
        genesis_formation_gate_decision(
            context,
            genesis,
            genesis_formation_required_remote_peers(4),
            &evidence,
        ),
        GenesisFormationGate::Proven
    );
}

#[test]
fn genesis_formation_gate_rejects_remote_chain_progress() {
    let genesis = B256::with_last_byte(1);
    let context = StartupDkgContext {
        last_execution_height: 0,
        last_consensus_finalized_height: 0,
        recovered_boundary_finalized: false,
        recovered_vrf_group_public_key: None,
        recovered_dkg_output_hash: None,
        genesis_formation_proven: false,
    };
    let evidence = RethGenesisPeerEvidence {
        connected_peers: 1,
        is_syncing: false,
        is_initially_syncing: false,
        peer_query_failed: false,
        peers: vec![RethGenesisPeerStatus {
            genesis,
            blockhash: B256::with_last_byte(2),
            latest_block: Some(11),
        }],
    };
    assert_eq!(
        genesis_formation_gate_decision(context, genesis, 1, &evidence),
        GenesisFormationGate::ExistingChainJoin
    );
}

#[test]
fn genesis_formation_gate_waits_while_reth_syncing_without_peer_quorum() {
    let genesis = B256::with_last_byte(1);
    let context = StartupDkgContext {
        last_execution_height: 0,
        last_consensus_finalized_height: 0,
        recovered_boundary_finalized: false,
        recovered_vrf_group_public_key: None,
        recovered_dkg_output_hash: None,
        genesis_formation_proven: false,
    };
    let evidence = RethGenesisPeerEvidence {
        connected_peers: 1,
        is_syncing: true,
        is_initially_syncing: false,
        peer_query_failed: false,
        peers: vec![RethGenesisPeerStatus {
            genesis,
            blockhash: genesis,
            latest_block: Some(0),
        }],
    };
    assert_eq!(
        genesis_formation_gate_decision(context, genesis, 2, &evidence),
        GenesisFormationGate::WaitForExecutionSync
    );
}

#[test]
fn test_build_boundary_artifact_maps_addresses() {
    let (keys, _participants, output, _polynomial) = run_test_dkg();

    let addresses = vec![
        Address::with_last_byte(0x11),
        Address::with_last_byte(0x22),
        Address::with_last_byte(0x33),
    ];

    let validator_set = validators::ValidatorSet {
        public_keys: keys.iter().map(|k| k.public_key()).collect(),
        addresses: addresses.clone(),
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 3],
    };

    let result = dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
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

    // All 3 addresses should be in the result.
    assert_eq!(result.reshare.new_active_set.len(), 3);
    assert!(result.reshare.new_active_set.contains(&addresses[0]));
    assert!(result.reshare.new_active_set.contains(&addresses[1]));
    assert!(result.reshare.new_active_set.contains(&addresses[2]));

    // Group public key should be a non-zero hash.
    assert_ne!(result.vrf_group_public_key, B256::ZERO);
    assert_ne!(result.reshare.active_set_hash, B256::ZERO);
}

#[test]
fn ost3_genesis_authority_comes_from_current_dkg_boundary_before_state_exists() {
    let (keys, _participants, output, _polynomial) = run_test_dkg();
    let addresses = vec![
        Address::with_last_byte(0x11),
        Address::with_last_byte(0x22),
        Address::with_last_byte(0x33),
    ];
    let validator_set = validators::ValidatorSet {
        public_keys: keys.iter().map(|key| key.public_key()).collect(),
        addresses: addresses.clone(),
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 3],
    };

    // Block 1 has not executed yet, so there is deliberately no provider/state
    // input here. The same DKG boundary that BoundaryOutcome will commit must be
    // the sole authority for the OST3 committee snapshot hash.
    let artifact = build_genesis_dkg_boundary_artifact(&validator_set, &output, true).unwrap();

    assert_eq!(artifact.epoch, 0);
    assert_eq!(artifact.vrf_material_version, 0);
    assert_ne!(artifact.committee_set_hash, B256::ZERO);
    assert_eq!(artifact.reshare.new_active_set, addresses);
}

#[test]
fn test_build_boundary_artifact_deterministic() {
    let (_keys, _participants, output, _polynomial) = run_test_dkg();

    let validator_set = validators::ValidatorSet {
        public_keys: _keys.iter().map(|k| k.public_key()).collect(),
        addresses: vec![
            Address::with_last_byte(0xAA),
            Address::with_last_byte(0xBB),
            Address::with_last_byte(0xCC),
        ],
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 3],
    };

    // Same inputs -> same output.
    let r1 = dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
        epoch: Epoch::new(1),
        validator_set: &validator_set,
        output: &output,
        is_full_dkg: true,
        dkg_cycle: 1,
        freeze_height: 10,
        planned_activation_height: 20,
        vrf_material_version: 1,
        is_validator_set_change: true,
        tee_expired_target_exclusions: Vec::new(),
    })
    .unwrap();
    let r2 = dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
        epoch: Epoch::new(1),
        validator_set: &validator_set,
        output: &output,
        is_full_dkg: true,
        dkg_cycle: 1,
        freeze_height: 10,
        planned_activation_height: 20,
        vrf_material_version: 1,
        is_validator_set_change: true,
        tee_expired_target_exclusions: Vec::new(),
    })
    .unwrap();
    assert_eq!(r1.vrf_group_public_key, r2.vrf_group_public_key);
    assert_eq!(r1.reshare.active_set_hash, r2.reshare.active_set_hash);
    assert_eq!(r1.reshare.new_active_set, r2.reshare.new_active_set);
    assert_eq!(r1.outcome, r2.outcome);
}

#[test]
fn test_build_boundary_artifact_allows_extra_validator_not_in_threshold_output() {
    let (keys, _participants, output, _polynomial) = run_test_dkg();
    let mut all_pks: Vec<_> = keys.iter().map(|k| k.public_key()).collect();
    let extra_key = bls12381::PrivateKey::random(rand_core_commonware::UnwrapErr(
        rand_commonware::rngs::SysRng,
    ));
    all_pks.push(extra_key.public_key());

    let refreshed_set = validators::ValidatorSet {
        public_keys: all_pks,
        addresses: vec![
            Address::with_last_byte(0x11),
            Address::with_last_byte(0x22),
            Address::with_last_byte(0x33),
            Address::with_last_byte(0x44),
        ],
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 4],
    };

    let result = dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
        epoch: Epoch::new(1),
        validator_set: &refreshed_set,
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
    assert_eq!(result.reshare.new_active_set.len(), 3);
}

#[test]
fn test_build_boundary_artifact_rejects_removed_validator_in_output() {
    let (keys, _participants, output, _polynomial) = run_test_dkg();
    let partial_set = validators::ValidatorSet {
        public_keys: keys.iter().take(2).map(|k| k.public_key()).collect(),
        addresses: vec![Address::with_last_byte(0x11), Address::with_last_byte(0x22)],
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 2],
    };

    let error = dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
        epoch: Epoch::new(1),
        validator_set: &partial_set,
        output: &output,
        is_full_dkg: false,
        dkg_cycle: 1,
        freeze_height: 10,
        planned_activation_height: 20,
        vrf_material_version: 1,
        is_validator_set_change: true,
        tee_expired_target_exclusions: Vec::new(),
    })
    .unwrap_err()
    .to_string();
    assert!(error.contains("absent from the validator set"));
}

#[test]
fn test_ordered_validator_addresses_rejects_missing_participant_key() {
    let key_a = bls12381::PrivateKey::random(rand_core_commonware::UnwrapErr(
        rand_commonware::rngs::SysRng,
    ));
    let key_b = bls12381::PrivateKey::random(rand_core_commonware::UnwrapErr(
        rand_commonware::rngs::SysRng,
    ));
    let participants: commonware_utils::ordered::Set<bls12381::PublicKey> =
        vec![key_a.public_key(), key_b.public_key()]
            .into_iter()
            .try_collect()
            .unwrap();
    let validator_set = validators::ValidatorSet {
        public_keys: vec![key_a.public_key()],
        addresses: vec![Address::with_last_byte(0x01)],
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing],
    };

    let err = ordered_validator_addresses(&participants, &validator_set)
        .unwrap_err()
        .to_string();
    assert!(err.contains("participant public key is missing"));
}
