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
    // This runs before `run_with_execution_runtime` creates the Tokio runtime
    // because `init_crs` uses `reqwest::blocking` internally.
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

    // Channels for the consensus thread in validator or follower mode.
    // In other full-node mode, the launcher spawns no thread and these stay unused.
    let (node_tx, node_rx) = oneshot::channel::<consensus::ConsensusLaunch>();
    let (consensus_dead_tx, mut consensus_dead_rx) = oneshot::channel::<()>();
    let shutdown_token = tokio_util::sync::CancellationToken::new();
    // A terminal protocol outcome must drain Radicle without racing the main
    // signal branch into aborting the stack before it returns its primary error.
    let radicle_shutdown_token = shutdown_token.child_token();

    // The launcher spawns the consensus thread only in validator or follower
    // (`--upstream`) mode. The `with_runner_and_components` closure makes this
    // decision because it knows `args`. Here we only prepare the thread closure.
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
    let runtime_config = runtime_config_for_command(&cli.command);
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
        let ocomp::StartupChainAuthority {
            initial_tee_policy,
            dkg_prepare_window_blocks,
            minimum_block_time_millis,
            ocomp_fork_install,
            ocomp_install_hash,
        } = ocomp::validate_startup_authority_and_pruning(builder.config())?;

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
            tee::prepare(builder.config(), &args, node_data_dir.clone(), &initial_tee_policy).await?;

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
            tree_format: "ckb-smt-v0.6.1-poseidon2-catalog-v3".to_owned(),
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
        let outbe_node = build_outbe_node(
            bridge.clone(),
            evm_signer,
            runtime_body_readers,
            compressed_tree_service.clone(),
            (ocomp_fork_install, shutdown.clone()),
        );
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
                    }))?;
                    let outbe_api = create_outbe_rpc_handler(
                        Arc::clone(&provider),
                        bridge,
                        projection_readiness.clone(),
                        (is_validator, is_follower),
                    )
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
                    .with_ocomp_lysis_openings(ocomp::lysis_openings_runtime(Arc::clone(&provider)))
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

        let tee::TeeRuntimeWorkers {
            mut tee_lease_exit_rx,
            tee_lease_guard_handle,
            tee_canary_handle,
            upgrade_promotion,
            upgrade_handle,
        } = tee::start_runtime_workers(
            (&node, &args),
            (proof_chain_id, genesis_hash, &node_data_dir),
            (local_tee_identity, tee_admission_anchor, &initial_tee_policy),
            (tee_canary_status.clone(), shutdown_token.clone(), shutdown.clone()),
        )?;

        let (radicle_consensus, radicle_observer, radicle_drained) = radicle::start(
            &node,
            &args,
            (radicle_preflight, radicle_status.clone()),
            radicle_shutdown_token.clone(),
            shutdown.clone(),
        )?;

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

        if log_consensus_launch_mode(args.is_validator, args.upstream.is_some()) {
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

            let exit_cause = super::shutdown::wait_for_launcher_exit(
                &shutdown,
                Some(&mut consensus_dead_rx),
                &mut ocomp_exit_rx,
                &upgrade_promotion,
                &mut tee_lease_exit_rx,
            ).await;

            tracing::debug!(?exit_cause, "draining application before global execution shutdown");
            super::shutdown::join_consensus_thread(consensus_lifecycle, &shutdown);
        } else {
            let _ = super::shutdown::wait_for_launcher_exit(
                &shutdown,
                None,
                &mut ocomp_exit_rx,
                &upgrade_promotion,
                &mut tee_lease_exit_rx,
            ).await;
        }

        super::shutdown::drain_launcher_tasks(
            &shutdown_token,
            &shutdown,
            super::shutdown::LauncherDrainTasks {
                tee_lease_guard_handle,
                radicle_observer,
                tee_canary_handle,
                txpool_maintenance_handle,
                upgrade_handle,
            },
        ).await;

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

fn runtime_config_for_command(
    command: &reth_ethereum::cli::interface::Commands<OutbeChainSpecParser, ConsensusArgs>,
) -> reth_ethereum::tasks::RuntimeConfig {
    // Preserve the pool overrides normally applied by Reth's default CLI runner.
    match command {
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
    }
}

fn build_outbe_node(
    bridge: ConsensusExecutionBridge,
    evm_signer: Option<Arc<OutbeEvmSigner>>,
    runtime_body_readers: outbe_offchain_data::RuntimeBodyReaders,
    compressed_tree_service: Arc<CompressedTreeService>,
    (ocomp_fork_install, shutdown): (
        Arc<outbe_metadosis::config::OcompForkInstallV1>,
        outbe_node::shutdown::NodeShutdown,
    ),
) -> OutbeNode {
    let outbe_node = match evm_signer {
        Some(signer) => OutbeNode::with_bridge_and_evm_signer(
            bridge,
            signer,
            runtime_body_readers,
            compressed_tree_service,
        ),
        None => OutbeNode::with_bridge(bridge, runtime_body_readers, compressed_tree_service),
    };
    outbe_node
        .with_ocomp_fork_install(ocomp_fork_install)
        .with_shutdown(shutdown)
}

fn create_outbe_rpc_handler<P>(
    provider: Arc<P>,
    bridge: ConsensusExecutionBridge,
    projection_readiness: ProjectionReadinessHandle,
    (is_validator, is_follower): (bool, bool),
) -> outbe_rpc::OutbeApiHandler<P> {
    if is_validator {
        outbe_rpc::OutbeApiHandler::with_bridge(provider, bridge, projection_readiness)
    } else if is_follower {
        outbe_rpc::OutbeApiHandler::with_follower_bridge(provider, bridge, projection_readiness)
    } else {
        outbe_rpc::OutbeApiHandler::new(provider, projection_readiness)
    }
}

fn log_consensus_launch_mode(is_validator: bool, has_upstream: bool) -> bool {
    if has_upstream {
        info!("outbe node launched in FOLLOWER mode (--upstream)");
        true
    } else if is_validator {
        info!("outbe node launched in VALIDATOR mode");
        true
    } else {
        info!("outbe node launched in FULL NODE mode - no consensus thread spawned");
        false
    }
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
