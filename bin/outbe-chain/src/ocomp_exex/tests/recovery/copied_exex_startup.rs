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
        outbe_consensus::proof::init_consensus_chain_id(outbe_primitives::chain::TESTNET_CHAIN_ID)
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
    PinnedProtocolBundle::decode(&bundle.encode_canonical(&limits).unwrap(), hash, &limits).unwrap()
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
        outbe_offchain_storage::RocksDbStorage::open(root.join("projection/system/shared"))
            .unwrap(),
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
        Some(bytes) => assert!(
            !serde_json::from_slice::<reth_provider::StorageSettings>(&bytes)
                .unwrap()
                .is_v2()
        ),
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
            payload_shutdown =
                Some(tokio::time::timeout(Duration::from_secs(10), payload.subscribe()).await??);
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
                        .any(|p| p.number == target.block_number && p.hash == target.block_hash),
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
    if !in_isolated_case("copied_fatal_enters_actual_exex_fatal_handoff_before_any_finished_height")
    {
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
