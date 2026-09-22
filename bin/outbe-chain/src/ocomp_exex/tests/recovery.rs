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
    if !crate::test_utils::in_isolated_process(
        "ocomp_exex::tests::recovery::real_reth_restart_stream_never_reexecutes_an_older_closure_against_ce",
    ) {
        return;
    }
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

// Native component restart tests; these do not claim a full process launch.
pub(super) mod copied_native {
    use super::*;
    use alloy_consensus::{Header, Sealable, SignableTransaction, TxLegacy};
    use alloy_primitives::{Signature, U256};
    use outbe_node::finalized_frame::{read_bounded_finalized_frames, RethFinalizedFrameSource};
    use outbe_ocomp::{
        control::poc_schema_limits,
        embedded_runtime::{EmbeddedOcompBundleConfigV1, EmbeddedOcompDomainConfigV1},
    };
    use outbe_primitives::{OutbeHeader, OutbePrimitives, OutbeReceipt, OutbeTxEnvelope};
    use reth_ethereum::provider::db::{
        database::Database,
        init_db,
        mdbx::DatabaseArguments,
        models::StoredBlockBodyIndices,
        tables,
        transaction::{DbTx, DbTxMut},
    };
    use reth_provider::{
        providers::{
            ProviderFactoryBuilder, ReadOnlyConfig, RocksDBProvider, StaticFileProviderBuilder,
        },
        BlockHashReader, BlockIdReader, BlockReader, ReceiptProvider, StateProviderFactory,
        StaticFileSegment, StaticFileWriter,
    };
    use std::{fs, path::Path};

    pub(in crate::ocomp_exex::tests) fn copy_tree(from: &Path, to: &Path) {
        assert!(from.is_dir());
        fs::create_dir_all(to).unwrap();
        for child in fs::read_dir(from).unwrap() {
            let child = child.unwrap();
            let source = child.path();
            let destination = to.join(child.file_name());
            let metadata = fs::symlink_metadata(&source).unwrap();
            assert!(!metadata.file_type().is_symlink());
            if metadata.is_dir() {
                copy_tree(&source, &destination);
            } else {
                assert!(metadata.is_file());
                fs::copy(&source, &destination).unwrap();
            }
            fs::set_permissions(destination, metadata.permissions()).unwrap();
        }
    }

    pub(in crate::ocomp_exex::tests) fn chain() -> Arc<reth_chainspec::ChainSpec<OutbeHeader>> {
        // Building mainnet recomputes its allocation trie. Reuse the immutable
        // fixture across frames and restarts instead of rebuilding it per read.
        static CHAIN: std::sync::OnceLock<Arc<reth_chainspec::ChainSpec<OutbeHeader>>> =
            std::sync::OnceLock::new();
        CHAIN
            .get_or_init(|| {
                Arc::new(
                    reth_chainspec::ChainSpecBuilder::mainnet()
                        .build()
                        .map_header(OutbeHeader::new),
                )
            })
            .clone()
    }

    // Real MDBX headers/body indices/receipts and static-file transactions.
    // Frames are storage fixtures, not claims that their transactions were executed.
    pub(in crate::ocomp_exex::tests) fn write_frames(
        root: &Path,
        first: u64,
        last: u64,
    ) -> Vec<ProjectionCheckpoint> {
        let db = init_db(root.join("db"), DatabaseArguments::test()).unwrap();
        let tx = db.tx_mut().unwrap();
        let settings = reth_provider::StorageSettings::v1();
        match tx
            .get::<tables::Metadata>("storage_settings".into())
            .unwrap()
        {
            Some(bytes) => assert!(!serde_json::from_slice::<reth_provider::StorageSettings>(
                &bytes
            )
            .unwrap()
            .is_v2()),
            None => tx
                .put::<tables::Metadata>(
                    "storage_settings".into(),
                    serde_json::to_vec(&settings).unwrap(),
                )
                .unwrap(),
        }
        let mut parent = if first == 0 {
            B256::ZERO
        } else {
            tx.get::<tables::CanonicalHeaders>(first - 1)
                .unwrap()
                .unwrap()
        };
        let files = StaticFileProviderBuilder::read_write(root.join("static_files"))
            .with_blocks_per_file(1_000)
            .build::<OutbePrimitives>()
            .unwrap();
        let mut writer = files
            .get_writer(first, StaticFileSegment::Transactions)
            .unwrap();
        let mut headers = files.get_writer(first, StaticFileSegment::Headers).unwrap();
        let mut points = Vec::new();
        for height in first..=last {
            let transaction: OutbeTxEnvelope = TxLegacy {
                nonce: height,
                gas_limit: 21_000,
                ..Default::default()
            }
            .into_signed(Signature::new(U256::ONE, U256::ONE, false))
            .into();
            let receipt = OutbeReceipt {
                success: true,
                cumulative_gas_used: 21_000,
                ..Default::default()
            };
            let header = if height == 0 {
                chain().genesis_header().clone()
            } else {
                OutbeHeader::new(Header {
                    number: height,
                    parent_hash: parent,
                    timestamp: height,
                    gas_limit: 30_000_000,
                    gas_used: 21_000,
                    transactions_root: alloy_consensus::proofs::calculate_transaction_root(
                        std::slice::from_ref(&transaction),
                    ),
                    receipts_root: alloy_consensus::proofs::calculate_receipt_root(&[
                        alloy_consensus::TxReceipt::with_bloom_ref(&receipt),
                    ]),
                    ..Default::default()
                })
            };
            let hash = header.hash_slow();
            headers.append_header(&header, &hash).unwrap();
            tx.put::<tables::CanonicalHeaders>(height, hash).unwrap();
            tx.put::<tables::HeaderNumbers>(hash, height).unwrap();
            tx.put::<tables::Headers<OutbeHeader>>(height, header)
                .unwrap();
            tx.put::<tables::BlockBodyIndices>(
                height,
                StoredBlockBodyIndices {
                    first_tx_num: height.saturating_sub(1),
                    tx_count: u64::from(height != 0),
                },
            )
            .unwrap();
            writer.increment_block(height).unwrap();
            if height != 0 {
                writer.append_transaction(height - 1, &transaction).unwrap();
                tx.put::<tables::Receipts<OutbeReceipt>>(height - 1, receipt)
                    .unwrap();
            }
            points.push(ProjectionCheckpoint {
                block_number: height,
                block_hash: hash,
            });
            parent = hash;
        }
        drop(writer);
        drop(headers);
        files.commit().unwrap();
        type Stage = <tables::StageCheckpoints as reth_ethereum::provider::db::table::Table>::Value;
        for stage in ["Headers", "Bodies", "Execution", "Finish"] {
            tx.put::<tables::StageCheckpoints>(stage.into(), Stage::new(last))
                .unwrap();
        }
        tx.put::<tables::ChainState>(tables::ChainStateKey::LastFinalizedBlock, last)
            .unwrap();
        tx.commit().unwrap();
        drop(files);
        drop(db);
        drop(
            RocksDBProvider::builder(root.join("rocksdb"))
                .with_default_tables()
                .build()
                .unwrap(),
        );
        points
    }

    pub(in crate::ocomp_exex::tests) fn provider(
        root: &Path,
    ) -> impl BlockHashReader
           + BlockReader<Receipt = OutbeReceipt>
           + StateProviderFactory
           + Clone
           + 'static {
        let factory = ProviderFactoryBuilder::<outbe_node::OutbeNode>::default()
            .open_read_only(
                chain(),
                ReadOnlyConfig::from_datadir(root).no_watch(),
                reth_ethereum::tasks::Runtime::test(),
            )
            .unwrap();
        assert!(!reth_provider::StorageSettingsCache::cached_storage_settings(&factory).is_v2());
        reth_provider::providers::BlockchainProvider::new(factory).unwrap()
    }

    pub(in crate::ocomp_exex::tests) fn bundle() -> PinnedProtocolBundle {
        let limits = poc_schema_limits();
        let install = outbe_metadosis::test_support::ForkInstallScenario::final_at(
            outbe_ocompregistry::OCOMP_POC_FINAL_ACTIVATION_HEIGHT,
            chain().chain().id(),
            chain().genesis_hash(),
        )
        .unwrap()
        .into_install();
        let bundle = install.protocol_bundle;
        let hash = bundle.protocol_bundle_hash(&limits).unwrap();
        PinnedProtocolBundle::decode(&bundle.encode_canonical(&limits).unwrap(), hash, &limits)
            .unwrap()
    }

    pub(in crate::ocomp_exex::tests) fn runtime<P>(
        provider: P,
        root: &Path,
        bundle: PinnedProtocolBundle,
    ) -> EmbeddedOcompExExV1<P> {
        runtime_with_endpoint(provider, root, bundle, "127.0.0.1:0".parse().unwrap())
    }

    pub(in crate::ocomp_exex::tests) fn runtime_with_endpoint<P>(
        provider: P,
        root: &Path,
        bundle: PinnedProtocolBundle,
        worker_address: std::net::SocketAddr,
    ) -> EmbeddedOcompExExV1<P> {
        let genesis = ProjectionCheckpoint {
            block_number: 0,
            block_hash: chain().genesis_hash(),
        };
        let closure_checkpoint = ContiguousCheckpointStoreV1::open(
            root.join("exporter-v1/discovery/closure-checkpoint-v1"),
            genesis,
        )
        .unwrap();
        let closed = closure_checkpoint.current().unwrap();
        let domain = EmbeddedOcompDomainV1::open(EmbeddedOcompDomainConfigV1 {
            domain_root: root.to_path_buf(),
            registry_generation: 1,
            bundles: vec![EmbeddedOcompBundleConfigV1 {
                worker_address,
                identity: EndpointIdentity {
                    chain_id: chain().chain().id(),
                    genesis_hash: genesis.block_hash,
                    boot_nonce: B256::repeat_byte(0x81),
                    protocol_bundle_hash: bundle.hash(),
                },
                protocol_bundle: bundle.clone(),
            }],
            policy: EmbeddedNodePolicyV1::FullNode,
            validator_rpc_url: None,
            limits: poc_schema_limits(),
        })
        .unwrap();
        let (publisher, _) = outbe_primitives::projection::projection_readiness(
            closed,
            ProjectionStatus::CatchingUp {
                checkpoint: Some(closed),
            },
        );
        let (exit, _) = tokio::sync::mpsc::unbounded_channel();
        let (compute_tx, compute_rx) = std::sync::mpsc::channel();
        let (vote_tx, vote_rx) = std::sync::mpsc::channel();
        let (materialization_tx, materialization_rx) = std::sync::mpsc::channel();
        let (payout_tx, payout_rx) = std::sync::mpsc::channel();
        let spool = outbe_ocomp::discovery_spool::DiscoverySpoolV1::open(
            root.join("exporter-v1/discovery")
                .join(hex::encode(bundle.hash())),
            chain().chain().id(),
            genesis.block_hash,
            poc_schema_limits(),
        )
        .unwrap();
        EmbeddedOcompExExV1 {
            provider,
            policy: EmbeddedNodePolicyV1::FullNode,
            domain,
            readiness: OcompReadinessV1(Arc::new(std::sync::Mutex::new(publisher))),
            exit,
            requests: BTreeMap::new(),
            materialized_requests: BTreeSet::new(),
            jobs: BTreeMap::new(),
            intent_jobs: BTreeMap::new(),
            discovery_spools: BTreeMap::from([(bundle.hash(), spool)]),
            pending_offers: BTreeMap::new(),
            acknowledged_exports: BTreeSet::new(),
            retention_selector: Arc::new(
                outbe_node::ocomp::retention::SharedOcompRetentionSelector::new(),
            ),
            closure_checkpoint,
            latest_scanned_checkpoint: closed,
            state: EmbeddedOcompJobsV1::new(EmbeddedOcompModeV1::FullNode),
            scanned_height: closed.block_number,
            scanned_hash: closed.block_hash,
            compute_tx,
            compute_rx,
            vote_tx,
            vote_rx,
            materialization_tx,
            materialization_rx,
            materialization_active: None,
            payout_tx,
            payout_rx,
            payout_active: false,
            materialization_attempt_heights: BTreeMap::new(),
            chain_id: chain().chain().id(),
            genesis_hash: genesis.block_hash,
            fatal: None,
        }
    }

    pub(in crate::ocomp_exex::tests) fn catch_up<P>(
        runtime: &mut EmbeddedOcompExExV1<P>,
        target: ProjectionCheckpoint,
    ) -> Vec<u64>
    where
        P: BlockIdReader
            + BlockHashReader
            + BlockReader
            + ReceiptProvider<Receipt = OutbeReceipt>
            + StateProviderFactory
            + Clone
            + Send
            + Sync
            + 'static,
    {
        let source = RethFinalizedFrameSource::new(runtime.provider.clone());
        let mut visited = Vec::new();
        while let Some(batch) = read_bounded_finalized_frames(
            &source,
            runtime.scanned_height + 1,
            (target.block_number, target.block_hash).into(),
        )
        .unwrap()
        {
            for frame in batch.frames() {
                visited.push(frame.identity().number);
                runtime.record_scanned_frame(frame).unwrap();
            }
            runtime.flush_closure_checkpoint().unwrap();
        }
        visited
    }

    #[test]
    fn copied_native_closure_reads_c_plus_one_across_batches_and_reopens_at_k() {
        for closed_height in [3, 205] {
            let donor = tempfile::tempdir().unwrap();
            let receiver = tempfile::tempdir().unwrap();
            let points = write_frames(&donor.path().join("chain"), 0, 205);
            let domain = donor.path().join("ocomp");
            let bundle = bundle();
            let mut initial = runtime(
                provider(&donor.path().join("chain")),
                &domain,
                bundle.clone(),
            );
            catch_up(&mut initial, points[closed_height]);
            drop(initial);
            copy_tree(donor.path(), receiver.path());
            assert!(
                receiver
                    .path()
                    .join("chain/db/mdbx.dat")
                    .metadata()
                    .unwrap()
                    .len()
                    > 0
            );
            donor.close().unwrap();
            let mut restored = runtime(
                provider(&receiver.path().join("chain")),
                &receiver.path().join("ocomp"),
                bundle.clone(),
            );
            let source = RethFinalizedFrameSource::new(restored.provider.clone());
            let mut visited = Vec::new();
            let mut batches = 0;
            while let Some(batch) = read_bounded_finalized_frames(
                &source,
                restored.scanned_height + 1,
                (205, points[205].block_hash).into(),
            )
            .unwrap()
            {
                batches += 1;
                for frame in batch.frames() {
                    visited.push(frame.identity().number);
                    restored.record_scanned_frame(frame).unwrap();
                }
                restored.flush_closure_checkpoint().unwrap();
            }
            assert_eq!(
                visited,
                ((closed_height as u64 + 1)..=205).collect::<Vec<_>>()
            );
            assert_eq!(batches, if closed_height == 3 { 3 } else { 0 });
            assert_eq!(restored.closure_checkpoint.current().unwrap(), points[205]);
            drop(source);
            drop(restored);
            let later = write_frames(&receiver.path().join("chain"), 206, 207);
            let mut continued = runtime(
                provider(&receiver.path().join("chain")),
                &receiver.path().join("ocomp"),
                bundle.clone(),
            );
            let source = RethFinalizedFrameSource::new(continued.provider.clone());
            let batch = read_bounded_finalized_frames(
                &source,
                continued.scanned_height + 1,
                (207, later[1].block_hash).into(),
            )
            .unwrap()
            .unwrap();
            assert_eq!(batch.frames()[0].identity().number, 206);
            for frame in batch.frames() {
                continued.record_scanned_frame(frame).unwrap();
            }
            assert_eq!(
                continued.flush_closure_checkpoint().unwrap(),
                Some(later[1])
            );
            drop(source);
            drop(continued);
            let second = runtime(
                provider(&receiver.path().join("chain")),
                &receiver.path().join("ocomp"),
                bundle,
            );
            assert_eq!(second.closure_checkpoint.current().unwrap(), later[1]);
            assert_eq!(second.scanned_height, 207);
        }
    }

    #[test]
    fn copied_missing_replay_body_or_receipt_fails_without_advancing_closure() {
        for missing in ["body", "receipt"] {
            let donor = tempfile::tempdir().unwrap();
            let receiver = tempfile::tempdir().unwrap();
            let points = write_frames(&donor.path().join("chain"), 0, 5);
            let mut initial = runtime(
                provider(&donor.path().join("chain")),
                &donor.path().join("ocomp"),
                bundle(),
            );
            catch_up(&mut initial, points[3]);
            drop(initial);
            copy_tree(donor.path(), receiver.path());
            donor.close().unwrap();
            let db = init_db(receiver.path().join("chain/db"), DatabaseArguments::test()).unwrap();
            let tx = db.tx_mut().unwrap();
            if missing == "receipt" {
                tx.delete::<tables::Receipts<OutbeReceipt>>(3, None)
                    .unwrap();
            } else {
                tx.delete::<tables::BlockBodyIndices>(4, None).unwrap();
            }
            tx.commit().unwrap();
            drop(db);
            let restored = runtime(
                provider(&receiver.path().join("chain")),
                &receiver.path().join("ocomp"),
                bundle(),
            );
            let source = RethFinalizedFrameSource::new(restored.provider.clone());
            let error = read_bounded_finalized_frames(&source, 4, (5, points[5].block_hash).into())
                .unwrap_err();
            assert!(
                format!("{error:#}").contains(if missing == "receipt" {
                    "receipt"
                } else {
                    "block"
                }),
                "{error:#}"
            );
            assert_eq!(restored.closure_checkpoint.current().unwrap(), points[3]);
            assert_eq!(restored.scanned_height, 3);
        }
    }
    use outbe_ocomp_protocol::{
        common::BoundedBytes,
        control::{FinalizedJobSpecV1, FinalizedJobSummaryV1},
        hash::hash_framed,
        intent::{
            ActivationPreconditionsV1, ContributorTargetPreconditionV1, DayType,
            FrozenMetadosisValuesV1, JobIntentV1, MetadosisAttemptPreconditionV1,
            MetadosisExpectedStatus, NodTargetPreconditionV1, TributeInputBindingV1,
        },
        registry::HashDomain,
        result::{
            lysis_v1_empty_semantic_event_root, CarryOverCreditActionV1, CarryOverReason,
            CompletionStatus, ConservationTotalsV1, ExactCountsV1, MetadosisCompletionSummaryV1,
            ResultRootsV1,
        },
        state::{OcompFinalizedJobV1, OcompJobRecordV1},
    };
    fn hash(byte: u8) -> B256 {
        B256::repeat_byte(if byte == 0 { 0xff } else { byte })
    }
    fn finalized_job_spec(
        seed: u8,
        cursor: u64,
        chain_id: u64,
        genesis_hash: B256,
    ) -> FinalizedJobSpecV1 {
        let limits = poc_schema_limits();
        let day = 20_260_901_u32;
        let bundle = bundle();
        let bundle = bundle.bundle();
        let protocol_bundle_hash = bundle.protocol_bundle_hash(&limits).unwrap();
        let collection_key = hash(seed.wrapping_add(2));
        let collection_root = hash(seed.wrapping_add(3));
        let nominal = U256::from(1);
        let intent = JobIntentV1 {
            chain_id,
            genesis_hash,
            fork_id: bundle.fork_id,
            wwd: day,
            pending_nonce: 0,
            attempt: 0,
            protocol_bundle_hash,
            ce_sealed_root: hash(seed.wrapping_add(5)),
            sealed_tribute_collection_key: collection_key,
            sealed_tribute_collection_root: collection_root,
            authenticated_day_count: 1,
            authenticated_day_nominal: nominal,
            pre_admission_envelope_hash: hash(seed.wrapping_add(6)),
            source_availability_policy_id: hash(seed.wrapping_add(7)),
            frozen_metadosis_values: FrozenMetadosisValuesV1 {
                day_type: DayType::Green,
                day_limit: nominal,
                previous_vwap: nominal,
                current_vwap: nominal,
                gratis_demand: U256::ZERO,
                day_gratis_limit_minor: U256::ZERO,
                lysis_limit_minor: nominal,
                desis_limit_minor: U256::ZERO,
                auction_entry_prices: Vec::new(),
                request_limit_split_receipt_hash: hash(seed.wrapping_add(8)),
            },
            logical_evaluation_height: cursor,
            logical_evaluation_time: cursor,
            activation_preconditions: ActivationPreconditionsV1 {
                tribute: TributeInputBindingV1 {
                    wwd: day,
                    source_generation: 1,
                    collection_key,
                    sealed_collection_root: collection_root,
                    exact_count: 1,
                    exact_nominal_total: nominal,
                },
                nod: NodTargetPreconditionV1 {
                    wwd: day,
                    target_generation: 1,
                    namespace_root_before: hash(seed.wrapping_add(9)),
                    max_nod_count: 1,
                },
                contributors: ContributorTargetPreconditionV1 {
                    worldwide_day: day,
                    expected_series_version: 1,
                    max_contributor_count: 1,
                    max_eligible_nominal_total: nominal,
                },
                metadosis: MetadosisAttemptPreconditionV1 {
                    wwd: day,
                    pending_nonce: 0,
                    expected_status: MetadosisExpectedStatus::OffchainPending,
                    state_version: 1,
                },
            },
            result_validator_set_epoch: 1,
            result_committee_set_hash: hash(seed.wrapping_add(10)),
            result_ocomp_binding_hash: hash(seed.wrapping_add(11)),
            result_member_count: 4,
            result_quorum_threshold: 3,
            custody_committee_epoch_hash: None,
        };
        let finalized_block_hash = hash(seed.wrapping_add(12));
        let finalized_state_root = hash(seed.wrapping_add(13));
        FinalizedJobSpecV1 {
            summary: FinalizedJobSummaryV1 {
                cursor,
                job_id: intent
                    .job_id(finalized_block_hash, finalized_state_root, &limits)
                    .unwrap(),
                intent_id: intent.intent_id(&limits).unwrap(),
                finalized_block_hash,
                finalized_state_root,
                protocol_bundle_hash,
                open_height: cursor + outbe_ocomp_protocol::state::RESULT_VOTE_MIN_FINALITY_DEPTH,
                deadline_height: cursor
                    + outbe_ocomp_protocol::state::RESULT_VOTE_MIN_FINALITY_DEPTH
                    + 1_800,
            },
            canonical_job_intent: BoundedBytes(intent.encode_canonical(&limits).unwrap()),
        }
    }

    fn refresh_arithmetic(result: &mut LysisResultV1) {
        result.arithmetic_commitment = hash_framed(
            HashDomain::LysisArithmetic,
            &result
                .arithmetic_summary()
                .encode_canonical(&poc_schema_limits())
                .unwrap(),
        )
        .unwrap();
        result.encode_canonical(&poc_schema_limits()).unwrap();
    }

    // Compact native-result fixture, bound to the actual canonical JobIntent
    // and B-derived JobId. This proves stored evidence, not worker execution.
    fn result_for(job: &OcompJobRecordV1) -> LysisResultV1 {
        let intent = &job.intent;
        let frozen = &intent.frozen_metadosis_values;
        let unused = frozen.lysis_limit_minor;
        let conservation = ConservationTotalsV1 {
            tribute_nominal_total: intent.authenticated_day_nominal,
            eligible_nominal_total: U256::ZERO,
            day_limit: frozen.day_limit,
            gratis_demand: frozen.gratis_demand,
            day_gratis_limit_minor: frozen.day_gratis_limit_minor,
            lysis_limit_minor: frozen.lysis_limit_minor,
            desis_limit_minor: frozen.desis_limit_minor,
            lysis_allocation_minor: U256::ZERO,
            unused_lysis_limit_minor: unused,
            carry_over_credit: unused,
            nod_cost_total: U256::ZERO,
        };
        let mut result = LysisResultV1 {
            protocol_bundle_hash: intent.protocol_bundle_hash,
            job_id: job.finalized.as_ref().unwrap().job_id,
            attempt: intent.attempt,
            input_manifest_hash: B256::repeat_byte(0x35),
            plan_hash: B256::repeat_byte(0x36),
            unit_artifact_root: B256::repeat_byte(0x37),
            fidelity_fraction_root: B256::repeat_byte(0x38),
            gratis_prefix_root: B256::repeat_byte(0x39),
            result_chunk_count: 1,
            result_chunk_list_root: B256::repeat_byte(0x3a),
            carry_over_credit: CarryOverCreditActionV1 {
                source_wwd: intent.wwd,
                reason: CarryOverReason::UnusedLysis,
                amount: unused,
            },
            metadosis_completion_summary: MetadosisCompletionSummaryV1 {
                wwd: intent.wwd,
                pending_nonce: intent.pending_nonce,
                day_type: frozen.day_type,
                tribute_nominal_total: intent.authenticated_day_nominal,
                day_limit: frozen.day_limit,
                gratis_demand: frozen.gratis_demand,
                day_gratis_limit_minor: frozen.day_gratis_limit_minor,
                lysis_limit_minor: frozen.lysis_limit_minor,
                desis_limit_minor: frozen.desis_limit_minor,
                lysis_allocation_minor: U256::ZERO,
                unused_lysis_limit_minor: unused,
                carry_over_credit: unused,
                status: CompletionStatus::Completed,
                logical_evaluation_height: intent.logical_evaluation_height,
                logical_evaluation_time: intent.logical_evaluation_time,
            },
            tribute_count: intent.authenticated_day_count,
            tribute_nominal_total: intent.authenticated_day_nominal,
            unused_lysis_limit_minor: unused,
            roots: ResultRootsV1 {
                nod_root: B256::repeat_byte(0x31),
                bucket_root: B256::repeat_byte(0x32),
                contributor_root: B256::repeat_byte(0x33),
                output_manifest_root: B256::repeat_byte(0x34),
            },
            counts: ExactCountsV1 {
                tribute_count: intent.authenticated_day_count,
                nod_count: intent.authenticated_day_count,
                bucket_count: 0,
                contributor_count: 0,
                semantic_event_count: 0,
            },
            conservation,
            arithmetic_commitment: B256::ZERO,
            event_summary_hash: lysis_v1_empty_semantic_event_root().unwrap(),
        };
        refresh_arithmetic(&mut result);
        result.validate_finalized_intent(intent).unwrap();
        result
    }
    const WORKER_CHILD_ROOT: &str = "OUTBE_COPIED_OCOMP_WORKER_ROOT";
    const WORKER_CHILD_SUPERVISOR: &str = "OUTBE_COPIED_OCOMP_SUPERVISOR";
    const WORKER_CHILD_METRICS: &str = "OUTBE_COPIED_OCOMP_METRICS";

    #[test]
    fn copied_worker_child() {
        let Some(root) = std::env::var_os(WORKER_CHILD_ROOT) else {
            return;
        };
        let root = std::path::PathBuf::from(root);
        let bundle = bundle();
        outbe_ocomp::worker::run_worker(outbe_ocomp::worker::WorkerConfig {
            identity: EndpointIdentity {
                chain_id: chain().chain().id(),
                genesis_hash: chain().genesis_hash(),
                boot_nonce: B256::repeat_byte(0x81),
                protocol_bundle_hash: bundle.hash(),
            },
            supervisor_address: std::env::var(WORKER_CHILD_SUPERVISOR)
                .unwrap()
                .parse()
                .unwrap(),
            observability_address: std::env::var(WORKER_CHILD_METRICS)
                .unwrap()
                .parse()
                .unwrap(),
            cas_root: root.join("cas-v1"),
            cas_limits: outbe_ocomp::cas::CasLimits {
                max_object_bytes: 1_048_576,
                max_total_bytes: u64::MAX,
            },
            inbox_root: root.join("worker-inbox-v1"),
            inbox_limits: outbe_ocomp::inbox::WorkerInboxLimits {
                max_artifact_bytes: 1_048_576,
                max_total_bytes: 67_108_864,
            },
            protocol_bundle: bundle,
        })
        .unwrap();
    }

    struct ChildWorker(std::process::Child);
    impl Drop for ChildWorker {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn available_endpoint(pair: bool) -> std::net::SocketAddr {
        loop {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            if !pair {
                return address;
            }
            if let Some(port) = address.port().checked_add(1) {
                if std::net::TcpListener::bind((address.ip(), port)).is_ok() {
                    return address;
                }
            }
        }
    }

    fn worker_started_count(
        client: &reqwest::blocking::Client,
        address: std::net::SocketAddr,
    ) -> u64 {
        let text = client
            .get(format!("http://{address}/metrics"))
            .send()
            .unwrap()
            .error_for_status()
            .unwrap()
            .text()
            .unwrap();
        text.lines()
            .find_map(|line| {
                line.strip_prefix("outbe_ocomp_worker_units_started_total ")
                    .map(|value| value.parse().unwrap())
            })
            .unwrap_or(0)
    }

    #[test]
    fn copied_saved_result_is_restored_before_compute_with_a_connected_real_worker() {
        use outbe_node::ocomp::local_result::LocalLysisResultStore;
        use outbe_ocomp::embedded::EmbeddedJobEventV1;
        for closed_height in [3, 5] {
            let donor = tempfile::tempdir().unwrap();
            let receiver = tempfile::tempdir().unwrap();
            let points = write_frames(&donor.path().join("chain"), 0, 5);
            let bundle = bundle();
            let spec = finalized_job_spec(0x31, 2, chain().chain().id(), chain().genesis_hash());
            let intent =
                JobIntentV1::decode_canonical(&spec.canonical_job_intent.0, &poc_schema_limits())
                    .unwrap();
            let job = OcompJobRecordV1 {
                intent,
                intent_height: spec.summary.cursor,
                status: OcompJobStatus::VotingOpen,
                finalized: Some(OcompFinalizedJobV1 {
                    job_id: spec.summary.job_id,
                    finalized_request_block_hash: spec.summary.finalized_block_hash,
                    finalized_request_state_root: spec.summary.finalized_state_root,
                    finality_recorded_height: spec.summary.cursor,
                    open_height: spec.summary.open_height,
                    deadline_height: spec.summary.deadline_height,
                    quorum: None,
                }),
                terminal: None,
            };
            job.validate_semantics(&poc_schema_limits()).unwrap();
            let result = result_for(&job);
            let digest = result.result_digest(&poc_schema_limits()).unwrap();
            let local_path = donor.path().join("ocomp/node-v1/local-results");
            fs::create_dir_all(local_path.parent().unwrap()).unwrap();
            let store = LocalLysisResultStore::open(&local_path, poc_schema_limits()).unwrap();
            store
                .commit(
                    spec.summary.job_id,
                    &result.encode_canonical(&poc_schema_limits()).unwrap(),
                )
                .unwrap();
            drop(store);
            let mut initial = runtime(
                provider(&donor.path().join("chain")),
                &donor.path().join("ocomp"),
                bundle.clone(),
            );
            catch_up(&mut initial, points[closed_height]);
            drop(initial);
            copy_tree(donor.path(), receiver.path());
            donor.close().unwrap();
            let public = receiver.path().join("ocomp");
            let endpoint = available_endpoint(true);
            let metrics = available_endpoint(false);
            let mut restored = runtime_with_endpoint(
                provider(&receiver.path().join("chain")),
                &public,
                bundle.clone(),
                endpoint,
            );
            let mut child = ChildWorker(
                std::process::Command::new(std::env::current_exe().unwrap())
                    .arg("copied_worker_child")
                    .arg("--test-threads=1")
                    .env(WORKER_CHILD_ROOT, &public)
                    .env(WORKER_CHILD_SUPERVISOR, endpoint.to_string())
                    .env(WORKER_CHILD_METRICS, metrics.to_string())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .unwrap(),
            );
            let client = reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(2))
                .build()
                .unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            loop {
                assert!(
                    child.0.try_wait().unwrap().is_none(),
                    "worker subprocess exited before observation"
                );
                if let Ok(response) = client.get(format!("http://{metrics}/status")).send() {
                    if let Ok(status) =
                        response.json::<outbe_ocomp::worker_observability::WorkerStatusV1>()
                    {
                        if status.phase == outbe_ocomp::worker_observability::WorkerPhaseV1::Idle {
                            break;
                        }
                    }
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "worker did not register"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
            let before = worker_started_count(&client, metrics);
            assert_eq!(before, 0);
            let generation = restored
                .state
                .observe_job(spec.summary.job_id, spec.summary.deadline_height)
                .unwrap();
            restored.jobs.insert(
                spec.summary.job_id,
                RuntimeJobV1 {
                    record: DiscoveryRecord {
                        generation: 1,
                        cursor: spec.summary.cursor,
                        spec: spec.clone(),
                    },
                    generation,
                    cancelled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    compute_started: false,
                    vote_eligibility: LocalVoteEligibilityV1::Pending,
                    vote_started: false,
                    canonical_result: Some(result.clone()),
                },
            );
            // Supply caller-provided canonical completion to the existing restore-before-compute
            // boundary. This unit does not authenticate a quorum receipt or execute Lysis.
            restored
                .state
                .reduce(
                    spec.summary.job_id,
                    EmbeddedJobEventV1::CanonicalCompleted {
                        result_digest: digest,
                    },
                )
                .unwrap();
            restored.restore_local_result(spec.summary.job_id).unwrap();
            restored
                .ensure_compute_started(spec.summary.job_id)
                .unwrap();
            assert_eq!(
                restored.state.state(spec.summary.job_id),
                Some(EmbeddedJobStateV1::Verified)
            );
            assert_eq!(
                restored
                    .domain
                    .verify_exact_canonical_result(spec.summary.job_id, &result)
                    .unwrap()
                    .result_digest,
                digest
            );
            assert_eq!(worker_started_count(&client, metrics), before);
            assert!(restored.compute_rx.try_recv().is_err());
            assert!(child.0.try_wait().unwrap().is_none());
            drop(child);
            drop(restored);
            let second = runtime(provider(&receiver.path().join("chain")), &public, bundle);
            assert_eq!(
                second
                    .domain
                    .verify_exact_canonical_result(spec.summary.job_id, &result)
                    .unwrap()
                    .result_digest,
                digest
            );
        }
    }
    #[test]
    fn copied_prepared_spool_retirement_waits_for_c_then_stays_retired_after_k() {
        let donor = tempfile::tempdir().unwrap();
        let receiver = tempfile::tempdir().unwrap();
        let points = write_frames(&donor.path().join("chain"), 0, 5);
        let bundle = bundle();
        let spec = finalized_job_spec(0x21, 2, chain().chain().id(), chain().genesis_hash());
        let mut initial = runtime(
            provider(&donor.path().join("chain")),
            &donor.path().join("ocomp"),
            bundle.clone(),
        );
        catch_up(&mut initial, points[3]);
        let spool = initial.discovery_spools.get(&bundle.hash()).unwrap();
        let (offer, _) = spool.put_offer(1, &spec).unwrap();
        spool.prepare_retirement(&offer, 5).unwrap();
        assert_eq!(
            spool
                .complete_retirements_through(3)
                .unwrap()
                .waiting_for_checkpoint,
            1
        );
        drop(initial);
        copy_tree(donor.path(), receiver.path());
        donor.close().unwrap();
        let mut restored = runtime(
            provider(&receiver.path().join("chain")),
            &receiver.path().join("ocomp"),
            bundle.clone(),
        );
        assert_eq!(
            restored
                .discovery_spools
                .get(&bundle.hash())
                .unwrap()
                .complete_retirements_through(3)
                .unwrap()
                .waiting_for_checkpoint,
            1
        );
        catch_up(&mut restored, points[5]);
        let spool = restored.discovery_spools.get(&bundle.hash()).unwrap();
        assert_eq!(
            spool.complete_retirements_through(5).unwrap().completed,
            0,
            "ordinary flush already completed retirement"
        );
        assert!(spool.pending(&offer.observation_id).unwrap().is_none());
        drop(restored);
        let later = write_frames(&receiver.path().join("chain"), 6, 7);
        let mut restarted = runtime(
            provider(&receiver.path().join("chain")),
            &receiver.path().join("ocomp"),
            bundle.clone(),
        );
        catch_up(&mut restarted, later[1]);
        assert!(restarted
            .discovery_spools
            .get(&bundle.hash())
            .unwrap()
            .pending(&offer.observation_id)
            .unwrap()
            .is_none());
        assert_eq!(restarted.closure_checkpoint.current().unwrap(), later[1]);
    }

    #[test]
    fn copied_native_fatal_evidence_remains_authoritative_on_reopen() {
        let donor = tempfile::tempdir().unwrap();
        let receiver = tempfile::tempdir().unwrap();
        write_frames(&donor.path().join("chain"), 0, 1);
        let initial = runtime(
            provider(&donor.path().join("chain")),
            &donor.path().join("ocomp"),
            bundle(),
        );
        let evidence = initial.domain.fatal_evidence_root().to_path_buf();
        persist_generic_fatal_evidence(&evidence, B256::repeat_byte(0x41), "copied fatal evidence")
            .unwrap();
        let relative = evidence.strip_prefix(donor.path()).unwrap().to_path_buf();
        drop(initial);
        copy_tree(donor.path(), receiver.path());
        donor.close().unwrap();
        let copied = receiver.path().join(relative);
        assert!(load_persisted_fatal_evidence(&copied)
            .unwrap()
            .unwrap()
            .contains("copied fatal evidence"));
        // This is the same startup reader; the component fixture does not run the ExEx fatal wait loop.
        persist_generic_fatal_evidence(&copied, B256::repeat_byte(0x42), "replacement").unwrap();
        assert!(load_persisted_fatal_evidence(&copied)
            .unwrap()
            .unwrap()
            .contains("copied fatal evidence"));
    }

    mod copied_retention {
        use super::*;
        use alloy_primitives::{Address, Log};
        use alloy_sol_types::SolEvent as _;
        use outbe_compressed_entities::WwdEntityId;
        use outbe_consensus::finalization::parent_cert_store::FinalizedParentCertStore;
        use outbe_metadosis::{precompile::IMetadosis, proof_layout::OCOMP_JOB_RECORDS_BASE_SLOT};
        use outbe_node::{
            finalized_frame::FinalizedFrame,
            ocomp::retention::{
                inspect_retention_journal, observe_finalized_request, read_ocomp_job_record_at,
                OcompRetentionCoordinator, PinRecordV1, PinStateV1, RethFinalizedInputProofSource,
            },
        };
        use outbe_ocomp_protocol::{
            generated_shape::OCOMP_POC_CANDIDATE_LIMITS_V1,
            intent::intent_storage_key,
            state::{LysisTerminalV1, OcompFinalizedJobV1, OcompJobRecordV1, OcompTerminalOutcome},
        };
        use outbe_offchain_storage::{
            AtomicWriteBatch, RocksDbStorage, StorageError, StorageWriter, StorageWriterHandle,
        };
        use outbe_primitives::{
            addresses::METADOSIS_ADDRESS,
            storage::{
                hashmap::HashMapStorageProvider,
                types::{StorageBytes, StorageKey},
                StorageHandle,
            },
            time::WorldwideDay,
        };
        use outbe_tribute::{
            RetainedTributePin, RetainedTributeReader, RetainedTributeWriter, TributeData,
            TributeRepositoryWriter,
        };
        use reth_primitives_traits::StorageEntry;
        use std::sync::atomic::{AtomicBool, Ordering};

        const H: u64 = 170;

        fn journal(root: &Path) -> std::path::PathBuf {
            root.join("ocomp_retention")
        }

        fn body_store(root: &Path) -> Arc<RocksDbStorage> {
            Arc::new(RocksDbStorage::open(root.join("retained-projection")).unwrap())
        }

        // The only fault seam forwards the real RocksDB atomic delete, then reports
        // an unavailable result once, simulating an ambiguous committed write.
        // The coordinator has already durably published GcPending at this point.
        struct FailAfterCommittedDelete {
            storage: Arc<RocksDbStorage>,
            armed: AtomicBool,
        }

        impl StorageWriter for FailAfterCommittedDelete {
            fn apply_atomic(&self, batch: &AtomicWriteBatch) -> Result<(), StorageError> {
                self.storage.apply_atomic(batch)?;
                if self.armed.swap(false, Ordering::SeqCst) {
                    return Err(StorageError::Unavailable {
                        source: Box::new(std::io::Error::other("injected after committed GC page")),
                    });
                }
                Ok(())
            }
        }

        fn owner(
            root: &Path,
            storage: Arc<RocksDbStorage>,
            writer: StorageWriterHandle,
        ) -> OcompRetentionCoordinator {
            // Construct the concrete public factory here: the helper's opaque return
            // type does not expose HeaderProvider<Header = OutbeHeader> to this caller.
            let factory = ProviderFactoryBuilder::<outbe_node::OutbeNode>::default()
                .open_read_only(
                    chain(),
                    ReadOnlyConfig::from_datadir(root).no_watch(),
                    reth_ethereum::tasks::Runtime::test(),
                )
                .unwrap();
            assert!(
                !reth_provider::StorageSettingsCache::cached_storage_settings(&factory).is_v2()
            );
            let source = RethFinalizedInputProofSource::new(
                reth_provider::providers::BlockchainProvider::new(factory).unwrap(),
                FinalizedParentCertStore::new(),
            );
            OcompRetentionCoordinator::open_with_retained_tributes(
                journal(root),
                Arc::new(source),
                Arc::new(RetainedTributeWriter::new(storage, writer)),
            )
        }

        fn frame(root: &Path, height: u64) -> FinalizedFrame {
            let provider = provider(root);
            let hash = provider.block_hash(height).unwrap().unwrap();
            let source = RethFinalizedFrameSource::new(provider);
            let batch = read_bounded_finalized_frames(&source, height, (height, hash).into())
                .unwrap()
                .unwrap();
            assert_eq!(batch.frames().len(), 1);
            batch.frames()[0].clone()
        }

        fn current_record(
            root: &Path,
            expected: &OcompJobRecordV1,
            height: u64,
        ) -> OcompJobRecordV1 {
            let provider = provider(root);
            let hash = provider.block_hash(height).unwrap().unwrap();
            let actual = read_ocomp_job_record_at(
                &provider,
                hash,
                expected.intent.intent_id(&poc_schema_limits()).unwrap(),
                &poc_schema_limits(),
            )
            .unwrap();
            assert_eq!(&actual, expected);
            actual
        }

        // Typed owner encodes the records; only these test-native words are seeded.
        // They are not EVM-executed and this fixture does not authenticate state roots
        // or parent certificates. All subsequent candidate and terminal reads use
        // the production RethFinalizedInputProofSource and actual native provider.
        fn write_records(root: &Path, records: &[OcompJobRecordV1]) {
            let mut words = HashMapStorageProvider::new_with_chain_identity(
                chain().chain().id(),
                chain().genesis_hash(),
            );
            StorageHandle::enter(&mut words, |storage| {
                for record in records {
                    let limits = poc_schema_limits();
                    record.validate_semantics(&limits).unwrap();
                    let key =
                        intent_storage_key(record.intent.intent_id(&limits).unwrap()).unwrap();
                    StorageBytes::new(
                        key.mapping_slot(U256::from(OCOMP_JOB_RECORDS_BASE_SLOT)),
                        METADOSIS_ADDRESS,
                        storage.clone(),
                    )
                    .write(&record.encode_canonical(&limits).unwrap())
                    .unwrap();
                }
            });
            let db = init_db(root.join("db"), DatabaseArguments::test()).unwrap();
            let tx = db.tx_mut().unwrap();
            tx.delete::<tables::PlainStorageState>(METADOSIS_ADDRESS, None)
                .unwrap();
            tx.put::<tables::PlainAccountState>(METADOSIS_ADDRESS, Default::default())
                .unwrap();
            for ((address, slot), value) in words.storage {
                if !value.is_zero() {
                    tx.put::<tables::PlainStorageState>(
                        address,
                        StorageEntry {
                            key: B256::from(slot.to_be_bytes::<32>()),
                            value,
                        },
                    )
                    .unwrap();
                }
            }
            tx.commit().unwrap();
        }

        fn intent(count: usize) -> JobIntentV1 {
            let spec = finalized_job_spec(0x41, 1, chain().chain().id(), chain().genesis_hash());
            let mut intent =
                JobIntentV1::decode_canonical(&spec.canonical_job_intent.0, &poc_schema_limits())
                    .unwrap();
            let nominal = U256::from(count);
            intent.authenticated_day_count = u32::try_from(count).unwrap();
            intent.authenticated_day_nominal = nominal;
            intent.activation_preconditions.tribute.exact_count = u32::try_from(count).unwrap();
            intent.activation_preconditions.tribute.exact_nominal_total = nominal;
            intent.activation_preconditions.nod.max_nod_count = u32::try_from(count).unwrap();
            intent
                .activation_preconditions
                .contributors
                .max_contributor_count = u32::try_from(count).unwrap();
            intent
                .activation_preconditions
                .contributors
                .max_eligible_nominal_total = nominal;
            intent.frozen_metadosis_values.day_limit = nominal;
            intent.frozen_metadosis_values.lysis_limit_minor = nominal;
            intent.validate_semantics().unwrap();
            intent
        }

        // Amend only the just-created tip before any successor/C exists, preserving
        // the exact receipt commitment. This is fixture construction, not recovery.
        fn request_tip(root: &Path, height: u64, intent: &JobIntentV1) -> ProjectionCheckpoint {
            let event = IMetadosis::OffchainJobRequested {
                intentId: intent.intent_id(&poc_schema_limits()).unwrap(),
                wwd: intent.wwd,
                pendingNonce: intent.pending_nonce,
                attempt: intent.attempt,
                activationPreconditionsHash: intent
                    .activation_preconditions
                    .activation_preconditions_hash(&poc_schema_limits())
                    .unwrap(),
            };
            let receipt = OutbeReceipt {
                success: true,
                cumulative_gas_used: 21_000,
                logs: vec![Log {
                    address: METADOSIS_ADDRESS,
                    data: event.encode_log_data(),
                }],
                ..Default::default()
            };
            let db = init_db(root.join("db"), DatabaseArguments::test()).unwrap();
            let tx = db.tx_mut().unwrap();
            assert_eq!(
                tx.get::<tables::ChainState>(tables::ChainStateKey::LastFinalizedBlock)
                    .unwrap(),
                Some(height)
            );
            let old = tx.get::<tables::CanonicalHeaders>(height).unwrap().unwrap();
            let mut header = tx
                .get::<tables::Headers<OutbeHeader>>(height)
                .unwrap()
                .unwrap();
            header.inner.receipts_root = alloy_consensus::proofs::calculate_receipt_root(&[
                alloy_consensus::TxReceipt::with_bloom_ref(&receipt),
            ]);
            let hash = header.hash_slow();
            tx.delete::<tables::HeaderNumbers>(old, None).unwrap();
            tx.put::<tables::HeaderNumbers>(hash, height).unwrap();
            tx.put::<tables::CanonicalHeaders>(height, hash).unwrap();
            // Pinned Reth reads headers from static files, not the MDBX mirror.
            // Replace only this unobserved donor tip during fixture construction.
            let files = StaticFileProviderBuilder::read_write(root.join("static_files"))
                .with_blocks_per_file(1_000)
                .build::<OutbePrimitives>()
                .unwrap();
            {
                let mut headers = files
                    .get_writer(height, StaticFileSegment::Headers)
                    .unwrap();
                headers.prune_headers(1).unwrap();
            }
            files.commit().unwrap();
            {
                let mut headers = files
                    .get_writer(height, StaticFileSegment::Headers)
                    .unwrap();
                headers.append_header(&header, &hash).unwrap();
            }
            files.commit().unwrap();
            drop(files);
            tx.put::<tables::Headers<OutbeHeader>>(height, header)
                .unwrap();
            tx.put::<tables::Receipts<OutbeReceipt>>(height - 1, receipt)
                .unwrap();
            tx.commit().unwrap();
            ProjectionCheckpoint {
                block_number: height,
                block_hash: hash,
            }
        }

        fn register(
            root: &Path,
            records: &mut Vec<OcompJobRecordV1>,
            intent: JobIntentV1,
            height: u64,
        ) {
            if height == 1 {
                write_frames(root, 0, 1);
            } else {
                write_frames(root, height, height);
            }
            let point = request_tip(root, height, &intent);
            records.push(OcompJobRecordV1 {
                intent,
                intent_height: height,
                status: OcompJobStatus::AwaitingFinality,
                finalized: None,
                terminal: None,
            });
            write_records(root, records);
            let request = frame(root, height);
            let observation = observe_finalized_request(&request)
                .unwrap()
                .expect("native receipt request");
            {
                let storage = body_store(root);
                let coordinator = owner(root, storage.clone(), storage);
                coordinator
                    .reconcile_finalized_frame(&request, Some(observation))
                    .unwrap();
            }
            let record = records.last_mut().unwrap();
            let job_id = record
                .intent
                .job_id(point.block_hash, request.state_root(), &poc_schema_limits())
                .unwrap();
            record.status = OcompJobStatus::VotingOpen;
            record.finalized = Some(OcompFinalizedJobV1 {
                job_id,
                finalized_request_block_hash: point.block_hash,
                finalized_request_state_root: request.state_root(),
                finality_recorded_height: height + 1,
                open_height: height + 5,
                deadline_height: 20,
                quorum: None,
            });
            write_records(root, records);
            let record = current_record(root, records.last().unwrap(), height);
            let storage = body_store(root);
            let coordinator = owner(root, storage.clone(), storage);
            coordinator
                .bind_canonical_finalized_job(point.block_hash, &record)
                .unwrap();
        }

        fn expire(record: &mut OcompJobRecordV1) {
            record.status = OcompJobStatus::Expired;
            record.terminal = Some(LysisTerminalV1 {
                outcome: OcompTerminalOutcome::Expired,
                terminal_height: 20,
                terminal_time: 20,
                completed_binding: None,
            });
        }

        fn reconcile_tip(root: &Path, height: u64) {
            let finalized = frame(root, height);
            let storage = body_store(root);
            let coordinator = owner(root, storage.clone(), storage);
            coordinator
                .reconcile_finalized_frame(&finalized, None)
                .unwrap();
        }

        fn pin(record: &OcompJobRecordV1) -> RetainedTributePin {
            RetainedTributePin {
                input_lease_id: record.intent.input_lease_id().unwrap(),
                worldwide_day: WorldwideDay::new(record.intent.wwd),
            }
        }

        fn retain(root: &Path, record: &OcompJobRecordV1, count: usize) {
            let storage = body_store(root);
            let repository = TributeRepositoryWriter::new(storage.clone(), storage.clone());
            let retained = RetainedTributeReader::new(storage.clone());
            for ordinal in 0..count {
                let mut digest = [0u8; 32];
                digest[..8].copy_from_slice(&(ordinal as u64).to_be_bytes());
                let tribute_id =
                    WwdEntityId::from_day_and_digest(pin(record).worldwide_day, digest);
                repository
                    .put(&TributeData {
                        tribute_id,
                        owner: Address::repeat_byte(0x52),
                        worldwide_day: pin(record).worldwide_day,
                        issuance_amount_minor: U256::ONE,
                        issuance_currency: 840,
                        nominal_amount_minor: U256::ONE,
                        reference_currency: 978,
                        tribute_price_minor: U256::ONE,
                        exclude_from_intex_issuance: false,
                    })
                    .unwrap();
                storage
                    .apply_atomic(
                        &retained
                            .plan_retain_current(pin(record), tribute_id)
                            .unwrap(),
                    )
                    .unwrap();
                repository.delete(tribute_id).unwrap();
            }
        }

        fn remaining(root: &Path, record: &OcompJobRecordV1) -> usize {
            RetainedTributeReader::new(body_store(root))
                .list_by_day(pin(record), None, 1_024)
                .unwrap()
                .records
                .len()
        }

        fn durable_record(root: &Path, record: &OcompJobRecordV1) -> PinRecordV1 {
            let key = record
                .finalized
                .as_ref()
                .unwrap()
                .finalized_request_block_hash;
            inspect_retention_journal(journal(root))
                .unwrap()
                .records
                .into_iter()
                .find_map(|(candidate, record)| (candidate == key).then_some(record))
                .unwrap()
        }

        fn close_through(root: &Path, height: u64) -> ProjectionCheckpoint {
            let provider = provider(root);
            let point = ProjectionCheckpoint {
                block_number: height,
                block_hash: provider.block_hash(height).unwrap().unwrap(),
            };
            let mut runtime = runtime(provider, root, bundle());
            let old = runtime.closure_checkpoint.current().unwrap();
            let visited = catch_up(&mut runtime, point);
            assert_eq!(visited, (old.block_number + 1..=height).collect::<Vec<_>>());
            assert_eq!(runtime.closure_checkpoint.current().unwrap(), point);
            point
        }

        fn assert_c(root: &Path, expected: ProjectionCheckpoint) {
            let runtime = runtime(provider(root), root, bundle());
            assert_eq!(runtime.closure_checkpoint.current().unwrap(), expected);
        }

        #[test]
        fn copied_rocksdb_partial_and_empty_gc_pending_resume_without_reconstructing_bodies() {
            for empty_after_committed_write in [false, true] {
                let donor = tempfile::tempdir().unwrap();
                let receiver = tempfile::tempdir().unwrap();
                let page = OCOMP_POC_CANDIDATE_LIMITS_V1.max_tributes_per_work_shard as usize;
                let count = if empty_after_committed_write {
                    1
                } else {
                    page + 1
                };
                let mut records = Vec::new();
                register(donor.path(), &mut records, intent(count), 1);
                retain(donor.path(), &records[0], count);
                assert_eq!(remaining(donor.path(), &records[0]), count);
                write_frames(donor.path(), 2, 100);
                expire(&mut records[0]);
                write_records(donor.path(), &records);
                reconcile_tip(donor.path(), 100);
                close_through(donor.path(), 100);
                // Actual terminal observation at100 makes release due at164.
                assert!(matches!(
                    durable_record(donor.path(), &records[0]).state,
                    PinStateV1::Terminal {
                        terminal_height: 100,
                        release_height: 164,
                        ..
                    }
                ));
                write_frames(donor.path(), 101, H);
                let closed = close_through(donor.path(), H);
                {
                    let storage = body_store(donor.path());
                    let writer: StorageWriterHandle = if empty_after_committed_write {
                        Arc::new(FailAfterCommittedDelete {
                            storage: storage.clone(),
                            armed: AtomicBool::new(true),
                        })
                    } else {
                        storage.clone()
                    };
                    let coordinator = owner(donor.path(), storage, writer);
                    let result = coordinator.release_due(closed.block_number);
                    if empty_after_committed_write {
                        assert!(result.is_err());
                    } else {
                        assert!(result.unwrap().is_none());
                    }
                }
                assert!(matches!(
                    durable_record(donor.path(), &records[0]).state,
                    PinStateV1::GcPending { .. }
                ));
                let expected_remaining = usize::from(!empty_after_committed_write);
                assert_eq!(remaining(donor.path(), &records[0]), expected_remaining);
                copy_tree(donor.path(), receiver.path());
                donor.close().unwrap();
                assert!(receiver
                    .path()
                    .join("retained-projection/CURRENT")
                    .is_file());
                assert_c(receiver.path(), closed);
                assert_eq!(remaining(receiver.path(), &records[0]), expected_remaining);
                assert!(matches!(
                    durable_record(receiver.path(), &records[0]).state,
                    PinStateV1::GcPending { .. }
                ));
                {
                    let storage = body_store(receiver.path());
                    let coordinator = owner(receiver.path(), storage.clone(), storage);
                    assert!(coordinator
                        .release_due(closed.block_number)
                        .unwrap()
                        .is_some());
                }
                assert_eq!(remaining(receiver.path(), &records[0]), 0);
                assert!(matches!(
                    durable_record(receiver.path(), &records[0]).state,
                    PinStateV1::Released { .. }
                ));
                write_frames(receiver.path(), H + 1, H + 2);
                let k = close_through(receiver.path(), H + 2);
                assert_c(receiver.path(), k);
                {
                    let storage = body_store(receiver.path());
                    let coordinator = owner(receiver.path(), storage.clone(), storage);
                    assert!(coordinator.release_due(k.block_number).unwrap().is_none());
                }
                assert_eq!(remaining(receiver.path(), &records[0]), 0);
                assert!(matches!(
                    durable_record(receiver.path(), &records[0]).state,
                    PinStateV1::Released { .. }
                ));
            }
        }

        #[test]
        fn copied_rocksdb_shared_live_lease_survives_first_release_and_collects_after_last_job_at_k(
        ) {
            exercise_shared_lease_copy(false);
        }

        #[test]
        fn copied_pruned_released_pin_stays_absent_while_shared_lease_remains_live() {
            exercise_shared_lease_copy(true);
        }

        // Construct an already-compacted native image, not a pressure-compaction
        // execution. Keep every surviving record byte and the latest generation.
        fn remove_old_released_frame(root: &Path, key: B256) {
            let mut expected = inspect_retention_journal(journal(root)).unwrap();
            assert_ne!(expected.last_updated, key);
            let removed = expected
                .records
                .iter()
                .find(|(id, _)| *id == key)
                .unwrap()
                .1;
            assert!(matches!(removed.state, PinStateV1::Released { .. }));
            expected.records.retain(|(id, _)| *id != key);
            assert!(!expected.records.is_empty());
            let path = journal(root).join("pin.v1");
            let bytes = fs::read(&path).unwrap();
            assert_eq!(&bytes[..8], b"OUTBPIN1");
            assert_eq!(u16::from_be_bytes(bytes[8..10].try_into().unwrap()), 6);
            let count = u16::from_be_bytes(bytes[50..52].try_into().unwrap());
            let mut output = bytes[..50].to_vec();
            output.extend_from_slice(&(count - 1).to_be_bytes());
            let mut offset = 52;
            let mut removed_count = 0;
            for _ in 0..count {
                let id = B256::from_slice(&bytes[offset..offset + 32]);
                let len = u16::from_be_bytes(bytes[offset + 32..offset + 34].try_into().unwrap())
                    as usize;
                let end = offset + 34 + len;
                if id == key {
                    removed_count += 1;
                } else {
                    output.extend_from_slice(&bytes[offset..end]);
                }
                offset = end;
            }
            assert_eq!(removed_count, 1);
            assert_eq!(offset, bytes.len() - 32);
            let checksum = alloy_primitives::keccak256(&output);
            output.extend_from_slice(checksum.as_slice());
            fs::write(path, output).unwrap();
            assert_eq!(inspect_retention_journal(journal(root)).unwrap(), expected);
        }

        fn exercise_shared_lease_copy(pruned: bool) {
            let donor = tempfile::tempdir().unwrap();
            let receiver = tempfile::tempdir().unwrap();
            let first = intent(1);
            let mut second = first.clone();
            // Different output precondition, same authenticated source opening.
            // PoC attempt/pending_nonce remain their required zero values.
            second.activation_preconditions.nod.namespace_root_before = B256::repeat_byte(0x91);
            assert_eq!(
                first.input_lease_id().unwrap(),
                second.input_lease_id().unwrap()
            );
            assert_ne!(
                first.intent_id(&poc_schema_limits()).unwrap(),
                second.intent_id(&poc_schema_limits()).unwrap()
            );
            let mut records = Vec::new();
            register(donor.path(), &mut records, first, 1);
            register(donor.path(), &mut records, second, 2);
            retain(donor.path(), &records[0], 1);
            write_frames(donor.path(), 3, 100);
            expire(&mut records[0]);
            write_records(donor.path(), &records);
            reconcile_tip(donor.path(), 100);
            close_through(donor.path(), 100);
            write_frames(donor.path(), 101, H);
            let closed = close_through(donor.path(), H);
            {
                let storage = body_store(donor.path());
                let coordinator = owner(donor.path(), storage.clone(), storage);
                assert!(coordinator
                    .release_due(closed.block_number)
                    .unwrap()
                    .is_some());
            }
            assert!(matches!(
                durable_record(donor.path(), &records[0]).state,
                PinStateV1::Released { .. }
            ));
            assert!(matches!(
                durable_record(donor.path(), &records[1]).state,
                PinStateV1::Finalized { .. }
            ));
            assert_eq!(remaining(donor.path(), &records[1]), 1);
            let retired_key = records[0]
                .finalized
                .as_ref()
                .unwrap()
                .finalized_request_block_hash;
            let closed = if pruned {
                // A later ordinary transition preserves a live lease and makes
                // the old Released entry eligible for an absent-record fixture.
                write_frames(donor.path(), H + 1, 180);
                expire(&mut records[1]);
                write_records(donor.path(), &records);
                reconcile_tip(donor.path(), 180);
                let point = close_through(donor.path(), 180);
                remove_old_released_frame(donor.path(), retired_key);
                point
            } else {
                closed
            };
            copy_tree(donor.path(), receiver.path());
            donor.close().unwrap();
            assert_c(receiver.path(), closed);
            assert_eq!(remaining(receiver.path(), &records[1]), 1);
            {
                let storage = body_store(receiver.path());
                let coordinator = owner(receiver.path(), storage.clone(), storage);
                assert!(coordinator
                    .release_due(closed.block_number)
                    .unwrap()
                    .is_none());
            }
            assert_eq!(remaining(receiver.path(), &records[1]), 1);
            if !pruned {
                write_frames(receiver.path(), H + 1, 180);
                expire(&mut records[1]);
                write_records(receiver.path(), &records);
                reconcile_tip(receiver.path(), 180);
                close_through(receiver.path(), 180);
            } else {
                assert!(inspect_retention_journal(journal(receiver.path()))
                    .unwrap()
                    .records
                    .iter()
                    .all(|(key, _)| *key != retired_key));
            }
            assert!(matches!(
                durable_record(receiver.path(), &records[1]).state,
                PinStateV1::Terminal {
                    terminal_height: 180,
                    release_height: 244,
                    ..
                }
            ));
            write_frames(receiver.path(), 181, 250);
            let k = close_through(receiver.path(), 250);
            {
                let storage = body_store(receiver.path());
                let coordinator = owner(receiver.path(), storage.clone(), storage);
                assert!(coordinator.release_due(k.block_number).unwrap().is_some());
            }
            assert_eq!(remaining(receiver.path(), &records[1]), 0);
            assert_c(receiver.path(), k);
            {
                let storage = body_store(receiver.path());
                let coordinator = owner(receiver.path(), storage.clone(), storage);
                assert!(coordinator.release_due(k.block_number).unwrap().is_none());
            }
            for record in &records[usize::from(pruned)..] {
                assert!(matches!(
                    durable_record(receiver.path(), record).state,
                    PinStateV1::Released { .. }
                ));
            }
            if pruned {
                assert!(inspect_retention_journal(journal(receiver.path()))
                    .unwrap()
                    .records
                    .iter()
                    .all(|(key, _)| *key != retired_key));
            }
            assert_eq!(remaining(receiver.path(), &records[1]), 0);
        }
    }
}

mod copied_exex_startup {
    use super::*;
    use alloy_consensus::{Header, Sealable};
    use alloy_eips::BlockNumHash;
    use alloy_primitives::U256;
    use eyre::ensure;
    use futures::FutureExt;
    use outbe_evm::OutbeEvmConfig;
    use outbe_node::projection::{
        prepare_offchain_data_projection, validate_offchain_data_checkpoint,
        OffchainDataProjectionConfig,
    };
    use outbe_node::{OutbeBeaconConsensus, OutbeNode, OutbePoolBuilder};
    use outbe_ocomp::control::poc_schema_limits;
    use outbe_ocomp::embedded_runtime::{EmbeddedOcompBundleConfigV1, EmbeddedOcompDomainConfigV1};
    use outbe_primitives::{OutbeHeader, OutbePrimitives, OutbeReceipt, OutbeTxEnvelope};
    use reth_ethereum::{
        exex::{ExExContext, ExExNotifications, Wal},
        network::NetworkManager,
        node::core::{args::DatadirArgs, primitives::Head},
        provider::db::{
            database::Database,
            init_db,
            mdbx::DatabaseArguments,
            models::StoredBlockBodyIndices,
            tables,
            transaction::{DbTx, DbTxMut},
            DatabaseEnv,
        },
        tasks::Runtime,
    };
    use reth_node_builder::{
        common::WithConfigs,
        components::{Components, NoopPayloadServiceBuilder, PayloadServiceBuilder, PoolBuilder},
        BuilderContext, NodeAdapter, NodeConfig, RethFullAdapter,
    };
    use reth_provider::{
        providers::{
            ProviderFactoryBuilder, ReadOnlyConfig, RocksDBProvider, StaticFileProviderBuilder,
        },
        ChainSpecProvider, HeaderProvider, StaticFileSegment, StaticFileWriter,
    };
    use std::{fs, panic::AssertUnwindSafe, path::Path};

    type Native = RethFullAdapter<DatabaseEnv, OutbeNode>;
    type NativeProvider = <Native as reth_node_builder::FullNodeTypes>::Provider;
    const H: u64 = 30;
    const K: u64 = H + 1;
    const FATAL_DETAIL: &str = "copied fatal startup sentinel";

    // Consensus namespace binding is process-global. Each case gets a fresh
    // libtest process, rather than relying on suite order or mutating a binding.
    fn in_isolated_case(name: &str) -> bool {
        const CHILD_CASE: &str = "OUTBE_TEST_COPIED_EXEX_CASE";
        const STARTED: &str = "OUTBE_TEST_COPIED_EXEX_STARTED";
        if std::env::var(CHILD_CASE).ok().as_deref() == Some(name) {
            outbe_consensus::proof::init_consensus_chain_id(
                outbe_primitives::chain::TESTNET_CHAIN_ID,
            )
            .unwrap();
            fs::write(std::env::var_os(STARTED).expect("child witness path"), name).unwrap();
            return true;
        }
        let witness = tempfile::tempdir().unwrap();
        let started = witness.path().join("started");
        let exact = format!("ocomp_exex::tests::recovery::copied_exex_startup::{name}");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &exact, "--nocapture", "--test-threads=1"])
            .env(CHILD_CASE, name)
            .env(STARTED, &started)
            .env("RAYON_NUM_THREADS", "2")
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(120);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    assert!(status.success(), "isolated case {name} failed: {status}");
                    assert_eq!(
                        fs::read_to_string(&started).unwrap(),
                        name,
                        "child filter ran no case"
                    );
                    return false;
                }
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                result => {
                    let _ = child.kill();
                    let reaped = child.wait();
                    panic!("isolated case {name} did not finish: {result:?}; reap={reaped:?}");
                }
            }
        }
    }

    fn chain() -> Arc<reth_chainspec::ChainSpec<OutbeHeader>> {
        static CHAIN: std::sync::OnceLock<Arc<reth_chainspec::ChainSpec<OutbeHeader>>> =
            std::sync::OnceLock::new();
        CHAIN
            .get_or_init(|| {
                Arc::new(
                    reth_chainspec::ChainSpecBuilder::mainnet()
                        .chain(outbe_primitives::chain::TESTNET_CHAIN_ID.into())
                        .build()
                        .map_header(OutbeHeader::new),
                )
            })
            .clone()
    }

    fn bundle() -> PinnedProtocolBundle {
        let limits = poc_schema_limits();
        let install = outbe_metadosis::test_support::ForkInstallScenario::final_at(
            outbe_ocompregistry::OCOMP_POC_FINAL_ACTIVATION_HEIGHT,
            chain().chain().id(),
            chain().genesis_hash(),
        )
        .unwrap()
        .into_install();
        let bundle = install.protocol_bundle;
        let hash = bundle.protocol_bundle_hash(&limits).unwrap();
        PinnedProtocolBundle::decode(&bundle.encode_canonical(&limits).unwrap(), hash, &limits)
            .unwrap()
    }

    fn open_native(root: &Path, runtime: Runtime) -> NativeProvider {
        let factory = ProviderFactoryBuilder::<OutbeNode>::default()
            .open_read_only(
                chain(),
                ReadOnlyConfig::from_datadir(root).no_watch(),
                runtime,
            )
            .unwrap();
        reth_provider::providers::BlockchainProvider::new(factory).unwrap()
    }

    fn projection_config(root: &Path) -> OffchainDataProjectionConfig {
        OffchainDataProjectionConfig {
            chain_id: chain().chain().id(),
            genesis_hash: chain().genesis_hash(),
            storage: outbe_offchain_storage::StorageConfig {
                start_block: 1,
                backend: outbe_offchain_storage::StorageBackend::RocksDb(
                    outbe_offchain_storage::RocksDbConfig {
                        path: root.join("projection"),
                        secondary_path: root.join("projection-readers"),
                    },
                ),
            },
        }
    }

    fn exex_config(root: &Path) -> OcompExExConfigV1 {
        let bundle = bundle();
        OcompExExConfigV1 {
            domain_root: root.join("ocomp"),
            discovery_spool_root: root.join("ocomp/exporter-v1/discovery"),
            bundles: vec![OcompExExBundleConfigV1 {
                worker_address: "127.0.0.1:0".parse().unwrap(),
                identity: EndpointIdentity {
                    chain_id: chain().chain().id(),
                    genesis_hash: chain().genesis_hash(),
                    boot_nonce: B256::repeat_byte(0x81),
                    protocol_bundle_hash: bundle.hash(),
                },
                protocol_bundle: bundle,
            }],
            policy: EmbeddedNodePolicyV1::FullNode,
            validator_rpc_url: None,
            chain_id: chain().chain().id(),
            genesis_hash: chain().genesis_hash(),
            retention_selector: Arc::new(
                outbe_node::ocomp::retention::SharedOcompRetentionSelector::new(),
            ),
            // These frames have no request/retention events; they cannot establish
            // the installed-retention integration obligation.
            retention_required: false,
        }
    }

    fn closed(root: &Path) -> ProjectionCheckpoint {
        ContiguousCheckpointStoreV1::open(
            root.join("ocomp/exporter-v1/discovery/closure-checkpoint-v1"),
            ProjectionCheckpoint {
                block_number: 0,
                block_hash: chain().genesis_hash(),
            },
        )
        .unwrap()
        .current()
        .unwrap()
    }

    fn projected(root: &Path) -> Option<ProjectionCheckpoint> {
        let storage = Arc::new(
            outbe_offchain_storage::RocksDbStorage::open(root.join("projection")).unwrap(),
        );
        outbe_offchain_data::read_projection_state(
            outbe_offchain_data::ProjectionConfig {
                chain_id: chain().chain().id(),
                genesis_hash: chain().genesis_hash(),
                start_block: 1,
            },
            storage,
        )
        .unwrap()
        .unwrap()
        .checkpoint
    }

    fn assert_no_submissions(root: &Path) {
        assert!(!root.join("ocomp/ocomp-evm-key.hex").exists());
        assert!(!root.join("ocomp/ocomp-key-v1.hex").exists());
        for relative in [
            "supervisor-v1/vote-submissions",
            "supervisor-v1/payout-submissions",
            "supervisor-v1/materialization-submissions",
            "supervisor-v1/sign-once",
        ] {
            let path = root.join("ocomp").join(relative);
            assert!(
                !path.exists() || fs::read_dir(&path).unwrap().next().is_none(),
                "FullNode wrote validator submission material at {}",
                path.display()
            );
        }
    }

    // Real native storage fixture; transactions are signed but not EVM-executed.
    fn write_frames(root: &Path, first: u64, last: u64) -> Vec<ProjectionCheckpoint> {
        let db = init_db(root.join("db"), DatabaseArguments::test()).unwrap();
        let tx = db.tx_mut().unwrap();
        let settings = reth_provider::StorageSettings::v1();
        match tx
            .get::<tables::Metadata>("storage_settings".into())
            .unwrap()
        {
            Some(bytes) => assert!(!serde_json::from_slice::<reth_provider::StorageSettings>(
                &bytes
            )
            .unwrap()
            .is_v2()),
            None => tx
                .put::<tables::Metadata>(
                    "storage_settings".into(),
                    serde_json::to_vec(&settings).unwrap(),
                )
                .unwrap(),
        }
        // Native genesis fixture state. The production FIFO is empty at 1/1;
        // all-zero storage is malformed, not an empty initialized queue.
        // Preserve the initialization history, not only the latest words.
        if first == 0 {
            use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
            use reth_ethereum::provider::db::{models::ShardedKey, table::Table};
            type Word = <tables::PlainStorageState as Table>::Value;
            type HistoryKey = <tables::StoragesHistory as Table>::Key;
            type HistoryBlocks = <tables::StoragesHistory as Table>::Value;
            let mut owner = HashMapStorageProvider::new_with_chain_identity(
                chain().chain().id(),
                chain().genesis_hash(),
            );
            StorageHandle::enter(&mut owner, |storage| {
                let nod = outbe_nod::NodContract::new(storage);
                nod.ocomp_materialization_head_sequence.write(1).unwrap();
                nod.ocomp_materialization_tail_sequence.write(1).unwrap();
                assert!(nod.ocomp_materialization_head().unwrap().is_none());
            });
            for ((address, slot), value) in owner.storage {
                assert!(!value.is_zero());
                let key = B256::from(slot.to_be_bytes::<32>());
                tx.put::<tables::PlainAccountState>(address, Default::default())
                    .unwrap();
                tx.put::<tables::PlainStorageState>(address, Word { key, value })
                    .unwrap();
                tx.put::<tables::StorageChangeSets>(
                    (0, address).into(),
                    Word {
                        key,
                        value: U256::ZERO,
                    },
                )
                .unwrap();
                tx.put::<tables::StoragesHistory>(
                    HistoryKey {
                        address,
                        sharded_key: ShardedKey {
                            key,
                            highest_block_number: u64::MAX,
                        },
                    },
                    HistoryBlocks::new(vec![0]).unwrap(),
                )
                .unwrap();
            }
        }

        let mut parent = if first == 0 {
            B256::ZERO
        } else {
            tx.get::<tables::CanonicalHeaders>(first - 1)
                .unwrap()
                .unwrap()
        };
        let files = StaticFileProviderBuilder::read_write(root.join("static_files"))
            .with_blocks_per_file(1_000)
            .build::<OutbePrimitives>()
            .unwrap();
        let mut writer = files
            .get_writer(first, StaticFileSegment::Transactions)
            .unwrap();
        let mut headers = files.get_writer(first, StaticFileSegment::Headers).unwrap();
        let mut points = Vec::new();
        for height in first..=last {
            let signer =
                outbe_primitives::signer::OutbeEvmSigner::from_secret_bytes([0x41; 32]).unwrap();
            let input = outbe_primitives::system_tx::SystemTxInputV2::CycleTick;
            let unsigned = outbe_primitives::system_tx::build_unsigned_system_tx(
                input.kind(),
                0,
                height,
                chain().chain().id(),
                input.encode().unwrap(),
            )
            .unwrap();
            let transaction: OutbeTxEnvelope = signer.sign_unsigned(unsigned).unwrap();
            let receipt = OutbeReceipt {
                success: true,
                cumulative_gas_used: 21_000,
                ..Default::default()
            };
            let header = if height == 0 {
                chain().genesis_header().clone()
            } else {
                OutbeHeader::new(Header {
                    number: height,
                    parent_hash: parent,
                    timestamp: 1_800_000_000 + height,
                    gas_limit: 30_000_000,
                    gas_used: 21_000,
                    transactions_root: alloy_consensus::proofs::calculate_transaction_root(
                        std::slice::from_ref(&transaction),
                    ),
                    receipts_root: alloy_consensus::proofs::calculate_receipt_root(&[
                        alloy_consensus::TxReceipt::with_bloom_ref(&receipt),
                    ]),
                    ..Default::default()
                })
            };
            let hash = header.hash_slow();
            headers.append_header(&header, &hash).unwrap();
            tx.put::<tables::CanonicalHeaders>(height, hash).unwrap();
            tx.put::<tables::HeaderNumbers>(hash, height).unwrap();
            tx.put::<tables::Headers<OutbeHeader>>(height, header)
                .unwrap();
            tx.put::<tables::BlockBodyIndices>(
                height,
                StoredBlockBodyIndices {
                    first_tx_num: height.saturating_sub(1),
                    tx_count: u64::from(height != 0),
                },
            )
            .unwrap();
            writer.increment_block(height).unwrap();
            if height != 0 {
                writer.append_transaction(height - 1, &transaction).unwrap();
                tx.put::<tables::Receipts<OutbeReceipt>>(height - 1, receipt)
                    .unwrap();
            }
            points.push(ProjectionCheckpoint {
                block_number: height,
                block_hash: hash,
            });
            parent = hash;
        }
        drop(writer);
        drop(headers);
        files.commit().unwrap();
        type Stage = <tables::StageCheckpoints as reth_ethereum::provider::db::table::Table>::Value;
        for stage in ["Headers", "Bodies", "Execution", "Finish"] {
            tx.put::<tables::StageCheckpoints>(stage.into(), Stage::new(last))
                .unwrap();
        }
        tx.put::<tables::ChainState>(tables::ChainStateKey::LastFinalizedBlock, last)
            .unwrap();
        tx.commit().unwrap();
        drop(files);
        drop(db);
        drop(
            RocksDBProvider::builder(root.join("rocksdb"))
                .with_default_tables()
                .build()
                .unwrap(),
        );
        points
    }

    // The adapter is sufficient: no RPC add-ons, engine, peer loop or node process.
    // The event stream and native finality, not notification delivery, drive work.
    fn run_owned(
        root: &Path,
        target: ProjectionCheckpoint,
        expect_fatal: bool,
    ) -> eyre::Result<Vec<BlockNumHash>> {
        let executor = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(2)
            .enable_all()
            .build()?;
        let outcome = executor.block_on(async {
            let runtime = Runtime::test();
            let manager = runtime.take_task_manager_handle().expect("fixture manager");
            let mut exex_tasks = tokio::task::JoinSet::new();
            let mut notification_owner = None;
            let mut payload_shutdown = None;
            let result = AssertUnwindSafe(async {
                let provider = open_native(&root.join("chain"), runtime.clone());
                let chain = provider.chain_spec();
                let header = provider
                    .sealed_header(target.block_number)?
                    .ok_or_else(|| eyre::eyre!("missing copied native target"))?;
                ensure!(header.hash() == target.block_hash, "native target mismatch");
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
                fs::create_dir_all(config.datadir().data_dir())?;
                let head = Head {
                    number: target.block_number,
                    hash: target.block_hash,
                    difficulty: header.header().inner.difficulty,
                    total_difficulty: U256::ZERO,
                    timestamp: header.header().inner.timestamp,
                };
                let builder = BuilderContext::<Native>::new(
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
                    .build_pool(&builder, evm.clone())
                    .await?;
                let network_config = builder.build_network_config(
                    builder
                        .network_config_builder()?
                        .disable_discovery()
                        .disable_nat()
                        .listener_addr(([127, 0, 0, 1], 0).into()),
                );
                let network_owner = NetworkManager::builder(network_config).await?;
                let payload = NoopPayloadServiceBuilder::default()
                    .spawn_payload_builder_service(&builder, pool.clone(), evm.clone())
                    .await?;
                payload_shutdown = Some(
                    tokio::time::timeout(Duration::from_secs(10), payload.subscribe()).await??,
                );
                let adapter: NodeAdapter<Native> = NodeAdapter {
                    components: Components {
                        transaction_pool: pool,
                        evm_config: evm.clone(),
                        consensus: Arc::new(OutbeBeaconConsensus::new(chain)),
                        network: network_owner.handle(),
                        payload_builder_handle: payload,
                    },
                    task_executor: runtime.clone(),
                    provider: provider.clone(),
                };
                let prepared = prepare_offchain_data_projection(projection_config(root))?;
                let ready = validate_offchain_data_checkpoint(prepared, &provider)?;
                let current = closed(root);
                let (publisher, readiness) = outbe_primitives::projection::projection_readiness(
                    current,
                    ProjectionStatus::CatchingUp {
                        checkpoint: Some(current),
                    },
                );
                let (exit, mut exit_rx) = tokio::sync::mpsc::unbounded_channel();
                let (events, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
                let (notifications_tx, notifications_rx) = tokio::sync::mpsc::channel(1);
                notification_owner = Some(notifications_tx);
                let wal = Wal::<OutbePrimitives>::new(root.join("wal"))?;
                let notifications = ExExNotifications::new(
                    (target.block_number, target.block_hash).into(),
                    provider,
                    evm,
                    notifications_rx,
                    wal.handle(),
                );
                let ctx = ExExContext {
                    head: (target.block_number, target.block_hash).into(),
                    config,
                    reth_config: Default::default(),
                    events,
                    notifications,
                    components: adapter,
                };
                exex_tasks.spawn(run_ocomp_exex(
                    ctx,
                    ready,
                    exex_config(root),
                    publisher,
                    exit,
                ));
                let mut finished = Vec::new();
                if expect_fatal {
                    let event = tokio::time::timeout(Duration::from_secs(10), exit_rx.recv())
                        .await?
                        .ok_or_else(|| eyre::eyre!("fatal exit channel closed without an event"))?;
                    ensure!(
                        event.failure.message.contains(FATAL_DETAIL),
                        "wrong fatal: {:?}",
                        event.failure
                    );
                    ensure!(
                        matches!(readiness.current(), ProjectionStatus::Fatal { error, .. }
                        if error.message.contains(FATAL_DETAIL)),
                        "missing persisted-fatal readiness"
                    );
                    // The guard is before first FinishedHeight and frame/projection work.
                    ensure!(
                        event_rx.try_recv().is_err(),
                        "fatal startup published FinishedHeight"
                    );
                } else {
                    tokio::time::timeout(Duration::from_secs(10), async {
                        loop {
                            if let Ok(event) = exit_rx.try_recv() {
                                eyre::bail!("unexpected ExEx fatal: {:?}", event.failure);
                            }
                            while let Ok(ExExEvent::FinishedHeight(point)) = event_rx.try_recv() {
                                finished.push(point);
                            }
                            if matches!(readiness.current(), ProjectionStatus::Ready { checkpoint }
                                if checkpoint == target)
                            {
                                break Ok::<_, eyre::Report>(());
                            }
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                    })
                    .await??;
                    // Observe more than one ordinary reader tick at fixed native finality.
                    tokio::time::sleep(POLL_INTERVAL * 2 + Duration::from_millis(50)).await;
                    while let Ok(ExExEvent::FinishedHeight(point)) = event_rx.try_recv() {
                        finished.push(point);
                    }
                    ensure!(exit_rx.try_recv().is_err(), "quiet tick reported fatal");
                    ensure!(
                            finished
                                .iter()
                                .any(|p| p.number == target.block_number
                                    && p.hash == target.block_hash),
                            "actual ExEx never published target FinishedHeight"
                        );
                    ensure!(
                        finished.iter().all(|p| p.number <= target.block_number),
                        "advanced past native finality"
                    );
                }
                // A fatal handoff also must keep the ExEx pending until its owner tears down.
                ensure!(
                    tokio::time::timeout(Duration::from_millis(50), exex_tasks.join_next())
                        .await
                        .is_err(),
                    "ExEx unexpectedly returned before owned teardown"
                );
                drop(network_owner);
                drop(wal);
                Ok::<_, eyre::Report>(finished)
            })
            .catch_unwind()
            .await;

            // This cleanup runs on success, Result failure, and assertion panic.
            // Joining the ExEx drops its JoinSet and its worker server (Drop joins
            // the server thread). Sender closure below witnesses notification drain exit.
            exex_tasks.shutdown().await;
            let drain = if let Some(sender) = notification_owner {
                tokio::time::timeout(Duration::from_secs(10), sender.closed()).await
            } else {
                Ok(())
            };
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
                drain.wrap_err("notification drain did not release its receiver")?;
                manager_result.wrap_err("component task manager timeout")???;
                ensure!(
                    payload_result.wrap_err("payload shutdown timeout")?,
                    "unexpected payload event"
                );
                ensure!(
                    graceful.wrap_err("component shutdown waiter panicked")?,
                    "component tasks did not stop"
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

    fn seed_fatal(root: &Path) {
        let config = exex_config(root);
        let domain = EmbeddedOcompDomainV1::open(EmbeddedOcompDomainConfigV1 {
            domain_root: config.domain_root,
            registry_generation: 1,
            bundles: config
                .bundles
                .into_iter()
                .map(|lane| EmbeddedOcompBundleConfigV1 {
                    worker_address: lane.worker_address,
                    identity: lane.identity,
                    protocol_bundle: lane.protocol_bundle,
                })
                .collect(),
            policy: EmbeddedNodePolicyV1::FullNode,
            validator_rpc_url: None,
            limits: poc_schema_limits(),
        })
        .unwrap();
        persist_generic_fatal_evidence(
            domain.fatal_evidence_root(),
            B256::repeat_byte(0x41),
            FATAL_DETAIL,
        )
        .unwrap();
        // Ordinary domain Drop shuts down and joins the worker server.
    }

    #[test]
    fn copied_fatal_enters_actual_exex_fatal_handoff_before_any_finished_height() {
        if !in_isolated_case(
            "copied_fatal_enters_actual_exex_fatal_handoff_before_any_finished_height",
        ) {
            return;
        }
        let donor = tempfile::tempdir().unwrap();
        let recipient = tempfile::tempdir().unwrap();
        let points = write_frames(&donor.path().join("chain"), 0, H);
        let target = points[H as usize];
        run_owned(donor.path(), target, false).unwrap();
        assert_eq!(closed(donor.path()), target);
        assert_eq!(projected(donor.path()), Some(target));
        seed_fatal(donor.path());
        copied_native::copy_tree(donor.path(), recipient.path());
        donor.close().unwrap();
        let fatal_root = recipient.path().join("ocomp/node-v1/fatal-evidence");
        let before = load_persisted_fatal_evidence(&fatal_root).unwrap().unwrap();
        assert!(before.contains(FATAL_DETAIL));
        // Give the actual reader real work beyond its copied C=H. The fatal
        // guard must suppress it rather than merely stay idle at an equal tip.
        let later = write_frames(&recipient.path().join("chain"), K, K)[0];
        run_owned(recipient.path(), later, true).unwrap();
        assert_eq!(closed(recipient.path()), target);
        assert_eq!(projected(recipient.path()), Some(target));
        assert_eq!(
            load_persisted_fatal_evidence(&fatal_root).unwrap().unwrap(),
            before
        );
        assert_no_submissions(recipient.path());
    }

    #[test]
    fn copied_fullnode_quiet_h_and_next_frame_reach_k_without_validator_submission() {
        if !in_isolated_case(
            "copied_fullnode_quiet_h_and_next_frame_reach_k_without_validator_submission",
        ) {
            return;
        }
        let donor = tempfile::tempdir().unwrap();
        let recipient = tempfile::tempdir().unwrap();
        let points = write_frames(&donor.path().join("chain"), 0, H);
        let h = points[H as usize];
        // C and projection are advanced by the real ExEx, not stamped into files.
        run_owned(donor.path(), h, false).unwrap();
        assert_eq!(closed(donor.path()), h);
        assert_eq!(projected(donor.path()), Some(h));
        copied_native::copy_tree(donor.path(), recipient.path());
        donor.close().unwrap();
        let quiet = run_owned(recipient.path(), h, false).unwrap();
        assert!(quiet
            .iter()
            .all(|point| point.number == H && point.hash == h.block_hash));
        assert_eq!(closed(recipient.path()), h);
        assert_no_submissions(recipient.path());
        // Every owned provider/task has been dropped before appending fixture data.
        let k = write_frames(&recipient.path().join("chain"), K, K)[0];
        let advanced = run_owned(recipient.path(), k, false).unwrap();
        assert!(advanced
            .iter()
            .any(|point| point.number == H && point.hash == h.block_hash));
        assert!(advanced
            .iter()
            .any(|point| point.number == K && point.hash == k.block_hash));
        assert_eq!(closed(recipient.path()), k);
        assert_eq!(projected(recipient.path()), Some(k));
        assert_no_submissions(recipient.path());
        let current = run_owned(recipient.path(), k, false).unwrap();
        assert!(current
            .iter()
            .all(|point| point.number == K && point.hash == k.block_hash));
        assert_no_submissions(recipient.path());
        // This test has no pending NOD/payout obligation. It establishes actual
        // quiet/next-frame control flow and FullNode non-submission, not all of
        // Task08 Tests-first7 or completed-Lysis/pending-action acceptance.
    }
}
