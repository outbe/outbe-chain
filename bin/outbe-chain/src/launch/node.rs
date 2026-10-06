use crate::*;
mod consensus;
mod ocomp;
mod radicle;
mod storage;
mod tee;
use storage::StorageExitGuard;
type LaunchConfig = reth_node_builder::NodeConfig<ChainSpec<OutbeHeader>>;

/// Run the main node (Reth execution + Commonware consensus).
pub(crate) fn run_node() -> eyre::Result<()> {
    // TEE offer decryption routes exclusively through the enclave sidecar
    // (`--tee-enclave-socket` -> persistent production NodeHost authorization).
    // The offer-decryption key exists only inside the enclave (single path, no
    // in-process key material).

    // Pool lifetime hardening. Must run BEFORE CLI parsing: clap reads these as
    // its own defaults, so explicit `--txpool.*` flags still win.
    let _ = outbe_default_txpool_values().try_init();
    let _ = outbe_default_rpc_values().try_init();

    let mut cli = Cli::<OutbeChainSpecParser, ConsensusArgs, OutbeRpcModuleValidator>::parse();
    apply_outbe_gas_price_oracle_defaults(&mut cli.command);

    // Initialize the hash-pinned Barretenberg global CRS before block
    // execution. Tribute admission is consensus-critical, so a node that
    // cannot initialize the verifier must not start. Database and other
    // operator commands never execute proofs and must remain offline.
    // This still runs before `Cli::run` creates the Tokio runtime because
    // `setup_srs` uses `reqwest::blocking` internally.
    initialize_crs_for_command(&cli.command, || {
        let srs_path = std::env::var("OUTBE_BB_SRS_PATH").ok();
        outbe_zk_backend::barretenberg::init_crs().map_err(eyre::Report::from)?;
        tracing::info!(
            num_points = outbe_zk_backend::barretenberg::CANONICAL_SRS_POINTS,
            path = ?srs_path,
            "Barretenberg SRS initialized"
        );
        Ok(())
    })?;

    let bridge = ConsensusExecutionBridge::new();

    // Channels for validator-mode consensus thread.
    // For full-node mode, no thread is spawned and these are unused.
    let (node_tx, node_rx) = oneshot::channel::<consensus::ConsensusLaunch>();
    let (consensus_dead_tx, mut consensus_dead_rx) = oneshot::channel::<()>();
    let shutdown_token = tokio_util::sync::CancellationToken::new();
    // A terminal protocol outcome must drain Radicle without racing the main
    // signal branch into aborting the stack before it returns its primary error.
    let radicle_shutdown_token = shutdown_token.child_token();

    // The launcher spawns the consensus thread conditionally. See inside
    // run_with_components, where `args.is_validator` is known. For now, prepare the closure.
    let shutdown_token_clone = shutdown_token.clone();
    let radicle_shutdown_for_consensus = radicle_shutdown_token.clone();
    let bridge_for_consensus = bridge.clone();
    let consensus_thread_fn = move || {
        consensus::run(
            node_rx,
            consensus_dead_tx,
            shutdown_token_clone,
            radicle_shutdown_for_consensus,
            bridge_for_consensus,
        )
    };

    // Thread 1 (main): Reth execution layer.
    let bridge_for_evm = bridge.clone();
    let components = move |spec: Arc<ChainSpec<OutbeHeader>>| {
        let fork_install =
            outbe_node::ocomp::fork::require_startup_ocomp_fork_install(spec.as_ref())
                .expect("chain spec parser validated OCOMP fork install");
        let activation = outbe_primitives::system_tx::OcompLifecycleActivation::at_block(
            fork_install.activation_height,
        );
        let mut evm =
            outbe_evm::OutbeEvmConfig::new_with_bridge(spec.clone(), bridge_for_evm.clone())
                .with_ocomp_lifecycle_activation(activation);
        evm = evm.with_ocomp_fork_install(fork_install);
        (
            evm,
            Arc::new(
                OutbeBeaconConsensus::new(spec)
                    .with_max_extra_data_size(outbe_node::consensus::OUTBE_MAX_EXTRA_DATA_SIZE)
                    .with_ocomp_lifecycle_activation(activation),
            ),
        )
    };

    // This owner outlives cancellation of the launcher and Reth runtime teardown.
    let process_shutdown = outbe_node::shutdown::NodeShutdown::default();
    let launcher_shutdown = process_shutdown.clone();
    let offchain_close = StorageExitGuard::default();
    let offchain_close_registration = offchain_close.observer();
    // Preserve the pool overrides normally applied by Reth's default CLI runner.
    let runtime_config = match &cli.command {
        reth_ethereum::cli::interface::Commands::Node(command) => {
            reth_ethereum::tasks::RuntimeConfig::default().with_rayon(
                reth_ethereum::tasks::RayonConfig {
                    reserved_cpu_cores: command.engine.reserved_cpu_cores,
                    proof_storage_worker_threads: command.engine.storage_worker_count,
                    proof_account_worker_threads: command.engine.account_worker_count,
                    prewarming_threads: command.engine.prewarming_threads,
                    ..Default::default()
                },
            )
        }
        _ => reth_ethereum::tasks::RuntimeConfig::default(),
    };
    let command_result = execution_runtime::run_with_execution_runtime(runtime_config, |runner| {
    cli.with_runner_and_components::<OutbeNode>(runner, components, async move |builder, args| {
        let shutdown = launcher_shutdown.clone();
        let result: eyre::Result<()> = async move {
        let _cancel_on_launcher_drop = shutdown_token.clone().drop_guard();
        args.validate()?;
        let (radicle_preflight, radicle_status) = radicle::preflight(&args).await?;
        info!(
            target: "outbe::protocol",
            formingPeriodSeconds = outbe_chain_constants::get_metadosis_forming_period_seconds(),
            lookbackDelaySeconds = outbe_chain_constants::get_metadosis_lookback_delay_seconds(),
            offeringPeriodSeconds = outbe_chain_constants::get_metadosis_offering_period_seconds(),
            waitingPeriodSeconds = outbe_chain_constants::get_metadosis_waiting_period_seconds(),
            advanceIntervalSeconds =
                outbe_chain_constants::get_metadosis_advance_interval_seconds(),
            "effective genesis protocol parameters"
        );
        let tee_attestation_v1 =
            outbe_evm::tee_attestation_activation::TeeAttestationChainSpecStateV1::from_chain_spec(
                builder.config().chain.as_ref(),
            );
        let tee_activation = tee_attestation_v1.activation().map_err(|error| {
            eyre::eyre!("invalid mandatory teeAttestationV1 ChainSpec: {error}")
        })?;
        let initial_tee_policy = tee_activation
            .policy_at(outbe_evm::tee_attestation_activation::TEE_ATTESTATION_V1_ACTIVATION_HEIGHT)
            .map_err(eyre::Report::msg)?;
        let dkg_prepare_window_blocks = builder
            .config()
            .chain
            .genesis
            .config
            .extra_fields
            .get_deserialized::<u64>("dkgPrepareWindowBlocks")
            .transpose()
            .map_err(|error| eyre::eyre!("invalid dkgPrepareWindowBlocks: {error}"))?
            .unwrap_or(outbe_consensus::config::DEFAULT_DKG_PREPARE_WINDOW_BLOCKS);
        let minimum_block_time_millis = builder
            .config()
            .chain
            .genesis
            .config
            .extra_fields
            .get_deserialized::<u64>("minBlockTimeMs")
            .transpose()
            .map_err(|error| eyre::eyre!("invalid minBlockTimeMs: {error}"))?
            .unwrap_or(outbe_consensus::timing::DEFAULT_MIN_BLOCK_TIME_MS);
        info!(
            attestation_mode = ?initial_tee_policy.attestation_mode,
            activation_height = tee_activation.manifest.activation_height,
            policy_schedule_hash = %tee_activation.manifest.policy_schedule_hash,
            "validated mandatory TEE attestation ChainSpec authority"
        );
        let ocomp_fork_install =
            outbe_node::ocomp::fork::require_startup_ocomp_fork_install(
                builder.config().chain.as_ref(),
            )?;
        let ocomp_limits = outbe_ocomp_protocol::profile::poc_schema_limits();
        let ocomp_install_hash = ocomp_fork_install.install_hash(&ocomp_limits)?;
        info!(
            activation_height = ocomp_fork_install.activation_height,
            classification = ?ocomp_fork_install.classification,
            install_hash = %ocomp_install_hash,
            "validated genesis-active immutable OCOMP chain-manifest install"
        );

        let prune_config = builder
            .config()
            .pruning
            .prune_config(builder.config().chain.as_ref());
        validate_compressed_storage_runtime_config(CompressedStorageRuntimeConfig {
            persistence_threshold: builder.config().engine.persistence_threshold,
            memory_block_buffer_target: builder.config().engine.memory_block_buffer_target(),
            max_pending_acks: outbe_consensus::config::MAX_PENDING_ACKS,
            receipts_pruning_enabled: prune_config
                .as_ref()
                .is_some_and(|config| config.has_receipts_pruning()),
            account_history_pruning_enabled: prune_config
                .as_ref()
                .is_some_and(|config| config.segments.account_history.is_some()),
            storage_history_pruning_enabled: prune_config
                .as_ref()
                .is_some_and(|config| config.segments.storage_history.is_some()),
        })?;

        let node_data_dir = builder
            .config()
            .datadir
            .clone()
            .resolve_datadir(reth_ethereum::chainspec::EthChainSpec::chain(
                builder.config().chain.as_ref(),
            ))
            .data_dir()
            .to_path_buf();
        let ocomp::OcompBootstrap { config: ocomp_exex_config, retention_selector, readiness_publisher: ocomp_readiness_publisher, readiness: ocomp_readiness } =
            ocomp::prepare(builder.config(), &args, &node_data_dir, &ocomp_fork_install, ocomp_install_hash)?;
        let ocomp_readiness_for_consensus =
            args.upstream.is_some().then(|| ocomp_readiness.clone());
        let (ocomp_exit_tx, mut ocomp_exit_rx) = tokio::sync::mpsc::unbounded_channel();
        let (evm_signer, local_tee_identity, tee_admission_anchor) =
            tee::prepare(builder.config(), &args, node_data_dir.clone(), initial_tee_policy).await?;

        let offchain_data = args.offchain_data()?;
        validate_adr005_node_mode(args.is_validator, args.upstream.is_some())?;
        let projection_config = OffchainDataProjectionConfig {
            chain_id: builder.config().chain.chain().id(),
            genesis_hash: builder.config().chain.genesis_hash(),
            storage: outbe_offchain_storage::StorageConfig::load(offchain_data.storage_config)?,
        };
        let projection_retention_selector = Arc::clone(&retention_selector);
        let prepared_projection = tokio::task::spawn_blocking(move || {
            prepare_offchain_data_projection_with_retention(
                projection_config,
                projection_retention_selector,
                offchain_close_registration,
            )
        })
        .await
        .wrap_err("offchain-data startup validation worker failed")??;
        let runtime_body_readers = prepared_projection.runtime_body_readers();
        let proof_body_readers = runtime_body_readers.clone();
        let proof_chain_id = builder.config().chain.chain().id();
        let projection_readiness = prepared_projection.readiness();
        let retained_tribute_writer = prepared_projection.retained_tribute_writer();
        let projection_retention_fence = prepared_projection.retention_fence();
        let ce_data_dir = builder
            .config()
            .datadir
            .clone()
            .resolve_datadir(reth_ethereum::chainspec::EthChainSpec::chain(
                builder.config().chain.as_ref(),
            ))
            .data_dir()
            .to_path_buf();
        let genesis_hash = builder.config().chain.genesis_hash();
        let ce_identity = EnvironmentIdentity {
            local_storage_schema_version: LOCAL_STORAGE_SCHEMA_VERSION,
            chain_id: builder.config().chain.chain().id(),
            genesis_hash,
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            topology: outbe_compressed_entities::CeTopologyV1.encode(),
            tree_format: "ckb-smt-v0.6.1-poseidon-catalog-v3".to_owned(),
            vendor_revision: "ad555350c866b2265d87d2d7fbd146fbc918bfe5".to_owned(),
        };
        let genesis_marker = FinalizedMarker {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            height: 0,
            block_hash: genesis_hash,
            parent_block_hash: Default::default(),
            parent_root: Default::default(),
            new_root: outbe_compressed_entities::sealed_root(Default::default())?,
        };
        let ce_db = CeMdbx::open(&ce_data_dir, ce_identity, genesis_marker)
            .wrap_err("failed to open and validate compressed-entity MDBX")?;
        // ADR-009 fixes only the provisional sharded topology. Production CE
        // work/cache coefficients remain deliberately open until ADR-017.
        let compressed_tree_service = Arc::new(CompressedTreeService::new(
            ce_db,
            CandidateCacheLimits {
                max_candidates: usize::MAX,
                max_encoded_bytes: usize::MAX,
            },
        )?);
        compressed_tree_service.discard_speculative_candidates()?;
        let outbe_node = match evm_signer {
            Some(signer) => OutbeNode::with_bridge_and_evm_signer(
                bridge.clone(),
                signer,
                runtime_body_readers,
                compressed_tree_service.clone(),
            ),
            None => OutbeNode::with_bridge(
                bridge.clone(),
                runtime_body_readers,
                compressed_tree_service.clone(),
            ),
        };
        let outbe_node = outbe_node
            .with_ocomp_fork_install(ocomp_fork_install)
            .with_shutdown(shutdown.clone());
        let projection_readiness_for_rpc = projection_readiness.clone();
        let radicle_status_for_rpc = radicle_status.clone();
        // Canary-fed enclave health. The tee-canary worker (spawned after node launch)
        // publishes it, and `outbe_consensusStatus.enclave` reads it.
        let tee_canary_status = outbe_tee::TeeEnclaveHealthChannel::disabled();
        let tee_canary_status_for_rpc = tee_canary_status.clone();

        let NodeHandle {
            node,
            node_exit_future,
        } = builder
            .node(outbe_node)
            .install_exex("outbe-finalized", move |ctx| {
                let projection_provider = ctx.provider().clone();
                let config = ocomp_exex_config.clone();
                let readiness = ocomp_readiness_publisher.clone();
                let exit = ocomp_exit_tx.clone();
                async move {
                    let ready_projection = tokio::task::spawn_blocking(move || {
                        validate_offchain_data_checkpoint(
                            prepared_projection,
                            &projection_provider,
                        )
                    })
                    .await
                    .wrap_err("offchain-data checkpoint validation worker failed")??;
                    Ok(ocomp_exex::run_ocomp_exex(
                        ctx,
                        ready_projection,
                        config,
                        readiness,
                        exit,
                    ))
                }
            })
            .apply(|mut builder| {
                configure_outbe_engine_args(&mut builder.config_mut().engine);
                let discovery = &mut builder.config_mut().network.discovery;
                discovery.enable_discv5_discovery = true;
                // SSA-1: disable reth DNS discovery so the `hickory-proto` code
                // path (RUSTSEC-2025 NSEC3 unbounded-loop DoS, no upstream fix)
                // is unreachable. outbe peers via discv5 + static bootnodes and
                // configures no DNS ENR tree, so DNS discovery provided nothing
                // here anyway. Disabling it removes the attack surface.
                discovery.disable_dns_discovery = true;
                builder
            })
            .extend_rpc_modules({
                let bridge = bridge.clone();
                let is_validator = args.is_validator;
                let is_follower = args.upstream.is_some();
                let projection_readiness = projection_readiness_for_rpc.clone();
                let compressed_tree_service = compressed_tree_service.clone();
                let proof_body_readers = proof_body_readers.clone();
                let radicle_status = radicle_status_for_rpc.clone();
                let tee_enclave_health = tee_canary_status_for_rpc.clone();
                move |ctx| {
                    use outbe_rpc::OutbeApiServer as _;
                    let provider = Arc::new(ctx.provider().clone());
                    let context_provider = Arc::clone(&provider);
                    outbe_tee::call_context::install_provider(Arc::new(move || {
                        outbe_node::tee_call_context::read(
                            context_provider.as_ref(),
                            proof_chain_id,
                            genesis_hash,
                        )
                    }))
                    .map_err(eyre::Report::msg)?;
                    // Validators get the full bridge-backed handler.
                    // `--upstream` followers also run a marshal and CAN serve
                    // `outbe_getFinalization` (chaining followers), but must NOT
                    // report validator status. They get a follower-scoped handler
                    // that exposes only the finalization-serving capability.
                    let outbe_api = (if is_validator {
                        outbe_rpc::OutbeApiHandler::with_bridge(
                            Arc::clone(&provider),
                            bridge,
                            projection_readiness.clone(),
                        )
                    } else if is_follower {
                        outbe_rpc::OutbeApiHandler::with_follower_bridge(
                            Arc::clone(&provider),
                            bridge,
                            projection_readiness.clone(),
                        )
                    } else {
                        outbe_rpc::OutbeApiHandler::new(
                            Arc::clone(&provider),
                            projection_readiness.clone(),
                        )
                    })
                    .with_chain_identity(proof_chain_id, genesis_hash)
                    .with_point_reads(
                        compressed_tree_service.clone(),
                        proof_body_readers.clone(),
                        proof_chain_id,
                    )
                    .with_tee_renewal_schedule(
                        dkg_prepare_window_blocks,
                        minimum_block_time_millis,
                    )
                    .with_ocomp_lysis_openings(outbe_rpc::OcompLysisOpeningsRuntimeV1::new({
                        let provider = Arc::clone(&provider);
                        move |intent_id, canonical_request| {
                            let limits = outbe_ocomp_protocol::profile::poc_schema_limits();
                            let request = outbe_ocomp_protocol::control::BuildLysisOpeningsV1::decode_body(
                                canonical_request.as_ref(),
                                &limits,
                            )
                            .map_err(|error| format!("decode OCOMP openings request: {error}"))?;
                            let finalized_head = provider
                                .finalized_block_num_hash()
                                .map_err(|error| format!("read finalized head: {error}"))?
                                .ok_or_else(|| "finalized head is unavailable".to_owned())?;
                            let record = outbe_node::ocomp::retention::read_ocomp_job_record_at(
                                provider.as_ref(),
                                finalized_head.hash,
                                intent_id,
                                &limits,
                            )
                            .map_err(|error| format!("read finalized OCOMP job: {error}"))?;
                            let finalized = record
                                .finalized
                                .as_ref()
                                .ok_or_else(|| "OCOMP job is not finalized".to_owned())?;
                            if !ocomp_job_available_for_calculation(record.status) {
                                return Err(
                                    "OCOMP job is not available for calculation or replay"
                                        .to_owned(),
                                );
                            }
                            if request.job_id != finalized.job_id {
                                return Err("OCOMP openings request JobId mismatch".to_owned());
                            }
                            let candidate = outbe_node::ocomp::retention::CandidatePinV1 {
                                block_number: record.intent_height,
                                block_hash: finalized.finalized_request_block_hash,
                                state_root: finalized.finalized_request_state_root,
                                intent_id,
                                wwd: record.intent.wwd,
                                ce_sealed_root: record.intent.ce_sealed_root,
                                protocol_bundle_hash: record.intent.protocol_bundle_hash,
                                input_lease_id: record
                                    .intent
                                    .input_lease_id()
                                    .map_err(|error| format!("derive input lease: {error}"))?,
                            };
                            let openings = outbe_node::ocomp::build_lysis_openings(
                                provider.as_ref(),
                                &limits,
                                candidate,
                                request.subjects,
                            )
                            .map_err(|error| format!("build exact OCOMP openings: {error}"))?;
                            if openings.job_id != finalized.job_id {
                                return Err("OCOMP openings JobId mismatch".to_owned());
                            }
                            openings
                                .encode_body(&limits)
                                .map(alloy_primitives::Bytes::from)
                                .map_err(|error| format!("encode OCOMP openings: {error}"))
                        }
                    }))
                    .with_radicle_status(radicle_status.clone())
                    .with_tee_enclave_health(tee_enclave_health.clone());
                    ctx.modules.merge_if_module_configured(
                        RethRpcModule::Other("outbe".to_owned()),
                        outbe_api.into_rpc(),
                    )?;
                    info!("outbe_* RPC namespace registered where configured");
                    Ok(())
                }
            })
            .launch()
            .await
            .wrap_err("failed launching execution node")?;

        shutdown.observe_engine_exit(&node.task_executor, node_exit_future)?;

        let validator_has_recovery_anchor = args.is_validator && tee_admission_anchor.is_some();
        let mut tee_lease_guard_gate = TeeLeaseGuardGateV1::new(tee_admission_anchor);
        if let Some(admission) = read_gated_finalized_local_tee_admission(
            &node.provider,
            proof_chain_id,
            genesis_hash,
            local_tee_identity,
            &mut tee_lease_guard_gate,
        )?
        {
            let rejection = if validator_has_recovery_anchor {
                validator_recovery_startup_admission_rejection(admission)
            } else {
                tee_lease_admission_rejection(admission)
            };
            if let Some(reason) = rejection {
                eyre::bail!("local node rejected by finalized TEE lease state: {reason}");
            }
        }
        require_validator_tee_recovery_complete_v1(
            args.is_validator,
            tee_lease_guard_gate,
            &node_data_dir,
        )?;
        let (tee_lease_exit_tx, mut tee_lease_exit_rx) =
            tokio::sync::mpsc::unbounded_channel();
        let lease_check = run_tee_lease_guard_v1(
            node.provider.clone(),
            super::admission::TeeLeaseGuardConfigV1 {
                chain: outbe_node::tee_remote_session::RegistryChainIdentity {
                    chain_id: proof_chain_id,
                    genesis_hash,
                },
                identity: local_tee_identity,
                gate: tee_lease_guard_gate,
            },
            shutdown_token.clone(),
        );
        let lease_outcome = shutdown.clone();
        let tee_lease_guard_handle = tokio::spawn(shutdown.track_task("TEE lease guard", async move {
            let verdict = match lease_check.await {
                Ok(None) => return,
                Ok(Some(reason)) => Ok(reason),
                Err(error) => Err(error),
            };
            // Publish an actual failure before notifying a launcher that may
            // concurrently choose a different exit branch or be cancelled.
            let reason = tee_lease_exit_reason(Some(verdict), &lease_outcome);
            let _ = tee_lease_exit_tx.send(Ok(reason));
        }));

        let (radicle_consensus, radicle_observer, radicle_drained) = radicle::start(
            &node, &args, radicle_preflight, radicle_status.clone(),
            radicle_shutdown_token.clone(), shutdown.clone(),
        )?;

        // Periodic enclave canary (signal only): known-plaintext decrypt +
        // Health telemetry through the process-global session. `0` disables.
        let tee_canary_handle = (args.tee_canary_interval_secs > 0).then(|| {
            tokio::spawn(shutdown.track_task("TEE canary worker", outbe_node::tee_canary::run_tee_canary_worker(
                outbe_node::tee_canary::GlobalEnclaveRequester,
                outbe_node::tee_canary::TeeCanaryConfig {
                    interval: std::time::Duration::from_secs(args.tee_canary_interval_secs),
                    failure_threshold: args.tee_canary_failure_threshold,
                },
                tee_canary_status.clone(),
                shutdown_token.clone(),
            )))
        });
        // Pending staleness eviction. This is node-local pool policy, so it runs in
        // every mode. Full nodes are the public RPC ingress. They shed stuck
        // transactions that would otherwise be re-gossiped to validators.
        let txpool_maintenance_handle = tokio::spawn(shutdown.track_task("txpool maintenance task", outbe_txpool::maintain::maintain_outbe_pool(
            node.provider.clone(),
            node.pool.clone(),
            outbe_txpool::maintain::OutbePoolMaintainConfig {
                staleness_interval_secs: args.txpool_pending_staleness_secs,
            },
        )));
        let upgrade_promotion = Arc::new(tokio::sync::Notify::new());
        let upgrade_handle = if initial_tee_policy.attestation_mode
            == outbe_primitives::tee_attestation_v1::AttestationMode::DcapRequired
        {
            let provider = node.provider.clone();
            let promoted = upgrade_promotion.clone();
            Some(tokio::spawn(shutdown.track_task("enclave-upgrade watcher", run_upgrade_promotion_worker_v1(
                provider,
                UpgradePromotionWorkerConfigV1 {
                    chain_id: proof_chain_id,
                    genesis_hash,
                    node_data_dir: node_data_dir.clone(),
                    poll_secs: TEE_UPGRADE_POLL_SECS,
                    warning_blocks: TEE_UPGRADE_WARNING_BLOCKS,
                    critical_blocks: TEE_UPGRADE_CRITICAL_BLOCKS,
                    promoted,
                },
            ))))
        } else {
            None
        };

        let durable_ce_adapter = Arc::new(RethDurableCeState::new(node.provider.clone()));
        let durable_ce_state: Arc<dyn DurableCeState> = durable_ce_adapter.clone();
        let canonical_ce_replay: Arc<dyn CanonicalCeReplaySource> = durable_ce_adapter;
        let finalized_ce_tree: Arc<dyn FinalizedCeTree> = compressed_tree_service.clone();
        let finalized_ce_committer: Arc<dyn FinalizedCeCommitter> =
            Arc::new(RethCeFinalizer::new(durable_ce_state, finalized_ce_tree));
        let startup_ce_tree: Arc<dyn StartupCeTree> = compressed_tree_service.clone();
        let ce_startup_recovery: Arc<dyn CeStartupRecovery> = Arc::new(
            CeStartupRecoveryCoordinator::new(canonical_ce_replay, startup_ce_tree),
        );

        outbe_engine::validators::check_binary_version_compatibility(
            &node.provider,
            outbe_evm::handlers::update::registry(),
        )?;

        if args.is_validator || args.upstream.is_some() {
            if args.upstream.is_some() {
                info!("outbe node launched in FOLLOWER mode (--upstream)");
            } else {
                info!("outbe node launched in VALIDATOR mode");
            }

            // Spawn the consensus thread for validator OR follower mode. The
            // follower branch inside `run_consensus_stack` selects the lightweight
            // follow stack (no consensus engine).
            let consensus_lifecycle = ConsensusThreadGuard::new(
                shutdown_token.clone(),
                thread::spawn(consensus_thread_fn),
            ).with_outcome(shutdown.clone());

            let _ = node_tx.send((
                node,
                args,
                projection_readiness,
                ocomp_readiness_for_consensus,
                retained_tribute_writer,
                projection_retention_fence,
                retention_selector,
                finalized_ce_committer,
                ce_startup_recovery,
                radicle_consensus,
                radicle_drained,
            ));

            let exit_cause = tokio::select! {
                () = shutdown.engine_exited() => {
                    info!("execution node exited");
                    LauncherExitCause::NodeExited
                }
                _ = &mut consensus_dead_rx => {
                    info!("consensus node exited");
                    LauncherExitCause::ConsensusExited
                }
                exit = ocomp_exit_rx.recv() => {
                    if let Some(exit) = exit {
                        tracing::error!(
                            failure_class = ?exit.failure.class,
                            failure = %exit.failure.message,
                            "embedded OCOMP requested node shutdown"
                        );
                        shutdown.record_failure(eyre::eyre!("embedded OCOMP failure ({:?}): {}", exit.failure.class, exit.failure.message));
                    } else {
                        shutdown.record_failure(eyre::eyre!("embedded OCOMP exit channel closed without a verdict"));
                    }
                    LauncherExitCause::OcompRequested
                }
                () = upgrade_promotion.notified() => {
                    info!("finalized enclave upgrade requested execution restart");
                    LauncherExitCause::UpgradeRequested
                }
                rejection = tee_lease_exit_rx.recv() => {
                    let reason = tee_lease_exit_reason(rejection, &shutdown);
                    tracing::error!(
                        reason = %reason,
                        "finalized TEE lease guard requested node shutdown"
                    );
                    LauncherExitCause::TeeLeaseRejected
                }
                signal = tokio::signal::ctrl_c() => {
                    if let Err(error) = signal {
                        shutdown.record_failure(eyre::eyre!(error).wrap_err("shutdown signal listener failed"));
                    }
                    info!("received shutdown signal");
                    LauncherExitCause::CtrlC
                }
            };

            tracing::debug!(?exit_cause, "draining application before global execution shutdown");

            let consensus_joined = consensus_lifecycle.join();
            tracing::debug!("consensus thread join completed");
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                handle_consensus_thread_join(consensus_joined)
            })) {
                Ok(Ok(())) => {}
                Ok(Err(error)) => shutdown.record_failure(error),
                Err(panic) => shutdown.record_panic(panic),
            }
        } else {
            info!("outbe node launched in FULL NODE mode - no consensus thread spawned");

            tokio::select! {
                () = shutdown.engine_exited() => {
                    info!("execution node exited");
                }
                signal = tokio::signal::ctrl_c() => {
                    if let Err(error) = signal {
                        shutdown.record_failure(eyre::eyre!(error).wrap_err("shutdown signal listener failed"));
                    }
                    info!("received shutdown signal");
                }
                exit = ocomp_exit_rx.recv() => {
                    if let Some(exit) = exit {
                        tracing::error!(
                            failure_class = ?exit.failure.class,
                            failure = %exit.failure.message,
                            "embedded OCOMP requested node shutdown"
                        );
                        shutdown.record_failure(eyre::eyre!("embedded OCOMP failure ({:?}): {}", exit.failure.class, exit.failure.message));
                    } else {
                        shutdown.record_failure(eyre::eyre!("embedded OCOMP exit channel closed without a verdict"));
                    }
                }
                () = upgrade_promotion.notified() => {
                    info!("finalized enclave upgrade requested execution restart");
                }
                rejection = tee_lease_exit_rx.recv() => {
                    let reason = tee_lease_exit_reason(rejection, &shutdown);
                    tracing::error!(
                        reason = %reason,
                        "finalized TEE lease guard requested full-node shutdown"
                    );
                }
            }
        }

        shutdown_token.cancel();
        if let Err(error) = tee_lease_guard_handle.await {
            shutdown.record_failure(eyre::eyre!(error).wrap_err("TEE lease guard panicked"));
        }
        if let Some(mut handle) = radicle_observer {
            match tokio::time::timeout(RADICLE_DRAIN_DEADLINE, &mut handle).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => shutdown.record_failure(eyre::eyre!(error).wrap_err("Radicle observer panicked")),
                Err(error) => {
                    tracing::warn!("Radicle observer join deadline exceeded");
                    shutdown.record_failure(eyre::eyre!(error).wrap_err("Radicle observer join deadline exceeded"));
                    handle.abort();
                    if let Err(error) = handle.await {
                        if !error.is_cancelled() {
                            shutdown.record_failure(eyre::eyre!(error).wrap_err("Radicle observer failed while reaping"));
                        }
                    }
                }
            }
        }
        if let Some(handle) = tee_canary_handle {
            tracing::debug!("waiting for TEE canary worker shutdown");
            if let Err(error) = handle.await {
                shutdown.record_failure(eyre::eyre!(error).wrap_err("TEE canary worker panicked"));
            }
            tracing::debug!("TEE canary worker shutdown completed");
        }
        // The maintenance loop ends with its canonical-state stream; abort it
        // explicitly so shutdown never waits on a live provider subscription.
        txpool_maintenance_handle.abort();
        match txpool_maintenance_handle.await {
            Ok(()) => {}
            Err(error) if error.is_cancelled() => {}
            Err(error) => {
                shutdown.record_failure(eyre::eyre!("txpool maintenance task panicked: {error}"));
            }
        }
        if let Some(handle) = upgrade_handle {
            handle.abort();
            match handle.await {
                Ok(()) => {}
                Err(error) if error.is_cancelled() => {}
                Err(error) => {
                    shutdown.record_failure(eyre::eyre!("enclave-upgrade watcher panicked: {error}"));
                }
            }
        }

        Ok(())
        }.await;
        if let Err(error) = result {
            // Enter Reth's normal graceful teardown even on launcher failure.
            // The process result below retains the error. This is not success.
            launcher_shutdown.record_failure(error);
        }
        Ok(())
    })
    })
    .wrap_err("execution node failed");

    // Reth's engine OS thread can release the last offchain DB owner only after
    // its graceful termination acknowledgement and the Tokio runtime drain.
    drop(offchain_close);
    process_shutdown.finish(command_result)
}

pub(crate) fn ocomp_job_available_for_calculation(
    status: outbe_ocomp_protocol::state::OcompJobStatus,
) -> bool {
    matches!(
        status,
        outbe_ocomp_protocol::state::OcompJobStatus::VotingOpen
            | outbe_ocomp_protocol::state::OcompJobStatus::Completed
    )
}
