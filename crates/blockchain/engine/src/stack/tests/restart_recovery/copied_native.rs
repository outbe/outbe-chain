use super::*;
use crate::ce_finalizer::RethDurableCeState;
use crate::ce_recovery::{CanonicalCeReplaySource, CeStartupRecoveryCoordinator};
use alloy_consensus::Sealable;
use alloy_primitives::U256;
use outbe_compressed_entities::{
    sealed_root, CandidateCacheLimits, CeMdbx, CeTopologyV1, CompressedTreeService,
    EnvironmentIdentity, ExactParentIdentity, FinalizedMarker, ACTIVE_COMMITMENT_SCHEME,
    LOCAL_STORAGE_SCHEMA_VERSION,
};
use outbe_primitives::{
    addresses::COMPRESSED_ENTITIES_ADDRESS,
    projection::{projection_readiness, ProjectionStatus},
};
use reth_ethereum::{
    chainspec::ChainSpec,
    node::api::NodeTypesWithDBAdapter,
    provider::db::{
        database::Database,
        init_db,
        mdbx::DatabaseArguments,
        table::Table,
        tables::{self, ChainStateKey},
        transaction::{DbTx, DbTxMut},
        DatabaseEnv,
    },
    tasks::Runtime,
};
use reth_provider::{
    providers::{BlockchainProvider, RocksDBProvider, StaticFileProvider},
    static_file::StaticFileSegment,
    ProviderFactory, StaticFileProviderBuilder, StaticFileWriter, StorageSettings,
    StorageSettingsCache,
};
use std::{fs, path::Path};

mod disk;
use disk::copy_tree;
pub(in crate::stack::tests) use disk::inventory;

type Nodes = NodeTypesWithDBAdapter<outbe_node::OutbeNode, DatabaseEnv>;
type Provider = BlockchainProvider<Nodes>;
type Stage = <tables::StageCheckpoints as Table>::Value;
type StorageWord = <tables::PlainStorageState as Table>::Value;
type Certificate = outbe_consensus::marshal_types::Finalization;
const PREFIX: &str = "copied-native-recovery";
pub(in crate::stack::tests) const H: u64 = 3;
const K: u64 = 5;

pub(in crate::stack::tests) struct DiskFixture {
    pub(in crate::stack::tests) root: tempfile::TempDir,
    pub(in crate::stack::tests) headers: Vec<OutbeHeader>,
    blocks: Vec<ConsensusBlock>,
    certificates: Vec<Certificate>,
    provider: HybridSchemeProvider<MinSig>,
}

fn genesis_header() -> OutbeHeader {
    OutbeHeader::new(Header::default())
}

fn genesis_block() -> ConsensusBlock {
    ConsensusBlock::from_sealed(SealedBlock::seal_slow(
        Block::default().map_header(|_| genesis_header()),
    ))
}

fn genesis_marker() -> FinalizedMarker {
    FinalizedMarker {
        commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
        height: 0,
        block_hash: genesis_header().hash_slow(),
        parent_block_hash: B256::ZERO,
        parent_root: B256::ZERO,
        new_root: sealed_root(B256::ZERO).unwrap(),
    }
}

fn ce_identity() -> EnvironmentIdentity {
    EnvironmentIdentity {
        local_storage_schema_version: LOCAL_STORAGE_SCHEMA_VERSION,
        chain_id: outbe_consensus::proof::consensus_chain_id(),
        genesis_hash: genesis_header().hash_slow(),
        commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
        topology: CeTopologyV1.encode(),
        tree_format: "ckb-smt-v0.6.1-poseidon-catalog-v3".into(),
        vendor_revision: "ad555350c866b2265d87d2d7fbd146fbc918bfe5".into(),
    }
}

fn open_tree(root: &Path) -> Arc<CompressedTreeService> {
    Arc::new(
        CompressedTreeService::new(
            CeMdbx::open(root, ce_identity(), genesis_marker()).unwrap(),
            CandidateCacheLimits {
                max_candidates: 8,
                max_encoded_bytes: 1_048_576,
            },
        )
        .unwrap(),
    )
}

pub(in crate::stack::tests) fn open_static_headers(
    root: &Path,
) -> StaticFileProvider<outbe_primitives::OutbePrimitives> {
    StaticFileProviderBuilder::read_write(root.join("static_files"))
        .with_blocks_per_file_for_segment(StaticFileSegment::Headers, 1)
        .build()
        .unwrap()
}

pub(in crate::stack::tests) fn remove_native_header(root: &Path, height: u64) {
    // Header and canonical-hash columns share this one-block native jar.
    // MDBX Headers/CanonicalHeaders are not authoritative for these reads.
    open_static_headers(root)
        .delete_jar(StaticFileSegment::Headers, height)
        .unwrap();
}

pub(in crate::stack::tests) fn replace_native_header(
    root: &Path,
    height: u64,
    header: &OutbeHeader,
    hash: B256,
) {
    use reth_provider::HeaderProvider as _;
    let files = open_static_headers(root);
    let last = files
        .get_highest_static_file_block(StaticFileSegment::Headers)
        .unwrap();
    let headers: Vec<_> = (0..=last)
        .map(|number| {
            let sealed = files.sealed_header(number).unwrap().unwrap();
            if number == height {
                (header.clone(), hash)
            } else {
                (sealed.header().clone(), sealed.hash())
            }
        })
        .collect();
    // The native writer chooses a range from its current index. Rebuilding
    // this tiny segment avoids asking it to write into an interior gap.
    files.delete_segment(StaticFileSegment::Headers).unwrap();
    let mut writer = files.latest_writer(StaticFileSegment::Headers).unwrap();
    for (number, (header, hash)) in headers.into_iter().enumerate() {
        writer.increment_block(number as u64).unwrap();
        writer
            .append_header_direct(&header, U256::ZERO, &hash)
            .unwrap();
    }
    writer.commit().unwrap();
}

pub(in crate::stack::tests) fn open_native(root: &Path) -> (Provider, Arc<CompressedTreeService>) {
    let db = init_db(root.join("db"), DatabaseArguments::test()).unwrap();
    let chain = Arc::new(ChainSpec::<OutbeHeader> {
        // Match the already-bound unit-test Marshal namespace. Production
        // installs its real network domain before either component is built.
        chain: outbe_consensus::proof::consensus_chain_id().into(),
        genesis_header: SealedHeader::seal_slow(genesis_header()),
        ..Default::default()
    });
    let factory = ProviderFactory::<Nodes>::new(
        db,
        chain,
        open_static_headers(root),
        RocksDBProvider::new(root.join("rocksdb")).unwrap(),
        Runtime::test(),
    )
    .unwrap();
    assert_eq!(factory.cached_storage_settings(), StorageSettings::v1());
    (BlockchainProvider::new(factory).unwrap(), open_tree(root))
}

pub(in crate::stack::tests) fn recover_native(
    root: &Path,
    processed: u64,
    target: u64,
) -> eyre::Result<FinalizedMarker> {
    let (provider, tree) = open_native(root);
    let forkchoice = read_reth_recovery_forkchoice(&provider.canonical_in_memory_state())?;
    assert_eq!(forkchoice.head.block_number, target);
    assert_eq!(forkchoice.safe.unwrap().block_number, target);
    assert_eq!(forkchoice.finalized.unwrap().block_number, target);
    let source = Arc::new(RethDurableCeState::new(provider));
    // Even an equal CE marker must read A and A-1 from the real historical provider.
    let checkpoint = source
        .durable_checkpoint(target)?
        .expect("durable checkpoint");
    assert_eq!(checkpoint.height, target);
    let recovery = CeStartupRecoveryCoordinator::new(source, tree);
    recover_ce_at_reconciled_anchor(&recovery, processed, target)
}

fn persist_fixture_headers(
    tx: &impl DbTxMut,
    headers: &[OutbeHeader],
    through: u64,
) -> eyre::Result<()> {
    // This fixture deliberately uses supported v1 routing: native MDBX
    // state, history and receipt tables. Never rely on an implicit default.
    tx.put::<tables::Metadata>(
        "storage_settings".into(),
        serde_json::to_vec(&StorageSettings::v1())?,
    )?;
    for header in &headers[..=through as usize] {
        tx.put::<tables::CanonicalHeaders>(header.inner.number, header.hash_slow())?;
        tx.put::<tables::HeaderNumbers>(header.hash_slow(), header.inner.number)?;
        tx.put::<tables::Headers<OutbeHeader>>(header.inner.number, header.clone())?;
        tx.put::<tables::BlockBodyIndices>(
            header.inner.number,
            reth_ethereum::provider::db::models::StoredBlockBodyIndices {
                first_tx_num: 0,
                tx_count: 0,
            },
        )?;
    }
    Ok(())
}

fn persist_genesis_ce_root(tx: &impl DbTxMut) -> eyre::Result<()> {
    // All fixture blocks leave CE unchanged, so this native genesis slot is
    // legitimately identical at every historical height. No rewind stubs.
    tx.put::<tables::PlainAccountState>(COMPRESSED_ENTITIES_ADDRESS, Default::default())?;
    tx.put::<tables::PlainStorageState>(
        COMPRESSED_ENTITIES_ADDRESS,
        StorageWord {
            key: B256::with_last_byte(1),
            value: U256::from_be_bytes(genesis_marker().new_root.0),
        },
    )?;
    // A nonzero genesis slot must also be indexed as previously written.
    // Otherwise native historical lookup classifies it as NotYetWritten.
    type HistoryKey = <tables::StoragesHistory as Table>::Key;
    type HistoryBlocks = <tables::StoragesHistory as Table>::Value;
    tx.put::<tables::StoragesHistory>(
        HistoryKey {
            address: COMPRESSED_ENTITIES_ADDRESS,
            sharded_key: reth_ethereum::provider::db::models::ShardedKey {
                key: B256::with_last_byte(1),
                highest_block_number: u64::MAX,
            },
        },
        HistoryBlocks::new(vec![0])?,
    )?;
    tx.put::<tables::StorageChangeSets>(
        (0, COMPRESSED_ENTITIES_ADDRESS).into(),
        StorageWord {
            key: B256::with_last_byte(1),
            value: U256::ZERO,
        },
    )?;
    Ok(())
}

fn seed_reth(root: &Path, headers: &[OutbeHeader], through: u64) {
    let db = init_db(root.join("db"), DatabaseArguments::test()).unwrap();
    let tx = db.tx_mut().unwrap();
    persist_fixture_headers(&tx, headers, through).expect("native fixture headers");
    persist_genesis_ce_root(&tx).expect("native fixture CE root");
    for stage in ["Execution", "Finish"] {
        tx.put::<tables::StageCheckpoints>(stage.into(), Stage::new(through))
            .unwrap();
    }
    tx.put::<tables::ChainState>(ChainStateKey::LastFinalizedBlock, through)
        .unwrap();
    tx.put::<tables::ChainState>(ChainStateKey::LastSafeBlock, through)
        .unwrap();
    tx.commit().unwrap();
    fs::create_dir_all(root.join("static_files")).unwrap();
    let files = open_static_headers(root);
    let first = files
        .get_highest_static_file_block(StaticFileSegment::Headers)
        .map_or(0, |height| height + 1);
    if first <= through {
        let mut writer = files.latest_writer(StaticFileSegment::Headers).unwrap();
        for header in &headers[first as usize..=through as usize] {
            writer.append_header(header, &header.hash_slow()).unwrap();
        }
        writer.commit().unwrap();
    }
}

fn advance_ce(root: &Path, headers: &[OutbeHeader], through: u64) {
    let tree = open_tree(root);
    let marker = tree.finalized_marker().unwrap();
    for height in marker.height + 1..=through {
        let previous = tree.finalized_marker().unwrap();
        let parent = tree
            .open_parent(ExactParentIdentity {
                commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                block_number: previous.height,
                block_hash: previous.block_hash,
                root: previous.new_root,
            })
            .unwrap();
        let seal = parent.prepare_seal(height, &[], &[]).unwrap();
        assert_eq!(seal.new_root(), genesis_marker().new_root);
        let hash = headers[height as usize].hash_slow();
        tree.publish_candidate(hash, seal).unwrap();
        tree.apply_finalized(height, hash, genesis_marker().new_root)
            .unwrap();
    }
}

fn certificates(blocks: &[ConsensusBlock]) -> (HybridSchemeProvider<MinSig>, Vec<Certificate>) {
    let keys: Vec<_> = (1u64..=3).map(bls12381::PrivateKey::from_seed).collect();
    let participants: Set<bls12381::PublicKey> = keys
        .iter()
        .map(|key| key.public_key())
        .try_collect()
        .unwrap();
    let dkg = bootstrap_dkg(3).unwrap();
    let signers: Vec<_> = keys
        .iter()
        .map(|key| {
            let index = participants.index(&key.public_key()).unwrap();
            HybridScheme::signer(
                &config::outbe_app_namespace(),
                participants.clone(),
                key.clone(),
                dkg.polynomial.clone(),
                dkg.shares[index.get() as usize].clone(),
            )
            .unwrap()
        })
        .collect();
    let verifier = HybridScheme::<MinSig>::verifier(
        &config::outbe_app_namespace(),
        participants,
        dkg.polynomial,
    )
    .unwrap();
    let certificates = blocks
        .iter()
        .skip(1)
        .map(|block| {
            let round = Round::new(Epoch::new(0), View::new(block.number()));
            let proposal = Proposal::new(round, round.view().previous().unwrap(), block.digest());
            let signatures: Vec<_> = signers
                .iter()
                .map(|signer| Finalize::sign(signer, proposal.clone()).unwrap())
                .collect();
            Finalization::from_finalizes(
                &verifier,
                commonware_utils::iter::NonEmpty::try_new(signatures.iter()).unwrap(),
                &Sequential,
            )
            .unwrap()
        })
        .collect();
    let provider = HybridSchemeProvider::new();
    let _ = provider.register(Epoch::new(0), verifier);
    (provider, certificates)
}

#[derive(Clone)]
struct LimitedAck {
    through: u64,
    held: Arc<StdMutex<Vec<commonware_utils::acknowledgement::Exact>>>,
    delivered: Arc<StdMutex<Vec<u64>>>,
}
impl Reporter for LimitedAck {
    type Activity = outbe_consensus::marshal_types::MarshalUpdate;
    fn report(&mut self, update: Self::Activity) -> Feedback {
        if let Update::Block(block, ack) = update {
            self.delivered.lock().unwrap().push(block.number());
            if block.number() <= self.through {
                ack.acknowledge();
            } else {
                self.held.lock().unwrap().push(ack);
            }
        }
        Feedback::Ok
    }
}

pub(in crate::stack::tests) struct Observation {
    pub(in crate::stack::tests) processed: u64,
    pub(in crate::stack::tests) hash: B256,
    pub(in crate::stack::tests) recovered: RecoveredApplicationFinalization,
    delivered: Vec<u64>,
}

async fn assert_heartbeat_only(
    engine_rx: &mut tokio::sync::mpsc::UnboundedReceiver<
        reth_ethereum::node::api::BeaconEngineMessage<OutbePayloadTypes>,
    >,
    hash: B256,
) {
    use alloy_rpc_types_engine::{PayloadStatus, PayloadStatusEnum};
    use reth_ethereum::node::api::{BeaconEngineMessage, OnForkChoiceUpdated};
    while let Some(message) = engine_rx.recv().await {
        let BeaconEngineMessage::ForkchoiceUpdated {
            state,
            payload_attrs,
            tx,
        } = message
        else {
            panic!("exact recovered H caused payload execution");
        };
        assert!(
            payload_attrs.is_none(),
            "exact recovered H requested a payload build"
        );
        assert_eq!(state.head_block_hash, hash);
        assert_eq!(state.safe_block_hash, hash);
        assert_eq!(state.finalized_block_hash, hash);
        tx.send(Ok(OnForkChoiceUpdated::valid(PayloadStatus::from_status(
            PayloadStatusEnum::Valid,
        ))))
        .unwrap();
    }
}

async fn archive_stage_reached(
    mailbox: &mut outbe_consensus::marshal_types::MarshalMailbox,
    reporter: &LimitedAck,
    acknowledge_through: u64,
    target: u64,
) -> bool {
    if mailbox
        .get_processed_height()
        .await
        .map(|height| height.get())
        != Some(acknowledge_through)
    {
        return false;
    }
    if mailbox.get_block(Height::new(target)).await.is_none() {
        return false;
    }
    if mailbox
        .get_finalization(Height::new(target))
        .await
        .is_none()
    {
        return false;
    }
    acknowledge_through == target || reporter.delivered.lock().unwrap().contains(&target)
}

async fn wait_for_archive_stage(
    mailbox: &mut outbe_consensus::marshal_types::MarshalMailbox,
    reporter: &LimitedAck,
    acknowledge_through: u64,
    target: u64,
) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if archive_stage_reached(mailbox, reporter, acknowledge_through, target).await {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("native archive/ACK progress did not reach the requested fixture stage");
}

impl DiskFixture {
    pub(in crate::stack::tests) fn new(processed: u64) -> Self {
        Self::with_ce_height(processed, H)
    }

    pub(in crate::stack::tests) fn with_ce_height(processed: u64, ce_height: u64) -> Self {
        let root = tempfile::tempdir().unwrap();
        let genesis = genesis_header();
        assert_eq!(genesis.hash_slow(), genesis_block().block_hash());
        let headers = outbe_consensus::test_harness::linked_headers(
            genesis,
            K,
            ACTIVE_COMMITMENT_SCHEME,
            || genesis_marker().new_root,
        );
        let blocks: Vec<_> = headers
            .iter()
            .map(|header| {
                ConsensusBlock::from_sealed(SealedBlock::seal_slow(
                    Block::default().map_header(|_| header.clone()),
                ))
            })
            .collect();
        let (provider, certificates) = certificates(&blocks);
        seed_reth(root.path(), &headers, H);
        drop(
            reth_ethereum::provider::db::create_db(
                root.path().join("compressed_entities/smt"),
                DatabaseArguments::test(),
            )
            .unwrap(),
        );
        advance_ce(root.path(), &headers, ce_height);
        fs::create_dir_all(root.path().join("recipient-identity")).unwrap();
        fs::write(
            root.path().join("recipient-identity/key"),
            b"donor-local-key-never-copied",
        )
        .unwrap();
        let fixture = Self {
            root,
            headers,
            blocks,
            certificates,
            provider,
        };
        let observed = fixture.phase(fixture.root.path(), 1, H, processed);
        assert_eq!(observed.processed, processed);
        fixture
    }

    pub(in crate::stack::tests) fn copy_to(&self, destination: &Path) {
        for name in ["marshal", "db", "compressed_entities", "static_files"] {
            copy_tree(&self.root.path().join(name), &destination.join(name));
        }
        assert!(destination.join("db/mdbx.dat").is_file());
        assert!(destination
            .join("compressed_entities/smt/mdbx.dat")
            .is_file());
        let disk = inventory(&destination.join("marshal"));
        for required in ["finalizations", "blocks", "application-metadata"] {
            assert!(
                disk.iter().any(|(path, (directory, _, length, _))| path
                    .to_string_lossy()
                    .contains(required)
                    && !directory
                    && *length > 0),
                "missing copied {required}: {disk:?}"
            );
        }
        assert!(!destination.join("recipient-identity").exists());
        fs::create_dir_all(destination.join("recipient-identity")).unwrap();
        fs::write(
            destination.join("recipient-identity/key"),
            b"recipient-own-key",
        )
        .unwrap();
    }

    pub(in crate::stack::tests) fn ack_with_executor(&self, root: &Path, target: u64) {
        let config = commonware_tokio::Config::default()
            .with_worker_threads(1)
            .with_max_blocking_threads(1)
            .with_catch_panics(true)
            .with_storage_directory(root.join("marshal"));
        let provider = self.provider.clone();
        let genesis = self.headers[0].hash_slow();
        let hash = self.headers[target as usize].hash_slow();
        commonware_tokio::Runner::new(config).start(move |context| async move {
            let (engine_tx, mut engine_rx) = tokio::sync::mpsc::unbounded_channel();
            // The live actor may send its ordinary periodic heartbeat while
            // Marshal opens its archives. Reply to that readjustment of the
            // same forkchoice, but reject any payload execution/build request.
            let engine = context
                .child("heartbeat_receiver")
                .spawn(move |_| async move {
                    assert_heartbeat_only(&mut engine_rx, hash).await;
                });
            let checkpoint = ProjectionCheckpoint {
                block_number: target,
                block_hash: hash,
            };
            let (_publisher, readiness) = projection_readiness(
                ProjectionCheckpoint {
                    block_number: 0,
                    block_hash: genesis,
                },
                ProjectionStatus::Ready { checkpoint },
            );
            let (executor, executor_mailbox) = ExecutorActor::new(
                context.child("copied_executor"),
                ConsensusEngineHandle::new(engine_tx),
                outbe_consensus::executor::actor::RecoveredFinalizedState {
                    genesis_hash: genesis,
                    last_finalized_height: target,
                    last_finalized_hash: hash,
                },
                readiness,
                None,
            );
            let (mailbox, resolver, marshal) =
                super::super::harness::start_recovery_marshal_in_partition(
                    context,
                    provider,
                    executor_mailbox,
                    PREFIX.into(),
                    genesis_block(),
                )
                .await;
            let executor = executor.start(mailbox.clone(), Height::new(target));
            tokio::time::timeout(Duration::from_secs(10), async {
                while mailbox
                    .get_processed_height()
                    .await
                    .map(|height| height.get())
                    != Some(target)
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("ordinary executor did not acknowledge copied exact H");
            drop(resolver);
            tokio::time::timeout(Duration::from_secs(10), marshal)
                .await
                .unwrap()
                .unwrap();
            // Marshal owns the only executor reporter. Normal shutdown closes it,
            // so executor exits through its ordinary mailbox-closed branch.
            tokio::time::timeout(Duration::from_secs(10), executor)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            drop(mailbox);
            tokio::time::timeout(Duration::from_secs(10), engine)
                .await
                .unwrap()
                .unwrap();
        });
    }

    pub(in crate::stack::tests) fn missing_archive_error(&self, root: &Path) -> String {
        let config = commonware_tokio::Config::default()
            .with_worker_threads(1)
            .with_max_blocking_threads(1)
            .with_catch_panics(true)
            .with_storage_directory(root.join("marshal"));
        let provider = self.provider.clone();
        commonware_tokio::Runner::new(config).start(move |context| async move {
            let reporter = LimitedAck {
                through: 0,
                held: Arc::new(StdMutex::new(vec![])),
                delivered: Arc::new(StdMutex::new(vec![])),
            };
            let (mailbox, resolver, actor) =
                super::super::harness::start_recovery_marshal_in_partition(
                    context.child("marshal_fixture"),
                    provider,
                    reporter,
                    PREFIX.into(),
                    genesis_block(),
                )
                .await;
            let error = recover_application_finalized_round(context, mailbox.clone(), H)
                .await
                .unwrap_err()
                .to_string();
            drop(resolver);
            tokio::time::timeout(Duration::from_secs(10), actor)
                .await
                .unwrap()
                .unwrap();
            drop(mailbox);
            error
        })
    }

    pub(in crate::stack::tests) fn phase(
        &self,
        root: &Path,
        first_new: u64,
        target: u64,
        acknowledge_through: u64,
    ) -> Observation {
        let path = root.join("marshal");
        let config = commonware_tokio::Config::default()
            .with_worker_threads(1)
            .with_max_blocking_threads(1)
            .with_catch_panics(true)
            .with_storage_directory(&path);
        let provider = self.provider.clone();
        let entries: Vec<_> = (first_new..=target)
            .map(|height| {
                (
                    self.blocks[height as usize].clone(),
                    self.certificates[(height - 1) as usize].clone(),
                )
            })
            .collect();
        let expected_hash = self.blocks[target as usize].block_hash();
        commonware_tokio::Runner::new(config).start(move |context| async move {
            let reporter = LimitedAck {
                through: acknowledge_through,
                held: Arc::new(StdMutex::new(vec![])),
                delivered: Arc::new(StdMutex::new(vec![])),
            };
            let reporter_keepalive = reporter.clone();
            let (mut mailbox, resolver, actor) =
                super::super::harness::start_recovery_marshal_in_partition(
                    context.child("marshal_fixture"),
                    provider,
                    reporter,
                    PREFIX.into(),
                    genesis_block(),
                )
                .await;
            for (block, certificate) in entries {
                assert!(mailbox.verified(certificate.proposal.round, block).await);
                let _ = mailbox.report(Activity::Finalization(certificate));
            }
            wait_for_archive_stage(
                &mut mailbox,
                &reporter_keepalive,
                acknowledge_through,
                target,
            )
            .await;
            let recovered = recover_application_finalized_round(
                context.child("recovered"),
                mailbox.clone(),
                target,
            )
            .await
            .unwrap()
            .unwrap();
            let archived_hash = mailbox
                .get_block(Height::new(target))
                .await
                .unwrap()
                .block_hash();
            assert_eq!(archived_hash, expected_hash);
            assert_eq!(recovered.digest, Digest(archived_hash));
            let observation = Observation {
                processed: mailbox.get_processed_height().await.unwrap().get(),
                hash: archived_hash,
                recovered,
                delivered: reporter_keepalive.delivered.lock().unwrap().clone(),
            };
            // Resolver closure is the ordinary actor exit path. Await it before
            // dropping held ACKs or copying files. Never abort the donor actor.
            drop(resolver);
            tokio::time::timeout(Duration::from_secs(10), actor)
                .await
                .unwrap()
                .unwrap();
            drop(mailbox);
            drop(reporter_keepalive);
            observation
        })
    }
}

#[test]
fn copied_marshal_archives_processed_forkchoice_and_ce_reopen_at_h_then_k() {
    for processed in [H - 1, H] {
        let fixture = DiskFixture::new(processed);
        let recipient = tempfile::tempdir().unwrap();
        fixture.copy_to(recipient.path());
        let donor_before = inventory(fixture.root.path());
        let key_before = fs::read(recipient.path().join("recipient-identity/key")).unwrap();
        let copied = fixture.phase(recipient.path(), H + 1, H, processed);
        assert_eq!(copied.processed, processed);
        let native_head = {
            let (provider, _) = open_native(recipient.path());
            read_reth_recovery_forkchoice(&provider.canonical_in_memory_state())
                .unwrap()
                .head
        };
        assert_eq!(native_head.block_hash, copied.hash);
        let (height, hash, _) = reconcile_recovered_execution_head(
            native_head.block_number,
            native_head.block_hash,
            Some(copied.recovered),
        )
        .unwrap();
        assert_eq!(height, H);
        assert_eq!(hash, fixture.headers[H as usize].hash_slow());
        assert_eq!(
            recover_native(recipient.path(), processed, height)
                .unwrap()
                .height,
            H
        );
        fixture.ack_with_executor(recipient.path(), H);
        let acknowledged = fixture.phase(recipient.path(), H + 1, H, H);
        assert_eq!(acknowledged.processed, H);
        // Test data advances through the ordinary native CE apply path and
        // real Marshal finalization+ACK. This is not a process/EVM-execution test.
        seed_reth(recipient.path(), &fixture.headers, K);
        advance_ce(recipient.path(), &fixture.headers, K);
        let advanced = fixture.phase(recipient.path(), H + 1, K, K);
        assert_eq!(advanced.processed, K);
        assert_eq!(
            advanced.delivered,
            vec![H + 1, K],
            "first ordinary successor must be H+1"
        );
        let reopened = fixture.phase(recipient.path(), K + 1, K, K);
        assert_eq!(reopened.hash, fixture.headers[K as usize].hash_slow());
        assert!(
            reopened.delivered.is_empty(),
            "K restart redelivered acknowledged history"
        );
        assert_eq!(recover_native(recipient.path(), K, K).unwrap().height, K);
        assert_eq!(
            fs::read(recipient.path().join("recipient-identity/key")).unwrap(),
            key_before
        );
        assert_eq!(inventory(fixture.root.path()), donor_before);
    }
}
