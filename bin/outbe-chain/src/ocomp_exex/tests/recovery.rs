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
async fn real_reth_restart_stream_never_reexecutes_an_older_closure_against_ce() {
    use outbe_compressed_entities::{
        CandidateCacheLimits, CeMdbx, CeTopologyV1, Commitment, CompressedTreeService, EntityRef,
        EnvironmentIdentity, ExactParentIdentity, FinalLeafMutation, FinalizedMarker, WwdEntityId,
        ACTIVE_COMMITMENT_SCHEME, LOCAL_STORAGE_SCHEMA_VERSION,
    };
    use outbe_primitives::{OutbeHeader, OutbePrimitives};
    use reth_chainspec::ChainSpecBuilder;
    use reth_ethereum::exex::{ExExHead, ExExNotification, ExExNotifications, Wal};
    use reth_provider::{test_utils::MockEthProvider, Chain};

    for changed_root in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let parent_hash = B256::repeat_byte(0x38);
        let head_hash = B256::repeat_byte(0x39);
        let parent_root = outbe_compressed_entities::sealed_root(B256::ZERO).unwrap();
        let db = CeMdbx::open(
            &root.path().join("ce"),
            EnvironmentIdentity {
                local_storage_schema_version: LOCAL_STORAGE_SCHEMA_VERSION,
                chain_id: outbe_primitives::chain::MAINNET_CHAIN_ID,
                genesis_hash: parent_hash,
                commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                topology: CeTopologyV1.encode(),
                tree_format: "ckb-smt-v0.6.1-poseidon-catalog-v3".to_owned(),
                vendor_revision: "ad555350c866b2265d87d2d7fbd146fbc918bfe5".to_owned(),
            },
            FinalizedMarker {
                commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                height: 0,
                block_hash: parent_hash,
                parent_block_hash: B256::ZERO,
                parent_root: B256::ZERO,
                new_root: parent_root,
            },
        )
        .unwrap();
        let ce = Arc::new(
            CompressedTreeService::new(
                db,
                CandidateCacheLimits {
                    max_candidates: 4,
                    max_encoded_bytes: 1_000_000,
                },
            )
            .unwrap(),
        );
        let parent = ExactParentIdentity {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            block_number: 0,
            block_hash: parent_hash,
            root: parent_root,
        };
        let mut id = [7_u8; 32];
        id[..4].copy_from_slice(&1_u32.to_be_bytes());
        let mutations = if changed_root {
            vec![FinalLeafMutation {
                entity: EntityRef::Tribute(WwdEntityId::try_from(id.as_slice()).unwrap()),
                final_leaf: Some(Commitment::try_from([3_u8; 32]).unwrap()),
            }]
        } else {
            Vec::new()
        };
        let batch = ce
            .open_parent(parent)
            .unwrap()
            .prepare_seal(1, &mutations, &[])
            .unwrap();
        let head_root = batch.new_root();
        ce.publish_candidate(head_hash, batch).unwrap();
        ce.apply_finalized(1, head_hash, head_root).unwrap();
        assert_eq!(head_root != parent_root, changed_root);
        assert!(
            ce.open_parent(parent).is_err(),
            "historical CE parent must remain rejected"
        );
        let marker = ce.finalized_marker().unwrap();

        let spec = Arc::new(
            ChainSpecBuilder::mainnet()
                .chain(outbe_primitives::chain::MAINNET_CHAIN_ID.into())
                .build()
                .map_header(OutbeHeader::new),
        );
        let evm = outbe_evm::OutbeEvmConfig::new(spec).with_compressed_tree_service(ce.clone());
        // Deliberately no historical bodies: accidental execution backfill
        // cannot silently pass by executing an empty block fixture.
        let provider = MockEthProvider::<OutbePrimitives>::new();
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        let wal = Wal::<OutbePrimitives>::new(root.path().join("wal")).unwrap();
        let stream =
            ExExNotifications::new((1, head_hash).into(), provider, evm, receiver, wal.handle())
                .with_head(ExExHead::new((0, parent_hash).into()));
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
        assert_eq!(ce.finalized_marker().unwrap(), marker);
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
        "replay pressure must allow the independent GC worker to reclaim closed history"
    );
}

#[tokio::test]
async fn live_drain_does_not_wait_for_blocked_projection_or_provider_work() {
    let (mut sender, receiver) = futures::channel::mpsc::channel::<eyre::Result<u64>>(1);
    let drain = tokio::spawn(drain_exex_notifications(receiver));
    // No consumer/projection progress is supplied. More live notifications
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

#[test]
fn cleared_or_replaced_outage_generation_cannot_trigger_an_old_deadline() {
    let since = tokio::time::Instant::now() - PROJECTION_RECOVERY_DEADLINE;
    let std_since = since.into_std();
    let (sender, receiver) = tokio::sync::watch::channel(Some(RuntimeBodyFailure::Unavailable {
        generation: 7,
        since: std_since,
    }));
    assert!(consume_projection_runtime_deadline(&receiver, &mut Some((7, since)),).is_some());

    sender.send_replace(None);
    let mut cleared_state = Some((7, since));
    assert!(consume_projection_runtime_deadline(&receiver, &mut cleared_state).is_none());
    assert!(cleared_state.is_none());

    sender.send_replace(Some(RuntimeBodyFailure::Unavailable {
        generation: 8,
        since: std_since,
    }));
    let mut replaced_state = Some((7, since));
    assert!(consume_projection_runtime_deadline(&receiver, &mut replaced_state).is_none());
    assert!(replaced_state.is_none());
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
fn deterministic_projection_task_error_is_not_reported_as_mongo_timeout() {
    let failure = projection_task_failure(eyre::eyre!("malformed finalized frame"));
    assert_eq!(failure.class, ProjectionFailureClass::Other);
    assert!(failure.message.contains("malformed finalized frame"));
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
