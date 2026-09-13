use super::*;

struct GatedCeCommitter {
    called: Mutex<Option<tokio::sync::oneshot::Sender<FinalizedCeBlock>>>,
    release: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
}

impl FinalizedCeCommitter for GatedCeCommitter {
    fn commit_finalized(
        &self,
        block: FinalizedCeBlock,
    ) -> futures::future::BoxFuture<'static, eyre::Result<()>> {
        self.called
            .lock()
            .expect("called lock")
            .take()
            .expect("single finalized commit")
            .send(block)
            .expect("test must observe commit barrier");
        let release = self
            .release
            .lock()
            .expect("release lock")
            .take()
            .expect("single finalized commit");
        Box::pin(async move {
            release
                .await
                .map_err(|_| eyre::eyre!("test release dropped"))?;
            Ok(())
        })
    }
}

#[test]
fn finalized_syncing_delivery_acks_and_heartbeat_repeats_fcu() {
    commonware_runtime::deterministic::Runner::default().start(|context| async move {
        let genesis = B256::repeat_byte(0x01);
        let block = executor_test_block(7, 0x77);
        let finalized_hash = block.block_hash();
        let (engine_tx, mut engine_rx) = tokio::sync::mpsc::unbounded_channel();
        let engine = ConsensusEngineHandle::new(engine_tx);
        let (_projection_publisher, projection_readiness) =
            ready_projection_for_block(genesis, &block);
        let (mut actor, _mailbox) = super::ExecutorActor::new(
            context.child("test"),
            engine,
            genesis,
            0,
            genesis,
            projection_readiness,
            None,
        );

        let engine_task = context.child("engine_task").spawn(move |_ctx| async move {
            let Some(message) = engine_rx.recv().await else {
                panic!("engine channel closed before new_payload");
            };
            match message {
                BeaconEngineMessage::NewPayload { tx, .. } => tx
                    .send(Ok(PayloadStatus::from_status(PayloadStatusEnum::Syncing)))
                    .expect("new_payload response receiver must be alive"),
                other => panic!("unexpected first engine message: {other:?}"),
            }

            let Some(message) = engine_rx.recv().await else {
                panic!("engine channel closed before finalized FCU");
            };
            match message {
                BeaconEngineMessage::ForkchoiceUpdated {
                    state,
                    payload_attrs,
                    tx,
                } => {
                    assert_eq!(state.head_block_hash, finalized_hash);
                    assert_eq!(state.finalized_block_hash, finalized_hash);
                    assert!(payload_attrs.is_none());
                    tx.send(Ok(OnForkChoiceUpdated::syncing()))
                        .expect("finalized FCU response receiver must be alive");
                }
                other => panic!("unexpected second engine message: {other:?}"),
            }

            let Some(message) = engine_rx.recv().await else {
                panic!("engine channel closed before heartbeat FCU");
            };
            match message {
                BeaconEngineMessage::ForkchoiceUpdated {
                    state,
                    payload_attrs,
                    tx,
                } => {
                    assert_eq!(state.head_block_hash, finalized_hash);
                    assert_eq!(state.finalized_block_hash, finalized_hash);
                    assert!(payload_attrs.is_none());
                    tx.send(Ok(OnForkChoiceUpdated::valid(PayloadStatus::from_status(
                        PayloadStatusEnum::Valid,
                    ))))
                    .expect("heartbeat FCU response receiver must be alive");
                }
                other => panic!("unexpected third engine message: {other:?}"),
            }
        });

        let (ack, waiter) = Exact::handle();
        actor
            .handle_marshal_update(Update::Block(block.into(), ack))
            .await
            .expect("finalized Syncing delivery must process without a fatal error");
        waiter
            .await
            .expect("finalized Syncing delivery must acknowledge marshal");
        assert_eq!(actor.state.finalized_height, Height::new(7));
        assert_eq!(actor.state.forkchoice.finalized_block_hash, finalized_hash);

        actor.send_fcu_heartbeat().await;
        engine_task.await.expect("engine task must complete");
    });
}

#[test]
fn canonical_genesis_anchor_is_acknowledged_without_execution() {
    commonware_runtime::deterministic::Runner::default().start(|context| async move {
        let block = executor_test_block(0, 0x00);
        let genesis = block.block_hash();
        let (engine_tx, mut engine_rx) = tokio::sync::mpsc::unbounded_channel();
        let engine = ConsensusEngineHandle::new(engine_tx);
        let (_projection_publisher, projection_readiness) = ready_projection(
            genesis,
            ProjectionCheckpoint {
                block_number: 0,
                block_hash: genesis,
            },
        );
        let (mut actor, _mailbox) = super::ExecutorActor::new(
            context.child("test"),
            engine,
            genesis,
            0,
            genesis,
            projection_readiness,
            None,
        );

        let (ack, waiter) = Exact::handle();
        actor
            .handle_marshal_update(Update::Block(block.into(), ack))
            .await
            .expect("canonical genesis anchor must be accepted");
        waiter
            .await
            .expect("canonical genesis anchor must acknowledge marshal");

        assert_eq!(actor.state.finalized_height, Height::zero());
        assert_eq!(actor.state.forkchoice.finalized_block_hash, genesis);
        assert!(
            engine_rx.try_recv().is_err(),
            "genesis is already canonical and must not be sent through new_payload"
        );
    });
}

#[test]
fn recovered_canonical_block_is_acknowledged_without_reexecution() {
    commonware_runtime::deterministic::Runner::default().start(|context| async move {
        let genesis = B256::repeat_byte(0x01);
        let block = executor_test_block(28, 0x28);
        let recovered_hash = block.block_hash();
        let (engine_tx, mut engine_rx) = tokio::sync::mpsc::unbounded_channel();
        let engine = ConsensusEngineHandle::new(engine_tx);
        // The durable projection has already consumed the recovered EL head.
        // Re-executing the same marshal delivery would ask it to regress to
        // parent 27 and fail with ProjectionAhead.
        let (_projection_publisher, projection_readiness) = ready_projection(
            genesis,
            ProjectionCheckpoint {
                block_number: 28,
                block_hash: recovered_hash,
            },
        );
        let (mut actor, _mailbox) = super::ExecutorActor::new(
            context.child("test"),
            engine,
            genesis,
            28,
            recovered_hash,
            projection_readiness,
            None,
        );

        let (ack, waiter) = Exact::handle();
        actor
            .handle_marshal_update(Update::Block(block.into(), ack))
            .await
            .expect("exact recovered canonical block must be idempotently accepted");
        waiter
            .await
            .expect("exact recovered canonical block must acknowledge marshal");
        assert!(
            engine_rx.try_recv().is_err(),
            "an already canonical block must not be sent through new_payload"
        );
        assert_eq!(actor.state.finalized_height, Height::new(28));
        assert_eq!(actor.state.forkchoice.finalized_block_hash, recovered_hash);
    });
}

#[test]
fn recovered_height_with_conflicting_hash_still_fails_closed() {
    commonware_runtime::deterministic::Runner::default().start(|context| async move {
        let genesis = B256::repeat_byte(0x01);
        let canonical = executor_test_block(28, 0x28);
        let conflicting = executor_test_block(28, 0x29);
        assert_ne!(canonical.block_hash(), conflicting.block_hash());
        let (engine_tx, _engine_rx) = tokio::sync::mpsc::unbounded_channel();
        let engine = ConsensusEngineHandle::new(engine_tx);
        let (_projection_publisher, projection_readiness) = ready_projection(
            genesis,
            ProjectionCheckpoint {
                block_number: 28,
                block_hash: canonical.block_hash(),
            },
        );
        let (mut actor, _mailbox) = super::ExecutorActor::new(
            context.child("test"),
            engine,
            genesis,
            28,
            canonical.block_hash(),
            projection_readiness,
            None,
        );

        let (ack, waiter) = Exact::handle();
        let result = actor
            .handle_marshal_update(Update::Block(conflicting.into(), ack))
            .await;
        assert!(result.is_err(), "same-height conflicting block must fail");
        assert!(
            waiter.await.is_err(),
            "same-height conflicting block must not acknowledge marshal"
        );
        assert_eq!(actor.state.finalized_height, Height::new(28));
        assert_eq!(
            actor.state.forkchoice.finalized_block_hash,
            canonical.block_hash()
        );
    });
}

#[test]
fn conflicting_genesis_anchor_fails_without_acknowledging_marshal() {
    commonware_runtime::deterministic::Runner::default().start(|context| async move {
        let canonical_genesis = B256::repeat_byte(0x01);
        let conflicting = executor_test_block(0, 0xff);
        assert_ne!(conflicting.block_hash(), canonical_genesis);
        let (engine_tx, mut engine_rx) = tokio::sync::mpsc::unbounded_channel();
        let engine = ConsensusEngineHandle::new(engine_tx);
        let (_projection_publisher, projection_readiness) = ready_projection(
            canonical_genesis,
            ProjectionCheckpoint {
                block_number: 0,
                block_hash: canonical_genesis,
            },
        );
        let (mut actor, _mailbox) = super::ExecutorActor::new(
            context.child("test"),
            engine,
            canonical_genesis,
            0,
            canonical_genesis,
            projection_readiness,
            None,
        );

        let (ack, waiter) = Exact::handle();
        let result = actor
            .handle_marshal_update(Update::Block(conflicting.into(), ack))
            .await;

        assert!(result.is_err(), "a conflicting genesis anchor must fail");
        assert!(
            waiter.await.is_err(),
            "a conflicting genesis anchor must not acknowledge marshal"
        );
        assert_eq!(actor.state.finalized_height, Height::zero());
        assert_eq!(
            actor.state.forkchoice.finalized_block_hash,
            canonical_genesis
        );
        assert!(
            engine_rx.try_recv().is_err(),
            "a conflicting genesis anchor must fail before execution"
        );
    });
}

#[test]
fn marshal_ack_waits_for_compressed_storage_commit_barrier() {
    commonware_runtime::deterministic::Runner::default().start(|context| async move {
        use futures::FutureExt as _;

        let genesis = B256::repeat_byte(0x01);
        let block = executor_test_block(7, 0x78);
        let expected = FinalizedCeBlock {
            height: block.number(),
            block_hash: block.block_hash(),
            parent_block_hash: block.parent_hash(),
        };
        let (engine_tx, mut engine_rx) = tokio::sync::mpsc::unbounded_channel();
        let engine = ConsensusEngineHandle::new(engine_tx);
        let (_projection_publisher, projection_readiness) =
            ready_projection_for_block(genesis, &block);
        let (called_tx, called_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let committer = Arc::new(GatedCeCommitter {
            called: Mutex::new(Some(called_tx)),
            release: Mutex::new(Some(release_rx)),
        });
        let (actor, _mailbox) = super::ExecutorActor::new(
            context.child("test"),
            engine,
            genesis,
            0,
            genesis,
            projection_readiness,
            None,
        );
        let mut actor = actor.with_finalized_ce_committer(committer);

        let engine_task = context.child("engine_task").spawn(move |_ctx| async move {
            match engine_rx.recv().await.expect("new payload message") {
                BeaconEngineMessage::NewPayload { tx, .. } => tx
                    .send(Ok(PayloadStatus::from_status(PayloadStatusEnum::Valid)))
                    .expect("new payload receiver"),
                other => panic!("unexpected first engine message: {other:?}"),
            }
            match engine_rx.recv().await.expect("finalized FCU message") {
                BeaconEngineMessage::ForkchoiceUpdated { tx, .. } => tx
                    .send(Ok(OnForkChoiceUpdated::valid(PayloadStatus::from_status(
                        PayloadStatusEnum::Valid,
                    ))))
                    .expect("FCU receiver"),
                other => panic!("unexpected second engine message: {other:?}"),
            }
        });

        let (ack, waiter) = Exact::handle();
        let mut waiter = Box::pin(waiter);
        let actor_task = context.child("actor_task").spawn(move |_ctx| async move {
            actor
                .handle_marshal_update(Update::Block(block.into(), ack))
                .await
        });

        assert_eq!(called_rx.await.expect("commit barrier call"), expected);
        assert!(
            waiter.as_mut().now_or_never().is_none(),
            "Marshal must remain unacknowledged while CE persistence is blocked"
        );
        release_tx.send(()).expect("release commit barrier");
        actor_task
            .await
            .expect("actor task must complete")
            .expect("finalized delivery must succeed after CE commit");
        waiter
            .await
            .expect("Marshal must be acknowledged after CE commit");
        engine_task.await.expect("engine task must complete");
    });
}

// bp-2 regression: a *finalized* block the execution layer rejects must fail
// fast - `handle_marshal_update` returns a structured `Err` (the supervisor
// shuts the node down) and the marshal `Exact` ack is left UNACKNOWLEDGED
// (cancels), never silently dropped after a `warn!`. Deleting the fail-fast
// and going back to acking/ignoring makes this test fail.
#[test]
fn rejected_finalized_block_fails_fast_without_acknowledging_marshal() {
    commonware_runtime::deterministic::Runner::default().start(|context| async move {
        let genesis = B256::repeat_byte(0x01);
        let block = executor_test_block(7, 0x77);
        let (engine_tx, mut engine_rx) = tokio::sync::mpsc::unbounded_channel();
        let engine = ConsensusEngineHandle::new(engine_tx);
        let (_projection_publisher, projection_readiness) =
            ready_projection_for_block(genesis, &block);
        let (mut actor, _mailbox) = super::ExecutorActor::new(
            context.child("test"),
            engine,
            genesis,
            0,
            genesis,
            projection_readiness,
            None,
        );

        // Execution layer rejects the finalized block.
        let engine_task = context.child("engine_task").spawn(move |_ctx| async move {
            let Some(message) = engine_rx.recv().await else {
                panic!("engine channel closed before new_payload");
            };
            match message {
                BeaconEngineMessage::NewPayload { tx, .. } => tx
                    .send(Ok(PayloadStatus::from_status(PayloadStatusEnum::Invalid {
                        validation_error: "test: rejected finalized block".to_string(),
                    })))
                    .expect("new_payload response receiver must be alive"),
                other => panic!("unexpected engine message: {other:?}"),
            }
        });

        let (ack, waiter) = Exact::handle();
        let result = actor
            .handle_marshal_update(Update::Block(block.into(), ack))
            .await;

        // Fail-fast: a fatal error propagates (node will shut down deterministically).
        assert!(
            result.is_err(),
            "an unprocessable finalized block must return a fatal error, \
                 not silently continue"
        );
        // The block was not applied, so the marshal ack must NOT be acknowledged:
        // it cancels. Acking here would lie to marshal progress tracking.
        assert!(
            waiter.await.is_err(),
            "rejected finalized block must leave the marshal ack canceled, \
                 not acknowledged"
        );
        // Finalized state did not advance.
        assert_eq!(actor.state.finalized_height, Height::zero());
        engine_task.await.expect("engine task must complete");
    });
}
