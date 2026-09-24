use crate::*;

/// Run the main node (Reth execution + Commonware consensus).
pub(crate) fn run_node() -> eyre::Result<()> {
    // TEE offer decryption routes exclusively through the enclave sidecar
    // (`--tee-enclave-socket` -> persistent production NodeHost authorization);
    // the offer-decryption key exists only inside the enclave (single path, no
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
    let (node_tx, node_rx) = oneshot::channel::<(
        OutbeFullNode,
        ConsensusArgs,
        ProjectionReadinessHandle,
        Option<ProjectionReadinessHandle>,
        Arc<RetainedTributeWriter>,
        Arc<ProjectionRetentionFence>,
        Arc<SharedOcompRetentionSelector>,
        Arc<dyn FinalizedCeCommitter>,
        Arc<dyn CeStartupRecovery>,
        Option<(
            outbe_radicle::integration::EndpointNetworkService,
            outbe_radicle::integration::LocalEndpointIdentityHandle,
            outbe_radicle::integration::RadicleStatusHandle,
            outbe_radicle::integration::EndpointTaskOwner,
        )>,
        Option<oneshot::Receiver<()>>,
    )>();
    let (consensus_dead_tx, mut consensus_dead_rx) = oneshot::channel::<()>();
    let shutdown_token = tokio_util::sync::CancellationToken::new();
    // A terminal protocol outcome must drain Radicle without racing the main
    // signal branch into aborting the stack before it returns its primary error.
    let radicle_shutdown_token = shutdown_token.child_token();

    // Consensus thread is spawned conditionally - see inside run_with_components
    // where `args.is_validator` is known. For now, prepare the closure.
    let shutdown_token_clone = shutdown_token.clone();
    let radicle_shutdown_for_consensus = radicle_shutdown_token.clone();
    let bridge_for_consensus = bridge.clone();
    let consensus_thread_fn = move || -> eyre::Result<()> {
        let (
            node,
            mut args,
            projection_readiness,
            ocomp_readiness,
            retained_tribute_writer,
            projection_retention_fence,
            retention_selector,
            finalized_ce_committer,
            ce_startup_recovery,
            radicle,
            radicle_drained,
        ) = match node_rx.blocking_recv() {
            Ok(v) => v,
            Err(_) => return Ok(()),
        };

        args.validate()?;

        let data_dir = node
            .config
            .datadir
            .clone()
            .resolve_datadir(reth_ethereum::chainspec::EthChainSpec::chain(
                &*node.chain_spec(),
            ))
            .data_dir()
            .to_path_buf();

        let consensus_storage = args
            .storage_dir
            .clone()
            .unwrap_or_else(|| data_dir.join("consensus"));

        // Write back effective storage_dir so the consensus stack sees it
        // even when the CLI did not provide --consensus.storage-dir.
        if args.storage_dir.is_none() {
            args.storage_dir = Some(consensus_storage.clone());
        }

        let keys_dir = args
            .keys_dir
            .clone()
            .unwrap_or_else(|| data_dir.join("keys"));

        if args.keys_dir.is_none() {
            args.keys_dir = Some(keys_dir.clone());
        }

        let chain_id = reth_ethereum::chainspec::EthChainSpec::chain(&*node.chain_spec()).id();
        outbe_consensus::proof::init_consensus_chain_id(chain_id)
            .wrap_err("bind consensus process to the selected chain id")?;
        outbe_consensus::storage_identity::bind_consensus_storage_identity(
            &consensus_storage,
            chain_id,
            node.chain_spec().genesis_hash(),
        )
        .wrap_err("validate consensus restart storage identity")?;

        // Migrate DKG files from legacy location (consensus/) to keys/.
        outbe_engine::stack::migrate_dkg_keys_if_needed(&consensus_storage, &keys_dir)?;

        info!(
            path = %consensus_storage.display(),
            "starting consensus runtime"
        );

        // initialize the append-only slashing journal at
        // `<consensus_storage>/slashing-journal.jsonl`. The journal
        // captures every SlashIndicator/ValidatorSet state transition
        // in JSONL form and is independent of reth log rotation. If
        // initialization fails, log a warning and continue - the
        // journal is best-effort observability and must not block node
        // startup.
        if let Err(error) = outbe_primitives::slashing_journal::init(&consensus_storage) {
            tracing::warn!(
                target: "outbe::slashing::journal",
                %error,
                "failed to initialize slashing journal - events will not be persisted to a sidecar file",
            );
        }

        if let Err(error) = outbe_primitives::governance_journal::init(&consensus_storage) {
            tracing::warn!(
                target: "outbe::governance::journal",
                %error,
                "failed to initialize governance journal - events will not be persisted to a sidecar file",
            );
        }

        let runtime_config = commonware_runtime::tokio::Config::default()
            .with_tcp_nodelay(Some(true))
            .with_worker_threads(args.worker_threads)
            .with_storage_directory(consensus_storage)
            .with_catch_panics(true);

        let runner = commonware_runtime::tokio::Runner::new(runtime_config);
        let node_lifetime_pin = node.clone();

        let ret: eyre::Result<()> = run_with_lifetime_pin(node_lifetime_pin, || {
            runner.start(async move |ctx| {
                let graceful_shutdown = ctx.child("shutdown");
                let application_shutdown = radicle_shutdown_for_consensus;
                let application_drain = outbe_engine::application_shutdown::ApplicationDrain::new(async move {
                    application_shutdown.cancel();
                    await_radicle_drain(radicle_drained, RADICLE_DRAIN_DEADLINE).await
                });
                let stack_application_drain = application_drain.clone();
                let (follower_drain, follower_shutdown) =
                    outbe_engine::follower_shutdown::follower_drain_pair();
                let mut stack_handle = ctx.child("consensus_stack").spawn(move |stack_ctx| {
                    outbe_engine::run_consensus_stack(
                        stack_ctx,
                        args,
                        node,
                        bridge_for_consensus,
                        {
                            let mut services = outbe_engine::ConsensusStackServices::new(
                                projection_readiness,
                                retained_tribute_writer,
                                projection_retention_fence,
                                retention_selector,
                                finalized_ce_committer,
                                ce_startup_recovery,
                            )
                            .with_follower_shutdown(follower_shutdown)
                            .with_application_drain(stack_application_drain);
                            if let Some(readiness) = ocomp_readiness {
                                services = services.with_ocomp_readiness(readiness);
                            }
                            if let Some((endpoint, local, status, owner)) = radicle {
                                services = services.with_radicle(status, endpoint, local, owner);
                            }
                            services
                        },
                    )
                });
                commonware_macros::select! {
                    _ = shutdown_token_clone.cancelled() => {
                        info!("consensus stack shutting down");
                        // The manager may still be using endpoint discovery. Keep
                        // Commonware transport alive until both owners have drained.
                        let radicle_result = application_drain.drain().await;
                        if let Err(error) = &radicle_result {
                            tracing::error!(%error, "Radicle drain failed before transport shutdown");
                        }
                        // Close follower delivery ingress and persist all accepted
                        // proofs while Marshal can still answer certificate reads.
                        let follower_result = follower_drain.drain(Duration::from_secs(5)).await;
                        if let Err(error) = &follower_result {
                            tracing::error!(%error, "follower drain failed before Marshal shutdown");
                        }
                        let stop_result = graceful_shutdown
                            .stop(0, Some(Duration::from_secs(5)))
                            .await;
                        let stack_result = await_consensus_stack_shutdown(
                            &mut stack_handle, Duration::from_secs(5),
                        ).await;
                        if let Err(error) = &stack_result {
                            tracing::error!(%error, "consensus stack failed during shutdown");
                        }
                        let stack_result = match (stack_result, follower_result) {
                            (Err(error), Err(drain)) => Err(error.wrap_err(format!("follower drain also failed: {drain:#}"))),
                            (Err(error), _) | (_, Err(error)) => Err(error),
                            (Ok(()), Ok(())) => Ok(()),
                        };
                        outbe_engine::application_shutdown::combine(
                            consensus_shutdown_result(stop_result, stack_result), radicle_result,
                        )
                    },
                    result = &mut stack_handle => {
                        let result = result.map_err(|error| {
                            eyre::eyre!("consensus stack task failed: {error:?}")
                        })?;
                        if let Err(e) = &result {
                            tracing::error!(%e, "consensus stack failed");
                        }
                        result
                    },
                }
            })
        });

        let _ = consensus_dead_tx.send(());
        ret
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
        let (radicle_preflight, radicle_status) = if args.is_validator {
            let socket = args
                .radicle_control_socket
                .as_ref()
                .expect("validated Radicle control socket");
            let sidecar = outbe_radicle::integration::query_sidecar(
                socket,
                std::time::Duration::from_secs(5),
            )
            .await
            .wrap_err("Radicle sidecar preflight failed")?;
            let evm_key = args
                .effective_validator_evm_key()?
                .ok_or_else(|| eyre::eyre!("validator EVM key is required for Radicle identity"))?;
            let validator = outbe_primitives::signer::OutbeEvmSigner::from_file(&evm_key)
                .wrap_err("load validator EVM key for Radicle identity")?
                .address();
            let (publisher, status) =
                outbe_radicle::integration::RadicleStatusChannel::enabled(
                    validator,
                    sidecar.node_id,
                );
            (Some((validator, sidecar, publisher)), status)
        } else {
            (
                None,
                outbe_radicle::integration::RadicleStatusChannel::disabled(),
            )
        };
        if radicle_preflight.is_none() {
            let mut metrics = outbe_radicle::integration::RadicleMetrics::default();
            metrics.record(&radicle_status.snapshot());
        }
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
        let ocomp_domain_root = node_data_dir
            .parent()
            .ok_or_else(|| eyre::eyre!("node data directory has no OCOMP domain parent"))?
            .join("ocomp")
            .join("domain-v1");
        let ocomp_bundle_bytes = ocomp_fork_install
            .protocol_bundle
            .encode_canonical(&ocomp_limits)?;
        let ocomp_bundle = outbe_ocomp::bundle::PinnedProtocolBundle::decode(
            &ocomp_bundle_bytes,
            ocomp_fork_install.request_profile.protocol_bundle_hash,
            &ocomp_limits,
        )?;
        let configured_ocomp_bundle_hashes =
            std::env::var("OCOMP_PROTOCOL_BUNDLE_HASHES").ok();
        let ocomp_bundles = load_installed_ocomp_bundles(
            &ocomp_domain_root,
            ocomp_bundle,
            configured_ocomp_bundle_hashes.as_deref(),
            &ocomp_limits,
        )?;
        let ocomp_worker_base_port = args
            .listen_address
            .port()
            .checked_add(1)
            .ok_or_else(|| eyre::eyre!("consensus port leaves no OCOMP Worker endpoint port"))?;
        let mut ocomp_runtime_bundles = Vec::with_capacity(ocomp_bundles.len());
        let ocomp_lane_port_stride = u16::try_from(
            outbe_ocomp::worker_transport::MAX_REGISTERED_WORKERS,
        )
        .map_err(|_| eyre::eyre!("OCOMP worker limit exceeds u16"))?
        .checked_add(2)
        .ok_or_else(|| eyre::eyre!("OCOMP bundle lane port stride overflow"))?;
        for (index, bundle) in ocomp_bundles.into_iter().enumerate() {
            let lane = u16::try_from(index)
                .map_err(|_| eyre::eyre!("OCOMP bundle lane count exceeds u16"))?;
            let port_offset = lane
                .checked_mul(ocomp_lane_port_stride)
                .ok_or_else(|| eyre::eyre!("OCOMP bundle lane port offset overflow"))?;
            let worker_port = ocomp_worker_base_port
                .checked_add(port_offset)
                .ok_or_else(|| eyre::eyre!("OCOMP bundle lane leaves no Worker endpoint port"))?;
            let worker_address = std::net::SocketAddr::new(
                std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                worker_port,
            );
            info!(
                bundle_hash = %bundle.hash(),
                lane = index,
                %worker_address,
                "loaded pinned OCOMP runtime bundle lane"
            );
            ocomp_runtime_bundles.push(ocomp_exex::OcompExExBundleConfigV1 {
                worker_address,
                identity: outbe_ocomp_protocol::local_control::EndpointIdentity {
                    chain_id: builder.config().chain.chain().id(),
                    genesis_hash: builder.config().chain.genesis_hash(),
                    boot_nonce: ocomp_install_hash,
                    protocol_bundle_hash: bundle.hash(),
                },
                protocol_bundle: bundle,
            });
        }
        let ocomp_policy = if args.is_validator {
            outbe_ocomp::embedded_runtime::EmbeddedNodePolicyV1::Validator
        } else {
            outbe_ocomp::embedded_runtime::EmbeddedNodePolicyV1::FullNode
        };
        let ocomp_validator_rpc_url = if args.is_validator {
            if !builder.config().rpc.http {
                eyre::bail!("validator OCOMP requires the local HTTP RPC server");
            }
            Some(format!(
                "http://127.0.0.1:{}",
                builder.config().rpc.http_port
            ))
        } else {
            None
        };
        let retention_selector = Arc::new(SharedOcompRetentionSelector::new());
        let discovery_spool_root = ocomp_domain_root.join("exporter-v1/discovery");
        let ocomp_exex_config = ocomp_exex::OcompExExConfigV1 {
            domain_root: ocomp_domain_root,
            discovery_spool_root,
            bundles: ocomp_runtime_bundles,
            policy: ocomp_policy,
            validator_rpc_url: ocomp_validator_rpc_url,
            chain_id: builder.config().chain.chain().id(),
            genesis_hash: builder.config().chain.genesis_hash(),
            retention_selector: Arc::clone(&retention_selector),
            retention_required: args.is_validator || args.upstream.is_some(),
        };
        let ocomp_baseline = ProjectionCheckpoint {
            block_number: 0,
            block_hash: builder.config().chain.genesis_hash(),
        };
        let (ocomp_readiness_publisher, ocomp_readiness) = projection_readiness(
            ocomp_baseline,
            ProjectionStatus::Ready {
                checkpoint: ocomp_baseline,
            },
        );
        let ocomp_readiness_for_consensus =
            args.upstream.is_some().then(|| ocomp_readiness.clone());
        let (ocomp_exit_tx, mut ocomp_exit_rx) = tokio::sync::mpsc::unbounded_channel();
        let evm_signer = if args.is_validator {
            let evm_key_path = args
                .effective_validator_evm_key()?
                .ok_or_else(|| eyre::eyre!("validator mode requires an EVM signer key"))?;
            let signer =
                Arc::new(OutbeEvmSigner::from_file(&evm_key_path).wrap_err_with(|| {
                    format!(
                        "failed to load validator EVM key from {}",
                        evm_key_path.display()
                    )
                })?);
            info!(
                address = %signer.address(),
                path = %evm_key_path.display(),
                "loaded validator EVM signer"
            );
            Some(signer)
        } else {
            None
        };
        let validator_evm_address = evm_signer.as_ref().map(|signer| signer.address());
        // Every network declares exactly one attestation policy in genesis. The
        // local session protocol is an independent, explicit operator choice:
        // GramineDirectDev may use either the development transport or a real
        // SGX, production NodeHost session. There is no connection fallback.
        let socket = args.tee_enclave_socket.clone().ok_or_else(|| {
            eyre::eyre!(
                "mandatory {:?} ChainSpec requires --tee-enclave-socket before node startup",
                initial_tee_policy.attestation_mode
            )
        })?;
        let endpoint = socket
            .to_str()
            .ok_or_else(|| eyre::eyre!("TEE enclave endpoint is not valid UTF-8"))?;
        let tee_session = args
            .tee_session_mode
            .resolve(initial_tee_policy.attestation_mode)
            .map_err(eyre::Report::msg)?;
        let (node_host_signing, reth_p2p_public) = load_reth_p2p_node_host_signer(
            &builder.config().network,
            builder.config().datadir().p2p_secret(),
        )?;
        outbe_tee::call_context::set_snapshot(outbe_tee::call_context::EnclaveCallContextV1 {
            chain_id: builder.config().chain.chain().id(),
            genesis_hash: builder.config().chain.genesis_hash(),
            ..Default::default()
        }).map_err(eyre::Report::msg)?;
        let expected_enclave_id = match tee_session {
            outbe_engine::args::ResolvedTeeSession::ProductionNodeHost => {
                use k256::ecdsa::signature::hazmat::PrehashSigner as _;

                let client = outbe_tee::connect_or_initialize_node_host_enclave(
                    endpoint,
                    &node_data_dir,
                    outbe_tee::NodeHostIdentityV1 {
                        network_binding: initial_tee_policy.network_binding(),
                        reth_p2p_public,
                    },
                    |hash| {
                        let (signature, recovery): (
                            k256::ecdsa::Signature,
                            k256::ecdsa::RecoveryId,
                        ) = node_host_signing
                            .sign_prehash(hash.as_slice())
                            .map_err(|error| error.to_string())?;
                        let mut bytes = [0_u8; 65];
                        bytes[..64].copy_from_slice(signature.to_bytes().as_slice());
                        bytes[64] = recovery.to_byte();
                        Ok(bytes)
                    },
                )
                .wrap_err("NodeHost enclave initialization failed")?;
                // Session material for reconnect-with-identity-revalidation:
                // loaded once here (takes the NodeHost file lock), never in the
                // request hot path.
                let (manifest, node_host) =
                    outbe_tee::node_host::committed_node_host_session_material(&node_data_dir)
                        .wrap_err("committed NodeHost session material load failed")?;
                let enclave_id = manifest
                    .enclave_id()
                    .map_err(|error| eyre::eyre!("derive committed enclave identity: {error}"))?;
                outbe_tee::install_authorized_enclave_client(
                    client,
                    endpoint.to_owned(),
                    node_data_dir.clone(),
                    manifest,
                    node_host,
                )
                .wrap_err("enclave session install failed")?;
                Some(enclave_id)
            }
            outbe_engine::args::ResolvedTeeSession::Development => {
                let client = outbe_tee::EnclaveClient::connect_endpoint(endpoint)
                    .wrap_err("development enclave connection failed")?;
                outbe_tee::install_enclave_client(client, endpoint.to_owned())
                    .wrap_err("enclave session install failed")?;
                None
            }
        };
        let local_tee_identity = outbe_engine::validators::LocalTeeRuntimeIdentityV1 {
            reth_p2p_public,
            expected_enclave_id,
            validator: validator_evm_address,
        };
        info!(
            socket = %socket.display(),
            node_host_identity = "reth-p2p-secp256k1",
            attestation_mode = ?initial_tee_policy.attestation_mode,
            session_mode = ?tee_session,
            "mandatory TEE enclave sidecar connected before execution launch",
        );

        let tee_admission_anchor = if args.is_validator {
            if expected_enclave_id.is_some() {
                outbe_tee::load_finalized_join_admission_anchor(&node_data_dir)
                    .wrap_err("load durable validator join admission anchor")?
                    .map(|durable| {
                        validator_admission_anchor_from_durable_v1(
                            durable,
                            builder.config().chain.chain().id(),
                            builder.config().chain.genesis_hash(),
                            local_tee_identity,
                        )
                    })
                    .transpose()?
            } else {
                None
            }
        } else {
            // A follower re-executes every protected transaction and therefore must
            // already hold the exact permanent offer key committed by the running
            // chain. Prove that invariant before Reth opens networking, RPC, sync or
            // execution. Losing the key is terminal for this node identity: startup
            // never invokes recovery, replacement or another bootstrap path.
            let upstream = args.upstream.as_deref().ok_or_else(|| {
                eyre::eyre!(
                    "full-node startup requires --upstream to authenticate the chain offer key"
                )
            })?;
            let admission_anchor =
                require_upstream_fullnode_tee_admission(upstream, local_tee_identity).await?;
            let expected_offer = outbe_engine::read_upstream_tribute_offer_public_key(upstream)
                .await
                .wrap_err("failed to read mandatory offer key from the selected upstream")?;
            if expected_offer.is_zero() {
                return Err(eyre::eyre!(
                    "selected upstream has no mandatory OST3 offer key; refusing full-node startup"
                ));
            }
            let resident_offer = outbe_tee::resident_offer_public_key_v1()
                .wrap_err("failed to read the local enclave resident offer key")?;
            if resident_offer != expected_offer {
                return Err(eyre::eyre!(
                    "local enclave does not hold the selected chain's exact offer key; refusing execution startup (no recovery or fallback)"
                ));
            }
            info!(
                offer_public_key = %resident_offer,
                %upstream,
                "full-node resident offer key matched upstream before execution launch"
            );
            Some(admission_anchor)
        };

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
            )
        })
        .await
        .wrap_err("offchain-data startup validation worker failed")??;
        shutdown.retain_storage_close(prepared_projection.storage_close_observer())?;
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
        // Canary-fed enclave health: published by the tee-canary worker (spawned
        // after node launch), read by `outbe_consensusStatus.enclave`.
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
                // here anyway; disabling it removes the attack surface.
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
                        outbe_node::tee_call_context::read(context_provider.as_ref(), proof_chain_id, genesis_hash)
                    })).map_err(eyre::Report::msg)?;
                    // Validators get the full bridge-backed handler.
                    // `--upstream` followers also run a marshal and CAN serve
                    // `outbe_getFinalization` (chaining followers), but must NOT
                    // report validator status; they get a follower-scoped handler
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
            proof_chain_id,
            genesis_hash,
            local_tee_identity,
            tee_lease_guard_gate,
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

        let (radicle_consensus, radicle_observer, radicle_drained) = if let Some((
            validator,
            sidecar,
            publisher,
        )) = radicle_preflight
        {
            use outbe_radicle::manager::SnapshotReader as _;

            let exact = node
                .provider
                .finalized_block_num_hash()
                .wrap_err("read finalized head for Radicle startup")?
                .map(|block| outbe_radicle::manager::FinalizedBlock {
                    number: block.number,
                    hash: block.hash,
                })
                .unwrap_or(outbe_radicle::manager::FinalizedBlock {
                    number: 0,
                    hash: genesis_hash,
                });
            let raw_snapshots: Arc<dyn outbe_radicle::manager::SnapshotReader> = Arc::new(
                outbe_radicle::manager::RethSnapshotReader::new(
                    node.provider.clone(),
                    proof_chain_id,
                    genesis_hash,
                ),
            );
            let observed_snapshots = Arc::new(
                outbe_radicle::integration::ObservedSnapshotReader::new(
                    raw_snapshots,
                    publisher.clone(),
                ),
            );
            let initial = observed_snapshots
                .read_exact(exact)
                .wrap_err("read exact Radicle startup snapshot")?;
            match initial
                .validators
                .iter()
                .find(|candidate| candidate.address == validator)
            {
                None => {}
                Some(candidate) if candidate.node_id.is_none() => {
                    eyre::bail!("active validator has no Radicle NodeId binding");
                }
                Some(candidate) if candidate.node_id != Some(sidecar.node_id) => {
                    eyre::bail!("local Radicle NodeId does not match finalized validator binding");
                }
                Some(_) => publisher.mark_startup_ready(),
            }

            let (endpoint, resolver, evidence) =
                outbe_radicle::integration::EndpointNetwork::build(
                    outbe_radicle::endpoint::ChainIdentity {
                        chain_id: proof_chain_id,
                        genesis_hash,
                    },
                    radicle_status.clone(),
                );
            let (local_endpoint_publisher, local_endpoint) =
                outbe_radicle::integration::LocalEndpointIdentityChannel::create(
                    outbe_radicle::integration::LocalEndpointIdentity {
                        validator,
                        node_id: sidecar.node_id,
                        addresses: sidecar.addresses,
                    },
                );
            let pinned_node_id = sidecar.node_id;
            let radicle_control_socket = args
                .radicle_control_socket
                .clone()
                .expect("validated Radicle control socket");
            let repository_status: Arc<dyn outbe_radicle::manager::RepositoryStatus> = Arc::new(
                outbe_radicle::manager::HttpRepositoryStatus::new(
                    args.radicle_status_address
                        .expect("validated Radicle status address"),
                    std::time::Duration::from_secs(5),
                )?,
            );
            let repository_status = Arc::new(
                outbe_radicle::integration::ObservedRepositoryStatus::new(
                    repository_status,
                    publisher.clone(),
                ),
            );
            let manager = outbe_radicle::manager::RadicleManager::start(
                outbe_radicle::manager::ManagerConfig {
                    self_validator: validator,
                    local_node_id: sidecar.node_id,
                    repair_interval: outbe_radicle::integration::PRODUCTION_REPAIR_INTERVAL,
                    retry: outbe_radicle::manager::RetryPolicy::default(),
                },
                outbe_radicle::manager::ManagerDependencies {
                    finality: Arc::new(
                        outbe_radicle::integration::GenesisFallbackFinalizedFeed::new(
                            Arc::new(outbe_radicle::manager::RethFinalizedFeed::new(
                                node.provider.clone(),
                            )),
                            exact,
                        ),
                    ),
                    snapshots: observed_snapshots,
                    endpoints: Arc::new(resolver.clone()),
                    control: Arc::new(outbe_radicle::manager::NativeHeartwoodControl::new(
                        radicle_control_socket.clone(),
                        std::time::Duration::from_secs(5),
                    )),
                    repository_status,
                },
            );
            let observer_shutdown = radicle_shutdown_token.clone();
            let endpoint_owner = outbe_radicle::integration::EndpointTaskOwner::default();
            let observer_endpoint_owner = endpoint_owner.clone();
            let observer_publisher = publisher.clone();
            let observer_resolver = resolver.clone();
            let observer_status = radicle_status.clone();
            let observer_outcome = shutdown.clone();
            let observer_tracker = shutdown.clone();
            let (drained_tx, drained_rx) = oneshot::channel();
            // Register with Reth's graceful drain before returning to its
            // cancellable launcher. Dropping the launcher must not abort cleanup.
            let observer = node.task_executor.spawn_with_graceful_shutdown_signal(async move |guard| {
                observer_tracker.track_task("Radicle observer", async move {
                let manager = manager;
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
                let mut local_endpoint_interval = tokio::time::interval(
                    outbe_radicle::integration::PRODUCTION_REPAIR_INTERVAL,
                );
                local_endpoint_interval.set_missed_tick_behavior(
                    tokio::time::MissedTickBehavior::Skip,
                );
                let mut metrics = outbe_radicle::integration::RadicleMetrics::default();
                loop {
                    tokio::select! {
                        _ = observer_shutdown.cancelled() => {
                            if let Err(error) = outbe_radicle::integration::shutdown_bounded(
                                std::time::Duration::from_secs(5),
                                manager,
                                observer_endpoint_owner.shutdown(&observer_resolver),
                            ).await {
                                tracing::error!(%error, "Radicle integration shutdown failed");
                                observer_outcome.record_failure(eyre::eyre!(error).wrap_err("Radicle integration shutdown failed"));
                            }
                            break;
                        }
                        _ = interval.tick() => {
                            observer_publisher.observe_manager(manager.status());
                            observer_publisher.observe_evidence(evidence.snapshot());
                            metrics.record(&observer_status.snapshot());
                        }
                        _ = local_endpoint_interval.tick() => {
                            let sidecar = tokio::select! {
                                _ = observer_shutdown.cancelled() => continue,
                                result = outbe_radicle::integration::query_sidecar(
                                    &radicle_control_socket,
                                    std::time::Duration::from_secs(5),
                                ) => result,
                            };
                            match sidecar {
                                Ok(sidecar) if sidecar.node_id == pinned_node_id => {
                                    let _ = local_endpoint_publisher.update(
                                        sidecar.node_id,
                                        sidecar.addresses,
                                    );
                                }
                                Ok(sidecar) => {
                                    local_endpoint_publisher.unavailable();
                                    tracing::error!(
                                        expected_node_id = ?pinned_node_id,
                                        actual_node_id = ?sidecar.node_id,
                                        "Radicle sidecar NodeId changed; endpoint publication suppressed"
                                    );
                                }
                                Err(error) => {
                                    local_endpoint_publisher.unavailable();
                                    tracing::warn!(
                                        %error,
                                        "Radicle sidecar identity refresh failed; endpoint publication suppressed"
                                    );
                                }
                            }
                        }
                    }
                }
                    let _ = drained_tx.send(());
                }).await;
                drop(guard);
            });
            (
                Some((endpoint, local_endpoint, radicle_status.clone(), endpoint_owner)),
                Some(observer),
                Some(drained_rx),
            )
        } else {
            (None, None, None)
        };

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
        // Pending staleness eviction. Node-local pool policy, so it runs in
        // every mode - full nodes are the public RPC ingress and shed stuck
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

            // Spawn the consensus thread for validator OR follower mode; the
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
            // The process result below retains the error; this is not success.
            launcher_shutdown.record_failure(error);
        }
        Ok(())
    })
    })
    .wrap_err("execution node failed");

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
