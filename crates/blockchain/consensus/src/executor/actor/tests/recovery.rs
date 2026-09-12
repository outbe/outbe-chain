use super::*;

#[test]
fn recovered_forkchoice_attempt_sends_exact_finalized_identity_without_attributes() {
    commonware_runtime::deterministic::Runner::default().start(|context| async move {
        let genesis = B256::repeat_byte(0x01);
        let recovered = ProjectionCheckpoint {
            block_number: 7,
            block_hash: B256::repeat_byte(0x77),
        };
        let (engine_tx, mut engine_rx) = tokio::sync::mpsc::unbounded_channel();
        let engine = ConsensusEngineHandle::new(engine_tx);
        let (_projection_publisher, projection_readiness) = ready_projection(genesis, recovered);
        let (actor, _mailbox) = super::ExecutorActor::new(
            context.child("test"),
            engine,
            genesis,
            recovered.block_number,
            recovered.block_hash,
            projection_readiness,
            None,
        );

        let engine_task = context.child("engine_task").spawn(move |_ctx| async move {
            let Some(message) = engine_rx.recv().await else {
                panic!("recovered forkchoice attempt must send an FCU message");
            };
            match message {
                BeaconEngineMessage::ForkchoiceUpdated {
                    state,
                    payload_attrs,
                    tx,
                } => {
                    assert_eq!(state.head_block_hash, recovered.block_hash);
                    assert_eq!(state.safe_block_hash, recovered.block_hash);
                    assert_eq!(state.finalized_block_hash, recovered.block_hash);
                    assert!(payload_attrs.is_none());
                    tx.send(Ok(OnForkChoiceUpdated::valid(PayloadStatus::from_status(
                        PayloadStatusEnum::Valid,
                    ))))
                    .expect("test engine response receiver must be alive");
                }
                other => panic!("unexpected engine message: {other:?}"),
            }
        });

        let outcome = actor.replay_recovered_forkchoice_once(recovered).await;
        assert_eq!(outcome, super::RecoveredForkchoiceAttempt::Valid);
        engine_task.await.expect("engine task must complete");
    });
}

#[test]
fn recovered_forkchoice_attempt_rejects_anchor_mismatch_before_engine_io() {
    commonware_runtime::deterministic::Runner::default().start(|context| async move {
        let genesis = B256::repeat_byte(0x01);
        let recovered = ProjectionCheckpoint {
            block_number: 7,
            block_hash: B256::repeat_byte(0x77),
        };
        let expected = ProjectionCheckpoint {
            block_number: 8,
            block_hash: B256::repeat_byte(0x88),
        };
        let (engine_tx, mut engine_rx) = tokio::sync::mpsc::unbounded_channel();
        let engine = ConsensusEngineHandle::new(engine_tx);
        let (_projection_publisher, projection_readiness) = ready_projection(genesis, recovered);
        let (actor, _mailbox) = super::ExecutorActor::new(
            context,
            engine,
            genesis,
            recovered.block_number,
            recovered.block_hash,
            projection_readiness,
            None,
        );

        let outcome = actor.replay_recovered_forkchoice_once(expected).await;

        assert!(matches!(
            outcome,
            super::RecoveredForkchoiceAttempt::Fatal(message)
                if message.contains("does not match startup anchor")
        ));
        assert!(engine_rx.try_recv().is_err());
    });
}

#[test]
fn recovered_forkchoice_attempt_repeats_identical_state_after_syncing() {
    commonware_runtime::deterministic::Runner::default().start(|context| async move {
        let genesis = B256::repeat_byte(0x01);
        let recovered = ProjectionCheckpoint {
            block_number: 7,
            block_hash: B256::repeat_byte(0x77),
        };
        let (engine_tx, mut engine_rx) = tokio::sync::mpsc::unbounded_channel();
        let engine = ConsensusEngineHandle::new(engine_tx);
        let (_projection_publisher, projection_readiness) = ready_projection(genesis, recovered);
        let (actor, _mailbox) = super::ExecutorActor::new(
            context.child("test"),
            engine,
            genesis,
            recovered.block_number,
            recovered.block_hash,
            projection_readiness,
            None,
        );

        let engine_task = context.child("engine_task").spawn(move |_ctx| async move {
            for syncing in [true, false] {
                let Some(BeaconEngineMessage::ForkchoiceUpdated {
                    state,
                    payload_attrs,
                    tx,
                }) = engine_rx.recv().await
                else {
                    panic!("expected recovered FCU attempt");
                };
                assert_eq!(state.head_block_hash, recovered.block_hash);
                assert_eq!(state.safe_block_hash, recovered.block_hash);
                assert_eq!(state.finalized_block_hash, recovered.block_hash);
                assert!(payload_attrs.is_none());
                let response = if syncing {
                    OnForkChoiceUpdated::syncing()
                } else {
                    OnForkChoiceUpdated::valid(PayloadStatus::from_status(PayloadStatusEnum::Valid))
                };
                tx.send(Ok(response))
                    .expect("test engine response receiver must be alive");
            }
        });

        assert_eq!(
            actor.replay_recovered_forkchoice_once(recovered).await,
            super::RecoveredForkchoiceAttempt::Syncing,
        );
        assert_eq!(
            actor.replay_recovered_forkchoice_once(recovered).await,
            super::RecoveredForkchoiceAttempt::Valid,
        );
        engine_task.await.expect("engine task must complete");
    });
}

#[test]
fn recovered_forkchoice_attempt_distinguishes_payload_invalid_from_hard_error() {
    commonware_runtime::deterministic::Runner::default().start(|context| async move {
        let genesis = B256::repeat_byte(0x01);
        let recovered = ProjectionCheckpoint {
            block_number: 7,
            block_hash: B256::repeat_byte(0x77),
        };
        let (engine_tx, mut engine_rx) = tokio::sync::mpsc::unbounded_channel();
        let engine = ConsensusEngineHandle::new(engine_tx);
        let (_projection_publisher, projection_readiness) = ready_projection(genesis, recovered);
        let (actor, _mailbox) = super::ExecutorActor::new(
            context.child("test"),
            engine,
            genesis,
            recovered.block_number,
            recovered.block_hash,
            projection_readiness,
            None,
        );

        let engine_task = context.child("engine_task").spawn(move |_ctx| async move {
            let Some(BeaconEngineMessage::ForkchoiceUpdated { tx, .. }) = engine_rx.recv().await
            else {
                panic!("expected payload-invalid recovered FCU attempt");
            };
            tx.send(Ok(OnForkChoiceUpdated::with_invalid(
                PayloadStatus::from_status(PayloadStatusEnum::Invalid {
                    validation_error: "test invalid payload".to_string(),
                }),
            )))
            .expect("test engine response receiver must be alive");

            let Some(BeaconEngineMessage::ForkchoiceUpdated { tx, .. }) = engine_rx.recv().await
            else {
                panic!("expected invalid-state recovered FCU attempt");
            };
            tx.send(Ok(OnForkChoiceUpdated::invalid_state()))
                .expect("test engine response receiver must be alive");
        });

        assert!(matches!(
            actor.replay_recovered_forkchoice_once(recovered).await,
            super::RecoveredForkchoiceAttempt::Invalid(message)
                if message.contains("test invalid payload")
        ));
        assert!(matches!(
            actor.replay_recovered_forkchoice_once(recovered).await,
            super::RecoveredForkchoiceAttempt::Fatal(message)
                if message.contains("invalid forkchoice state")
        ));
        engine_task.await.expect("engine task must complete");
    });
}

#[test]
fn recovered_forkchoice_attempt_can_repeat_after_lost_response() {
    commonware_runtime::deterministic::Runner::default().start(|context| async move {
        let genesis = B256::repeat_byte(0x01);
        let recovered = ProjectionCheckpoint {
            block_number: 7,
            block_hash: B256::repeat_byte(0x77),
        };
        let (engine_tx, mut engine_rx) = tokio::sync::mpsc::unbounded_channel();
        let engine = ConsensusEngineHandle::new(engine_tx);
        let (_projection_publisher, projection_readiness) = ready_projection(genesis, recovered);
        let (actor, _mailbox) = super::ExecutorActor::new(
            context.child("test"),
            engine,
            genesis,
            recovered.block_number,
            recovered.block_hash,
            projection_readiness,
            None,
        );

        let engine_task = context.child("engine_task").spawn(move |_ctx| async move {
            let Some(BeaconEngineMessage::ForkchoiceUpdated { state, tx, .. }) =
                engine_rx.recv().await
            else {
                panic!("expected recovered FCU attempt with lost response");
            };
            assert_eq!(state.finalized_block_hash, recovered.block_hash);
            drop(tx);

            let Some(BeaconEngineMessage::ForkchoiceUpdated { state, tx, .. }) =
                engine_rx.recv().await
            else {
                panic!("expected repeated recovered FCU attempt");
            };
            assert_eq!(state.finalized_block_hash, recovered.block_hash);
            tx.send(Ok(OnForkChoiceUpdated::valid(PayloadStatus::from_status(
                PayloadStatusEnum::Valid,
            ))))
            .expect("test engine response receiver must be alive");
        });

        assert!(matches!(
            actor.replay_recovered_forkchoice_once(recovered).await,
            super::RecoveredForkchoiceAttempt::Retryable(_)
        ));
        assert_eq!(
            actor.replay_recovered_forkchoice_once(recovered).await,
            super::RecoveredForkchoiceAttempt::Valid,
        );
        engine_task.await.expect("engine task must complete");
    });
}
