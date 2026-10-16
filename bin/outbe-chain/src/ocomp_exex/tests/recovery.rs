use super::*;

#[test]
fn drain_fatal_cannot_be_overwritten_by_an_inflight_reader_tick() {
    let checkpoint = ProjectionCheckpoint {
        block_number: 5,
        block_hash: B256::repeat_byte(5),
    };
    let (publisher, handle) = outbe_primitives::projection::projection_readiness(
        checkpoint,
        ProjectionStatus::Ready { checkpoint },
    );
    let readiness = OcompReadinessV1(Arc::new(std::sync::Mutex::new(publisher)));
    let drain = readiness.clone();
    let (sent, received) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        drain.publish(ProjectionStatus::Fatal {
            checkpoint: None,
            error: ProjectionFailure::new(ProjectionFailureClass::Other, "drain closed"),
        });
        sent.send(()).unwrap();
    });
    received.recv().unwrap();
    readiness.publish(ProjectionStatus::Ready { checkpoint });
    thread.join().unwrap();
    assert!(
        matches!(handle.current(), ProjectionStatus::Fatal { error, .. } if error.message.as_ref() == "drain closed")
    );
}

#[tokio::test]
async fn real_reth_restart_stream_does_not_execute_history_before_the_closure() {
    if !crate::test_utils::in_isolated_process(
        "ocomp_exex::tests::recovery::real_reth_restart_stream_does_not_execute_history_before_the_closure",
    ) {
        return;
    }
    use outbe_primitives::{OutbeHeader, OutbePrimitives};
    use reth_chainspec::ChainSpecBuilder;
    use reth_ethereum::exex::{ExExHead, ExExNotification, ExExNotifications, Wal};
    use reth_provider::{test_utils::MockEthProvider, Chain};

    for closure_height in [0, 1] {
        let root = tempfile::tempdir().unwrap();
        let parent_hash = B256::repeat_byte(0x38);
        let head_hash = B256::repeat_byte(0x39);
        let spec = Arc::new(
            ChainSpecBuilder::mainnet()
                .chain(outbe_primitives::chain::MAINNET_CHAIN_ID.into())
                .build()
                .map_header(OutbeHeader::new),
        );
        let evm = outbe_evm::OutbeEvmConfig::new(spec);
        // No historical bodies exist. Accidental execution backfill must fail.
        let provider = MockEthProvider::<OutbePrimitives>::new();
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        let wal = Wal::<OutbePrimitives>::new(root.path().join("wal")).unwrap();
        let closure_hash = if closure_height == 0 {
            parent_hash
        } else {
            head_hash
        };
        let stream =
            ExExNotifications::new((1, head_hash).into(), provider, evm, receiver, wal.handle())
                .with_head(ExExHead::new((closure_height, closure_hash).into()));
        let mut stream = without_execution_backfill(stream);
        sender
            .send(ExExNotification::ChainCommitted {
                new: Arc::new(Chain::default()),
            })
            .await
            .unwrap();
        let live = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("live notification stalled behind historical backfill")
            .expect("notification stream closed")
            .expect("historical execution was attempted");
        assert!(matches!(live, ExExNotification::ChainCommitted { .. }));
    }
}

#[test]
fn deterministic_retention_conflicts_are_not_retried_as_storage_outages() {
    for error in [
        outbe_node::ocomp::retention::RetentionError::ConflictingCandidate,
        outbe_node::ocomp::retention::RetentionError::Source("event/state mismatch".to_owned()),
        outbe_node::ocomp::retention::RetentionError::Poisoned,
    ] {
        assert!(matches!(
            classify_retention_reconciliation(Err(error), true),
            RetentionReconciliationDispositionV1::Fatal(_)
        ));
    }
    assert!(
        matches!(
            classify_retention_reconciliation(
                Err(outbe_node::ocomp::retention::RetentionError::RegistryCapacity),
                true
            ),
            RetentionReconciliationDispositionV1::RetryFrame(_)
        ),
        "replay pressure must allow the independent release worker to release closed journal entries"
    );
}

#[tokio::test]
async fn live_drain_does_not_wait_for_blocked_finalized_reader_work() {
    let (mut sender, receiver) = futures::channel::mpsc::channel::<eyre::Result<u64>>(1);
    let drain = tokio::spawn(drain_exex_notifications(receiver));
    // No finalized-reader progress is supplied. More live notifications
    // than channel capacity must still be delivered without accumulating.
    tokio::time::timeout(Duration::from_secs(2), async {
        for height in 1..=128 {
            futures::SinkExt::send(&mut sender, Ok(height))
                .await
                .unwrap();
        }
    })
    .await
    .expect("live notification drain was blocked by unrelated work");
    drop(sender);
    assert!(drain.await.unwrap().to_string().contains("stream closed"));
}

#[test]
fn restart_reannounces_durable_closure_without_advancing_and_detects_lost_consumer() {
    let root = tempfile::tempdir().unwrap();
    let genesis = ProjectionCheckpoint {
        block_number: 0,
        block_hash: B256::repeat_byte(1),
    };
    let closed = ProjectionCheckpoint {
        block_number: 338,
        block_hash: B256::repeat_byte(2),
    };
    let path = root.path().canonicalize().unwrap().join("closure");
    let store = ContiguousCheckpointStoreV1::open(&path, genesis).unwrap();
    store.compare_and_advance_to(genesis, closed).unwrap();
    drop(store); // crash after persistence, before FinishedHeight delivery
    let restored = ContiguousCheckpointStoreV1::open(&path, genesis).unwrap();
    let (events, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    publish_finished_height(&events, restored.current().unwrap()).unwrap();
    assert!(
        matches!(receiver.try_recv().unwrap(), ExExEvent::FinishedHeight(height)
            if height.number == closed.block_number && height.hash == closed.block_hash)
    );
    drop(receiver);
    assert!(publish_finished_height(&events, closed).is_err());
    assert_eq!(restored.current().unwrap(), closed);
}

#[test]
fn finished_height_consumer_loss_is_graceful_only_during_shutdown() {
    let checkpoint = ProjectionCheckpoint {
        block_number: 38,
        block_hash: B256::repeat_byte(0x38),
    };
    let (events, receiver) = tokio::sync::mpsc::unbounded_channel();
    drop(receiver);
    let error = publish_finished_height(&events, checkpoint).unwrap_err();

    let (stop, shutdown) = reth_ethereum::tasks::shutdown::signal();
    assert!(!finished_height_consumer_closed_during_shutdown(
        &error, &shutdown
    ));
    stop.fire();
    assert!(finished_height_consumer_closed_during_shutdown(
        &error, &shutdown
    ));
    assert!(!finished_height_consumer_closed_during_shutdown(
        &eyre::eyre!("unrelated finalized reader failure"),
        &shutdown,
    ));
}

#[tokio::test]
async fn shutdown_handoff_keeps_reth_exex_alive_until_node_teardown() {
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(10),
            wait_for_node_teardown()
        )
        .await
        .is_err(),
        "OCOMP must not complete the Reth ExEx future during shutdown"
    );
}

#[test]
fn retention_unavailability_and_quarantine_retry_without_a_fatal_node_exit() {
    let unavailable = classify_retention_reconciliation(
        Err(
            outbe_node::ocomp::retention::RetentionError::JournalUnavailable {
                operation: "fsync temporary",
                path: std::path::PathBuf::from("pin.v1.tmp"),
                reason: "injected journal durability failure".to_owned(),
            },
        ),
        true,
    );
    assert!(matches!(
        unavailable,
        RetentionReconciliationDispositionV1::RetryFrame(
            outbe_node::ocomp::retention::RetentionError::JournalUnavailable { .. }
        )
    ));

    let quarantined = classify_retention_reconciliation(
        Err(outbe_node::ocomp::retention::RetentionError::Quarantined(
            "injected journal ambiguity".to_owned(),
        )),
        true,
    );
    assert!(matches!(
        quarantined,
        RetentionReconciliationDispositionV1::RetryFrame(
            outbe_node::ocomp::retention::RetentionError::Quarantined(_)
        )
    ));

    let wrapped_unavailable = Err::<(), _>(
        outbe_node::ocomp::retention::RetentionError::JournalUnavailable {
            operation: "fsync authoritative journal",
            path: std::path::PathBuf::from("pin.v1"),
            reason: "injected journal durability failure".to_owned(),
        },
    )
    .wrap_err("bind OCOMP retention to canonical finalized typed state")
    .expect_err("wrapped journal outage");
    assert!(retention_runtime_error_requires_frame_retry(
        &wrapped_unavailable
    ));

    let wrapped_quarantine =
        Err::<(), _>(outbe_node::ocomp::retention::RetentionError::Quarantined(
            "injected journal ambiguity".to_owned(),
        ))
        .wrap_err("commit exact exporter ACK to OCOMP retention")
        .expect_err("wrapped journal quarantine");
    assert!(retention_runtime_error_requires_frame_retry(
        &wrapped_quarantine
    ));
    assert!(!retention_runtime_error_requires_frame_retry(&eyre::eyre!(
        "unrelated finalized-frame failure"
    )));
}

#[tokio::test]
async fn fatal_handoff_keeps_exex_alive_until_node_teardown() {
    assert!(
        tokio::time::timeout(Duration::from_millis(10), wait_for_node_teardown())
            .await
            .is_err(),
        "fatal handoff must not complete the Reth ExEx future"
    );
}

#[test]
fn finalized_reader_lag_tracks_scan_and_open_job_closure_independently() {
    assert_eq!(finalized_reader_lags(1_000, 900, 700), (100, 300));
    assert_eq!(finalized_reader_lags(900, 1_000, 1_000), (0, 0));
}

#[tokio::test]
async fn notification_drain_reports_failure_and_closed_receiver() {
    let error = drain_exex_notifications(futures::stream::iter([
        Ok(()),
        Err(eyre::eyre!("injected stream error")),
    ]))
    .await;
    assert!(format!("{error:#}").contains("injected stream error"));
    let error = drain_exex_notifications(futures::stream::iter([Ok(())])).await;
    assert!(error.to_string().contains("stream closed"));
}

#[test]
fn sticky_fatal_evidence_survives_restart_and_is_write_once() {
    let root = tempfile::tempdir().unwrap();
    let job_id = B256::repeat_byte(0x51);
    persist_generic_fatal_evidence(root.path(), job_id, "first fatal").unwrap();
    persist_generic_fatal_evidence(root.path(), B256::repeat_byte(0x52), "later fatal").unwrap();

    let loaded = load_persisted_fatal_evidence(root.path())
        .unwrap()
        .expect("persisted fatal");
    assert!(loaded.contains("first fatal"));
    assert!(!loaded.contains("later fatal"));

    let mismatch_root = tempfile::tempdir().unwrap();
    persist_fatal_evidence(
        mismatch_root.path(),
        job_id,
        B256::repeat_byte(0x61),
        B256::repeat_byte(0x62),
    )
    .unwrap();
    assert!(load_persisted_fatal_evidence(mismatch_root.path())
        .unwrap()
        .expect("mismatch evidence")
        .contains("local_result_digest"));
}

// Native component restart tests. These tests do not claim a full process launch.
#[cfg(feature = "snapshot-integration")]
pub(super) mod copied_native;

#[cfg(feature = "snapshot-integration")]
mod copied_exex_startup;
