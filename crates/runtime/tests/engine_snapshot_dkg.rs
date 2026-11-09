#![cfg(feature = "snapshot-integration")]

#[cfg(feature = "snapshot-integration")]
mod copied_native_dkg_prerequisites {
    use alloy_consensus::Sealable;
    use alloy_primitives::{Address, U256};
    use commonware_codec::Encode as _;
    use commonware_consensus::types::Epoch;
    use commonware_cryptography::bls12381::{
        self,
        dkg::feldman_desmedt::Output,
        primitives::{group::Share, sharing::Sharing, variant::MinSig},
    };
    use commonware_cryptography::Signer as _;
    use commonware_utils::{ordered::Set, TryCollect as _};
    use eyre::{ensure, WrapErr as _};
    use futures::FutureExt as _;
    use outbe_consensus::bls;
    use outbe_consensus::dkg_manager;
    use outbe_engine::stack::snapshot_native::{
        load_pending_dkg_boundary, open_native, refresh_validator_set_at_height,
        remove_native_header, replace_native_header, restore_pending_dkg_activation,
        save_pending_dkg_boundary, save_pending_dkg_state, DiskFixture, DkgStateMaterial,
        DkgStateStore, FrozenValidatorSetRefresh, PendingDkgBoundarySnapshot,
        RestoredPendingDkgActivation, H,
    };
    use outbe_engine::validators;
    use outbe_evm::OutbeEvmConfig;
    use outbe_node::{OutbeBeaconConsensus, OutbeFullNode, OutbeNode, OutbePoolBuilder};
    use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
    use outbe_primitives::OutbeHeader;
    use reth_ethereum::{
        network::NetworkManager,
        node::core::{args::DatadirArgs, primitives::Head},
        provider::db::{
            database::Database,
            init_db,
            mdbx::DatabaseArguments,
            tables,
            transaction::{DbTx, DbTxMut},
            DatabaseEnv,
        },
        rpc::builder::auth::AuthServerHandle,
        tasks::Runtime,
    };
    use reth_node_builder::{
        common::WithConfigs,
        components::{Components, NoopPayloadServiceBuilder, PayloadServiceBuilder, PoolBuilder},
        AddOnsContext, BuilderContext, ConsensusEngineHandle, FullNode, Node, NodeAdapter,
        NodeConfig, RethFullAdapter,
    };
    use reth_provider::{BlockHashReader, ChainSpecProvider, HeaderProvider, StateProviderFactory};
    use std::{
        collections::{BTreeMap, BTreeSet},
        panic::AssertUnwindSafe,
        path::Path,
        sync::Arc,
        time::Duration,
    };

    type Native = RethFullAdapter<DatabaseEnv, OutbeNode>;
    type AddOns = <OutbeNode as Node<Native>>::AddOns;
    const FREEZE: u64 = 1;
    const CHANGE: u64 = 2;
    const OWNER: Address = Address::repeat_byte(0xa0);
    const MEMBERS: [Address; 3] = [
        Address::with_last_byte(0x11),
        Address::with_last_byte(0x22),
        Address::with_last_byte(0x33),
    ];
    const JOINER: Address = Address::with_last_byte(0x44);

    fn register_active(
        provider: &mut HashMapStorageProvider,
        address: Address,
        key: &bls12381::PublicKey,
    ) {
        let encoded = key.encode();
        let bytes: [u8; 48] = encoded.as_ref().try_into().unwrap();
        StorageHandle::enter(provider, |storage| {
            let mut contract = outbe_validatorset::contract::ValidatorSet::new(storage);
            let minimum = U256::from(1_000u64);
            contract.test_seed_authoritative_min_stake(minimum).unwrap();
            contract.register_validator(OWNER, address, &bytes).unwrap();
            contract
                .test_seed_authoritative_bonded(address, minimum)
                .unwrap();
            contract
                .activate_validator_via_boundary_for_test(address)
                .unwrap();
        });
    }

    fn seed_freeze_history(root: &Path, keys: &[bls12381::PrivateKey]) -> BTreeSet<Address> {
        let mut contract = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut contract, |storage| {
            let mut validators = outbe_validatorset::contract::ValidatorSet::new(storage);
            validators.config_owner.write(OWNER).unwrap();
            validators.config_is_initialized.write(true).unwrap();
            validators.set_config_max_validators(128).unwrap();
        });
        for (&address, key) in MEMBERS.iter().zip(keys) {
            register_active(&mut contract, address, &key.public_key());
        }
        let frozen = contract.storage.clone();
        contract.set_block_number(CHANGE);
        StorageHandle::enter(&mut contract, |storage| {
            outbe_validatorset::contract::ValidatorSet::new(storage)
                .deactivate_validator(OWNER, MEMBERS[0])
                .unwrap();
        });
        register_active(
            &mut contract,
            JOINER,
            &bls12381::PrivateKey::from_seed(44).public_key(),
        );
        let current = contract.storage;
        outbe_engine::stack::snapshot_native::validator_history::persist(
            root,
            outbe_engine::stack::snapshot_native::validator_history::Snapshots {
                before: &frozen,
                after: &current,
                change_height: CHANGE,
            },
            |changed_addresses| {
                assert!(!changed_addresses.is_empty());
            },
        )
        .expect("native freeze validator history")
    }

    fn offline_component_config(
        root: &Path,
        chain: Arc<reth_ethereum::chainspec::ChainSpec<OutbeHeader>>,
    ) -> eyre::Result<NodeConfig<reth_ethereum::chainspec::ChainSpec<OutbeHeader>>> {
        let mut config = NodeConfig::new(chain)
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
        std::fs::create_dir_all(config.datadir().data_dir())?;
        Ok(config)
    }

    // All services belong to this invocation. This function installs the copied
    // provider initially. It starts no execution, peer loop, consensus engine or
    // process.
    fn with_native_components(
        root: &Path,
        check: impl FnOnce(&OutbeFullNode) -> eyre::Result<()>,
    ) -> eyre::Result<()> {
        let executor = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(2)
            .enable_all()
            .build()?;
        let outcome = executor.block_on(async {
            let runtime = Runtime::test();
            let manager = runtime
                .take_task_manager_handle()
                .expect("fixture task manager");
            let mut payload_shutdown = None;
            let result = AssertUnwindSafe(async {
                let provider = open_native(root);
                let chain = provider.chain_spec();
                let header = provider
                    .sealed_header(H)?
                    .ok_or_else(|| eyre::eyre!("missing copied head"))?;
                let config = offline_component_config(root, chain.clone())?;
                let head = Head {
                    number: H,
                    hash: header.hash(),
                    difficulty: header.header().inner.difficulty,
                    total_difficulty: U256::ZERO,
                    timestamp: header.header().inner.timestamp,
                };
                let ctx = BuilderContext::<Native>::new(
                    head,
                    provider.clone(),
                    runtime.clone(),
                    WithConfigs {
                        config: config.clone(),
                        toml_config: Default::default(),
                    },
                );
                let evm = OutbeEvmConfig::new(chain.clone(), outbe_evm::stub_host());
                let pool = OutbePoolBuilder::default()
                    .build_pool(&ctx, evm.clone())
                    .await?;
                let network_config = ctx.build_network_config(
                    ctx.network_config_builder()?
                        .disable_discovery()
                        .disable_nat()
                        .listener_addr(([127, 0, 0, 1], 0).into()),
                );
                let network_owner = NetworkManager::builder(network_config).await?;
                let network = network_owner.handle();
                let payload = NoopPayloadServiceBuilder::default()
                    .spawn_payload_builder_service(&ctx, pool.clone(), evm.clone())
                    .await?;
                // The builder retains every subscription sender until the service
                // exits. Closure of this receiver witnesses its shutdown.
                payload_shutdown = Some(
                    tokio::time::timeout(Duration::from_secs(10), payload.subscribe()).await??,
                );
                let adapter: NodeAdapter<Native> = NodeAdapter {
                    components: Components {
                        transaction_pool: pool.clone(),
                        evm_config: evm.clone(),
                        consensus: Arc::new(OutbeBeaconConsensus::new(chain)),
                        network: network.clone(),
                        payload_builder_handle: payload.clone(),
                    },
                    task_executor: runtime.clone(),
                    provider: provider.clone(),
                };
                let (engine_tx, _engine_rx) = tokio::sync::mpsc::unbounded_channel();
                let addons = AddOns::default()
                    .launch_add_ons_with_opt_engine(
                        AddOnsContext {
                            node: adapter,
                            config: &config,
                            beacon_engine_handle: ConsensusEngineHandle::new(engine_tx),
                            engine_events: Default::default(),
                            jwt_secret: *AuthServerHandle::noop().jwt_secret(),
                            sender_recovery_cache: None,
                        },
                        |_| Ok(()),
                        true,
                    )
                    .await?;
                ensure!(addons.rpc_server_handles.rpc.http_local_addr().is_none());
                ensure!(addons.rpc_server_handles.rpc.ws_local_addr().is_none());
                ensure!(addons.rpc_server_handles.rpc.ipc_endpoint().is_none());
                let node: OutbeFullNode = FullNode {
                    evm_config: evm,
                    pool,
                    network,
                    provider,
                    payload_builder_handle: payload,
                    task_executor: runtime.clone(),
                    data_dir: config.datadir(),
                    config,
                    add_ons_handle: addons,
                };
                let result = check(&node);
                drop(node);
                drop(network_owner);
                result
            })
            .catch_unwind()
            .await;
            // Always signal and observe shutdown, including assertion panics or
            // partially constructed components. The outer runtime is then dropped
            // before callers may mutate/reopen any native database.
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
                manager_result.wrap_err("fixture task manager shutdown timed out")???;
                ensure!(
                    payload_result.wrap_err("fixture payload service shutdown timed out")?,
                    "unexpected payload event"
                );
                ensure!(
                    graceful.wrap_err("fixture shutdown waiter panicked")?,
                    "fixture graceful tasks did not stop"
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

    fn run_test_dkg_complete() -> (
        Vec<bls12381::PrivateKey>,
        Set<bls12381::PublicKey>,
        Output<MinSig, bls12381::PublicKey>,
        Share,
        Sharing<MinSig>,
    ) {
        use commonware_cryptography::bls12381::dkg::feldman_desmedt::{Dealer, Info, Player};
        use commonware_cryptography::bls12381::primitives::sharing::Mode;
        use commonware_math::algebra::Random as _;
        use commonware_parallel::Sequential;
        use commonware_utils::N3f1;

        let mut keys: Vec<bls12381::PrivateKey> = (0..3)
            .map(|_| {
                bls12381::PrivateKey::random(rand_core_commonware::UnwrapErr(
                    rand_commonware::rngs::SysRng,
                ))
            })
            .collect();
        keys.sort_by(|a, b| {
            commonware_codec::Encode::encode(&a.public_key())
                .cmp(&commonware_codec::Encode::encode(&b.public_key()))
        });
        let participants: Set<bls12381::PublicKey> =
            keys.iter().map(|k| k.public_key()).try_collect().unwrap();
        let info = Info::<MinSig, bls12381::PublicKey>::new::<N3f1>(
            b"test",
            0,
            None,
            Mode::NonZeroCounter,
            commonware_cryptography::bls12381::dkg::feldman_desmedt::Reveal::V1,
            participants.clone(),
            participants.clone(),
        )
        .unwrap();
        let mut dealers = Vec::new();
        let mut pub_msgs = Vec::new();
        let mut all_priv_msgs = Vec::new();
        for key in &keys {
            let (dealer, pub_msg, priv_msgs) =
                Dealer::<MinSig, bls12381::PrivateKey>::start::<N3f1>(
                    rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng),
                    info.clone(),
                    key.clone(),
                    None,
                )
                .unwrap();
            dealers.push(dealer);
            pub_msgs.push(pub_msg);
            all_priv_msgs.push(priv_msgs);
        }
        let mut players: Vec<Player<MinSig, bls12381::PrivateKey>> = keys
            .iter()
            .map(|k| Player::new(info.clone(), k.clone()).unwrap())
            .collect();
        outbe_consensus::test_harness::acknowledge_fixture_dealings(
            &keys,
            outbe_consensus::test_harness::FixtureDealings {
                public_messages: &pub_msgs,
                private_messages: &all_priv_msgs,
            },
            &mut dealers,
            &mut players,
        );
        let mut logs = BTreeMap::new();
        for dealer in dealers {
            let signed_log = dealer.finalize::<N3f1>();
            if let Some((pk, log)) = signed_log.check(&info) {
                logs.insert(pk, log);
            }
        }
        let mut dkg_logs = commonware_cryptography::bls12381::dkg::feldman_desmedt::Logs::<
            MinSig,
            bls12381::PublicKey,
            N3f1,
        >::new(info.clone());
        for (dealer_pk, log) in logs {
            dkg_logs.record(dealer_pk, log);
        }
        let (output, share) = players
            .remove(0)
            .finalize::<N3f1, commonware_cryptography::bls12381::Batch>(
                &mut rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng),
                dkg_logs,
                &Sequential,
            )
            .unwrap();
        let polynomial = output.public().clone();
        (keys, participants, output, share, polynomial)
    }

    struct PendingFixture {
        disk: DiskFixture,
        snapshot: PendingDkgBoundarySnapshot,
        keys: Vec<bls12381::PrivateKey>,
        participants: Set<bls12381::PublicKey>,
        output: Output<MinSig, bls12381::PublicKey>,
        share: Share,
        polynomial: Sharing<MinSig>,
        changed_addresses: BTreeSet<Address>,
    }

    impl PendingFixture {
        fn new() -> Self {
            let disk = DiskFixture::new(H - 1);
            let (keys, participants, output, share, polynomial) = run_test_dkg_complete();
            let changed_addresses = seed_freeze_history(disk.root.path(), &keys);
            let validator_set = {
                let provider = open_native(disk.root.path());
                let state = provider
                    .state_by_block_hash(disk.headers[FREEZE as usize].hash_slow())
                    .unwrap();
                let frozen = validators::read_reshare_target_with_empty_tee_exclusions_from_state(
                    &outbe_runtime::OutbeRuntime::new(),
                    &state,
                )
                .unwrap();
                assert_eq!(frozen.validator_set.addresses, MEMBERS);
                frozen.validator_set
            };
            let artifact =
                dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
                    epoch: Epoch::new(1),
                    validator_set: &validator_set,
                    output: &output,
                    is_full_dkg: false,
                    dkg_cycle: 1,
                    freeze_height: FREEZE,
                    planned_activation_height: H + 1,
                    vrf_material_version: 1,
                    is_validator_set_change: false,
                    tee_expired_target_exclusions: Vec::new(),
                })
                .unwrap();
            let snapshot = PendingDkgBoundarySnapshot {
                artifact,
                completed_at_height: CHANGE,
            };
            Self {
                disk,
                snapshot,
                keys,
                participants,
                output,
                share,
                polynomial,
                changed_addresses,
            }
        }

        fn copy_for_same_participant(&self, recipient: &Path) {
            self.disk.copy_to(recipient);
            // The private share is this recipient's own ceremony output. It is
            // provisioned independently of the copied public native chain files.
            let own = recipient.join("own-dkg");
            std::fs::create_dir_all(&own).unwrap();
            save_pending_dkg_state(
                DkgStateStore::new(&own, &bls::KeyBackend::Plaintext),
                DkgStateMaterial {
                    share: &self.share,
                    polynomial: &self.polynomial,
                    output: &self.output,
                },
            )
            .unwrap();
            save_pending_dkg_boundary(&own, &self.snapshot).unwrap();
        }

        fn assert_restored(&self, root: &Path, node: &OutbeFullNode) -> eyre::Result<()> {
            let refreshed =
                refresh_validator_set_at_height(&outbe_runtime::OutbeRuntime::new(), node, FREEZE)?;
            let FrozenValidatorSetRefresh::Ready {
                validator_set,
                participants,
                tee_expired_target_exclusions,
            } = refreshed
            else {
                eyre::bail!("present native freeze state is unavailable");
            };
            ensure!(validator_set.addresses == MEMBERS);
            ensure!(participants == self.participants);
            ensure!(tee_expired_target_exclusions.is_empty());
            let current_state = node
                .provider
                .state_by_block_hash(self.disk.headers[H as usize].hash_slow())?;
            let current = validators::read_reshare_target_with_empty_tee_exclusions_from_state(
                &outbe_runtime::OutbeRuntime::new(),
                &current_state,
            )?;
            ensure!(current.validator_set.addresses == vec![MEMBERS[1], MEMBERS[2], JOINER]);
            let own = root.join("own-dkg");
            let snapshot = load_pending_dkg_boundary(&own)?.expect("durable pending boundary");
            let restored = restore_pending_dkg_activation(
                (&outbe_runtime::OutbeRuntime::new(), node),
                snapshot,
                &own,
                &bls::KeyBackend::Plaintext,
                &self.keys[0].public_key(),
            )?;
            let RestoredPendingDkgActivation::Participant(restored) = restored else {
                eyre::bail!("local participant was incorrectly restored as dealer-only");
            };
            ensure!(restored.target.freeze_height == FREEZE);
            ensure!(restored.target.validator_set.addresses == MEMBERS);
            ensure!(restored.target.participants == self.participants);
            ensure!(restored.complete.output == self.output);
            ensure!(restored.complete.share == self.share);
            ensure!(restored.boundary_artifact == self.snapshot.artifact);
            ensure!(restored.recovered_output.as_ref() == Some(&self.output));
            Ok(())
        }
    }

    fn pending_files(root: &Path) -> BTreeMap<String, Vec<u8>> {
        std::fs::read_dir(root.join("own-dkg"))
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                assert!(entry.file_type().unwrap().is_file());
                (
                    entry.file_name().into_string().unwrap(),
                    std::fs::read(entry.path()).unwrap(),
                )
            })
            .collect()
    }

    #[test]
    fn copied_native_dkg_restore_uses_freeze_history_and_recipient_pending_share_on_each_reopen() {
        let fixture = PendingFixture::new();
        let recipient = tempfile::tempdir().unwrap();
        fixture.copy_for_same_participant(recipient.path());
        let before = pending_files(recipient.path());
        for _ in 0..2 {
            with_native_components(recipient.path(), |node| {
                fixture.assert_restored(recipient.path(), node)
            })
            .unwrap();
            assert_eq!(pending_files(recipient.path()), before);
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum MissingPrerequisite {
        HeaderSegment,
        CorruptHeaderNumber,
        StorageChangeset,
    }

    fn damage_prerequisite(fixture: &PendingFixture, root: &Path, missing: MissingPrerequisite) {
        match missing {
            MissingPrerequisite::HeaderSegment => remove_native_header(root, FREEZE),
            MissingPrerequisite::CorruptHeaderNumber => {
                let mut invalid = fixture.disk.headers[FREEZE as usize].clone();
                invalid.inner.number = FREEZE + 1;
                replace_native_header(
                    root,
                    FREEZE,
                    &invalid,
                    fixture.disk.headers[FREEZE as usize].hash_slow(),
                );
            }
            MissingPrerequisite::StorageChangeset => {
                let db = init_db(root.join("db"), DatabaseArguments::test()).unwrap();
                let tx = db.tx_mut().unwrap();
                for &address in &fixture.changed_addresses {
                    assert!(tx
                        .delete::<tables::StorageChangeSets>((CHANGE, address).into(), None)
                        .unwrap());
                }
                tx.commit().unwrap();
            }
        }
    }

    fn assert_freeze_refresh_rejected(
        node: &OutbeFullNode,
        missing: MissingPrerequisite,
    ) -> eyre::Result<()> {
        match missing {
            MissingPrerequisite::HeaderSegment => {
                ensure!(matches!(
                    refresh_validator_set_at_height(
                        &outbe_runtime::OutbeRuntime::new(),
                        node,
                        FREEZE
                    )?,
                    FrozenValidatorSetRefresh::PendingBlockHash
                ));
            }
            MissingPrerequisite::CorruptHeaderNumber => {
                let error = refresh_validator_set_at_height(
                    &outbe_runtime::OutbeRuntime::new(),
                    node,
                    FREEZE,
                )
                .err()
                .expect("native invalid freeze header must fail");
                ensure!(format!("{error:#}").contains("canonical freeze header number mismatch"));
            }
            MissingPrerequisite::StorageChangeset => {
                let error = refresh_validator_set_at_height(
                    &outbe_runtime::OutbeRuntime::new(),
                    node,
                    FREEZE,
                )
                .err()
                .expect("native missing history must fail");
                ensure!(format!("{error:#}").contains("storage change set"));
            }
        }
        Ok(())
    }

    fn assert_missing_prerequisite_rejected(
        fixture: &PendingFixture,
        root: &Path,
        node: &OutbeFullNode,
        missing: MissingPrerequisite,
    ) -> eyre::Result<()> {
        ensure!(node.provider.block_hash(H)? == Some(fixture.disk.headers[H as usize].hash_slow()));
        let current_state = node
            .provider
            .state_by_block_hash(fixture.disk.headers[H as usize].hash_slow())?;
        let current = validators::read_reshare_target_with_empty_tee_exclusions_from_state(
            &outbe_runtime::OutbeRuntime::new(),
            &current_state,
        )?;
        ensure!(current.validator_set.addresses == vec![MEMBERS[1], MEMBERS[2], JOINER]);
        assert_freeze_refresh_rejected(node, missing)?;
        let own = root.join("own-dkg");
        let snapshot = load_pending_dkg_boundary(&own)?.expect("pending boundary still exists");
        let error = restore_pending_dkg_activation(
            (&outbe_runtime::OutbeRuntime::new(), node),
            snapshot,
            &own,
            &bls::KeyBackend::Plaintext,
            &fixture.keys[0].public_key(),
        )
        .err()
        .expect("ordinary restore must reject missing prerequisite");
        let message = format!("{error:#}");
        match missing {
            MissingPrerequisite::HeaderSegment => {
                ensure!(message.contains("pending DKG freeze-height state unavailable at height 1 during runtime restore"), "{message}");
            }
            MissingPrerequisite::CorruptHeaderNumber => {
                ensure!(
                    message.contains("canonical freeze header number mismatch"),
                    "{message}"
                );
            }
            MissingPrerequisite::StorageChangeset => {
                ensure!(
                    message.contains("storage change set")
                        && message.contains("at block #2 does not exist"),
                    "{message}"
                );
            }
        }
        Ok(())
    }

    fn assert_prerequisite_still_damaged(
        fixture: &PendingFixture,
        root: &Path,
        missing: MissingPrerequisite,
    ) {
        let provider = open_native(root);

        assert_eq!(
            provider.block_hash(H).unwrap(),
            Some(fixture.disk.headers[H as usize].hash_slow())
        );
        // Ordinary failure must not reconstruct the damaged prerequisite.
        match missing {
            MissingPrerequisite::HeaderSegment => {
                assert!(provider.block_hash(FREEZE).unwrap().is_none());
                assert!(provider.sealed_header(FREEZE).unwrap().is_none());
            }
            MissingPrerequisite::CorruptHeaderNumber => {
                let header = provider.sealed_header(FREEZE).unwrap().unwrap();
                assert_eq!(header.header().inner.number, FREEZE + 1);
                assert_eq!(
                    header.hash(),
                    fixture.disk.headers[FREEZE as usize].hash_slow()
                );
            }
            MissingPrerequisite::StorageChangeset => {
                drop(provider);
                let db = init_db(root.join("db"), DatabaseArguments::test()).unwrap();
                let tx = db.tx().unwrap();
                for &address in &fixture.changed_addresses {
                    assert!(tx
                        .get::<tables::StorageChangeSets>((CHANGE, address).into())
                        .unwrap()
                        .is_none());
                }
            }
        }
    }

    #[test]
    fn copied_native_dkg_restore_rejects_missing_freeze_segment_corrupt_header_or_required_history()
    {
        let fixture = PendingFixture::new();
        for missing in [
            MissingPrerequisite::HeaderSegment,
            MissingPrerequisite::CorruptHeaderNumber,
            MissingPrerequisite::StorageChangeset,
        ] {
            let recipient = tempfile::tempdir().unwrap();
            fixture.copy_for_same_participant(recipient.path());
            with_native_components(recipient.path(), |node| {
                fixture.assert_restored(recipient.path(), node)
            })
            .unwrap();
            let before = pending_files(recipient.path());
            damage_prerequisite(&fixture, recipient.path(), missing);
            with_native_components(recipient.path(), |node| {
                assert_missing_prerequisite_rejected(&fixture, recipient.path(), node, missing)
            })
            .unwrap();
            assert_eq!(
                pending_files(recipient.path()),
                before,
                "{missing:?} changed pending material"
            );
            assert_prerequisite_still_damaged(&fixture, recipient.path(), missing);
        }
    }
}
