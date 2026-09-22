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
            block_number: 24,
            block_hash: B256::repeat_byte(0x24),
        };
        let speculative_head = ProjectionCheckpoint {
            block_number: 25,
            block_hash: B256::repeat_byte(0x25),
        };
        let anchor_header = reth_recovery_header(anchor);
        let state = reth_chain_state::CanonicalInMemoryState::with_head(
            reth_recovery_header(speculative_head),
            Some(anchor_header.clone()),
            Some(anchor_header.clone()),
        );
        state.set_persisted(reth_recovery_header(speculative_head).num_hash());
        let updated_state = state.clone();
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = Arc::clone(&attempts);

        confirm_recovered_forkchoice(
            context,
            anchor,
            move || {
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                updated_state.set_canonical_head(anchor_header.clone());
                async { RecoveredForkchoiceAttempt::Valid }
            },
            || read_reth_recovery_forkchoice(&state),
        )
        .await
        .unwrap();

        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(state.get_persisted_num_hash().unwrap().number, 25);
        assert_eq!(state.hash_by_number(25), None);
        assert_eq!(
            read_reth_recovery_forkchoice(&state).unwrap(),
            exact_recovered_reth_readback(anchor)
        );
    });
}

fn reth_recovery_header(checkpoint: ProjectionCheckpoint) -> SealedHeader<OutbeHeader> {
    SealedHeader::new(
        OutbeHeader::new(Header {
            number: checkpoint.block_number,
            ..Default::default()
        }),
        checkpoint.block_hash,
    )
}

#[test]
fn recovery_reader_preserves_unset_safe_and_finalized_markers() {
    let genesis = ProjectionCheckpoint {
        block_number: 0,
        block_hash: B256::repeat_byte(0x10),
    };
    let state = reth_chain_state::CanonicalInMemoryState::with_head(
        reth_recovery_header(genesis),
        None,
        None,
    );
    assert_eq!(
        read_reth_recovery_forkchoice(&state).unwrap(),
        recovered_reth_readback(genesis, None, None)
    );
}

#[test]
fn recovery_reader_rejects_zero_canonical_head() {
    let state = reth_chain_state::CanonicalInMemoryState::with_head(
        reth_recovery_header(ProjectionCheckpoint {
            block_number: 24,
            block_hash: B256::ZERO,
        }),
        None,
        None,
    );
    let error = read_reth_recovery_forkchoice(&state).unwrap_err();
    assert!(error.to_string().contains("zero canonical head"), "{error}");
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

mod copied_store_controls {
    use super::super::restart_recovery::copied_native::{
        open_native, recover_native, remove_native_header, DiskFixture, H,
    };
    use super::*;
    use crate::ce_finalizer::RethDurableCeState;
    use crate::ce_recovery::{CanonicalCeReplaySource, CeStartupRecoveryCoordinator};
    use alloy_consensus::Sealable;
    use reth_ethereum::provider::db::{
        database::Database,
        init_db,
        mdbx::DatabaseArguments,
        tables,
        transaction::{DbTx, DbTxMut},
    };

    #[test]
    fn copied_disk_anchor_requires_both_archive_evidence_and_the_exact_execution_hash() {
        let fixture = DiskFixture::new(H - 1);
        let recipient = tempfile::tempdir().unwrap();
        fixture.copy_to(recipient.path());
        let observed = fixture.phase(recipient.path(), H + 1, H, H - 1);
        assert_eq!(
            select_certified_follower_recovery_height(CertifiedFollowerRecoveryFloors {
                marshal_processed: observed.processed,
                archive_finalization_tip: H,
                archive_block_tip: H,
                execution_tip: H,
                reth_finalized: H,
            })
            .unwrap(),
            H
        );
        let error = reconcile_recovered_execution_head(
            H,
            B256::repeat_byte(0xfe),
            Some(observed.recovered),
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("marshal finalization digest mismatch"));
        // Delete copied public consensus history, leaving real Reth/CE at H.
        // No manifest or independently stamped floor may replace those archives.
        std::fs::remove_dir_all(recipient.path().join("marshal")).unwrap();
        let error = fixture.missing_archive_error(recipient.path());
        assert!(
            error.contains("marshal finalization missing for finalized execution height 3"),
            "{error}"
        );
        assert!(
            select_certified_follower_recovery_height(CertifiedFollowerRecoveryFloors {
                marshal_processed: 0,
                archive_finalization_tip: 0,
                archive_block_tip: 0,
                execution_tip: H,
                reth_finalized: H,
            })
            .is_err()
        );
        let (_, tree) = open_native(recipient.path());
        assert_eq!(tree.finalized_marker().unwrap().height, H);
    }

    #[test]
    fn copied_equal_ce_marker_still_requires_real_parent_header_and_historical_root() {
        for missing_root in [false, true] {
            let fixture = DiskFixture::new(H - 1);
            let recipient = tempfile::tempdir().unwrap();
            fixture.copy_to(recipient.path());
            assert_eq!(
                recover_native(recipient.path(), H - 1, H).unwrap().height,
                H
            );
            let db = init_db(recipient.path().join("db"), DatabaseArguments::test()).unwrap();
            let tx = db.tx_mut().unwrap();
            if missing_root {
                tx.clear::<tables::PlainStorageState>().unwrap();
            }
            tx.commit().unwrap();
            drop(db);
            if !missing_root {
                remove_native_header(recipient.path(), H - 1);
            }
            let (provider, tree) = open_native(recipient.path());
            let before = tree.finalized_marker().unwrap();
            let recovery = CeStartupRecoveryCoordinator::new(
                std::sync::Arc::new(RethDurableCeState::new(provider)),
                tree.clone(),
            );
            let error = recovery.recover_before_participation(H).unwrap_err();
            assert!(!error.to_string().is_empty());
            assert_eq!(
                tree.finalized_marker().unwrap(),
                before,
                "failed history check advanced CE"
            );
        }
    }

    #[test]
    fn copied_lagging_ce_uses_native_receipts_and_keeps_its_marker_when_required_frame_is_missing()
    {
        for missing_frame in [false, true] {
            let fixture = DiskFixture::with_ce_height(H - 1, H - 1);
            let recipient = tempfile::tempdir().unwrap();
            fixture.copy_to(recipient.path());
            if missing_frame {
                let db = init_db(recipient.path().join("db"), DatabaseArguments::test()).unwrap();
                let tx = db.tx_mut().unwrap();
                tx.delete::<tables::BlockBodyIndices>(H, None).unwrap();
                tx.commit().unwrap();
            }
            let (provider, tree) = open_native(recipient.path());
            assert_eq!(tree.finalized_marker().unwrap().height, H - 1);
            let source = std::sync::Arc::new(RethDurableCeState::new(provider));
            let checkpoint = source.durable_checkpoint(H).unwrap().unwrap();
            assert_eq!(checkpoint.height, H);
            let recovery = CeStartupRecoveryCoordinator::new(source, tree.clone());
            let result = recovery.recover_before_participation(H);
            if missing_frame {
                assert!(result.is_err());
                assert_eq!(tree.finalized_marker().unwrap().height, H - 1);
            } else {
                let recovered = result.unwrap();
                assert_eq!(recovered.height, H);
                assert_eq!(
                    recovered.block_hash,
                    fixture.headers[H as usize].hash_slow()
                );
            }
        }
    }
    #[test]
    fn copied_current_tip_does_not_replace_missing_native_genesis_checkpoint() {
        let fixture = DiskFixture::new(H);
        let recipient = tempfile::tempdir().unwrap();
        fixture.copy_to(recipient.path());
        {
            let (provider, _) = open_native(recipient.path());
            let genesis = RethDurableCeState::new(provider)
                .durable_checkpoint(0)
                .unwrap()
                .unwrap();
            assert_eq!(genesis.block_hash, fixture.headers[0].hash_slow());
        }
        remove_native_header(recipient.path(), 0);
        let (provider, tree) = open_native(recipient.path());
        assert_eq!(tree.finalized_marker().unwrap().height, H);
        assert!(RethDurableCeState::new(provider)
            .durable_checkpoint(0)
            .unwrap()
            .is_none());
    }
}

mod copied_genesis_validator_history {
    use super::super::restart_recovery::copied_native::{open_native, DiskFixture, H};
    use crate::validators::read_consensus_validators_at_block;
    use alloy_consensus::Sealable;
    use alloy_primitives::{Address, B256, U256};
    use commonware_codec::Encode as _;
    use commonware_cryptography::{bls12381, Signer as _};
    use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
    use reth_ethereum::provider::db::{
        database::Database,
        init_db,
        mdbx::DatabaseArguments,
        models::ShardedKey,
        table::Table,
        tables,
        transaction::{DbTx, DbTxMut},
    };
    use std::{collections::BTreeSet, path::Path};

    type StorageWord = <tables::PlainStorageState as Table>::Value;
    type HistoryKey = <tables::StoragesHistory as Table>::Key;
    type HistoryBlocks = <tables::StoragesHistory as Table>::Value;
    const OWNER: Address = Address::repeat_byte(0xa0);
    const FOUNDER: Address = Address::with_last_byte(0x11);
    const JOINER: Address = Address::with_last_byte(0x22);

    fn consensus_key(seed: u64) -> bls12381::PublicKey {
        bls12381::PrivateKey::from_seed(seed).public_key()
    }

    fn activate(provider: &mut HashMapStorageProvider, address: Address, seed: u64) {
        let encoded = consensus_key(seed).encode();
        let public_key: [u8; 48] = encoded.as_ref().try_into().unwrap();
        StorageHandle::enter(provider, |storage| {
            let mut validators = outbe_validatorset::contract::ValidatorSet::new(storage);
            validators
                .register_validator(OWNER, address, &public_key)
                .unwrap();
            validators
                .activate_validator_via_boundary_for_test(address)
                .unwrap();
        });
    }

    // Build native contract words with the existing lifecycle fixture, then persist
    // both current state and actual historical before-values. The runtime reader
    // below only sees a reopened Reth provider, never this construction map.
    fn seed_distinct_validator_history(root: &Path) -> BTreeSet<Address> {
        let mut contract = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut contract, |storage| {
            let mut validators = outbe_validatorset::contract::ValidatorSet::new(storage);
            validators.config_owner.write(OWNER).unwrap();
            validators.config_is_initialized.write(true).unwrap();
            validators.set_config_max_validators(128).unwrap();
        });
        activate(&mut contract, FOUNDER, 11);
        let genesis = contract.storage.clone();
        contract.set_block_number(1);
        activate(&mut contract, JOINER, 22);
        let current = contract.storage;
        let keys: BTreeSet<_> = genesis.keys().chain(current.keys()).copied().collect();
        let addresses: BTreeSet<_> = keys.iter().map(|(address, _)| *address).collect();
        let mut changed_addresses = BTreeSet::new();
        let db = init_db(root.join("db"), DatabaseArguments::test()).unwrap();
        let tx = db.tx_mut().unwrap();
        for &address in &addresses {
            tx.put::<tables::PlainAccountState>(address, Default::default())
                .unwrap();
        }
        for (address, slot) in keys {
            let before = genesis.get(&(address, slot)).copied().unwrap_or_default();
            let after = current.get(&(address, slot)).copied().unwrap_or_default();
            let key = B256::from(slot.to_be_bytes::<32>());
            if !after.is_zero() {
                tx.put::<tables::PlainStorageState>(address, StorageWord { key, value: after })
                    .unwrap();
            }
            let mut writes = Vec::new();
            if !before.is_zero() {
                writes.push(0);
                tx.put::<tables::StorageChangeSets>(
                    (0, address).into(),
                    StorageWord {
                        key,
                        value: U256::ZERO,
                    },
                )
                .unwrap();
            }
            if before != after {
                changed_addresses.insert(address);
                writes.push(1);
                tx.put::<tables::StorageChangeSets>(
                    (1, address).into(),
                    StorageWord { key, value: before },
                )
                .unwrap();
            }
            if !writes.is_empty() {
                tx.put::<tables::StoragesHistory>(
                    HistoryKey {
                        address,
                        sharded_key: ShardedKey {
                            key,
                            highest_block_number: u64::MAX,
                        },
                    },
                    HistoryBlocks::new(writes).unwrap(),
                )
                .unwrap();
            }
        }
        assert!(
            !changed_addresses.is_empty(),
            "the genesis/current distinction must require native history"
        );
        tx.commit().unwrap();
        changed_addresses
    }

    fn assert_genesis_and_current(root: &Path, genesis_hash: B256, current_hash: B256) {
        let (provider, tree) = open_native(root);
        assert_eq!(tree.finalized_marker().unwrap().height, H);
        let genesis = read_consensus_validators_at_block(&provider, genesis_hash).unwrap();
        assert_eq!(genesis.addresses, vec![FOUNDER]);
        assert_eq!(genesis.public_keys, vec![consensus_key(11)]);
        let current = read_consensus_validators_at_block(&provider, current_hash).unwrap();
        assert_eq!(current.addresses, vec![FOUNDER, JOINER]);
        assert_eq!(
            current.public_keys,
            vec![consensus_key(11), consensus_key(22)]
        );
        assert_ne!(genesis.public_keys, current.public_keys);
    }

    #[test]
    fn copied_genesis_validator_trust_anchor_reads_native_history_instead_of_current_set() {
        let fixture = DiskFixture::new(H - 1);
        seed_distinct_validator_history(fixture.root.path());
        let genesis_hash = fixture.headers[0].hash_slow();
        let current_hash = fixture.headers[H as usize].hash_slow();
        assert_genesis_and_current(fixture.root.path(), genesis_hash, current_hash);
        let recipient = tempfile::tempdir().unwrap();
        fixture.copy_to(recipient.path());
        // The donor may disappear; the trust reader is given only the copied native provider.
        drop(fixture);
        assert_genesis_and_current(recipient.path(), genesis_hash, current_hash);
        assert_genesis_and_current(recipient.path(), genesis_hash, current_hash);
    }

    #[test]
    fn copied_genesis_validator_trust_anchor_rejects_missing_required_storage_changeset() {
        let fixture = DiskFixture::new(H - 1);
        let addresses = seed_distinct_validator_history(fixture.root.path());
        let genesis_hash = fixture.headers[0].hash_slow();
        let current_hash = fixture.headers[H as usize].hash_slow();
        let recipient = tempfile::tempdir().unwrap();
        fixture.copy_to(recipient.path());
        assert_genesis_and_current(recipient.path(), genesis_hash, current_hash);
        {
            let db = init_db(recipient.path().join("db"), DatabaseArguments::test()).unwrap();
            let tx = db.tx_mut().unwrap();
            // Leave the history index, headers, latest state and CE at H intact.
            // It now references missing native before-values for the block-1 change.
            for &address in &addresses {
                assert!(tx
                    .delete::<tables::StorageChangeSets>((1, address).into(), None)
                    .unwrap());
            }
            tx.commit().unwrap();
        }
        {
            let (provider, tree) = open_native(recipient.path());
            assert_eq!(tree.finalized_marker().unwrap().height, H);
            let current = read_consensus_validators_at_block(&provider, current_hash).unwrap();
            assert_eq!(current.addresses, vec![FOUNDER, JOINER]);
            assert_eq!(
                current.public_keys,
                vec![consensus_key(11), consensus_key(22)]
            );
            let error = read_consensus_validators_at_block(&provider, genesis_hash).unwrap_err();
            let message = format!("{error:#}");
            assert!(message.contains("storage change set"), "{message}");
            assert!(message.contains("at block #1 does not exist"), "{message}");
            assert_eq!(tree.finalized_marker().unwrap().height, H);
        }
        // The damaged copy did not repair its history or alter the donor.
        {
            let db = init_db(recipient.path().join("db"), DatabaseArguments::test()).unwrap();
            let tx = db.tx().unwrap();
            for &address in &addresses {
                assert!(tx
                    .get::<tables::StorageChangeSets>((1, address).into())
                    .unwrap()
                    .is_none());
            }
            assert_eq!(
                tx.get::<tables::CanonicalHeaders>(0).unwrap(),
                Some(genesis_hash)
            );
            assert_eq!(
                tx.get::<tables::CanonicalHeaders>(H).unwrap(),
                Some(current_hash)
            );
        }
        assert_genesis_and_current(fixture.root.path(), genesis_hash, current_hash);
    }
}

mod copied_native_dkg_prerequisites {
    use super::super::restart_recovery::copied_native::{
        open_native, remove_native_header, replace_native_header, DiskFixture, H,
    };
    use super::*;
    use alloy_consensus::Sealable;
    use alloy_primitives::U256;
    use eyre::ensure;
    use outbe_evm::OutbeEvmConfig;
    use outbe_node::{OutbeBeaconConsensus, OutbeFullNode, OutbeNode, OutbePoolBuilder};
    use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
    use reth_ethereum::{
        network::NetworkManager,
        node::core::{args::DatadirArgs, primitives::Head},
        provider::db::{
            database::Database,
            init_db,
            mdbx::DatabaseArguments,
            models::ShardedKey,
            table::Table,
            tables,
            transaction::{DbTx, DbTxMut},
            DatabaseEnv,
        },
        rpc::builder::auth::AuthServerHandle,
        tasks::Runtime,
    };
    use reth_node_builder::{
        common::WithConfigs,
        components::{Components, NoopPayloadServiceBuilder, PayloadServiceBuilder, PoolBuilder},
        AddOnsContext, BuilderContext, ConsensusEngineHandle, FullNode, Node, NodeAdapter,
        NodeConfig, RethFullAdapter,
    };
    use reth_provider::{BlockHashReader, ChainSpecProvider, HeaderProvider, StateProviderFactory};
    use std::{collections::BTreeSet, panic::AssertUnwindSafe, path::Path};

    type Native = RethFullAdapter<DatabaseEnv, OutbeNode>;
    type AddOns = <OutbeNode as Node<Native>>::AddOns;
    type StorageWord = <tables::PlainStorageState as Table>::Value;
    type HistoryKey = <tables::StoragesHistory as Table>::Key;
    type HistoryBlocks = <tables::StoragesHistory as Table>::Value;
    const FREEZE: u64 = 1;
    const CHANGE: u64 = 2;
    const OWNER: Address = Address::repeat_byte(0xa0);
    const MEMBERS: [Address; 3] = [
        Address::with_last_byte(0x11),
        Address::with_last_byte(0x22),
        Address::with_last_byte(0x33),
    ];
    const JOINER: Address = Address::with_last_byte(0x44);

    fn register_active(
        provider: &mut HashMapStorageProvider,
        address: Address,
        key: &bls12381::PublicKey,
    ) {
        let encoded = key.encode();
        let bytes: [u8; 48] = encoded.as_ref().try_into().unwrap();
        StorageHandle::enter(provider, |storage| {
            let mut contract = outbe_validatorset::contract::ValidatorSet::new(storage);
            contract.register_validator(OWNER, address, &bytes).unwrap();
            contract
                .activate_validator_via_boundary_for_test(address)
                .unwrap();
        });
    }

    fn seed_freeze_history(root: &Path, keys: &[bls12381::PrivateKey]) -> BTreeSet<Address> {
        let mut contract = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut contract, |storage| {
            let mut validators = outbe_validatorset::contract::ValidatorSet::new(storage);
            validators.config_owner.write(OWNER).unwrap();
            validators.config_is_initialized.write(true).unwrap();
            validators.set_config_max_validators(128).unwrap();
        });
        for (&address, key) in MEMBERS.iter().zip(keys) {
            register_active(&mut contract, address, &key.public_key());
        }
        let frozen = contract.storage.clone();
        contract.set_block_number(CHANGE);
        StorageHandle::enter(&mut contract, |storage| {
            outbe_validatorset::contract::ValidatorSet::new(storage)
                .deactivate_validator(OWNER, MEMBERS[0])
                .unwrap();
        });
        register_active(
            &mut contract,
            JOINER,
            &bls12381::PrivateKey::from_seed(44).public_key(),
        );
        let current = contract.storage;
        let keys: BTreeSet<_> = frozen.keys().chain(current.keys()).copied().collect();
        let addresses: BTreeSet<_> = keys.iter().map(|(address, _)| *address).collect();
        let mut changed_addresses = BTreeSet::new();
        let db = init_db(root.join("db"), DatabaseArguments::test()).unwrap();
        let tx = db.tx_mut().unwrap();
        for address in addresses {
            tx.put::<tables::PlainAccountState>(address, Default::default())
                .unwrap();
        }
        for (address, slot) in keys {
            let before = frozen.get(&(address, slot)).copied().unwrap_or_default();
            let after = current.get(&(address, slot)).copied().unwrap_or_default();
            let key = B256::from(slot.to_be_bytes::<32>());
            if !after.is_zero() {
                tx.put::<tables::PlainStorageState>(address, StorageWord { key, value: after })
                    .unwrap();
            }
            let mut writes = Vec::new();
            if !before.is_zero() {
                writes.push(0);
                tx.put::<tables::StorageChangeSets>(
                    (0, address).into(),
                    StorageWord {
                        key,
                        value: U256::ZERO,
                    },
                )
                .unwrap();
            }
            if before != after {
                changed_addresses.insert(address);
                writes.push(CHANGE);
                tx.put::<tables::StorageChangeSets>(
                    (CHANGE, address).into(),
                    StorageWord { key, value: before },
                )
                .unwrap();
            }
            if !writes.is_empty() {
                tx.put::<tables::StoragesHistory>(
                    HistoryKey {
                        address,
                        sharded_key: ShardedKey {
                            key,
                            highest_block_number: u64::MAX,
                        },
                    },
                    HistoryBlocks::new(writes).unwrap(),
                )
                .unwrap();
            }
        }
        assert!(!changed_addresses.is_empty());
        tx.commit().unwrap();
        changed_addresses
    }

    // All services belong to this invocation. The copied provider is installed
    // initially; no execution, peer loop, consensus engine or process is started.
    fn with_native_components(
        root: &Path,
        check: impl FnOnce(&OutbeFullNode) -> eyre::Result<()>,
    ) -> eyre::Result<()> {
        let executor = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(2)
            .enable_all()
            .build()?;
        let outcome = executor.block_on(async {
            let runtime = Runtime::test();
            let manager = runtime
                .take_task_manager_handle()
                .expect("fixture task manager");
            let mut payload_shutdown = None;
            let result = AssertUnwindSafe(async {
                let (provider, tree) = open_native(root);
                ensure!(
                    tree.finalized_marker()?.height == H,
                    "copied CE height changed"
                );
                let chain = provider.chain_spec();
                let header = provider
                    .sealed_header(H)?
                    .ok_or_else(|| eyre::eyre!("missing copied head"))?;
                let mut config = NodeConfig::new(chain.clone())
                    .with_unused_ports()
                    .with_datadir_args(DatadirArgs {
                        datadir: root.join("test-components").into(),
                        ..Default::default()
                    });
                config.rpc.http = false;
                config.rpc.ws = false;
                config.rpc.ipcdisable = true;
                config.rpc.disable_auth_server = true;
                config.network.bootnodes = Some(Vec::new());
                config.network.discovery.disable_discovery = true;
                config.txpool.disable_blobs_support = true;
                config.txpool.additional_validation_tasks = 0;
                config.txpool.disable_transactions_backup = true;
                std::fs::create_dir_all(config.datadir().data_dir())?;
                let head = Head {
                    number: H,
                    hash: header.hash(),
                    difficulty: header.header().inner.difficulty,
                    total_difficulty: U256::ZERO,
                    timestamp: header.header().inner.timestamp,
                };
                let ctx = BuilderContext::<Native>::new(
                    head,
                    provider.clone(),
                    runtime.clone(),
                    WithConfigs {
                        config: config.clone(),
                        toml_config: Default::default(),
                    },
                );
                let evm = OutbeEvmConfig::new(chain.clone());
                let pool = OutbePoolBuilder::default()
                    .build_pool(&ctx, evm.clone())
                    .await?;
                let network_config = ctx.build_network_config(
                    ctx.network_config_builder()?
                        .disable_discovery()
                        .disable_nat()
                        .listener_addr(([127, 0, 0, 1], 0).into()),
                );
                let network_owner = NetworkManager::builder(network_config).await?;
                let network = network_owner.handle();
                let payload = NoopPayloadServiceBuilder::default()
                    .spawn_payload_builder_service(&ctx, pool.clone(), evm.clone())
                    .await?;
                // The builder retains every subscription sender until the service
                // exits. Closure of this receiver witnesses its shutdown.
                payload_shutdown = Some(
                    tokio::time::timeout(Duration::from_secs(10), payload.subscribe()).await??,
                );
                let adapter: NodeAdapter<Native> = NodeAdapter {
                    components: Components {
                        transaction_pool: pool.clone(),
                        evm_config: evm.clone(),
                        consensus: Arc::new(OutbeBeaconConsensus::new(chain)),
                        network: network.clone(),
                        payload_builder_handle: payload.clone(),
                    },
                    task_executor: runtime.clone(),
                    provider: provider.clone(),
                };
                let (engine_tx, _engine_rx) = tokio::sync::mpsc::unbounded_channel();
                let addons = AddOns::default()
                    .launch_add_ons_with_opt_engine(
                        AddOnsContext {
                            node: adapter,
                            config: &config,
                            beacon_engine_handle: ConsensusEngineHandle::new(engine_tx),
                            engine_events: Default::default(),
                            jwt_secret: *AuthServerHandle::noop().jwt_secret(),
                        },
                        |_| Ok(()),
                        true,
                    )
                    .await?;
                ensure!(addons.rpc_server_handles.rpc.http_local_addr().is_none());
                ensure!(addons.rpc_server_handles.rpc.ws_local_addr().is_none());
                ensure!(addons.rpc_server_handles.rpc.ipc_endpoint().is_none());
                let node: OutbeFullNode = FullNode {
                    evm_config: evm,
                    pool,
                    network,
                    provider,
                    payload_builder_handle: payload,
                    task_executor: runtime.clone(),
                    data_dir: config.datadir(),
                    config,
                    add_ons_handle: addons,
                };
                let result = check(&node);
                drop(node);
                drop(network_owner);
                result
            })
            .catch_unwind()
            .await;
            // Always signal and observe shutdown, including assertion panics or
            // partially constructed components. The outer runtime is then dropped
            // before callers may mutate/reopen any native database.
            let _ = runtime.initiate_graceful_shutdown();
            let manager_result = tokio::time::timeout(Duration::from_secs(10), manager).await;
            let payload_result = if let Some(events) = payload_shutdown {
                tokio::time::timeout(Duration::from_secs(10), events.recv())
                    .await
                    .map(|event| event.is_none())
            } else {
                Ok(true)
            };
            let shutdown_runtime = runtime.clone();
            let graceful = tokio::task::spawn_blocking(move || {
                shutdown_runtime.graceful_shutdown_with_timeout(Duration::from_secs(10))
            })
            .await;
            let shutdown: eyre::Result<()> = (|| {
                manager_result.wrap_err("fixture task manager shutdown timed out")???;
                ensure!(
                    payload_result.wrap_err("fixture payload service shutdown timed out")?,
                    "unexpected payload event"
                );
                ensure!(
                    graceful.wrap_err("fixture shutdown waiter panicked")?,
                    "fixture graceful tasks did not stop"
                );
                Ok(())
            })();
            (result, shutdown)
        });
        drop(executor);
        match outcome {
            (Err(panic), _) => std::panic::resume_unwind(panic),
            (Ok(result), shutdown) => {
                shutdown?;
                result
            }
        }
    }

    struct PendingFixture {
        disk: DiskFixture,
        snapshot: PendingDkgBoundarySnapshot,
        keys: Vec<bls12381::PrivateKey>,
        participants: Set<bls12381::PublicKey>,
        output: Output<MinSig, bls12381::PublicKey>,
        share: Share,
        polynomial: Sharing<MinSig>,
        changed_addresses: BTreeSet<Address>,
    }

    impl PendingFixture {
        fn new() -> Self {
            let disk = DiskFixture::new(H - 1);
            let (keys, participants, output, share, polynomial) = run_test_dkg_complete();
            let changed_addresses = seed_freeze_history(disk.root.path(), &keys);
            let validator_set = {
                let (provider, _tree) = open_native(disk.root.path());
                let state = provider
                    .state_by_block_hash(disk.headers[FREEZE as usize].hash_slow())
                    .unwrap();
                let frozen =
                    validators::read_reshare_target_with_empty_tee_exclusions_from_state(&state)
                        .unwrap();
                assert_eq!(frozen.validator_set.addresses, MEMBERS);
                frozen.validator_set
            };
            let artifact =
                dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
                    epoch: Epoch::new(1),
                    validator_set: &validator_set,
                    output: &output,
                    is_full_dkg: false,
                    dkg_cycle: 1,
                    freeze_height: FREEZE,
                    planned_activation_height: H + 1,
                    vrf_material_version: 1,
                    is_validator_set_change: false,
                    tee_expired_target_exclusions: Vec::new(),
                })
                .unwrap();
            let snapshot = PendingDkgBoundarySnapshot {
                artifact,
                completed_at_height: CHANGE,
            };
            Self {
                disk,
                snapshot,
                keys,
                participants,
                output,
                share,
                polynomial,
                changed_addresses,
            }
        }

        fn copy_for_same_participant(&self, recipient: &Path) {
            self.disk.copy_to(recipient);
            // The private share is this recipient's own ceremony output. It is
            // provisioned independently of the copied public native chain files.
            let own = recipient.join("own-dkg");
            std::fs::create_dir_all(&own).unwrap();
            save_pending_dkg_state(
                &own,
                &self.share,
                &self.polynomial,
                &self.output,
                &bls::KeyBackend::Plaintext,
            )
            .unwrap();
            save_pending_dkg_boundary(&own, &self.snapshot).unwrap();
        }

        fn assert_restored(&self, root: &Path, node: &OutbeFullNode) -> eyre::Result<()> {
            let refreshed = refresh_validator_set_at_height(node, FREEZE)?;
            let FrozenValidatorSetRefresh::Ready {
                validator_set,
                participants,
                tee_expired_target_exclusions,
            } = refreshed
            else {
                eyre::bail!("present native freeze state is unavailable");
            };
            ensure!(validator_set.addresses == MEMBERS);
            ensure!(participants == self.participants);
            ensure!(tee_expired_target_exclusions.is_empty());
            let current_state = node
                .provider
                .state_by_block_hash(self.disk.headers[H as usize].hash_slow())?;
            let current = validators::read_reshare_target_with_empty_tee_exclusions_from_state(
                &current_state,
            )?;
            ensure!(current.validator_set.addresses == vec![MEMBERS[1], MEMBERS[2], JOINER]);
            let own = root.join("own-dkg");
            let snapshot = load_pending_dkg_boundary(&own)?.expect("durable pending boundary");
            let restored = restore_pending_dkg_activation(
                snapshot,
                &own,
                &bls::KeyBackend::Plaintext,
                &self.keys[0].public_key(),
                node,
            )?;
            let RestoredPendingDkgActivation::Participant(restored) = restored else {
                eyre::bail!("local participant was incorrectly restored as dealer-only");
            };
            ensure!(restored.target.freeze_height == FREEZE);
            ensure!(restored.target.validator_set.addresses == MEMBERS);
            ensure!(restored.target.participants == self.participants);
            ensure!(restored.complete.output == self.output);
            ensure!(restored.complete.share == self.share);
            ensure!(restored.boundary_artifact == self.snapshot.artifact);
            ensure!(restored.recovered_output.as_ref() == Some(&self.output));
            Ok(())
        }
    }

    fn pending_files(root: &Path) -> BTreeMap<String, Vec<u8>> {
        std::fs::read_dir(root.join("own-dkg"))
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                assert!(entry.file_type().unwrap().is_file());
                (
                    entry.file_name().into_string().unwrap(),
                    std::fs::read(entry.path()).unwrap(),
                )
            })
            .collect()
    }

    #[test]
    fn copied_native_dkg_restore_uses_freeze_history_and_recipient_pending_share_on_each_reopen() {
        let fixture = PendingFixture::new();
        let recipient = tempfile::tempdir().unwrap();
        fixture.copy_for_same_participant(recipient.path());
        let before = pending_files(recipient.path());
        for _ in 0..2 {
            with_native_components(recipient.path(), |node| {
                fixture.assert_restored(recipient.path(), node)
            })
            .unwrap();
            assert_eq!(pending_files(recipient.path()), before);
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum MissingPrerequisite {
        HeaderSegment,
        CorruptHeaderNumber,
        StorageChangeset,
    }

    #[test]
    fn copied_native_dkg_restore_rejects_missing_freeze_segment_corrupt_header_or_required_history()
    {
        let fixture = PendingFixture::new();
        for missing in [
            MissingPrerequisite::HeaderSegment,
            MissingPrerequisite::CorruptHeaderNumber,
            MissingPrerequisite::StorageChangeset,
        ] {
            let recipient = tempfile::tempdir().unwrap();
            fixture.copy_for_same_participant(recipient.path());
            with_native_components(recipient.path(), |node| {
                fixture.assert_restored(recipient.path(), node)
            })
            .unwrap();
            let before = pending_files(recipient.path());
            match missing {
                MissingPrerequisite::HeaderSegment => {
                    remove_native_header(recipient.path(), FREEZE)
                }
                MissingPrerequisite::CorruptHeaderNumber => {
                    let mut invalid = fixture.disk.headers[FREEZE as usize].clone();
                    invalid.inner.number = FREEZE + 1;
                    replace_native_header(
                        recipient.path(),
                        FREEZE,
                        &invalid,
                        fixture.disk.headers[FREEZE as usize].hash_slow(),
                    );
                }
                MissingPrerequisite::StorageChangeset => {
                    let db =
                        init_db(recipient.path().join("db"), DatabaseArguments::test()).unwrap();
                    let tx = db.tx_mut().unwrap();
                    for &address in &fixture.changed_addresses {
                        assert!(tx
                            .delete::<tables::StorageChangeSets>((CHANGE, address).into(), None)
                            .unwrap());
                    }
                    tx.commit().unwrap();
                }
            }
            with_native_components(recipient.path(), |node| {
                ensure!(node.provider.block_hash(H)? == Some(fixture.disk.headers[H as usize].hash_slow()));
                let current_state = node.provider.state_by_block_hash(fixture.disk.headers[H as usize].hash_slow())?;
                let current = validators::read_reshare_target_with_empty_tee_exclusions_from_state(&current_state)?;
                ensure!(current.validator_set.addresses == vec![MEMBERS[1], MEMBERS[2], JOINER]);
                match missing {
                    MissingPrerequisite::HeaderSegment => {
                        ensure!(matches!(refresh_validator_set_at_height(node, FREEZE)?, FrozenValidatorSetRefresh::PendingBlockHash));
                    }
                    MissingPrerequisite::CorruptHeaderNumber => {
                        let error = refresh_validator_set_at_height(node, FREEZE).err()
                            .expect("native invalid freeze header must fail");
                        ensure!(format!("{error:#}").contains("canonical freeze header number mismatch"));
                    }
                    MissingPrerequisite::StorageChangeset => {
                        let error = refresh_validator_set_at_height(node, FREEZE).err().expect("native missing history must fail");
                        ensure!(format!("{error:#}").contains("storage change set"));
                    }
                }
                let own = recipient.path().join("own-dkg");
                let snapshot = load_pending_dkg_boundary(&own)?.expect("pending boundary still exists");
                let error = restore_pending_dkg_activation(snapshot, &own, &bls::KeyBackend::Plaintext, &fixture.keys[0].public_key(), node)
                    .err().expect("ordinary restore must reject missing prerequisite");
                let message = format!("{error:#}");
                match missing {
                    MissingPrerequisite::HeaderSegment => {
                        ensure!(message.contains("pending DKG freeze-height state unavailable at height 1 during runtime restore"), "{message}");
                    }
                    MissingPrerequisite::CorruptHeaderNumber => {
                        ensure!(message.contains("canonical freeze header number mismatch"), "{message}");
                    }
                    MissingPrerequisite::StorageChangeset => {
                        ensure!(message.contains("storage change set") && message.contains("at block #2 does not exist"), "{message}");
                    }
                }
                Ok(())
            }).unwrap();
            assert_eq!(
                pending_files(recipient.path()),
                before,
                "{missing:?} changed pending material"
            );
            let (provider, tree) = open_native(recipient.path());
            assert_eq!(tree.finalized_marker().unwrap().height, H);
            assert_eq!(
                provider.block_hash(H).unwrap(),
                Some(fixture.disk.headers[H as usize].hash_slow())
            );
            // Ordinary failure must not reconstruct the damaged prerequisite.
            match missing {
                MissingPrerequisite::HeaderSegment => {
                    assert!(provider.block_hash(FREEZE).unwrap().is_none());
                    assert!(provider.sealed_header(FREEZE).unwrap().is_none());
                }
                MissingPrerequisite::CorruptHeaderNumber => {
                    let header = provider.sealed_header(FREEZE).unwrap().unwrap();
                    assert_eq!(header.header().inner.number, FREEZE + 1);
                    assert_eq!(
                        header.hash(),
                        fixture.disk.headers[FREEZE as usize].hash_slow()
                    );
                }
                MissingPrerequisite::StorageChangeset => {
                    drop(provider);
                    drop(tree);
                    let db =
                        init_db(recipient.path().join("db"), DatabaseArguments::test()).unwrap();
                    let tx = db.tx().unwrap();
                    for &address in &fixture.changed_addresses {
                        assert!(tx
                            .get::<tables::StorageChangeSets>((CHANGE, address).into())
                            .unwrap()
                            .is_none());
                    }
                }
            }
        }
    }
}
