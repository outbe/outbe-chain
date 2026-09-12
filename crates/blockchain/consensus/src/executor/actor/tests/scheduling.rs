use super::*;

#[test]
fn fcu_heartbeat_interval_is_shorter_than_watchdog_grace() {
    assert!(
        crate::config::DEFAULT_FCU_HEARTBEAT_INTERVAL < crate::config::EXECUTION_WATCHDOG_GRACE,
        "FCU heartbeat must retry before watchdog grace can trip"
    );
}

#[test]
fn heartbeat_sends_fcu_to_last_forkchoice_without_payload_attributes() {
    commonware_runtime::deterministic::Runner::default().start(|context| async move {
        let genesis = B256::repeat_byte(0x01);
        let target = B256::repeat_byte(0xAA);
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
        actor.state = actor.state.update_finalized(Height::new(7), Digest(target));

        let engine_task = context.child("engine_task").spawn(move |_ctx| async move {
            let Some(message) = engine_rx.recv().await else {
                panic!("heartbeat must send an FCU message");
            };
            match message {
                BeaconEngineMessage::ForkchoiceUpdated {
                    state,
                    payload_attrs,
                    tx,
                } => {
                    assert_eq!(state.head_block_hash, target);
                    assert_eq!(state.finalized_block_hash, target);
                    assert!(payload_attrs.is_none());
                    tx.send(Ok(OnForkChoiceUpdated::valid(PayloadStatus::from_status(
                        PayloadStatusEnum::Valid,
                    ))))
                    .expect("test engine response receiver must be alive");
                }
                other => panic!("unexpected engine message: {other:?}"),
            }
        });

        actor.send_fcu_heartbeat().await;
        engine_task.await.expect("engine task must complete");
    });
}

// TC-1 regression: a fatal Err from handle_marshal_update must PROPAGATE out
// of run_live_loop (via `?`) so run() ends and the supervisor select-arm
// treats the executor exit as fatal. Without propagation a rejected finalized
// block would be swallowed and the loop would keep running on diverged state.
#[test]
fn run_live_loop_propagates_fatal_executor_error() {
    commonware_runtime::deterministic::Runner::default().start(|context| async move {
        let genesis = B256::repeat_byte(0x01);
        let block = executor_test_block(7, 0x77);
        let (engine_tx, mut engine_rx) = tokio::sync::mpsc::unbounded_channel();
        let engine = ConsensusEngineHandle::new(engine_tx);
        let (mailbox_tx, mailbox_rx) = futures::channel::mpsc::unbounded();
        let (_projection_publisher, projection_readiness) =
            ready_projection_for_block(genesis, &block);

        let mut actor = super::ExecutorActor {
            context: context.child("test"),
            engine,
            state: LastCanonicalized::new(genesis),
            mailbox_rx,
            execution_finalized_height_tx: None,
            projection_readiness,
            ocomp_readiness: None,
            finalized_ce_committer: None,
            ancestry_readiness: None,
            // Heartbeat far in the future so the biased mailbox arm wins.
            fcu_heartbeat_interval: std::time::Duration::from_secs(3600),
            next_fcu_heartbeat_deadline: context.current() + std::time::Duration::from_secs(3600),
            pending_finalized_subscriptions: std::collections::BTreeMap::new(),
        };

        // Engine rejects the finalized block -> handle_finalize_inner Err.
        let engine_task = context.child("engine_task").spawn(move |_ctx| async move {
            match engine_rx.recv().await {
                Some(BeaconEngineMessage::NewPayload { tx, .. }) => {
                    tx.send(Ok(PayloadStatus::from_status(PayloadStatusEnum::Invalid {
                        validation_error: "test: rejected finalized block".to_string(),
                    })))
                    .expect("new_payload response receiver must be alive");
                }
                other => panic!("unexpected/absent engine message: {other:?}"),
            }
        });

        let (ack, _waiter) = Exact::handle();
        mailbox_tx
            .unbounded_send(crate::executor::ingress::Message::MarshalUpdate(Box::new(
                Update::Block(block.into(), ack),
            )))
            .expect("mailbox send must succeed");

        let result = actor.run_live_loop().await;
        assert!(
            result.is_err(),
            "run_live_loop must propagate the executor fail-fast Err so the supervisor \
                 shuts the node down; got {result:?}"
        );
        engine_task.await.expect("engine task must complete");
    });
}

#[test]
fn mailbox_updates_are_processed_before_ready_heartbeat() {
    commonware_runtime::deterministic::Runner::default().start(|context| async move {
        let genesis = B256::repeat_byte(0x01);
        let first = B256::repeat_byte(0x11);
        let second = B256::repeat_byte(0x22);
        let (engine_tx, mut engine_rx) = tokio::sync::mpsc::unbounded_channel();
        let engine = ConsensusEngineHandle::new(engine_tx);
        let (mailbox_tx, mailbox_rx) = futures::channel::mpsc::unbounded();
        let (_projection_publisher, projection_readiness) = ready_projection(
            genesis,
            ProjectionCheckpoint {
                block_number: 0,
                block_hash: genesis,
            },
        );

        let mut actor = super::ExecutorActor {
            context: context.child("test"),
            engine,
            state: LastCanonicalized::new(genesis),
            mailbox_rx,
            execution_finalized_height_tx: None,
            projection_readiness,
            ocomp_readiness: None,
            finalized_ce_committer: None,
            ancestry_readiness: None,
            fcu_heartbeat_interval: std::time::Duration::ZERO,
            next_fcu_heartbeat_deadline: context.current(),
            pending_finalized_subscriptions: std::collections::BTreeMap::new(),
        };

        for (height, digest) in [
            (Height::new(1), Digest(first)),
            (Height::new(2), Digest(second)),
        ] {
            let (response, _rx) = commonware_utils::channel::oneshot::channel();
            mailbox_tx
                .unbounded_send(crate::executor::ingress::Message::CanonicalizeHead(
                    crate::executor::ingress::CanonicalizeHead {
                        height,
                        digest,
                        response,
                    },
                ))
                .expect("test mailbox send must succeed");
        }

        let actor_task = context.child("actor_task").spawn(move |_ctx| async move {
            actor
                .run_live_loop()
                .await
                .expect("live loop must exit cleanly on mailbox close");
        });

        let mut heads = Vec::new();
        for _ in 0..3 {
            let Some(message) = engine_rx.recv().await else {
                panic!("engine channel closed before expected FCUs");
            };
            match message {
                BeaconEngineMessage::ForkchoiceUpdated {
                    state,
                    payload_attrs,
                    tx,
                } => {
                    assert!(payload_attrs.is_none());
                    heads.push(state.head_block_hash);
                    tx.send(Ok(OnForkChoiceUpdated::valid(PayloadStatus::from_status(
                        PayloadStatusEnum::Valid,
                    ))))
                    .expect("test engine response receiver must be alive");
                }
                other => panic!("unexpected engine message: {other:?}"),
            }
        }

        assert_eq!(heads, vec![first, second, second]);
        drop(mailbox_tx);
        actor_task.await.expect("actor task must complete");
    });
}

#[test]
fn finalized_subscriber_completes_immediately_when_height_already_finalized() {
    commonware_runtime::deterministic::Runner::default().start(|context| async move {
        let genesis = B256::repeat_byte(0x01);
        let finalized = B256::repeat_byte(0x07);
        let (engine_tx, _engine_rx) = tokio::sync::mpsc::unbounded_channel();
        let engine = ConsensusEngineHandle::new(engine_tx);
        let (_projection_publisher, projection_readiness) = ready_projection(
            genesis,
            ProjectionCheckpoint {
                block_number: 0,
                block_hash: genesis,
            },
        );
        let (mut actor, _mailbox) = super::ExecutorActor::new(
            context,
            engine,
            genesis,
            7,
            finalized,
            projection_readiness,
            None,
        );
        let (response, rx) = commonware_utils::channel::oneshot::channel();

        actor.handle_subscribe_finalized(Height::new(7), response);

        rx.await
            .expect("already-finalized subscription must complete");
    });
}

#[test]
fn ancestry_readiness_advances_from_executor_finalized_notifications() {
    commonware_runtime::deterministic::Runner::default().start(|context| async move {
        let genesis = B256::repeat_byte(0x01);
        let (engine_tx, _engine_rx) = tokio::sync::mpsc::unbounded_channel();
        let engine = ConsensusEngineHandle::new(engine_tx);
        let readiness = AncestryReadiness::new(0, 3);
        let (_projection_publisher, projection_readiness) = ready_projection(
            genesis,
            ProjectionCheckpoint {
                block_number: 0,
                block_hash: genesis,
            },
        );
        let (actor, _mailbox) = super::ExecutorActor::new(
            context,
            engine,
            genesis,
            0,
            genesis,
            projection_readiness,
            None,
        );
        let actor = actor.with_ancestry_readiness(readiness.clone());

        assert!(!readiness.is_ready());
        actor.notify_execution_finalized(Height::new(2));
        assert!(!readiness.is_ready());
        actor.notify_execution_finalized(Height::new(3));
        assert!(readiness.is_ready());
    });
}

#[test]
fn finalized_subscriber_completes_when_later_height_is_notified() {
    commonware_runtime::deterministic::Runner::default().start(|context| async move {
        let genesis = B256::repeat_byte(0x01);
        let (engine_tx, _engine_rx) = tokio::sync::mpsc::unbounded_channel();
        let engine = ConsensusEngineHandle::new(engine_tx);
        let (_projection_publisher, projection_readiness) = ready_projection(
            genesis,
            ProjectionCheckpoint {
                block_number: 0,
                block_hash: genesis,
            },
        );
        let (mut actor, _mailbox) = super::ExecutorActor::new(
            context,
            engine,
            genesis,
            0,
            genesis,
            projection_readiness,
            None,
        );
        let (response, rx) = commonware_utils::channel::oneshot::channel();

        actor.handle_subscribe_finalized(Height::new(3), response);
        assert_eq!(actor.pending_finalized_subscriptions.len(), 1);
        actor.notify_finalized_subscribers(Height::new(3));

        rx.await
            .expect("pending finalized subscription must complete");
        assert!(actor.pending_finalized_subscriptions.is_empty());
    });
}
