//! Configuration and native projection preflight after conventional file placement.
//! These component tests do not claim a full node process launch.

use std::{fs, path::Path, sync::Arc};

use alloy_primitives::B256;
use outbe_node::projection::{prepare_offchain_data_projection, OffchainDataProjectionConfig};
use outbe_offchain_data::{FinalizedBlock, OffchainDataProjection, ProjectionConfig};
use outbe_offchain_storage::{RocksDbStorage, StorageBackend, StorageConfig};
use outbe_primitives::projection::{ProjectionCheckpoint, ProjectionStatus};

fn copy_stopped_directory(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_stopped_directory(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[test]
fn copied_projection_uses_native_checkpoint_and_recipient_configuration_on_each_restart() {
    let donor = tempfile::tempdir().unwrap();
    let recipient = tempfile::tempdir().unwrap();
    let config = ProjectionConfig {
        chain_id: outbe_primitives::chain::TESTNET_CHAIN_ID,
        genesis_hash: B256::repeat_byte(0x71),
        start_block: 1,
    };
    let donor_storage = donor.path().join("projection");
    {
        let storage = Arc::new(RocksDbStorage::open(&donor_storage).unwrap());
        let mut projection =
            OffchainDataProjection::open(config, storage.clone(), storage).unwrap();
        for height in 1..=3 {
            projection
                .project_block(&FinalizedBlock {
                    number: height,
                    hash: B256::repeat_byte(height as u8),
                    receipts: Vec::new(),
                })
                .unwrap();
        }
    }
    let recipient_storage = recipient.path().join("projection");
    copy_stopped_directory(&donor_storage, &recipient_storage);
    assert!(recipient_storage.join("CURRENT").is_file());
    donor.close().unwrap();
    assert!(!donor_storage.exists());

    let configuration = recipient.path().join("configuration");
    fs::create_dir(&configuration).unwrap();
    let storage_file = configuration.join("offchain.toml");
    fs::write(
        &storage_file,
        "version = 1\nbackend = 'rocksdb'\nstart_block = 1\n[rocksdb]\npath = '../projection'\nsecondary_path = '../secondary'\n",
    )
    .unwrap();
    let storage = StorageConfig::load(&storage_file).unwrap();
    let StorageBackend::RocksDb(rocks) = &storage.backend else {
        panic!("expected native RocksDB storage");
    };
    assert_eq!(rocks.path, recipient_storage);
    assert_eq!(rocks.secondary_path, recipient.path().join("secondary"));
    assert_eq!(storage.start_block, 1);

    let secret = configuration.join("recipient-p2p.key");
    let default_secret = recipient.path().join("chain/discovery-secret");
    let network = reth_node_core::args::NetworkArgs {
        p2p_secret_key: Some(secret.clone()),
        ..Default::default()
    };
    let (_, own_public) =
        super::load_reth_p2p_node_host_signer(&network, default_secret.clone()).unwrap();
    let own_key_bytes = fs::read(&secret).unwrap();

    // No manifest, archive or validation receipt is supplied to ordinary preflight.
    for height in [3, 4] {
        let prepared = prepare_offchain_data_projection(OffchainDataProjectionConfig {
            chain_id: config.chain_id,
            genesis_hash: config.genesis_hash,
            storage: storage.clone(),
        })
        .unwrap();
        assert_eq!(
            prepared.readiness().current(),
            ProjectionStatus::CatchingUp {
                checkpoint: Some(ProjectionCheckpoint {
                    block_number: height,
                    block_hash: B256::repeat_byte(height as u8),
                }),
            }
        );
        drop(prepared);
        let (_, public) =
            super::load_reth_p2p_node_host_signer(&network, default_secret.clone()).unwrap();
        assert_eq!(public, own_public);
        assert_eq!(fs::read(&secret).unwrap(), own_key_bytes);
        assert!(!default_secret.exists());
        if height == 3 {
            let storage = Arc::new(RocksDbStorage::open(&recipient_storage).unwrap());
            let mut projection =
                OffchainDataProjection::open(config, storage.clone(), storage).unwrap();
            projection
                .project_block(&FinalizedBlock {
                    number: 4,
                    hash: B256::repeat_byte(4),
                    receipts: Vec::new(),
                })
                .unwrap();
        }
    }

    let mut wrong_start = storage.clone();
    wrong_start.start_block = 4;
    let error = prepare_offchain_data_projection(OffchainDataProjectionConfig {
        chain_id: config.chain_id,
        genesis_hash: config.genesis_hash,
        storage: wrong_start,
    })
    .err()
    .expect("snapshot height is not a replacement for native start_block");
    assert!(error.to_string().contains("start_block 1"));
    assert!(
        prepare_offchain_data_projection(OffchainDataProjectionConfig {
            chain_id: config.chain_id,
            genesis_hash: B256::repeat_byte(0x72),
            storage,
        })
        .is_err()
    );
    assert_eq!(fs::read(&secret).unwrap(), own_key_bytes);
}

mod copied_unequal_ce_projection {
    use super::copy_stopped_directory;
    use alloy_consensus::{Header, Sealable};
    use alloy_primitives::{B256, U256};
    use outbe_compressed_entities::{
        sealed_root, CandidateCacheLimits, CeMdbx, CeTopologyV1, CompressedTreeService,
        EnvironmentIdentity, FinalizedMarker, ACTIVE_COMMITMENT_SCHEME,
        LOCAL_STORAGE_SCHEMA_VERSION,
    };
    use outbe_engine::{
        ce_finalizer::RethDurableCeState,
        ce_recovery::{CanonicalCeReplaySource, CeStartupRecovery, CeStartupRecoveryCoordinator},
    };
    use outbe_node::{
        finalized_frame::{read_bounded_finalized_frames, RethFinalizedFrameSource},
        projection::{
            prepare_offchain_data_projection, validate_offchain_data_checkpoint,
            FinalizedProjectionSink, FinalizedTargetReconciliationV1, OffchainDataProjectionConfig,
        },
    };
    use outbe_offchain_storage::{RocksDbConfig, StorageBackend, StorageConfig};
    use outbe_primitives::{
        addresses::COMPRESSED_ENTITIES_ADDRESS,
        chain::TESTNET_CHAIN_ID,
        projection::{ProjectionCheckpoint, ProjectionReadinessHandle, ProjectionStatus},
        reshare_artifact::{
            encode_outbe_block_artifacts, CompressedEntitiesRootArtifact, OutbeBlockArtifacts,
        },
        OutbeHeader, OutbePrimitives,
    };
    use reth_ethereum::{
        chainspec::ChainSpec,
        node::api::NodeTypesWithDBAdapter,
        provider::db::{
            create_db,
            database::Database,
            init_db,
            mdbx::DatabaseArguments,
            models::StoredBlockBodyIndices,
            table::Table,
            tables::{self, ChainStateKey},
            transaction::{DbTx, DbTxMut},
            DatabaseEnv,
        },
        tasks::Runtime,
    };
    use reth_primitives_traits::SealedHeader;
    use reth_provider::{
        providers::{BlockchainProvider, RocksDBProvider, StaticFileProvider},
        static_file::StaticFileSegment,
        BlockHashReader, ProviderFactory, StaticFileProviderBuilder, StaticFileWriter,
        StorageSettings, StorageSettingsCache,
    };
    use std::{fs, path::Path, sync::Arc};

    type Nodes = NodeTypesWithDBAdapter<outbe_node::OutbeNode, DatabaseEnv>;
    type Provider = BlockchainProvider<Nodes>;
    type Stage = <tables::StageCheckpoints as Table>::Value;
    type StorageWord = <tables::PlainStorageState as Table>::Value;
    const H: u64 = 3;

    fn genesis_header() -> OutbeHeader {
        OutbeHeader::new(Header::default())
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

    fn chain() -> Arc<ChainSpec<OutbeHeader>> {
        Arc::new(ChainSpec {
            chain: TESTNET_CHAIN_ID.into(),
            genesis_header: SealedHeader::seal_slow(genesis_header()),
            ..Default::default()
        })
    }

    fn headers() -> Vec<OutbeHeader> {
        let mut headers = vec![genesis_header()];
        for height in 1..=H {
            headers.push(OutbeHeader::new(Header {
                number: height,
                parent_hash: headers.last().unwrap().hash_slow(),
                extra_data: encode_outbe_block_artifacts(&OutbeBlockArtifacts {
                    compressed_entities_root: Some(CompressedEntitiesRootArtifact {
                        commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                        r_sealed: genesis_marker().new_root,
                    }),
                    ..Default::default()
                })
                .unwrap(),
                ..Default::default()
            }));
        }
        headers
    }

    fn point(headers: &[OutbeHeader], height: u64) -> ProjectionCheckpoint {
        ProjectionCheckpoint {
            block_number: height,
            block_hash: headers[height as usize].hash_slow(),
        }
    }

    fn static_files(root: &Path) -> StaticFileProvider<OutbePrimitives> {
        StaticFileProviderBuilder::read_write(root.join("static_files"))
            .with_blocks_per_file_for_segment(StaticFileSegment::Headers, 1)
            .build()
            .unwrap()
    }

    fn open_provider(root: &Path) -> Provider {
        let db = init_db(root.join("db"), DatabaseArguments::test()).unwrap();
        let factory = ProviderFactory::<Nodes>::new(
            db,
            chain(),
            static_files(root),
            RocksDBProvider::new(root.join("rocksdb")).unwrap(),
            Runtime::test(),
        )
        .unwrap();
        assert_eq!(factory.cached_storage_settings(), StorageSettings::v1());
        BlockchainProvider::new(factory).unwrap()
    }

    fn open_tree(root: &Path) -> Arc<CompressedTreeService> {
        Arc::new(
            CompressedTreeService::new(
                CeMdbx::open(
                    root,
                    EnvironmentIdentity {
                        local_storage_schema_version: LOCAL_STORAGE_SCHEMA_VERSION,
                        chain_id: TESTNET_CHAIN_ID,
                        genesis_hash: genesis_header().hash_slow(),
                        commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                        topology: CeTopologyV1.encode(),
                        tree_format: "ckb-smt-v0.6.1-poseidon-catalog-v3".into(),
                        vendor_revision: "ad555350c866b2265d87d2d7fbd146fbc918bfe5".into(),
                    },
                    genesis_marker(),
                )
                .unwrap(),
                CandidateCacheLimits {
                    max_candidates: 4,
                    max_encoded_bytes: 1_048_576,
                },
            )
            .unwrap(),
        )
    }

    fn open_projection(
        root: &Path,
        provider: &Provider,
    ) -> (FinalizedProjectionSink, ProjectionReadinessHandle) {
        let prepared = prepare_offchain_data_projection(OffchainDataProjectionConfig {
            chain_id: TESTNET_CHAIN_ID,
            genesis_hash: genesis_header().hash_slow(),
            storage: StorageConfig {
                start_block: 1,
                backend: StorageBackend::RocksDb(RocksDbConfig {
                    path: root.join("projection"),
                    secondary_path: root.join("projection-secondary"),
                }),
            },
        })
        .unwrap();
        let readiness = prepared.readiness();
        let ready = validate_offchain_data_checkpoint(prepared, provider).unwrap();
        (FinalizedProjectionSink::new(ready), readiness)
    }

    // Native empty-body storage fixture: no EVM execution/state-root proof is
    // claimed. Actual CE roots, receipts and historical headers are read below.
    fn seed_reth(root: &Path, headers: &[OutbeHeader]) {
        let db = init_db(root.join("db"), DatabaseArguments::test()).unwrap();
        let tx = db.tx_mut().unwrap();
        tx.put::<tables::Metadata>(
            "storage_settings".into(),
            serde_json::to_vec(&StorageSettings::v1()).unwrap(),
        )
        .unwrap();
        for header in headers {
            let number = header.inner.number;
            let hash = header.hash_slow();
            tx.put::<tables::CanonicalHeaders>(number, hash).unwrap();
            tx.put::<tables::HeaderNumbers>(hash, number).unwrap();
            tx.put::<tables::Headers<OutbeHeader>>(number, header.clone())
                .unwrap();
            tx.put::<tables::BlockBodyIndices>(
                number,
                StoredBlockBodyIndices {
                    first_tx_num: 0,
                    tx_count: 0,
                },
            )
            .unwrap();
        }
        tx.put::<tables::PlainAccountState>(COMPRESSED_ENTITIES_ADDRESS, Default::default())
            .unwrap();
        // CE never changes in these blocks, so the actual native root slot is
        // identical at every queried historical height; no state-provider stub.
        tx.put::<tables::PlainStorageState>(
            COMPRESSED_ENTITIES_ADDRESS,
            StorageWord {
                key: B256::with_last_byte(1),
                value: U256::from_be_bytes(genesis_marker().new_root.0),
            },
        )
        .unwrap();
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
            HistoryBlocks::new(vec![0]).unwrap(),
        )
        .unwrap();
        tx.put::<tables::StorageChangeSets>(
            (0, COMPRESSED_ENTITIES_ADDRESS).into(),
            StorageWord {
                key: B256::with_last_byte(1),
                value: U256::ZERO,
            },
        )
        .unwrap();
        for stage in ["Execution", "Finish"] {
            tx.put::<tables::StageCheckpoints>(stage.into(), Stage::new(H))
                .unwrap();
        }
        tx.put::<tables::ChainState>(ChainStateKey::LastFinalizedBlock, H)
            .unwrap();
        tx.put::<tables::ChainState>(ChainStateKey::LastSafeBlock, H)
            .unwrap();
        tx.commit().unwrap();
        drop(db);
        let files = static_files(root);
        let mut writer = files.latest_writer(StaticFileSegment::Headers).unwrap();
        for header in headers {
            writer.append_header(header, &header.hash_slow()).unwrap();
        }
        writer.commit().unwrap();
    }

    fn apply_projection_suffix(
        sink: &mut FinalizedProjectionSink,
        provider: &Provider,
        next: u64,
        target: ProjectionCheckpoint,
    ) -> eyre::Result<Vec<u64>> {
        let source = RethFinalizedFrameSource::new(provider.clone());
        let Some(batch) = read_bounded_finalized_frames(
            &source,
            next,
            (target.block_number, target.block_hash).into(),
        )?
        else {
            return Ok(Vec::new());
        };
        let mut visited = Vec::new();
        for frame in batch.frames() {
            visited.push(frame.identity().number);
            assert_eq!(
                sink.project_frame(frame)?,
                ProjectionCheckpoint {
                    block_number: frame.identity().number,
                    block_hash: frame.identity().hash,
                }
            );
        }
        Ok(visited)
    }

    fn seed_pair(root: &Path, headers: &[OutbeHeader], q: u64, p: u64) {
        seed_reth(root, headers);
        // Match the existing engine fixture's small test-DB initialization.
        // CeMdbx remains the sole owner of table/identity/root initialization.
        drop(
            create_db(
                root.join("compressed_entities/smt"),
                DatabaseArguments::test(),
            )
            .unwrap(),
        );
        let provider = open_provider(root);
        let tree = open_tree(root);
        let source = Arc::new(RethDurableCeState::new(provider.clone()));
        let recovery = CeStartupRecoveryCoordinator::new(source.clone(), tree.clone());
        assert_eq!(recovery.recover_before_participation(q).unwrap().height, q);
        assert_eq!(
            tree.finalized_marker().unwrap().block_hash,
            point(headers, q).block_hash
        );
        let (mut sink, _) = open_projection(root, &provider);
        assert_eq!(sink.durable_checkpoint(), None);
        assert_eq!(
            apply_projection_suffix(&mut sink, &provider, 1, point(headers, p)).unwrap(),
            (1..=p).collect::<Vec<_>>()
        );
        assert_eq!(sink.durable_checkpoint(), Some(point(headers, p)));
        // Prove the to-be-removed H input exists without advancing either marker.
        assert_eq!(source.replay_block(H).unwrap().unwrap().number, H);
        let frame_source = RethFinalizedFrameSource::new(provider);
        let batch = read_bounded_finalized_frames(
            &frame_source,
            H,
            (H, point(headers, H).block_hash).into(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(batch.frames().len(), 1);
        assert_eq!(tree.finalized_marker().unwrap().height, q);
    }

    fn copy_pair(donor: &Path, recipient: &Path) {
        for relative in [
            "db",
            "static_files",
            "rocksdb",
            "compressed_entities",
            "projection",
        ] {
            copy_stopped_directory(&donor.join(relative), &recipient.join(relative));
        }
        // Native nonempty markers/data, without hashing an entire sparse CE map.
        assert!(fs::metadata(recipient.join("db/mdbx.dat")).unwrap().len() > 0);
        assert!(
            fs::metadata(recipient.join("compressed_entities/smt/mdbx.dat"))
                .unwrap()
                .len()
                > 0
        );
        assert!(recipient.join("projection/CURRENT").is_file());
    }

    fn assert_pair(root: &Path, headers: &[OutbeHeader], q: u64, p: u64) {
        let provider = open_provider(root);
        let tree = open_tree(root);
        let (sink, _) = open_projection(root, &provider);
        let marker = tree.finalized_marker().unwrap();
        assert_eq!(marker.height, q);
        assert_eq!(marker.block_hash, point(headers, q).block_hash);
        assert_eq!(marker.new_root, genesis_marker().new_root);
        assert_eq!(sink.durable_checkpoint(), Some(point(headers, p)));
    }

    #[test]
    fn copied_native_unequal_ce_and_projection_recover_their_own_suffix_then_reopen() {
        for (q, p) in [(H, H - 1), (H - 1, H)] {
            let donor = tempfile::tempdir().unwrap();
            let recipient = tempfile::tempdir().unwrap();
            let headers = headers();
            seed_pair(donor.path(), &headers, q, p);
            copy_pair(donor.path(), recipient.path());
            donor.close().unwrap();
            assert_pair(recipient.path(), &headers, q, p);
            {
                let provider = open_provider(recipient.path());
                let tree = open_tree(recipient.path());
                let source = Arc::new(RethDurableCeState::new(provider.clone()));
                let target = point(&headers, H);
                let checkpoint = source.durable_checkpoint(H).unwrap().unwrap();
                assert_eq!(checkpoint.block_hash, target.block_hash);
                assert_eq!(
                    checkpoint.parent_block_hash,
                    point(&headers, H - 1).block_hash
                );
                assert_eq!(checkpoint.root, genesis_marker().new_root);
                let (mut sink, readiness) = open_projection(recipient.path(), &provider);
                assert_eq!(sink.durable_checkpoint(), Some(point(&headers, p)));
                assert_eq!(
                    readiness.current(),
                    if p == H {
                        ProjectionStatus::Ready { checkpoint: target }
                    } else {
                        ProjectionStatus::CatchingUp {
                            checkpoint: Some(point(&headers, p)),
                        }
                    }
                );
                let recovery = CeStartupRecoveryCoordinator::new(source, tree.clone());
                assert_eq!(recovery.recover_before_participation(H).unwrap().height, H);
                assert_eq!(
                    sink.durable_checkpoint(),
                    Some(point(&headers, p)),
                    "CE replay cannot stamp P"
                );
                assert_eq!(
                    sink.reconcile_finalized_target(Some(target)).unwrap(),
                    FinalizedTargetReconciliationV1::Process {
                        target,
                        recovered_floor: Some(point(&headers, p))
                    }
                );
                assert_eq!(
                    apply_projection_suffix(&mut sink, &provider, p + 1, target).unwrap(),
                    (p + 1..=H).collect::<Vec<_>>()
                );
                sink.publish_progress(target).unwrap();
                assert_eq!(
                    readiness.current(),
                    ProjectionStatus::Ready { checkpoint: target }
                );
                assert_eq!(tree.finalized_marker().unwrap().height, H);
            }
            assert_pair(recipient.path(), &headers, H, H);
        }
    }

    #[test]
    fn copied_native_unequal_ce_projection_missing_suffix_keeps_independent_markers() {
        for (q, p) in [(H, H - 1), (H - 1, H)] {
            let donor = tempfile::tempdir().unwrap();
            let recipient = tempfile::tempdir().unwrap();
            let headers = headers();
            seed_pair(donor.path(), &headers, q, p);
            copy_pair(donor.path(), recipient.path());
            donor.close().unwrap();
            assert_pair(recipient.path(), &headers, q, p);
            {
                let db = init_db(recipient.path().join("db"), DatabaseArguments::test()).unwrap();
                let tx = db.tx_mut().unwrap();
                assert!(tx.delete::<tables::BlockBodyIndices>(H, None).unwrap());
                tx.commit().unwrap();
            }
            {
                let provider = open_provider(recipient.path());
                let tree = open_tree(recipient.path());
                let marker = tree.finalized_marker().unwrap();
                let source = Arc::new(RethDurableCeState::new(provider.clone()));
                // Equal/header/root prerequisites survive; only H's replay body
                // and receipt lookup was cut. No common marker is manufactured.
                assert!(source.durable_checkpoint(H).unwrap().is_some());
                assert_eq!(
                    provider.block_hash(H).unwrap(),
                    Some(point(&headers, H).block_hash)
                );
                let (mut sink, _) = open_projection(recipient.path(), &provider);
                let before_p = sink.durable_checkpoint();
                let recovery = CeStartupRecoveryCoordinator::new(source, tree.clone());
                let result = recovery.recover_before_participation(H);
                if q < H {
                    let error = result.unwrap_err();
                    assert!(
                        format!("{error:#}").contains("durable receipts missing"),
                        "{error:#}"
                    );
                } else {
                    assert_eq!(
                        result.unwrap(),
                        marker,
                        "equal CE does not need a replay suffix"
                    );
                }
                assert_eq!(tree.finalized_marker().unwrap(), marker);
                assert_eq!(sink.durable_checkpoint(), before_p);
                let target = point(&headers, H);
                assert_eq!(
                    sink.reconcile_finalized_target(Some(target)).unwrap(),
                    FinalizedTargetReconciliationV1::Process {
                        target,
                        recovered_floor: Some(point(&headers, p))
                    }
                );
                let result = apply_projection_suffix(&mut sink, &provider, p + 1, target);
                if p < H {
                    let error = result.unwrap_err();
                    let message = format!("{error:#}");
                    assert!(
                        message.contains("finalized block 3") && message.contains("unavailable"),
                        "{message}"
                    );
                } else {
                    assert!(
                        result.unwrap().is_empty(),
                        "equal P must request no old frame"
                    );
                }
                assert_eq!(sink.durable_checkpoint(), before_p);
                assert_eq!(tree.finalized_marker().unwrap(), marker);
            }
            assert_pair(recipient.path(), &headers, q, p);
            let db = init_db(recipient.path().join("db"), DatabaseArguments::test()).unwrap();
            assert!(db
                .tx()
                .unwrap()
                .get::<tables::BlockBodyIndices>(H)
                .unwrap()
                .is_none());
        }
    }
}
