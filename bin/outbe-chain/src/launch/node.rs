use crate::*;
mod authority;
mod consensus;
mod radicle;
mod tee;
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
    // CRS initialization blocks this thread and can use reqwest::blocking.
    // Initialize before starting the execution runtime.
    if command_requires_crs(&cli.command) {
        use outbe_zk_backend::barretenberg::BarretenbergHandler;
        BarretenbergHandler::init().map_err(eyre::Report::from)?;
        // double-check that the bb was initialized
        _ = BarretenbergHandler::global();
    }

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
        let evm = outbe_evm::OutbeEvmConfig::new_with_bridge(spec.clone(), bridge_for_evm.clone());
        (
            evm,
            Arc::new(
                OutbeBeaconConsensus::new(spec)
                    .with_max_extra_data_size(outbe_node::consensus::OUTBE_MAX_EXTRA_DATA_SIZE),
            ),
        )
    };

    // This owner outlives cancellation of the launcher and Reth runtime teardown.
    let process_shutdown = outbe_node::shutdown::NodeShutdown::default();
    let launcher_shutdown = process_shutdown.clone();
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
        let authority::StartupChainAuthority {
            initial_tee_policy,
            dkg_prepare_window_blocks,
            minimum_block_time_millis,
        } = authority::validate_startup_authority_and_pruning(builder.config())?;

        let node_data_dir = builder
            .config()
            .datadir
            .clone()
            .resolve_datadir(reth_ethereum::chainspec::EthChainSpec::chain(
                builder.config().chain.as_ref(),
            ))
            .data_dir()
            .to_path_buf();
        let (evm_signer, local_tee_identity, tee_admission_anchor) =
            tee::prepare(builder.config(), &args, node_data_dir.clone(), &initial_tee_policy).await?;

        validate_adr005_node_mode(args.is_validator, args.upstream.is_some())?;
        let proof_chain_id = builder.config().chain.chain().id();
        let genesis_hash = builder.config().chain.genesis_hash();
        let outbe_node = build_outbe_node(
            bridge.clone(),
            evm_signer,
            shutdown.clone(),
        );
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
                        (is_validator, is_follower),
                    )
                    .with_chain_identity(proof_chain_id, genesis_hash)
                    .with_tee_renewal_schedule(
                        dkg_prepare_window_blocks,
                        minimum_block_time_millis,
                    )
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
            (
                radicle_shutdown_token.clone(),
                shutdown.clone(),
                bridge.repository_publication_mailbox(),
            ),
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

        let finalized_execution_barrier: Arc<dyn FinalizedExecutionBarrier> =
            Arc::new(RethFinalizationBarrier::new(Arc::new(
                RethDurableExecutionState::new(node.provider.clone()),
            )));

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
                finalized_execution_barrier,
                radicle_consensus,
                radicle_drained,
            ));

            let exit_cause = super::shutdown::wait_for_launcher_exit(
                &shutdown,
                Some(&mut consensus_dead_rx),
                &upgrade_promotion,
                &mut tee_lease_exit_rx,
            ).await;

            tracing::debug!(?exit_cause, "draining application before global execution shutdown");
            super::shutdown::join_consensus_thread(consensus_lifecycle, &shutdown);
        } else {
            let _ = super::shutdown::wait_for_launcher_exit(
                &shutdown,
                None,
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
    shutdown: outbe_node::shutdown::NodeShutdown,
) -> OutbeNode {
    let outbe_node = match evm_signer {
        Some(signer) => OutbeNode::with_bridge_and_evm_signer(bridge, signer),
        None => OutbeNode::with_bridge(bridge),
    };
    outbe_node.with_shutdown(shutdown)
}

fn create_outbe_rpc_handler<P>(
    provider: Arc<P>,
    bridge: ConsensusExecutionBridge,
    (is_validator, is_follower): (bool, bool),
) -> outbe_rpc::OutbeApiHandler<P> {
    if is_validator {
        outbe_rpc::OutbeApiHandler::with_bridge(provider, bridge)
    } else if is_follower {
        outbe_rpc::OutbeApiHandler::with_follower_bridge(provider, bridge)
    } else {
        outbe_rpc::OutbeApiHandler::new(provider)
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
