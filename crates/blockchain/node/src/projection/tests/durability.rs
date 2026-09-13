use super::*;

#[test]
fn dropping_projection_waiter_never_waits_for_blocked_backend_work() {
    let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let receiver = spawn_detached_projection_work("projection-shutdown-test", move || {
        let _ = started_tx.send(());
        let _ = release_rx.recv();
    })
    .unwrap();
    started_rx.recv().unwrap();

    let started = std::time::Instant::now();
    drop(receiver);
    assert!(started.elapsed() < Duration::from_millis(50));
    release_tx.send(()).unwrap();
}

#[test]
fn projects_each_intermediate_block_and_reports_each_durable_checkpoint() {
    let provider = MockEthProvider::<reth_ethereum::EthPrimitives>::new();
    let first = add_empty_block(&provider, 1);
    let second = add_empty_block(&provider, 2);
    let runtime = initialized_runtime(1);
    let (logical_tx, mut logical_rx) = tokio::sync::mpsc::unbounded_channel();
    let (write_tx, mut write_rx) = tokio::sync::mpsc::unbounded_channel();
    let (recovery_tx, _recovery_rx) = tokio::sync::mpsc::unbounded_channel();

    let result = project_through_target(
        provider,
        &runtime,
        FinalizedTarget::new(2, second),
        &logical_tx,
        &write_tx,
        &recovery_tx,
    )
    .unwrap();

    assert_eq!(result, Some(FinalizedTarget::new(2, second)));
    assert_eq!(
        logical_rx.try_recv().unwrap(),
        FinalizedTarget::new(1, first)
    );
    assert_eq!(
        logical_rx.try_recv().unwrap(),
        FinalizedTarget::new(2, second)
    );
    assert_eq!(
        write_rx.try_recv().unwrap().checkpoint,
        FinalizedTarget::new(1, first)
    );
    assert_eq!(
        write_rx.try_recv().unwrap().checkpoint,
        FinalizedTarget::new(2, second)
    );
    assert!(write_rx.try_recv().is_err());
    let state = runtime.lock().unwrap();
    let checkpoint = state.projector.state().checkpoint.unwrap();
    assert_eq!(checkpoint.block_number, 2);
    assert_eq!(checkpoint.block_hash, second);
}

#[test]
fn frame_sink_returns_only_after_the_exact_durable_write_finishes() {
    let (durable, write_started, release_write, _write_finished) = BlockingWriteStorage::new();
    let projection_config = ProjectionConfig {
        chain_id: 1,
        genesis_hash: B256::repeat_byte(0x11),
        start_block: 1,
    };
    OffchainDataProjection::open(projection_config, durable.clone(), durable.clone()).unwrap();
    let overlay = Arc::new(PendingOverlayStorage::new(durable.clone()));
    let reader: StorageReaderHandle = overlay.clone();
    let logical_writer: StorageWriterHandle = overlay.clone();
    let durable_writer: StorageWriterHandle = durable.clone();
    let projector =
        OffchainDataProjection::open(projection_config, reader.clone(), logical_writer).unwrap();
    let (readiness_publisher, _readiness) = projection_readiness(
        ProjectionCheckpoint {
            block_number: 0,
            block_hash: projection_config.genesis_hash,
        },
        ProjectionStatus::Starting,
    );
    let (runtime_failure_tx, runtime_failure_rx) = tokio::sync::watch::channel(None);
    let mut sink = FinalizedProjectionSink::from_runtime(ProjectionRuntime {
        projector,
        readiness_publisher,
        projection_config,
        _reader: reader,
        overlay: Some(overlay),
        writer: durable_writer,
        _writer_lease: None,
        runtime_failure_sender: Some(runtime_failure_tx),
        runtime_failure_receiver: Some(runtime_failure_rx),
    });
    let frame = empty_finalized_frame(1, 1);
    durable.block_next_write.store(true, Ordering::Release);
    let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
    let sink_thread = std::thread::spawn(move || {
        let result = sink.project_frame(&frame);
        let _ = result_tx.send(result);
    });

    write_started
        .recv_timeout(Duration::from_secs(1))
        .expect("the sink must submit the exact durable batch");
    assert!(matches!(
        result_rx.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    release_write.send(()).unwrap();
    let checkpoint = result_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("the sink must return after durable commit")
        .unwrap();
    sink_thread.join().unwrap();

    assert_eq!(checkpoint.block_number, 1);
    assert_eq!(
        outbe_offchain_data::read_projection_state(projection_config, durable)
            .unwrap()
            .unwrap()
            .checkpoint,
        Some(checkpoint)
    );
}

#[test]
fn frame_sink_accepts_restart_replay_below_durable_p_and_rejects_conflicting_p() {
    let storage = Arc::new(MemoryStorage::new());
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
    let (readiness_publisher, readiness) = projection_readiness(
        ProjectionCheckpoint {
            block_number: 0,
            block_hash: projection_config.genesis_hash,
        },
        ProjectionStatus::Starting,
    );
    let (runtime_failure_tx, runtime_failure_rx) = tokio::sync::watch::channel(None);
    let mut sink = FinalizedProjectionSink::from_runtime(ProjectionRuntime {
        projector,
        readiness_publisher,
        projection_config,
        _reader: reader,
        overlay: Some(overlay),
        writer: durable_writer,
        _writer_lease: None,
        runtime_failure_sender: Some(runtime_failure_tx),
        runtime_failure_receiver: Some(runtime_failure_rx),
    });
    let first = empty_finalized_frame(1, 1);
    let second = empty_finalized_frame(2, 2);
    let conflicting_second = empty_finalized_frame(2, 3);
    let first_target = ProjectionCheckpoint {
        block_number: first.identity().number,
        block_hash: first.identity().hash,
    };

    assert_eq!(
        sink.reconcile_finalized_target(Some(first_target)).unwrap(),
        FinalizedTargetReconciliationV1::Process {
            target: first_target,
            recovered_floor: None,
        },
    );
    assert_eq!(
        sink.reconcile_finalized_target(None).unwrap(),
        FinalizedTargetReconciliationV1::AwaitingProviderRecovery,
    );
    assert_eq!(
        readiness.current(),
        ProjectionStatus::CatchingUp { checkpoint: None }
    );

    let first_checkpoint = sink.project_frame(&first).unwrap();
    sink.publish_progress(ProjectionCheckpoint {
        block_number: second.identity().number,
        block_hash: second.identity().hash,
    })
    .unwrap();
    assert_eq!(
        readiness.current(),
        ProjectionStatus::CatchingUp {
            checkpoint: Some(first_checkpoint),
        }
    );
    let durable_p = sink.project_frame(&second).unwrap();
    sink.publish_progress(durable_p).unwrap();
    assert_eq!(
        readiness.current(),
        ProjectionStatus::Ready {
            checkpoint: durable_p,
        }
    );

    assert_eq!(
        sink.reconcile_finalized_target(None).unwrap(),
        FinalizedTargetReconciliationV1::AwaitingProviderRecovery,
    );
    assert_eq!(
        readiness.current(),
        ProjectionStatus::CatchingUp {
            checkpoint: Some(durable_p),
        }
    );
    assert_eq!(
        sink.reconcile_finalized_target(Some(first_checkpoint))
            .unwrap(),
        FinalizedTargetReconciliationV1::AwaitingProviderRecovery,
    );
    assert_eq!(
        sink.reconcile_finalized_target(Some(durable_p)).unwrap(),
        FinalizedTargetReconciliationV1::Process {
            target: durable_p,
            recovered_floor: Some(durable_p),
        },
    );
    sink.publish_progress(durable_p).unwrap();
    assert_eq!(
        readiness.current(),
        ProjectionStatus::Ready {
            checkpoint: durable_p,
        }
    );
    assert!(sink
        .reconcile_finalized_target(Some(ProjectionCheckpoint {
            block_number: durable_p.block_number,
            block_hash: B256::repeat_byte(0xff),
        }))
        .is_err());

    assert_eq!(sink.project_frame(&first).unwrap(), durable_p);
    let error = sink.project_frame(&conflicting_second).unwrap_err();
    assert!(error
        .to_string()
        .contains("conflicts with durable projection hash"));
    assert_eq!(sink.durable_checkpoint(), Some(durable_p));

    drop(sink);
    let overlay = Arc::new(PendingOverlayStorage::new(storage.clone()));
    let reader: StorageReaderHandle = overlay.clone();
    let logical_writer: StorageWriterHandle = overlay.clone();
    let durable_writer: StorageWriterHandle = storage;
    let projector =
        OffchainDataProjection::open(projection_config, reader.clone(), logical_writer).unwrap();
    let (readiness_publisher, _readiness) = projection_readiness(
        ProjectionCheckpoint {
            block_number: 0,
            block_hash: projection_config.genesis_hash,
        },
        ProjectionStatus::CatchingUp {
            checkpoint: Some(durable_p),
        },
    );
    let (runtime_failure_tx, runtime_failure_rx) = tokio::sync::watch::channel(None);
    let mut restarted = FinalizedProjectionSink::from_runtime(ProjectionRuntime {
        projector,
        readiness_publisher,
        projection_config,
        _reader: reader,
        overlay: Some(overlay),
        writer: durable_writer,
        _writer_lease: None,
        runtime_failure_sender: Some(runtime_failure_tx),
        runtime_failure_receiver: Some(runtime_failure_rx),
    });
    let ahead = ProjectionCheckpoint {
        block_number: durable_p.block_number + 1,
        block_hash: B256::repeat_byte(0x33),
    };
    assert_eq!(
        restarted.reconcile_finalized_target(Some(ahead)).unwrap(),
        FinalizedTargetReconciliationV1::Process {
            target: ahead,
            recovered_floor: Some(durable_p),
        },
    );
}

#[test]
fn later_provider_failure_keeps_and_reports_earlier_durable_checkpoint() {
    let provider = MockEthProvider::<reth_ethereum::EthPrimitives>::new();
    let first = add_empty_block(&provider, 1);
    let runtime = initialized_runtime(1);
    let (logical_tx, mut logical_rx) = tokio::sync::mpsc::unbounded_channel();
    let (write_tx, mut write_rx) = tokio::sync::mpsc::unbounded_channel();
    let (recovery_tx, _recovery_rx) = tokio::sync::mpsc::unbounded_channel();

    let error = project_through_target(
        provider,
        &runtime,
        FinalizedTarget::new(2, B256::repeat_byte(2)),
        &logical_tx,
        &write_tx,
        &recovery_tx,
    )
    .unwrap_err();

    assert!(error
        .to_string()
        .contains("canonical block 2 is unavailable"));
    assert_eq!(
        projection_failure_class(&error),
        ProjectionFailureClass::HistoricalReceiptsUnavailable
    );
    assert_eq!(
        logical_rx.try_recv().unwrap(),
        FinalizedTarget::new(1, first)
    );
    assert_eq!(
        write_rx.try_recv().unwrap().checkpoint,
        FinalizedTarget::new(1, first)
    );
    assert!(write_rx.try_recv().is_err());
    let state = runtime.lock().unwrap();
    let checkpoint = state.projector.state().checkpoint.unwrap();
    assert_eq!(checkpoint.block_number, 1);
    assert_eq!(checkpoint.block_hash, first);
}

#[test]
fn ambiguous_mongo_result_retries_the_same_batch_before_advancing_durable_height() {
    let storage = Arc::new(AmbiguousFirstWriteStorage::default());
    let overlay = Arc::new(PendingOverlayStorage::new(storage.inner.clone()));
    let writer: StorageWriterHandle = storage.clone();
    let namespace = Namespace::new("records").unwrap();
    let key = Key::new(b"key".to_vec()).unwrap();
    let value = outbe_offchain_storage::Value::new(b"value".to_vec()).unwrap();
    let batch = AtomicWriteBatch::from_operations(vec![AtomicWriteOperation::put(
        namespace.clone(),
        key.clone(),
        value,
    )]);
    let checkpoint = FinalizedTarget::new(7, B256::repeat_byte(0x77));
    let (write_tx, write_rx) = tokio::sync::mpsc::unbounded_channel();
    let (checkpoint_tx, mut checkpoint_rx) = tokio::sync::mpsc::unbounded_channel();
    let writer_thread = std::thread::spawn(move || {
        super::run_durable_projection_writer(writer, write_rx, checkpoint_tx);
    });

    storage.ambiguous_next.store(true, Ordering::Release);
    overlay.apply_atomic(&batch).unwrap();
    let overlay_generation = overlay.current_generation();
    write_tx
        .send(super::DurableProjectionWrite {
            checkpoint,
            batch,
            overlay_ack: Some((overlay.clone(), overlay_generation)),
        })
        .unwrap();
    let durable = checkpoint_rx
        .blocking_recv()
        .expect("the exact batch must be retried after an ambiguous result");

    assert_eq!(durable, checkpoint);
    assert_eq!(storage.attempts.load(Ordering::Acquire), 2);
    assert_eq!(
        storage
            .inner
            .get(namespace.clone(), &key)
            .unwrap()
            .unwrap()
            .as_bytes(),
        b"value"
    );
    storage
        .inner
        .put(
            namespace.clone(),
            &key,
            &outbe_offchain_storage::Value::new(b"base-after-ack".to_vec()).unwrap(),
        )
        .unwrap();
    assert_eq!(
        overlay.get(namespace, &key).unwrap().unwrap().as_bytes(),
        b"base-after-ack",
        "durable ACK must retire the acknowledged overlay generation"
    );
    drop(write_tx);
    writer_thread.join().unwrap();
}

#[test]
fn durable_write_deadline_has_a_typed_error_for_lifecycle_classification() {
    let storage = Arc::new(FailAfterStartupStorage::default());
    storage
        .fail_writes_unavailable
        .store(true, Ordering::SeqCst);
    let writer: StorageWriterHandle = storage;
    let checkpoint = FinalizedTarget::new(7, B256::repeat_byte(0x77));
    let batch = AtomicWriteBatch::from_operations(vec![AtomicWriteOperation::put(
        Namespace::new("records").unwrap(),
        Key::new(b"key".to_vec()).unwrap(),
        outbe_offchain_storage::Value::new(b"value".to_vec()).unwrap(),
    )]);

    let report = apply_durable_projection_write_until(
        &writer,
        &DurableProjectionWrite {
            checkpoint,
            batch,
            overlay_ack: None,
        },
        Duration::ZERO,
    )
    .unwrap_err();
    let error = report
        .downcast_ref::<ProjectionWriteDeadlineError>()
        .expect("unavailable storage must preserve the typed recovery deadline");

    assert_eq!(error.block_number, checkpoint.number);
    assert_eq!(error.block_hash, checkpoint.hash);
}

#[test]
fn successful_write_after_the_absolute_deadline_never_publishes_progress() {
    struct SlowSuccessfulStorage;

    impl StorageWriter for SlowSuccessfulStorage {
        fn apply_atomic(&self, _batch: &AtomicWriteBatch) -> Result<(), StorageError> {
            std::thread::sleep(Duration::from_millis(10));
            Ok(())
        }
    }

    let writer: StorageWriterHandle = Arc::new(SlowSuccessfulStorage);
    let checkpoint = FinalizedTarget::new(7, B256::repeat_byte(0x78));
    let batch = AtomicWriteBatch::from_operations(vec![AtomicWriteOperation::put(
        Namespace::new("records").unwrap(),
        Key::new(b"key".to_vec()).unwrap(),
        outbe_offchain_storage::Value::new(b"value".to_vec()).unwrap(),
    )]);
    let report = apply_durable_projection_write_before(
        &writer,
        &DurableProjectionWrite {
            checkpoint,
            batch,
            overlay_ack: None,
        },
        std::time::Instant::now() + Duration::from_millis(1),
    )
    .unwrap_err();

    assert_eq!(
        projection_frame_failure_class(&report),
        ProjectionFailureClass::MongoReconnectDeadline
    );
}

#[test]
fn deterministic_projection_storage_failures_are_immediate_and_typed() {
    let checkpoint = FinalizedTarget::new(7, B256::repeat_byte(0x77));
    let batch = || {
        AtomicWriteBatch::from_operations(vec![AtomicWriteOperation::put(
            Namespace::new("records").unwrap(),
            Key::new(b"key".to_vec()).unwrap(),
            outbe_offchain_storage::Value::new(b"value".to_vec()).unwrap(),
        )])
    };

    for (lease_lost, expected) in [
        (false, ProjectionFailureClass::CorruptBody),
        (true, ProjectionFailureClass::WriterLeaseLost),
    ] {
        let storage = Arc::new(FailAfterStartupStorage::default());
        if lease_lost {
            storage.lose_writer_lease.store(true, Ordering::SeqCst);
        } else {
            storage.fail_writes.store(true, Ordering::SeqCst);
        }
        let writer: StorageWriterHandle = storage;
        let started = std::time::Instant::now();
        let report = apply_durable_projection_write_until(
            &writer,
            &DurableProjectionWrite {
                checkpoint,
                batch: batch(),
                overlay_ack: None,
            },
            PROJECTION_RECOVERY_DEADLINE,
        )
        .unwrap_err();
        assert!(started.elapsed() < Duration::from_millis(100));
        assert_eq!(projection_frame_failure_class(&report), expected);
    }
}

#[test]
fn every_storage_failure_kind_has_a_stable_projection_class() {
    let cases = [
        (
            StorageError::InvalidArgument("invalid".to_owned()),
            ProjectionFailureClass::StorageInvalidArgument,
        ),
        (
            StorageError::Unavailable {
                source: Box::new(std::io::Error::other("unavailable")),
            },
            ProjectionFailureClass::StorageUnavailable,
        ),
        (
            StorageError::Corruption("corrupt".to_owned()),
            ProjectionFailureClass::CorruptBody,
        ),
        (
            StorageError::Backend {
                source: Box::new(std::io::Error::other("backend")),
            },
            ProjectionFailureClass::StorageBackend,
        ),
        (
            StorageError::RequestDeadline,
            ProjectionFailureClass::StorageRequestDeadline,
        ),
        (
            StorageError::WriterLeaseLost,
            ProjectionFailureClass::WriterLeaseLost,
        ),
    ];
    for (error, expected) in cases {
        let report = eyre::Report::new(error);
        assert_eq!(projection_frame_failure_class(&report), expected);
    }
}

#[test]
fn writer_lease_loss_has_a_distinct_failure_class() {
    let error = eyre::Report::new(StorageError::WriterLeaseLost);

    assert_eq!(
        projection_failure_class(&error),
        ProjectionFailureClass::WriterLeaseLost
    );
}

fn empty_finalized_frame(number: u64, timestamp: u64) -> crate::finalized_frame::FinalizedFrame {
    let provider = MockEthProvider::<reth_ethereum::EthPrimitives>::new();
    let header = Header {
        number,
        timestamp,
        ..Default::default()
    };
    let hash = header.hash_slow();
    provider.add_block(hash, Block::new(header, Default::default()));
    provider.add_receipts(number, Vec::new());
    let source = RethFinalizedFrameSource::new(provider);
    read_bounded_finalized_frames(&source, number, BlockNumHash::new(number, hash))
        .unwrap()
        .unwrap()
        .frames()[0]
        .clone()
}

#[derive(Default)]
struct AmbiguousFirstWriteStorage {
    inner: Arc<MemoryStorage>,
    ambiguous_next: AtomicBool,
    attempts: AtomicUsize,
}

impl StorageWriter for AmbiguousFirstWriteStorage {
    fn apply_atomic(&self, batch: &AtomicWriteBatch) -> Result<(), StorageError> {
        self.attempts.fetch_add(1, Ordering::AcqRel);
        self.inner.apply_atomic(batch)?;
        if self.ambiguous_next.swap(false, Ordering::AcqRel) {
            return Err(StorageError::Unavailable {
                source: Box::new(std::io::Error::other("injected ambiguous MongoDB result")),
            });
        }
        Ok(())
    }
}

#[test]
fn retained_gc_claim_waits_for_projection_commit_fence() {
    let fence = Arc::new(ProjectionRetentionFence::default());
    let projection = fence
        .projection_guard()
        .expect("projection acquires shared fence");
    let worker_fence = Arc::clone(&fence);
    let (claimed_tx, claimed_rx) = std::sync::mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        let _claim = worker_fence
            .gc_claim_guard()
            .expect("GC acquires exclusive fence");
        claimed_tx.send(()).expect("publish GC claim");
    });

    assert!(claimed_rx.recv_timeout(Duration::from_millis(25)).is_err());
    drop(projection);
    claimed_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("GC claim proceeds after projection commit fence is released");
    worker.join().expect("GC fence worker joins");
}
