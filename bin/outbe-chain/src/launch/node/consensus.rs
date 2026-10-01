use super::*;

pub(super) type ConsensusLaunch = (
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
);

pub(super) fn run(
    node_rx: oneshot::Receiver<ConsensusLaunch>,
    consensus_dead_tx: oneshot::Sender<()>,
    shutdown_token_clone: tokio_util::sync::CancellationToken,
    radicle_shutdown_for_consensus: tokio_util::sync::CancellationToken,
    bridge_for_consensus: ConsensusExecutionBridge,
) -> eyre::Result<()> {
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
}
