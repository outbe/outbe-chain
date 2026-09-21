use super::*;

#[derive(Debug)]
struct RecordingCeStartupRecovery {
    requested_height: AtomicU64,
    marker: outbe_compressed_entities::FinalizedMarker,
}

impl CeStartupRecovery for RecordingCeStartupRecovery {
    fn recover_before_participation(
        &self,
        consensus_finalized_height: u64,
    ) -> std::result::Result<
        outbe_compressed_entities::FinalizedMarker,
        crate::ce_recovery::CeStartupRecoveryError,
    > {
        self.requested_height
            .store(consensus_finalized_height, Ordering::SeqCst);
        Ok(self.marker)
    }
}

#[test]
fn ce_recovery_uses_exact_archive_backed_head_when_ack_floor_lags() {
    let archive_height = 302;
    let marshal_processed_height = 301;
    let archive_hash = B256::repeat_byte(0x42);
    let round = Round::new(Epoch::new(3), View::new(17));
    let (recovery_anchor_height, _, _) = reconcile_recovered_execution_head(
        archive_height,
        archive_hash,
        Some(RecoveredApplicationFinalization {
            round,
            digest: Digest(archive_hash),
        }),
    )
    .unwrap();
    let marker = outbe_compressed_entities::FinalizedMarker {
        commitment_scheme_version: 1,
        height: archive_height,
        block_hash: archive_hash,
        parent_block_hash: B256::repeat_byte(0x41),
        parent_root: B256::repeat_byte(0x51),
        new_root: B256::repeat_byte(0x52),
    };
    let recovery = RecordingCeStartupRecovery {
        requested_height: AtomicU64::new(u64::MAX),
        marker,
    };

    let recovered = recover_ce_at_reconciled_anchor(
        &recovery,
        marshal_processed_height,
        recovery_anchor_height,
    )
    .unwrap();

    assert_eq!(recovered, marker);
    assert_eq!(
        recovery.requested_height.load(Ordering::SeqCst),
        archive_height,
        "CE recovery must use exact archived finality, not the lagging ACK floor"
    );
}

#[test]
fn benign_unfinalized_head_lead_is_recoverable() {
    // Steady state: head is exactly one block ahead of the finalized tip.
    assert!(unfinalized_head_lead_is_recoverable(70, 69));
    // A few blocks ahead during a finalization hiccup, up to the bound.
    assert!(unfinalized_head_lead_is_recoverable(
        69 + MAX_UNFINALIZED_HEAD_LEAD,
        69
    ));
}

#[test]
fn recovery_anchor_never_promotes_an_execution_only_head_to_finalized() {
    assert_eq!(durable_recovery_anchor_height(70, 69), 69);
    assert_eq!(durable_recovery_anchor_height(69, 69), 69);
    assert_eq!(durable_recovery_anchor_height(68, 69), 68);
    assert_eq!(durable_recovery_anchor_height(0, 0), 0);
}

#[test]
fn no_lead_is_not_a_recovery_case() {
    // head == finalized: recover(head) would have succeeded; not this arm.
    assert!(!unfinalized_head_lead_is_recoverable(69, 69));
    // head behind finalized (execution lags): saturating lead is 0.
    assert!(!unfinalized_head_lead_is_recoverable(68, 69));
}

#[test]
fn zero_finalized_tip_is_not_recoverable() {
    // No durable finalized tip at all -> fresh/corrupt, never the benign case.
    assert!(!unfinalized_head_lead_is_recoverable(5, 0));
}

#[test]
fn lead_beyond_bound_stays_fatal() {
    // A head far ahead of the finalized tip is suspicious, not an in-flight
    // head - it must NOT be silently tolerated.
    assert!(!unfinalized_head_lead_is_recoverable(
        69 + MAX_UNFINALIZED_HEAD_LEAD + 1,
        69
    ));
}

#[test]
fn bounded_head_lead_membership_drift_uses_recovered_boundary_committee() {
    use commonware_cryptography::Signer as _;
    use std::net::SocketAddr;

    let marshal_finalized_height = 100;
    let reth_head = marshal_finalized_height + MAX_UNFINALIZED_HEAD_LEAD;
    assert!(
        unfinalized_head_lead_is_recoverable(reth_head, marshal_finalized_height),
        "bounded Reth head lead should be treated as the benign restart window"
    );

    let temp = tempfile::tempdir().unwrap();
    let evm_key_path = temp.path().join("evm-key.hex");
    let evm_secret = [0x52u8; 32];
    std::fs::write(&evm_key_path, hex::encode(evm_secret)).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&evm_key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let evm_signer =
        outbe_primitives::signer::OutbeEvmSigner::from_secret_bytes(evm_secret).unwrap();

    let (keys, _participants, output, polynomial) = run_test_dkg();
    let local_key = &keys[0];
    let boundary_addresses = vec![
        evm_signer.address(),
        Address::with_last_byte(0x22),
        Address::with_last_byte(0x33),
    ];
    let boundary_validator_set = validators::ValidatorSet {
        public_keys: keys.iter().map(|key| key.public_key()).collect(),
        addresses: boundary_addresses.clone(),
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 3],
    };
    let recovered_boundary =
        dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
            epoch: Epoch::new(7),
            validator_set: &boundary_validator_set,
            output: &output,
            is_full_dkg: false,
            dkg_cycle: 6,
            freeze_height: 10,
            planned_activation_height: 20,
            vrf_material_version: 2,
            is_validator_set_change: true,
            tee_expired_target_exclusions: Vec::new(),
        })
        .unwrap();
    let boundary_participants =
        select_recovery_participants(output.players(), &recovered_boundary).unwrap();
    assert_eq!(&boundary_participants, output.players());

    // Simulate provider-latest state after an unfinalized membership-changing
    // head: old participant A has been removed, and a new D is present.
    let replacement_key = bls12381::PrivateKey::from_seed(99);
    let latest_after_unfinalized_removal = validators::ValidatorSet {
        public_keys: vec![
            keys[1].public_key(),
            keys[2].public_key(),
            replacement_key.public_key(),
        ],
        addresses: vec![
            Address::with_last_byte(0x22),
            Address::with_last_byte(0x33),
            Address::with_last_byte(0x44),
        ],
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 3],
    };
    assert!(
        ordered_validator_addresses(&boundary_participants, &latest_after_unfinalized_removal)
            .is_err(),
        "pre-fix provider-latest address mapping should fail when old A is absent"
    );

    let vrf_materials = VrfMaterialProvider::new(2, polynomial, None);
    let (_verifier_scheme, recovered_addresses) = epoch_validation_inputs(
        Epoch::new(7),
        &boundary_participants,
        &latest_after_unfinalized_removal,
        Some(&recovered_boundary),
        &vrf_materials,
    )
    .expect("bounded-head-lead recovery must use recovered boundary committee");
    assert_eq!(recovered_addresses, boundary_addresses);

    let args = crate::args::ConsensusArgs {
        is_validator: true,
        signing_key: Some(temp.path().join("signing-key.hex")),
        validator_evm_key: Some(evm_key_path),
        signing_share: None,
        public_polynomial: None,
        dkg_output: None,
        listen_address: "127.0.0.1:30400".parse::<SocketAddr>().unwrap(),
        storage_dir: None,
        keys_dir: None,
        trust_el_head: false,
        testnet_unix_time_offset_secs: None,
        consensus_peers: Vec::new(),
        use_local_defaults: true,
        payload_resolve_time_ms: 200,
        payload_return_time_ms: 450,
        worker_threads: 1,
        bls_key_backend: "plaintext".to_string(),
        bls_passphrase: None,
        tee_enclave_socket: None,
        tee_session_mode: crate::args::TeeSessionMode::PolicyDefault,
        tee_bootstrap_timeout_secs: 60,
        tee_canary_interval_secs: 30,
        tee_canary_failure_threshold: 3,
        txpool_pending_staleness_secs: 600,
        radicle_control_socket: None,
        radicle_status_address: None,
        upstream: None,
        upstream_nocertify: false,
        projection_storage_config: Some("/tmp/offchain-storage.toml".into()),
    };
    let signer_address = validate_validator_evm_signer(
        &args,
        local_key,
        &latest_after_unfinalized_removal,
        &latest_after_unfinalized_removal,
        Some((&boundary_participants, &recovered_boundary)),
        false,
    )
    .expect("old-epoch signer A should be authorized by recovered boundary, not latest state");
    assert_eq!(signer_address, Some(evm_signer.address()));
}

pub(in crate::stack::tests) mod copied_native {
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
        reshare_artifact::{
            encode_outbe_block_artifacts, CompressedEntitiesRootArtifact, OutbeBlockArtifacts,
        },
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
    use std::{
        fs,
        io::Read,
        path::{Path, PathBuf},
    };

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

    pub(in crate::stack::tests) fn open_native(
        root: &Path,
    ) -> (Provider, Arc<CompressedTreeService>) {
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

    fn seed_reth(root: &Path, headers: &[OutbeHeader], through: u64) {
        let db = init_db(root.join("db"), DatabaseArguments::test()).unwrap();
        let tx = db.tx_mut().unwrap();
        // This fixture deliberately uses supported v1 routing: native MDBX
        // state, history and receipt tables. Never rely on an implicit default.
        tx.put::<tables::Metadata>(
            "storage_settings".into(),
            serde_json::to_vec(&StorageSettings::v1()).unwrap(),
        )
        .unwrap();
        for header in &headers[..=through as usize] {
            tx.put::<tables::CanonicalHeaders>(header.inner.number, header.hash_slow())
                .unwrap();
            tx.put::<tables::HeaderNumbers>(header.hash_slow(), header.inner.number)
                .unwrap();
            tx.put::<tables::Headers<OutbeHeader>>(header.inner.number, header.clone())
                .unwrap();
            tx.put::<tables::BlockBodyIndices>(
                header.inner.number,
                reth_ethereum::provider::db::models::StoredBlockBodyIndices {
                    first_tx_num: 0,
                    tx_count: 0,
                },
            )
            .unwrap();
        }
        // All fixture blocks leave CE unchanged, so this native genesis slot is
        // legitimately identical at every historical height; no rewind stubs.
        tx.put::<tables::PlainAccountState>(COMPRESSED_ENTITIES_ADDRESS, Default::default())
            .unwrap();
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
                let proposal =
                    Proposal::new(round, round.view().previous().unwrap(), block.digest());
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

    impl DiskFixture {
        pub(in crate::stack::tests) fn new(processed: u64) -> Self {
            Self::with_ce_height(processed, H)
        }

        pub(in crate::stack::tests) fn with_ce_height(processed: u64, ce_height: u64) -> Self {
            let root = tempfile::tempdir().unwrap();
            let mut headers = vec![genesis_header()];
            assert_eq!(headers[0].hash_slow(), genesis_block().block_hash());
            for height in 1..=K {
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
            use alloy_rpc_types_engine::{PayloadStatus, PayloadStatusEnum};
            use reth_ethereum::node::api::{BeaconEngineMessage, OnForkChoiceUpdated};

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
                    genesis,
                    target,
                    hash,
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
                tokio::time::timeout(Duration::from_secs(10), async {
                    loop {
                        if mailbox
                            .get_processed_height()
                            .await
                            .map(|height| height.get())
                            == Some(acknowledge_through)
                            && mailbox.get_block(Height::new(target)).await.is_some()
                            && mailbox
                                .get_finalization(Height::new(target))
                                .await
                                .is_some()
                            && (acknowledge_through == target
                                || reporter_keepalive
                                    .delivered
                                    .lock()
                                    .unwrap()
                                    .contains(&target))
                        {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("native archive/ACK progress did not reach the requested fixture stage");
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
                // dropping held ACKs or copying files; never abort the donor actor.
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

    pub(in crate::stack::tests) fn inventory(
        root: &Path,
    ) -> BTreeMap<PathBuf, (bool, u32, u64, Vec<u8>)> {
        fn visit(
            base: &Path,
            path: &Path,
            rows: &mut BTreeMap<PathBuf, (bool, u32, u64, Vec<u8>)>,
        ) {
            for entry in fs::read_dir(path).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                let metadata = fs::symlink_metadata(&path).unwrap();
                assert!(!metadata.file_type().is_symlink());
                #[cfg(unix)]
                let mode = {
                    use std::os::unix::fs::PermissionsExt;
                    metadata.permissions().mode()
                };
                #[cfg(not(unix))]
                let mode = u32::from(metadata.permissions().readonly());
                if metadata.is_dir() {
                    rows.insert(
                        path.strip_prefix(base).unwrap().to_path_buf(),
                        (true, mode, 0, vec![]),
                    );
                    visit(base, &path, rows);
                } else {
                    let mut hasher = Sha256::default();
                    let mut file = fs::File::open(&path).unwrap();
                    let mut buffer = [0u8; 64 * 1024];
                    loop {
                        let n = file.read(&mut buffer).unwrap();
                        if n == 0 {
                            break;
                        }
                        hasher.update(&buffer[..n]);
                    }
                    rows.insert(
                        path.strip_prefix(base).unwrap().to_path_buf(),
                        (
                            false,
                            mode,
                            metadata.len(),
                            hasher.finalize().1.as_ref().to_vec(),
                        ),
                    );
                }
            }
        }
        let mut rows = BTreeMap::new();
        visit(root, root, &mut rows);
        rows
    }

    fn copy_entries(source: &Path, destination: &Path) {
        assert!(source.is_dir());
        assert!(!destination.exists());
        fs::create_dir_all(destination).unwrap();
        for entry in fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            let from = entry.path();
            let to = destination.join(entry.file_name());
            let metadata = fs::symlink_metadata(&from).unwrap();
            assert!(!metadata.file_type().is_symlink());
            if metadata.is_dir() {
                copy_entries(&from, &to);
            } else {
                fs::copy(&from, &to).unwrap();
                fs::set_permissions(&to, metadata.permissions()).unwrap();
            }
        }
        fs::set_permissions(destination, fs::metadata(source).unwrap().permissions()).unwrap();
    }

    fn copy_tree(source: &Path, destination: &Path) {
        copy_entries(source, destination);
        // Hash each native file once per side, not again at every parent directory.
        assert_eq!(inventory(source), inventory(destination));
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
}
