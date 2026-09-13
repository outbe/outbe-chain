use super::*;

#[test]
fn recover_application_finalized_round_returns_none_at_genesis_height() {
    let recovered = commonware_runtime::tokio::Runner::default().start(|context| async move {
        let clock = context.child("recover_clock");
        let (marshal_mailbox, resolver_keepalive, actor_handle) =
            start_recovery_marshal(context, HybridSchemeProvider::new()).await;

        let recovered = recover_application_finalized_round(clock, marshal_mailbox, 0)
            .await
            .unwrap();

        drop(resolver_keepalive);
        actor_handle.abort();
        let _ = actor_handle.await;
        recovered
    });

    assert_eq!(recovered, None);
}

#[test]
fn recover_application_finalized_round_reads_round_from_marshal_archive() {
    let recovered = commonware_runtime::tokio::Runner::default().start(|context| async move {
        let round = Round::new(Epoch::new(0), View::new(1175));
        let block = recovery_block(5700);
        let expected_digest = block.digest();
        let (provider, finalization) = recovery_finalization_fixture(&block, round);
        let clock = context.child("recover_clock");
        let (mut marshal_mailbox, resolver_keepalive, actor_handle) =
            start_recovery_marshal(context, provider).await;

        let _ = marshal_mailbox.verified(round, block).await;
        // 2026.5.0: `Reporter::report` is SYNC and returns `Feedback`.
        let _ = marshal_mailbox.report(Activity::Finalization(finalization));

        let recovered = recover_application_finalized_round(clock, marshal_mailbox, 5700)
            .await
            .unwrap();

        drop(resolver_keepalive);
        actor_handle.abort();
        let _ = actor_handle.await;
        (recovered, expected_digest)
    });

    assert_eq!(
        recovered.0,
        Some(RecoveredApplicationFinalization {
            round: Round::new(Epoch::new(0), View::new(1175)),
            digest: recovered.1,
        })
    );
}

#[test]
fn exact_marshal_finalization_promotes_recovery_anchor_to_execution_head() {
    let hash = B256::repeat_byte(0x42);
    let round = Round::new(Epoch::new(3), View::new(17));
    let reconciled = reconcile_recovered_execution_head(
        91,
        hash,
        Some(RecoveredApplicationFinalization {
            round,
            digest: Digest(hash),
        }),
    )
    .unwrap();

    assert_eq!(reconciled, (91, hash, Some(round)));
}

#[test]
fn mismatched_marshal_finalization_digest_fails_closed() {
    let error = reconcile_recovered_execution_head(
        91,
        B256::repeat_byte(0x42),
        Some(RecoveredApplicationFinalization {
            round: Round::new(Epoch::new(3), View::new(17)),
            digest: Digest(B256::repeat_byte(0x24)),
        }),
    )
    .unwrap_err()
    .to_string();

    assert!(error.contains("marshal finalization digest mismatch at execution height 91"));
    assert!(error.contains("execution=0x4242"));
    assert!(error.contains("marshal=0x2424"));
}

#[test]
fn certified_follower_recovery_anchor_is_bounded_by_exact_archive_and_execution() {
    let selected = select_certified_follower_recovery_height(CertifiedFollowerRecoveryFloors {
        marshal_processed: 357,
        archive_finalization_tip: 358,
        archive_block_tip: 358,
        execution_tip: 358,
        reth_finalized: 357,
    })
    .unwrap();

    assert_eq!(selected, 358);
}

#[test]
fn certified_follower_replay_suffix_covers_both_archive_tips_without_advancing_execution() {
    assert_eq!(
        certified_follower_replay_suffix_bounds(428, 428, 366),
        (366, 428)
    );
    assert_eq!(
        certified_follower_replay_suffix_bounds(421, 420, 366),
        (366, 421)
    );
    assert_eq!(
        certified_follower_replay_suffix_bounds(420, 421, 366),
        (366, 421)
    );
    assert_eq!(certified_follower_replay_suffix_bounds(0, 0, 0), (0, 0));
}

#[test]
fn certified_follower_normalized_archive_does_not_promote_observed_execution_anchor() {
    let selected = select_certified_follower_recovery_height(CertifiedFollowerRecoveryFloors {
        marshal_processed: 366,
        archive_finalization_tip: 428,
        archive_block_tip: 428,
        execution_tip: 366,
        reth_finalized: 366,
    })
    .unwrap();

    assert_eq!(selected, 366);
}

#[test]
fn certified_follower_recovery_anchor_rejects_unexplained_ack_gap() {
    let error = select_certified_follower_recovery_height(CertifiedFollowerRecoveryFloors {
        marshal_processed: 357,
        archive_finalization_tip: 359,
        archive_block_tip: 359,
        execution_tip: 359,
        reth_finalized: 357,
    })
    .unwrap_err()
    .to_string();

    assert!(error.contains("exceeds Marshal processed floor 357 by more than one block"));
}

#[test]
fn certified_follower_recovery_anchor_rejects_finality_regression() {
    let error = select_certified_follower_recovery_height(CertifiedFollowerRecoveryFloors {
        marshal_processed: 357,
        archive_finalization_tip: 357,
        archive_block_tip: 357,
        execution_tip: 357,
        reth_finalized: 358,
    })
    .unwrap_err()
    .to_string();

    assert!(error.contains("Reth finalized height 358 exceeds recovery anchor 357"));
}

#[test]
fn certified_follower_recovery_anchor_requires_matching_verified_records() {
    let block = recovery_block(358);
    let round = Round::new(Epoch::new(0), View::new(29));
    let (provider, finalization) = recovery_finalization_fixture(&block, round);

    let anchor = validate_certified_follower_recovery_record(
        358,
        block.block_hash(),
        &finalization,
        &block,
        &finalization,
        &block,
        &provider,
    )
    .unwrap();
    assert_eq!(anchor.checkpoint.block_number, 358);
    assert_eq!(anchor.checkpoint.block_hash, block.block_hash());

    let wrong_block = recovery_block(359);
    let error = validate_certified_follower_recovery_record(
        358,
        block.block_hash(),
        &finalization,
        &wrong_block,
        &finalization,
        &block,
        &provider,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("local archived block reports height 359, expected 358"));
}

fn recovered_reth_readback(
    head: ProjectionCheckpoint,
    safe: Option<ProjectionCheckpoint>,
    finalized: Option<ProjectionCheckpoint>,
) -> RecoveredRethForkchoice {
    RecoveredRethForkchoice {
        head,
        safe,
        finalized,
    }
}

fn exact_recovered_reth_readback(anchor: ProjectionCheckpoint) -> RecoveredRethForkchoice {
    recovered_reth_readback(anchor, Some(anchor), Some(anchor))
}

fn below_recovered_reth_readback(below: ProjectionCheckpoint) -> RecoveredRethForkchoice {
    recovered_reth_readback(below, Some(below), Some(below))
}

#[test]
fn recovered_fcu_requires_valid_response_and_exact_provider_identity() {
    let anchor = ProjectionCheckpoint {
        block_number: 358,
        block_hash: B256::repeat_byte(0x58),
    };

    assert_eq!(
        classify_recovered_fcu_attempt(
            anchor,
            &RecoveredForkchoiceAttempt::Valid,
            exact_recovered_reth_readback(anchor),
        ),
        RecoveredFcuAction::Complete,
    );
    let below = ProjectionCheckpoint {
        block_number: 357,
        block_hash: B256::repeat_byte(0x57),
    };
    assert_eq!(
        classify_recovered_fcu_attempt(
            anchor,
            &RecoveredForkchoiceAttempt::Valid,
            below_recovered_reth_readback(below),
        ),
        RecoveredFcuAction::Retry,
    );
}

#[test]
fn recovered_fcu_retries_syncing_and_transport_errors_without_changing_anchor() {
    let anchor = ProjectionCheckpoint {
        block_number: 358,
        block_hash: B256::repeat_byte(0x58),
    };
    let below = ProjectionCheckpoint {
        block_number: 357,
        block_hash: B256::repeat_byte(0x57),
    };

    assert_eq!(
        classify_recovered_fcu_attempt(
            anchor,
            &RecoveredForkchoiceAttempt::Syncing,
            recovered_reth_readback(below, None, None),
        ),
        RecoveredFcuAction::Retry,
    );
    assert_eq!(
        classify_recovered_fcu_attempt(
            anchor,
            &RecoveredForkchoiceAttempt::Retryable("response lost".to_string()),
            exact_recovered_reth_readback(anchor),
        ),
        RecoveredFcuAction::Complete,
    );
}

#[test]
fn recovered_fcu_rejects_invalid_or_conflicting_provider_finality() {
    let anchor = ProjectionCheckpoint {
        block_number: 358,
        block_hash: B256::repeat_byte(0x58),
    };

    assert_eq!(
        classify_recovered_fcu_attempt(
            anchor,
            &RecoveredForkchoiceAttempt::Invalid("rejected".to_string()),
            exact_recovered_reth_readback(anchor),
        ),
        RecoveredFcuAction::Fatal,
    );
    assert_eq!(
        classify_recovered_fcu_attempt(
            anchor,
            &RecoveredForkchoiceAttempt::Valid,
            exact_recovered_reth_readback(ProjectionCheckpoint {
                block_number: 358,
                block_hash: B256::repeat_byte(0x99),
            }),
        ),
        RecoveredFcuAction::Fatal,
    );
    assert_eq!(
        classify_recovered_fcu_attempt(
            anchor,
            &RecoveredForkchoiceAttempt::Valid,
            exact_recovered_reth_readback(ProjectionCheckpoint {
                block_number: 359,
                block_hash: B256::repeat_byte(0x59),
            }),
        ),
        RecoveredFcuAction::Fatal,
    );
}

#[test]
fn recovered_fcu_releases_projection_wait_without_running_executor_heartbeat() {
    use alloy_rpc_types_engine::{PayloadStatus, PayloadStatusEnum};
    use outbe_primitives::projection::{projection_readiness, ProjectionStatus, WaitOutcome};
    use reth_ethereum::node::api::{BeaconEngineMessage, OnForkChoiceUpdated};

    commonware_runtime::deterministic::Runner::default().start(|context| async move {
        let genesis = B256::repeat_byte(1);
        let below = ProjectionCheckpoint {
            block_number: 338,
            block_hash: B256::repeat_byte(0x38),
        };
        let anchor = ProjectionCheckpoint {
            block_number: 339,
            block_hash: B256::repeat_byte(0x39),
        };
        let (publisher, readiness) = projection_readiness(
            ProjectionCheckpoint {
                block_number: 0,
                block_hash: genesis,
            },
            ProjectionStatus::CatchingUp {
                checkpoint: Some(below),
            },
        );
        let waiting_parent = readiness.clone().wait_for(anchor, std::future::pending());
        tokio::pin!(waiting_parent);
        assert!(futures::poll!(&mut waiting_parent).is_pending());
        let provider = Arc::new(StdMutex::new(below_recovered_reth_readback(below)));
        let (engine_tx, mut engine_rx) = tokio::sync::mpsc::unbounded_channel();
        let (actor, _mailbox) = ExecutorActor::new(
            context.child("executor"),
            ConsensusEngineHandle::new(engine_tx),
            genesis,
            anchor.block_number,
            anchor.block_hash,
            readiness,
            None,
        );
        let updated_provider = Arc::clone(&provider);
        let publisher_keepalive = publisher.clone();
        let engine_task = context.child("engine").spawn(move |_| async move {
            let Some(BeaconEngineMessage::ForkchoiceUpdated {
                state,
                payload_attrs,
                tx,
            }) = engine_rx.recv().await
            else {
                panic!("expected recovered FCU before projection readiness");
            };
            assert_eq!(state.head_block_hash, anchor.block_hash);
            assert_eq!(state.safe_block_hash, anchor.block_hash);
            assert_eq!(state.finalized_block_hash, anchor.block_hash);
            assert!(payload_attrs.is_none());
            *updated_provider.lock().unwrap() = exact_recovered_reth_readback(anchor);
            tx.send(Ok(OnForkChoiceUpdated::valid(PayloadStatus::from_status(
                PayloadStatusEnum::Valid,
            ))))
            .unwrap();
            // The projector can now consume the certified recovered parent.
            publisher.publish(ProjectionStatus::Ready { checkpoint: anchor });
        });
        confirm_recovered_forkchoice(
            context.child("startup_barrier"),
            anchor,
            || actor.replay_recovered_forkchoice_once(anchor),
            || Ok(*provider.lock().unwrap()),
        )
        .await
        .unwrap();
        assert_eq!(waiting_parent.await, WaitOutcome::Ready);
        engine_task.await.unwrap();
        drop(publisher_keepalive);
        // actor.start() is deliberately never called: no live heartbeat is
        // available to rescue an incorrectly ordered startup barrier.
    });
}

#[test]
fn recovered_fcu_skips_engine_when_provider_is_already_exact() {
    commonware_runtime::deterministic::Runner::default().start(|context| async move {
        let anchor = ProjectionCheckpoint {
            block_number: 358,
            block_hash: B256::repeat_byte(0x58),
        };
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = Arc::clone(&attempts);

        confirm_recovered_forkchoice(
            context,
            anchor,
            move || {
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async { RecoveredForkchoiceAttempt::Valid }
            },
            move || Ok(exact_recovered_reth_readback(anchor)),
        )
        .await
        .unwrap();

        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 0);
    });
}

#[test]
fn recovered_fcu_reanchors_speculative_head_even_when_finalized_is_exact() {
    commonware_runtime::deterministic::Runner::default().start(|context| async move {
        let anchor = ProjectionCheckpoint {
            block_number: 358,
            block_hash: B256::repeat_byte(0x58),
        };
        let speculative_head = ProjectionCheckpoint {
            block_number: 359,
            block_hash: B256::repeat_byte(0x59),
        };
        let observations = Arc::new(StdMutex::new(std::collections::VecDeque::from([
            recovered_reth_readback(speculative_head, Some(anchor), Some(anchor)),
            exact_recovered_reth_readback(anchor),
        ])));
        let scripted_observations = Arc::clone(&observations);
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = Arc::clone(&attempts);

        confirm_recovered_forkchoice(
            context,
            anchor,
            move || {
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async { RecoveredForkchoiceAttempt::Valid }
            },
            move || Ok(scripted_observations.lock().unwrap().pop_front().unwrap()),
        )
        .await
        .unwrap();

        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(observations.lock().unwrap().is_empty());
    });
}

#[test]
fn recovered_fcu_retries_syncing_until_valid_and_provider_exact() {
    commonware_runtime::deterministic::Runner::default().start(|context| async move {
        let anchor = ProjectionCheckpoint {
            block_number: 358,
            block_hash: B256::repeat_byte(0x58),
        };
        let below = ProjectionCheckpoint {
            block_number: 357,
            block_hash: B256::repeat_byte(0x57),
        };
        let outcomes = Arc::new(StdMutex::new(std::collections::VecDeque::from([
            RecoveredForkchoiceAttempt::Syncing,
            RecoveredForkchoiceAttempt::Valid,
        ])));
        let observations = Arc::new(StdMutex::new(std::collections::VecDeque::from([
            below_recovered_reth_readback(below),
            below_recovered_reth_readback(below),
            exact_recovered_reth_readback(anchor),
        ])));
        let scripted_outcomes = Arc::clone(&outcomes);
        let scripted_observations = Arc::clone(&observations);

        confirm_recovered_forkchoice(
            context,
            anchor,
            move || {
                let outcome = scripted_outcomes.lock().unwrap().pop_front().unwrap();
                async move { outcome }
            },
            move || Ok(scripted_observations.lock().unwrap().pop_front().unwrap()),
        )
        .await
        .unwrap();

        assert!(outcomes.lock().unwrap().is_empty());
        assert!(observations.lock().unwrap().is_empty());
    });
}

#[test]
fn recovered_fcu_lost_response_completes_when_provider_is_exact() {
    commonware_runtime::deterministic::Runner::default().start(|context| async move {
        let anchor = ProjectionCheckpoint {
            block_number: 358,
            block_hash: B256::repeat_byte(0x58),
        };
        let below = ProjectionCheckpoint {
            block_number: 357,
            block_hash: B256::repeat_byte(0x57),
        };
        let observations = Arc::new(StdMutex::new(std::collections::VecDeque::from([
            below_recovered_reth_readback(below),
            exact_recovered_reth_readback(anchor),
        ])));
        let scripted_observations = Arc::clone(&observations);

        confirm_recovered_forkchoice(
            context,
            anchor,
            || async { RecoveredForkchoiceAttempt::Retryable("response lost".to_string()) },
            move || Ok(scripted_observations.lock().unwrap().pop_front().unwrap()),
        )
        .await
        .unwrap();

        assert!(observations.lock().unwrap().is_empty());
    });
}

#[test]
fn recovered_fcu_payload_invalid_fails_closed_even_if_provider_is_exact() {
    commonware_runtime::deterministic::Runner::default().start(|context| async move {
        let anchor = ProjectionCheckpoint {
            block_number: 358,
            block_hash: B256::repeat_byte(0x58),
        };
        let below = ProjectionCheckpoint {
            block_number: 357,
            block_hash: B256::repeat_byte(0x57),
        };
        let observations = Arc::new(StdMutex::new(std::collections::VecDeque::from([
            below_recovered_reth_readback(below),
            exact_recovered_reth_readback(anchor),
        ])));
        let scripted_observations = Arc::clone(&observations);

        let error = confirm_recovered_forkchoice(
            context,
            anchor,
            || async { RecoveredForkchoiceAttempt::Invalid("rejected".to_string()) },
            move || Ok(scripted_observations.lock().unwrap().pop_front().unwrap()),
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(error.contains("failed closed"));
    });
}

#[test]
fn recovered_fcu_attempt_timeout_exhaustion_fails_closed() {
    commonware_runtime::deterministic::Runner::default().start(|context| async move {
        let anchor = ProjectionCheckpoint {
            block_number: 358,
            block_hash: B256::repeat_byte(0x58),
        };
        let below = ProjectionCheckpoint {
            block_number: 357,
            block_hash: B256::repeat_byte(0x57),
        };

        let error = confirm_recovered_forkchoice(
            context,
            anchor,
            std::future::pending::<RecoveredForkchoiceAttempt>,
            move || Ok(below_recovered_reth_readback(below)),
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(error.contains("did not converge"));
        assert!(error.contains("timed out"));
    });
}

#[test]
fn recover_application_finalized_round_fails_when_archive_is_missing_height() {
    let error = commonware_runtime::tokio::Runner::default().start(|context| async move {
        let clock = context.child("recover_clock");
        let (marshal_mailbox, resolver_keepalive, actor_handle) =
            start_recovery_marshal(context, HybridSchemeProvider::new()).await;

        let error = recover_application_finalized_round(clock, marshal_mailbox, 5700)
            .await
            .unwrap_err()
            .to_string();

        drop(resolver_keepalive);
        actor_handle.abort();
        let _ = actor_handle.await;
        error
    });

    assert!(error.contains("marshal finalization missing for finalized execution height 5700"));
    assert!(error.contains("resync/rebuild consensus storage"));
}

#[test]
fn test_recovered_boundary_addresses_survive_latest_state_removal() {
    let (keys, participants, output, _polynomial) = run_test_dkg();
    let boundary_addresses = vec![
        Address::with_last_byte(0x11),
        Address::with_last_byte(0x22),
        Address::with_last_byte(0x33),
    ];
    let boundary_validator_set = validators::ValidatorSet {
        public_keys: keys.iter().map(|k| k.public_key()).collect(),
        addresses: boundary_addresses.clone(),
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 3],
    };
    let boundary = dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
        epoch: Epoch::new(7),
        validator_set: &boundary_validator_set,
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

    let latest_after_unfinalized_removal = validators::ValidatorSet {
        public_keys: keys.iter().skip(1).map(|k| k.public_key()).collect(),
        addresses: boundary_addresses.iter().skip(1).copied().collect(),
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 2],
    };
    assert!(
        ordered_validator_addresses(&participants, &latest_after_unfinalized_removal).is_err(),
        "provider-latest mapping should fail after an unfinalized removal of an old participant"
    );

    let recovered = ordered_addresses_from_recovered_boundary(&participants, &boundary).unwrap();
    assert_eq!(recovered, boundary_addresses);
}

// T-3 / behavioural counterpart of the removed source-grep test in
// `crates/blockchain/evm/tests/genesis.rs`. `validate_recovered_vrf_material`
// must reject when the locally-recovered VRF group public key disagrees with
// the finalized boundary artifact, and must accept when they match (or when
// no boundary is supplied - bootstrap path).
#[test]
fn validate_recovered_vrf_material_accepts_matching_boundary_rejects_mismatch() {
    let (_keys, _participants, _output, _share, polynomial) = run_test_dkg_complete();

    let local_group_pk =
        alloy_primitives::keccak256(commonware_codec::Encode::encode(polynomial.public()));

    // No boundary -> bootstrap path is allowed.
    super::validate_recovered_vrf_material(&polynomial, None).expect("bootstrap path must accept");

    // Matching boundary -> accept.
    let matching = test_boundary_with_vrf_hash(local_group_pk, 1);
    super::validate_recovered_vrf_material(&polynomial, Some(&matching))
        .expect("matching VRF group public key must accept");

    // Mismatching boundary -> reject with the operator-facing error string.
    let mismatching = test_boundary_with_vrf_hash(B256::repeat_byte(0xEE), 1);
    let err = super::validate_recovered_vrf_material(&polynomial, Some(&mismatching))
        .expect_err("mismatched VRF group public key must reject");
    assert!(
        err.to_string()
            .contains("saved DKG material does not match finalized VRF group public key"),
        "operator-facing error string must surface in the rejection: got {err}"
    );
}

// =============================================================================
// T4 - recovery picks participants from the recovered DKG output's committee
//      (the share holders), NOT the latest on-chain set, and fails fast when
// the restored material does not match the recovered boundary.
//
// `select_recovery_participants` is the pure decision the recovery path now
// uses at stack.rs section 7. The output's `players()` is already a sorted/deduped
// `commonware_utils::ordered::Set`, so participant indices derive from it
// canonically - the test asserts membership and the explicit drift error.
// =============================================================================

/// Build a `DkgBoundaryArtifact` whose `reshare.new_active_set` records `n`
/// distinct validator addresses - the committee the ceremony ran for.
fn test_boundary_with_active_set_len(n: usize) -> DkgBoundaryArtifact {
    let mut boundary = test_boundary_with_vrf_hash(B256::with_last_byte(0xC1), 7);
    boundary.reshare.new_active_set = (0..n).map(|i| Address::repeat_byte(i as u8 + 1)).collect();
    boundary
}

#[test]
fn recovery_uses_recovered_committee_not_latest() {
    // Recovered DKG output for a 3-validator committee. `players()` is the
    // sorted set of the three consensus pubkeys - the share holders.
    let recovered_players: commonware_utils::ordered::Set<bls12381::PublicKey> = (1u64..=3)
        .map(bls12381::PrivateKey::from_seed)
        .map(|key| key.public_key())
        .try_collect()
        .expect("3-key recovered participant set");

    // Subcase 1: the latest on-chain set has drifted to 4 keys, but the recovered
    // boundary recorded the 3-validator committee the material belongs to.
    // Recovery reconstructs against the recovered 3-key committee, ignoring latest.
    let boundary_ok = test_boundary_with_active_set_len(3);
    let resolved = super::select_recovery_participants(&recovered_players, &boundary_ok)
        .expect("matching committee size must reconstruct against the recovered committee");
    assert_eq!(
        resolved.len(),
        3,
        "must reconstruct against the recovered 3-key committee, not the drifted latest set"
    );
    assert_eq!(
        resolved, recovered_players,
        "resolved participants must be exactly the recovered DKG output's player set"
    );

    // Subcase 2: the recovered boundary records a 4-validator active set while the
    // restored DKG output has only 3 players - the consensus material does not
    // match the recovered chain boundary. Recovery must fail fast with an explicit
    // drift error rather than build the scheme against the wrong committee.
    let boundary_drift = test_boundary_with_active_set_len(4);
    let err = super::select_recovery_participants(&recovered_players, &boundary_drift)
        .expect_err("size mismatch between recovered material and boundary must fail fast");
    assert!(
        err.to_string()
            .contains("validator set has drifted from saved DKG"),
        "operator-facing drift error must surface in the rejection: got {err}"
    );
}
