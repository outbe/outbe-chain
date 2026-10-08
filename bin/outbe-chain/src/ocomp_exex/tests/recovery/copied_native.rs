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

pub(in crate::ocomp_exex::tests) fn initialize_storage_settings(tx: &(impl DbTx + DbTxMut)) {
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
}

pub(in crate::ocomp_exex::tests) struct FrameIdentity {
    pub(in crate::ocomp_exex::tests) height: u64,
    pub(in crate::ocomp_exex::tests) parent: B256,
    pub(in crate::ocomp_exex::tests) timestamp: u64,
}

pub(in crate::ocomp_exex::tests) fn frame_header(
    identity: FrameIdentity,
    transaction: &OutbeTxEnvelope,
    receipt: &OutbeReceipt,
) -> OutbeHeader {
    OutbeHeader::new(Header {
        number: identity.height,
        parent_hash: identity.parent,
        timestamp: identity.timestamp,
        gas_limit: 30_000_000,
        gas_used: 21_000,
        transactions_root: alloy_consensus::proofs::calculate_transaction_root(
            std::slice::from_ref(transaction),
        ),
        receipts_root: alloy_consensus::proofs::calculate_receipt_root(&[
            alloy_consensus::TxReceipt::with_bloom_ref(receipt),
        ]),
        ..Default::default()
    })
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
    initialize_storage_settings(&tx);
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
            frame_header(
                FrameIdentity {
                    height,
                    parent,
                    timestamp: height,
                },
                &transaction,
                &receipt,
            )
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
) -> impl BlockHashReader + BlockReader<Receipt = OutbeReceipt> + StateProviderFactory + Clone + 'static
{
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
    PinnedProtocolBundle::decode(&bundle.encode_canonical(&limits).unwrap(), hash, &limits).unwrap()
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
    let domain = outbe_ocomp::embedded_runtime::open_embedded_domain(EmbeddedOcompDomainConfigV1 {
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

use outbe_ocomp_protocol::{
    control::FinalizedJobSpecV1,
    hash::hash_framed,
    intent::JobIntentV1,
    registry::HashDomain,
    result::ResultRootsV1,
    state::{OcompFinalizedJobV1, OcompJobRecordV1},
};
fn finalized_job_spec(
    seed: u8,
    cursor: u64,
    chain_id: u64,
    genesis_hash: B256,
) -> FinalizedJobSpecV1 {
    outbe_ocomp::test_support::finalized_single_tribute_job(
        outbe_ocomp::test_support::FixtureJobIdentity {
            seed,
            cursor,
            chain_id,
            genesis_hash,
        },
        bundle().bundle(),
        || outbe_ocomp::test_support::FixtureJobTiming {
            open_height: cursor + outbe_ocomp_protocol::state::RESULT_VOTE_MIN_FINALITY_DEPTH,
            deadline_height: cursor
                + outbe_ocomp_protocol::state::RESULT_VOTE_MIN_FINALITY_DEPTH
                + 1_800,
        },
    )
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
    let mut result = outbe_ocomp::test_support::unused_lysis_result(
        intent,
        job.finalized.as_ref().unwrap().job_id,
        outbe_ocomp::test_support::FixtureResultCommitments {
            input_manifest_hash: B256::repeat_byte(0x35),
            plan_hash: B256::repeat_byte(0x36),
            unit_artifact_root: B256::repeat_byte(0x37),
            fidelity_fraction_root: B256::repeat_byte(0x38),
            gratis_prefix_root: B256::repeat_byte(0x39),
            result_chunk_list_root: B256::repeat_byte(0x3a),
            roots: ResultRootsV1 {
                nod_root: B256::repeat_byte(0x31),
                bucket_root: B256::repeat_byte(0x32),
                contributor_root: B256::repeat_byte(0x33),
                output_manifest_root: B256::repeat_byte(0x34),
            },
        },
    );
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

fn worker_started_count(client: &reqwest::blocking::Client, address: std::net::SocketAddr) -> u64 {
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
    use outbe_ocomp::embedded::EmbeddedJobEventV1;
    for closed_height in [3, 5] {
        let donor = tempfile::tempdir().unwrap();
        let receiver = tempfile::tempdir().unwrap();
        let points = write_frames(&donor.path().join("chain"), 0, 5);
        let bundle = bundle();
        let spec = finalized_job_spec(0x31, 2, chain().chain().id(), chain().genesis_hash());
        let (result, digest) = save_local_result_fixture(donor.path(), &spec);
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
        wait_for_idle_worker(&mut child, &client, metrics);
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
    // This is the same startup reader. The component fixture does not run the ExEx
    // fatal wait loop.
    persist_generic_fatal_evidence(&copied, B256::repeat_byte(0x42), "replacement").unwrap();
    assert!(load_persisted_fatal_evidence(&copied)
        .unwrap()
        .unwrap()
        .contains("copied fatal evidence"));
}

mod copied_retention;

mod replay;

fn wait_for_idle_worker(
    child: &mut ChildWorker,
    client: &reqwest::blocking::Client,
    metrics: std::net::SocketAddr,
) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "worker subprocess exited before observation"
        );
        if let Ok(response) = client.get(format!("http://{metrics}/status")).send() {
            if let Ok(status) = response.json::<outbe_ocomp::worker_observability::WorkerStatusV1>()
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
}

fn save_local_result_fixture(donor: &Path, spec: &FinalizedJobSpecV1) -> (LysisResultV1, B256) {
    use outbe_node::ocomp::local_result::LocalLysisResultStore;
    let intent =
        JobIntentV1::decode_canonical(&spec.canonical_job_intent.0, &poc_schema_limits()).unwrap();
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
    let local_path = donor.join("ocomp/node-v1/local-results");
    fs::create_dir_all(local_path.parent().unwrap()).unwrap();
    let store = LocalLysisResultStore::open(&local_path, poc_schema_limits()).unwrap();
    store
        .commit(
            spec.summary.job_id,
            &result.encode_canonical(&poc_schema_limits()).unwrap(),
        )
        .unwrap();
    drop(store);
    (result, digest)
}
