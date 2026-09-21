use super::*;

#[test]
fn transient_runtime_read_failure_clears_only_after_a_successful_probe() {
    let storage = Arc::new(FailAfterStartupStorage::default());
    storage
        .fail_writes_unavailable
        .store(true, Ordering::SeqCst);
    let writer: StorageWriterHandle = storage.clone();
    let since = std::time::Instant::now();
    let (failure_sender, failure_receiver) =
        tokio::sync::watch::channel(Some(RuntimeBodyFailure::Unavailable {
            generation: 7,
            since,
        }));
    let recovery = ProjectionRuntimeRecoveryHandle {
        writer,
        failure_sender,
    };

    assert_eq!(
        recovery.reconcile(7),
        ProjectionRuntimeRecoveryV1::Unavailable
    );
    assert_eq!(
        *failure_receiver.borrow(),
        Some(RuntimeBodyFailure::Unavailable {
            generation: 7,
            since,
        })
    );

    storage
        .fail_writes_unavailable
        .store(false, Ordering::SeqCst);
    assert_eq!(
        recovery.reconcile(7),
        ProjectionRuntimeRecoveryV1::Recovered
    );
    assert_eq!(*failure_receiver.borrow(), None);
}

#[test]
fn successful_probe_never_clears_a_newer_runtime_outage() {
    struct BlockingProbeStorage {
        entered: Arc<std::sync::Barrier>,
        release: Arc<std::sync::Barrier>,
    }

    impl StorageWriter for BlockingProbeStorage {
        fn verify_transaction_capability(&self) -> Result<(), StorageError> {
            self.entered.wait();
            self.release.wait();
            Ok(())
        }

        fn apply_atomic(&self, _batch: &AtomicWriteBatch) -> Result<(), StorageError> {
            Ok(())
        }
    }

    let entered = Arc::new(std::sync::Barrier::new(2));
    let release = Arc::new(std::sync::Barrier::new(2));
    let writer: StorageWriterHandle = Arc::new(BlockingProbeStorage {
        entered: entered.clone(),
        release: release.clone(),
    });
    let since = std::time::Instant::now();
    let (failure_sender, failure_receiver) =
        tokio::sync::watch::channel(Some(RuntimeBodyFailure::Unavailable {
            generation: 7,
            since,
        }));
    let recovery = ProjectionRuntimeRecoveryHandle {
        writer,
        failure_sender: failure_sender.clone(),
    };
    let task = std::thread::spawn(move || recovery.reconcile(7));
    entered.wait();
    failure_sender.send_replace(Some(RuntimeBodyFailure::Unavailable {
        generation: 8,
        since,
    }));
    release.wait();

    assert_eq!(
        task.join().unwrap(),
        ProjectionRuntimeRecoveryV1::Unavailable
    );
    assert_eq!(
        *failure_receiver.borrow(),
        Some(RuntimeBodyFailure::Unavailable {
            generation: 8,
            since,
        })
    );
}

#[tokio::test]
async fn control_loop_drains_notifications_and_emits_finished_heights_in_order() {
    use futures::{channel::mpsc, SinkExt};

    let provider = MockEthProvider::new();
    let first = add_empty_block(&provider, 1);
    let second = add_empty_block(&provider, 2);
    let runtime = initialized_runtime(1).into_inner().unwrap();
    let (mut notification_tx, notification_rx) = mpsc::channel(1);
    let (finality_tx, finality_rx) = mpsc::unbounded();
    let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
    let (exit_tx, _exit_rx) = tokio::sync::mpsc::unbounded_channel();
    let task = tokio::spawn(run_projection_loop(
        provider,
        notification_rx,
        finality_rx,
        events_tx,
        runtime,
        exit_tx,
    ));

    notification_tx.send(Ok(())).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), notification_tx.send(Ok(())))
        .await
        .unwrap()
        .unwrap();
    finality_tx
        .unbounded_send(FinalizedTarget::new(2, second))
        .unwrap();

    let first_event = tokio::time::timeout(Duration::from_secs(1), events_rx.recv())
        .await
        .unwrap()
        .unwrap();
    let second_event = tokio::time::timeout(Duration::from_secs(1), events_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first_event, ExExEvent::FinishedHeight((1, first).into()));
    assert_eq!(second_event, ExExEvent::FinishedHeight((2, second).into()));
    assert!(
        !task.is_finished(),
        "the critical projection loop stays alive"
    );
    task.abort();
}

#[tokio::test]
async fn deterministic_projection_failure_reports_exit_while_exex_keeps_draining() {
    use futures::{channel::mpsc, SinkExt};

    let provider = MockEthProvider::new();
    let block_hash = add_empty_block(&provider, 1);
    let storage = Arc::new(FailAfterStartupStorage::default());
    let reader: StorageReaderHandle = storage.clone();
    let writer: StorageWriterHandle = storage.clone();
    let projection_config = ProjectionConfig {
        chain_id: 1,
        genesis_hash: B256::repeat_byte(0x11),
        start_block: 1,
    };
    let projector =
        OffchainDataProjection::open(projection_config, reader.clone(), writer.clone()).unwrap();
    storage.fail_writes.store(true, Ordering::SeqCst);

    let (mut notification_tx, notification_rx) = mpsc::channel(1);
    let (finality_tx, finality_rx) = mpsc::unbounded();
    let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
    let (readiness_publisher, _readiness) = outbe_offchain_data::projection_readiness(
        outbe_offchain_data::ProjectionCheckpoint {
            block_number: 0,
            block_hash: B256::repeat_byte(0x11),
        },
        outbe_offchain_data::ProjectionStatus::Starting,
    );
    let (exit_tx, mut exit_rx) = tokio::sync::mpsc::unbounded_channel();
    let (runtime_failure_tx, runtime_failure_rx) = tokio::sync::watch::channel(None);
    let task = tokio::spawn(run_projection_loop(
        provider,
        notification_rx,
        finality_rx,
        events_tx,
        ProjectionRuntime {
            projector,
            readiness_publisher,
            projection_config,
            _reader: reader,
            overlay: None,
            writer,
            _writer_lease: None,
            runtime_failure_sender: Some(runtime_failure_tx),
            runtime_failure_receiver: Some(runtime_failure_rx),
        },
        exit_tx,
    ));
    finality_tx
        .unbounded_send(FinalizedTarget::new(1, block_hash))
        .unwrap();

    tokio::time::timeout(Duration::from_secs(1), async {
        while storage.failed_writes.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("projection worker must observe the injected storage failure");
    assert!(events_rx.try_recv().is_err());
    let exit = tokio::time::timeout(Duration::from_secs(1), exit_rx.recv())
        .await
        .expect("fatal projection failure must notify the node supervisor")
        .expect("projection exit channel must remain open");
    assert_eq!(
        exit.failure.class,
        outbe_offchain_data::ProjectionFailureClass::CorruptBody
    );

    notification_tx.send(Ok(())).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), notification_tx.send(Ok(())))
        .await
        .expect("ExEx must keep draining notifications after projection failure")
        .unwrap();
    assert!(
        !task.is_finished(),
        "projection failure must not stop the node loop"
    );
    task.abort();
}

#[tokio::test]
async fn runtime_body_corruption_reports_exit_while_exex_keeps_draining() {
    use futures::{channel::mpsc, SinkExt};

    let provider = MockEthProvider::<reth_ethereum::EthPrimitives>::new();
    let storage = Arc::new(MemoryStorage::new());
    let reader: StorageReaderHandle = storage.clone();
    let writer: StorageWriterHandle = storage;
    let projection_config = ProjectionConfig {
        chain_id: 1,
        genesis_hash: B256::repeat_byte(0x11),
        start_block: 1,
    };
    let projector =
        OffchainDataProjection::open(projection_config, reader.clone(), writer.clone()).unwrap();
    let (readiness_publisher, _readiness) = outbe_offchain_data::projection_readiness(
        outbe_offchain_data::ProjectionCheckpoint {
            block_number: 0,
            block_hash: projection_config.genesis_hash,
        },
        outbe_offchain_data::ProjectionStatus::Starting,
    );
    let (runtime_failure_tx, runtime_failure_rx) = tokio::sync::watch::channel(None);
    let (mut notification_tx, notification_rx) = mpsc::channel(1);
    let (_finality_tx, finality_rx) = mpsc::unbounded();
    let (events_tx, _events_rx) = tokio::sync::mpsc::unbounded_channel();
    let (exit_tx, mut exit_rx) = tokio::sync::mpsc::unbounded_channel();
    let task = tokio::spawn(run_projection_loop(
        provider,
        notification_rx,
        finality_rx,
        events_tx,
        ProjectionRuntime {
            projector,
            readiness_publisher,
            projection_config,
            _reader: reader,
            overlay: None,
            writer,
            _writer_lease: None,
            runtime_failure_sender: Some(runtime_failure_tx.clone()),
            runtime_failure_receiver: Some(runtime_failure_rx),
        },
        exit_tx,
    ));

    runtime_failure_tx.send_replace(Some(outbe_offchain_data::RuntimeBodyFailure::Fatal(
        outbe_offchain_data::ProjectionFailure::new(
            outbe_offchain_data::ProjectionFailureClass::CorruptBody,
            "dangling body index",
        ),
    )));
    let exit = tokio::time::timeout(Duration::from_secs(1), exit_rx.recv())
        .await
        .expect("body corruption must notify the node supervisor")
        .expect("projection exit channel must remain open");
    assert_eq!(
        exit.failure.class,
        outbe_offchain_data::ProjectionFailureClass::CorruptBody
    );

    notification_tx.send(Ok(())).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), notification_tx.send(Ok(())))
        .await
        .expect("ExEx must keep draining after a fatal runtime read")
        .unwrap();
    assert!(!task.is_finished());
    task.abort();
}

#[tokio::test]
async fn unexpected_exex_return_reports_fatal_and_stays_alive_for_common_shutdown() {
    let (publisher, readiness) = outbe_offchain_data::projection_readiness(
        outbe_offchain_data::ProjectionCheckpoint {
            block_number: 0,
            block_hash: B256::repeat_byte(0x11),
        },
        outbe_offchain_data::ProjectionStatus::Starting,
    );
    let (exit_tx, mut exit_rx) = tokio::sync::mpsc::unbounded_channel();
    let task = tokio::spawn(supervise_projection_future(
        async { Ok(()) },
        publisher,
        exit_tx,
    ));

    let exit = tokio::time::timeout(Duration::from_secs(1), exit_rx.recv())
        .await
        .expect("unexpected return must notify lifecycle owner")
        .expect("exit sender must remain open");
    assert_eq!(
        exit.failure.class,
        outbe_offchain_data::ProjectionFailureClass::ProjectorExited
    );
    assert!(matches!(
        readiness.current(),
        outbe_offchain_data::ProjectionStatus::Fatal { error, .. }
            if error.class == outbe_offchain_data::ProjectionFailureClass::ProjectorExited
    ));
    assert!(!task.is_finished());
    task.abort();
}

#[tokio::test]
async fn runtime_body_unavailability_uses_the_projection_recovery_session() {
    use futures::channel::mpsc;

    let provider = MockEthProvider::<reth_ethereum::EthPrimitives>::new();
    let storage = Arc::new(FailAfterStartupStorage::default());
    let reader: StorageReaderHandle = storage.clone();
    let writer: StorageWriterHandle = storage.clone();
    let projection_config = ProjectionConfig {
        chain_id: 1,
        genesis_hash: B256::repeat_byte(0x11),
        start_block: 1,
    };
    let projector =
        OffchainDataProjection::open(projection_config, reader.clone(), writer.clone()).unwrap();
    let (readiness_publisher, readiness) = outbe_offchain_data::projection_readiness(
        outbe_offchain_data::ProjectionCheckpoint {
            block_number: 0,
            block_hash: projection_config.genesis_hash,
        },
        outbe_offchain_data::ProjectionStatus::Ready {
            checkpoint: outbe_offchain_data::ProjectionCheckpoint {
                block_number: 0,
                block_hash: projection_config.genesis_hash,
            },
        },
    );
    let (runtime_failure_tx, runtime_failure_rx) = tokio::sync::watch::channel(None);
    let (_notification_tx, notification_rx) = mpsc::channel(1);
    let (_finality_tx, finality_rx) = mpsc::unbounded();
    let (events_tx, _events_rx) = tokio::sync::mpsc::unbounded_channel();
    let (exit_tx, mut exit_rx) = tokio::sync::mpsc::unbounded_channel();
    let task = tokio::spawn(run_projection_loop(
        provider,
        notification_rx,
        finality_rx,
        events_tx,
        ProjectionRuntime {
            projector,
            readiness_publisher,
            projection_config,
            _reader: reader,
            overlay: None,
            writer,
            _writer_lease: None,
            runtime_failure_sender: Some(runtime_failure_tx.clone()),
            runtime_failure_receiver: Some(runtime_failure_rx),
        },
        exit_tx,
    ));

    storage.fail_reads.store(true, Ordering::SeqCst);
    runtime_failure_tx.send_replace(Some(outbe_offchain_data::RuntimeBodyFailure::Unavailable {
        generation: 1,
        since: std::time::Instant::now(),
    }));
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if matches!(
                readiness.current(),
                outbe_offchain_data::ProjectionStatus::MongoUnavailable { .. }
            ) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("read-side outage must immediately disable readiness");
    assert!(exit_rx.try_recv().is_err());

    storage.fail_reads.store(false, Ordering::SeqCst);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if matches!(
                readiness.current(),
                outbe_offchain_data::ProjectionStatus::Ready { checkpoint }
                    if checkpoint.block_number == 0
            ) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("a successful storage probe must restore readiness");
    assert!(exit_rx.try_recv().is_err());
    task.abort();
}

#[test]
fn runtime_outage_requires_a_successful_probe_before_recovery_acknowledgement() {
    let runtime = initialized_runtime(1);
    let storage = Arc::new(FailAfterStartupStorage::default());
    storage.fail_reads.store(true, Ordering::SeqCst);
    {
        let mut runtime = runtime.lock().unwrap();
        runtime.writer = storage.clone();
        runtime
            .runtime_failure_sender
            .as_ref()
            .unwrap()
            .send_replace(Some(RuntimeBodyFailure::Unavailable {
                generation: 1,
                since: std::time::Instant::now(),
            }));
    }
    let (logical_tx, _logical_rx) = tokio::sync::mpsc::unbounded_channel();
    let (write_tx, _write_rx) = tokio::sync::mpsc::unbounded_channel();
    let (recovery_tx, mut recovery_rx) = tokio::sync::mpsc::unbounded_channel();
    let attempt = || {
        project_through_target(
            MockEthProvider::<reth_ethereum::EthPrimitives>::new(),
            &runtime,
            FinalizedTarget::new(0, B256::repeat_byte(0x11)),
            &logical_tx,
            &write_tx,
            &recovery_tx,
        )
    };

    assert_eq!(
        projection_failure_class(&attempt().unwrap_err()),
        ProjectionFailureClass::StorageUnavailable
    );
    assert!(recovery_rx.try_recv().is_err());

    storage.fail_reads.store(false, Ordering::SeqCst);
    assert_eq!(attempt().unwrap(), None);
    recovery_rx
        .try_recv()
        .expect("successful probe acknowledges recovery");
}

#[tokio::test]
async fn unavailable_mongo_write_retries_without_changing_logical_readiness() {
    use futures::channel::mpsc;

    let provider = MockEthProvider::new();
    let block_hash = add_empty_block(&provider, 1);
    let storage = Arc::new(FailAfterStartupStorage::default());
    let projection_config = ProjectionConfig {
        chain_id: 1,
        genesis_hash: B256::repeat_byte(0x11),
        start_block: 1,
    };
    OffchainDataProjection::open(projection_config, storage.clone(), storage.clone()).unwrap();
    let overlay = Arc::new(PendingOverlayStorage::new(storage.clone()));
    let reader: StorageReaderHandle = overlay.clone();
    let logical_writer: StorageWriterHandle = overlay.clone();
    let durable_writer: StorageWriterHandle = storage.clone();
    let projector =
        OffchainDataProjection::open(projection_config, reader.clone(), logical_writer).unwrap();
    let (readiness_publisher, readiness) = outbe_offchain_data::projection_readiness(
        outbe_offchain_data::ProjectionCheckpoint {
            block_number: 0,
            block_hash: projection_config.genesis_hash,
        },
        outbe_offchain_data::ProjectionStatus::Ready {
            checkpoint: outbe_offchain_data::ProjectionCheckpoint {
                block_number: 0,
                block_hash: projection_config.genesis_hash,
            },
        },
    );
    let (runtime_failure_tx, runtime_failure_rx) = tokio::sync::watch::channel(None);
    let (_notification_tx, notification_rx) = mpsc::channel(1);
    let (finality_tx, finality_rx) = mpsc::unbounded();
    let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
    let (exit_tx, mut exit_rx) = tokio::sync::mpsc::unbounded_channel();
    let task = tokio::spawn(run_projection_loop(
        provider,
        notification_rx,
        finality_rx,
        events_tx,
        ProjectionRuntime {
            projector,
            readiness_publisher,
            projection_config,
            _reader: reader,
            overlay: Some(overlay.clone()),
            writer: durable_writer,
            _writer_lease: None,
            runtime_failure_sender: Some(runtime_failure_tx),
            runtime_failure_receiver: Some(runtime_failure_rx),
        },
        exit_tx,
    ));

    storage
        .fail_writes_unavailable
        .store(true, Ordering::SeqCst);
    finality_tx
        .unbounded_send(FinalizedTarget::new(1, block_hash))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if matches!(
                readiness.current(),
                outbe_offchain_data::ProjectionStatus::Ready { checkpoint }
                    if checkpoint.block_number == 1 && checkpoint.block_hash == block_hash
            ) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("Mongo write failure must not hold back logical readiness");
    assert!(events_rx.try_recv().is_err());
    assert!(exit_rx.try_recv().is_err());

    storage
        .fail_writes_unavailable
        .store(false, Ordering::SeqCst);
    let event = tokio::time::timeout(Duration::from_secs(2), events_rx.recv())
        .await
        .expect("projection must retry after the one-second interval")
        .expect("ExEx event sender remains live");
    assert_eq!(event, ExExEvent::FinishedHeight((1, block_hash).into()));
    assert!(matches!(
        readiness.current(),
        outbe_offchain_data::ProjectionStatus::Ready { checkpoint }
            if checkpoint.block_number == 1 && checkpoint.block_hash == block_hash
    ));
    assert!(exit_rx.try_recv().is_err());
    assert!(!task.is_finished());
    task.abort();
}

#[tokio::test]
async fn blocked_mongo_write_does_not_block_logical_projection_readiness() {
    use futures::channel::mpsc;

    let provider = MockEthProvider::new();
    let block_hash = add_empty_block(&provider, 1);
    let next_block_hash = add_empty_block(&provider, 2);
    let (durable, write_started, release_write, write_finished) = BlockingWriteStorage::new();
    let projection_config = ProjectionConfig {
        chain_id: 1,
        genesis_hash: B256::repeat_byte(0x11),
        start_block: 1,
    };
    OffchainDataProjection::open(projection_config, durable.clone(), durable.clone()).unwrap();
    let overlay = Arc::new(PendingOverlayStorage::new(durable.clone()));
    let logical_reader: StorageReaderHandle = overlay.clone();
    let logical_writer: StorageWriterHandle = overlay.clone();
    let durable_writer: StorageWriterHandle = durable.clone();
    let projector =
        OffchainDataProjection::open(projection_config, logical_reader.clone(), logical_writer)
            .unwrap();
    let checkpoint = ProjectionCheckpoint {
        block_number: 0,
        block_hash: projection_config.genesis_hash,
    };
    let (readiness_publisher, readiness) =
        projection_readiness(checkpoint, ProjectionStatus::Ready { checkpoint });
    let (runtime_failure_tx, runtime_failure_rx) = tokio::sync::watch::channel(None);
    let (_notification_tx, notification_rx) = mpsc::channel(1);
    let (finality_tx, finality_rx) = mpsc::unbounded();
    let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
    let (exit_tx, _exit_rx) = tokio::sync::mpsc::unbounded_channel();
    durable.block_next_write.store(true, Ordering::Release);
    let task = tokio::spawn(run_projection_loop(
        provider,
        notification_rx,
        finality_rx,
        events_tx,
        ProjectionRuntime {
            projector,
            readiness_publisher,
            projection_config,
            _reader: logical_reader,
            overlay: Some(overlay.clone()),
            writer: durable_writer,
            _writer_lease: None,
            runtime_failure_sender: Some(runtime_failure_tx),
            runtime_failure_receiver: Some(runtime_failure_rx),
        },
        exit_tx,
    ));

    finality_tx
        .unbounded_send(FinalizedTarget::new(1, block_hash))
        .unwrap();
    tokio::task::spawn_blocking(move || write_started.recv().unwrap())
        .await
        .unwrap();

    let readiness_advanced = tokio::time::timeout(Duration::from_millis(200), async {
        loop {
            if matches!(
                readiness.current(),
                ProjectionStatus::Ready { checkpoint }
                    if checkpoint.block_number == 1 && checkpoint.block_hash == block_hash
            ) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    finality_tx
        .unbounded_send(FinalizedTarget::new(2, next_block_hash))
        .unwrap();
    let later_readiness_advanced = tokio::time::timeout(Duration::from_millis(200), async {
        loop {
            if matches!(
                readiness.current(),
                ProjectionStatus::Ready { checkpoint }
                    if checkpoint.block_number == 2
                        && checkpoint.block_hash == next_block_hash
            ) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(
        events_rx.try_recv().is_err(),
        "FinishedHeight must remain tied to the durable Mongo checkpoint"
    );

    release_write.send(()).unwrap();
    tokio::task::spawn_blocking(move || write_finished.recv().unwrap())
        .await
        .unwrap();
    let first_finished = tokio::time::timeout(Duration::from_secs(1), events_rx.recv())
        .await
        .expect("first Mongo commit must publish its durable height")
        .expect("ExEx event sender remains live");
    let second_finished = tokio::time::timeout(Duration::from_secs(1), events_rx.recv())
        .await
        .expect("queued Mongo commit must follow the first commit")
        .expect("ExEx event sender remains live");
    task.abort();

    assert!(
        readiness_advanced.is_ok(),
        "logical readiness must advance before the blocked Mongo write is acknowledged"
    );
    assert!(
        later_readiness_advanced.is_ok(),
        "a blocked Mongo writer must not stop later finalized blocks from advancing readiness"
    );
    assert_eq!(
        first_finished,
        ExExEvent::FinishedHeight((1, block_hash).into())
    );
    assert_eq!(
        second_finished,
        ExExEvent::FinishedHeight((2, next_block_hash).into())
    );
}

#[tokio::test]
async fn restart_replays_after_the_durable_checkpoint_before_mongo_catches_up() {
    use futures::channel::mpsc;

    let provider = MockEthProvider::new();
    let durable_hash = add_empty_block(&provider, 1);
    let replayed_hash = add_empty_block(&provider, 2);
    let (durable, write_started, release_write, write_finished) = BlockingWriteStorage::new();
    let projection_config = ProjectionConfig {
        chain_id: 1,
        genesis_hash: B256::repeat_byte(0x11),
        start_block: 1,
    };
    let mut durable_projection =
        OffchainDataProjection::open(projection_config, durable.clone(), durable.clone()).unwrap();
    durable_projection
        .project_block(&FinalizedBlock {
            number: 1,
            hash: durable_hash,
            receipts: Vec::new(),
        })
        .unwrap();
    drop(durable_projection);

    let overlay = Arc::new(PendingOverlayStorage::new(durable.clone()));
    let logical_reader: StorageReaderHandle = overlay.clone();
    let logical_writer: StorageWriterHandle = overlay.clone();
    let projector =
        OffchainDataProjection::open(projection_config, logical_reader.clone(), logical_writer)
            .unwrap();
    let durable_checkpoint = ProjectionCheckpoint {
        block_number: 1,
        block_hash: durable_hash,
    };
    let (readiness_publisher, readiness) = projection_readiness(
        ProjectionCheckpoint {
            block_number: 0,
            block_hash: projection_config.genesis_hash,
        },
        ProjectionStatus::Ready {
            checkpoint: durable_checkpoint,
        },
    );
    let (runtime_failure_tx, runtime_failure_rx) = tokio::sync::watch::channel(None);
    let (_notification_tx, notification_rx) = mpsc::channel(1);
    let (finality_tx, finality_rx) = mpsc::unbounded();
    let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
    let (exit_tx, mut exit_rx) = tokio::sync::mpsc::unbounded_channel();
    durable.block_next_write.store(true, Ordering::Release);
    let task = tokio::spawn(run_projection_loop(
        provider,
        notification_rx,
        finality_rx,
        events_tx,
        ProjectionRuntime {
            projector,
            readiness_publisher,
            projection_config,
            _reader: logical_reader,
            overlay: Some(overlay.clone()),
            writer: durable.clone(),
            _writer_lease: None,
            runtime_failure_sender: Some(runtime_failure_tx),
            runtime_failure_receiver: Some(runtime_failure_rx),
        },
        exit_tx,
    ));

    finality_tx
        .unbounded_send(FinalizedTarget::new(2, replayed_hash))
        .unwrap();
    tokio::task::spawn_blocking(move || write_started.recv().unwrap())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if matches!(
                readiness.current(),
                ProjectionStatus::Ready { checkpoint }
                    if checkpoint.block_number == 2 && checkpoint.block_hash == replayed_hash
            ) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("retained finalized receipts must rebuild logical readiness after restart");
    assert_eq!(
        outbe_offchain_data::read_projection_state(projection_config, durable.clone())
            .unwrap()
            .unwrap()
            .checkpoint,
        Some(durable_checkpoint),
        "Mongo checkpoint must remain the restart cursor until the replayed batch commits"
    );
    assert!(events_rx.try_recv().is_err());
    assert!(exit_rx.try_recv().is_err());

    release_write.send(()).unwrap();
    tokio::task::spawn_blocking(move || write_finished.recv().unwrap())
        .await
        .unwrap();
    let event = tokio::time::timeout(Duration::from_secs(1), events_rx.recv())
        .await
        .expect("replayed batch must eventually commit to Mongo")
        .expect("ExEx event sender remains live");
    assert_eq!(event, ExExEvent::FinishedHeight((2, replayed_hash).into()));
    task.abort();
}

#[tokio::test]
async fn fatal_status_stays_sticky_when_detached_worker_finishes_late() {
    use futures::channel::mpsc;

    let provider = MockEthProvider::new();
    let block_hash = add_empty_block(&provider, 1);
    let (storage, write_started, release_write, write_finished) = BlockingWriteStorage::new();
    let reader: StorageReaderHandle = storage.clone();
    let writer: StorageWriterHandle = storage.clone();
    let projection_config = ProjectionConfig {
        chain_id: 1,
        genesis_hash: B256::repeat_byte(0x11),
        start_block: 1,
    };
    let projector =
        OffchainDataProjection::open(projection_config, reader.clone(), writer.clone()).unwrap();
    let checkpoint = ProjectionCheckpoint {
        block_number: 0,
        block_hash: projection_config.genesis_hash,
    };
    let (readiness_publisher, readiness) = projection_readiness(
        checkpoint,
        outbe_offchain_data::ProjectionStatus::Ready { checkpoint },
    );
    let (runtime_failure_tx, runtime_failure_rx) = tokio::sync::watch::channel(None);
    let (_notification_tx, notification_rx) = mpsc::channel(1);
    let (finality_tx, finality_rx) = mpsc::unbounded();
    let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
    let (exit_tx, mut exit_rx) = tokio::sync::mpsc::unbounded_channel();
    storage.block_next_write.store(true, Ordering::Release);
    let task = tokio::spawn(run_projection_loop(
        provider,
        notification_rx,
        finality_rx,
        events_tx,
        ProjectionRuntime {
            projector,
            readiness_publisher,
            projection_config,
            _reader: reader,
            overlay: None,
            writer,
            _writer_lease: None,
            runtime_failure_sender: Some(runtime_failure_tx.clone()),
            runtime_failure_receiver: Some(runtime_failure_rx),
        },
        exit_tx,
    ));

    finality_tx
        .unbounded_send(FinalizedTarget::new(1, block_hash))
        .unwrap();
    tokio::task::spawn_blocking(move || write_started.recv().unwrap())
        .await
        .unwrap();
    runtime_failure_tx.send_replace(Some(outbe_offchain_data::RuntimeBodyFailure::Fatal(
        ProjectionFailure::new(ProjectionFailureClass::Other, "injected terminal failure"),
    )));
    let exit = tokio::time::timeout(Duration::from_secs(1), exit_rx.recv())
        .await
        .expect("fatal body-read failure must reach the lifecycle owner")
        .expect("exit channel remains open");
    assert_eq!(exit.failure.class, ProjectionFailureClass::Other);

    release_write.send(()).unwrap();
    tokio::task::spawn_blocking(move || write_finished.recv().unwrap())
        .await
        .unwrap();
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }

    assert!(matches!(
        readiness.current(),
        outbe_offchain_data::ProjectionStatus::Fatal { error, .. }
            if error.class == ProjectionFailureClass::Other
    ));
    assert!(events_rx.try_recv().is_err());
    assert!(!task.is_finished());
    task.abort();
}
