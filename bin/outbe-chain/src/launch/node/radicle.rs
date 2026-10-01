use super::*;

pub(super) type Preflight = (
    alloy_primitives::Address,
    outbe_radicle::integration::SidecarInfo,
    outbe_radicle::integration::RadicleStatusPublisher,
);
pub(super) type ConsensusEndpoint = (
    outbe_radicle::integration::EndpointNetworkService,
    outbe_radicle::integration::LocalEndpointIdentityHandle,
    outbe_radicle::integration::RadicleStatusHandle,
    outbe_radicle::integration::EndpointTaskOwner,
);
pub(super) type RadicleLaunch = (
    Option<ConsensusEndpoint>,
    Option<tokio::task::JoinHandle<()>>,
    Option<oneshot::Receiver<()>>,
);

pub(super) async fn preflight(
    args: &ConsensusArgs,
) -> eyre::Result<(
    Option<Preflight>,
    outbe_radicle::integration::RadicleStatusHandle,
)> {
    let (radicle_preflight, radicle_status) = if args.is_validator {
        let socket = args
            .radicle_control_socket
            .as_ref()
            .expect("validated Radicle control socket");
        let sidecar =
            outbe_radicle::integration::query_sidecar(socket, std::time::Duration::from_secs(5))
                .await
                .wrap_err("Radicle sidecar preflight failed")?;
        let evm_key = args
            .effective_validator_evm_key()?
            .ok_or_else(|| eyre::eyre!("validator EVM key is required for Radicle identity"))?;
        let validator = outbe_primitives::signer::OutbeEvmSigner::from_file(&evm_key)
            .wrap_err("load validator EVM key for Radicle identity")?
            .address();
        let (publisher, status) =
            outbe_radicle::integration::RadicleStatusChannel::enabled(validator, sidecar.node_id);
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
    Ok((radicle_preflight, radicle_status))
}

pub(super) fn start(
    node: &OutbeFullNode,
    args: &ConsensusArgs,
    radicle_preflight: Option<Preflight>,
    radicle_status: outbe_radicle::integration::RadicleStatusHandle,
    radicle_shutdown_token: tokio_util::sync::CancellationToken,
    shutdown: outbe_node::shutdown::NodeShutdown,
) -> eyre::Result<RadicleLaunch> {
    let proof_chain_id = node.chain_spec().chain().id();
    let genesis_hash = node.chain_spec().genesis_hash();
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
        let raw_snapshots: Arc<dyn outbe_radicle::manager::SnapshotReader> =
            Arc::new(outbe_radicle::manager::RethSnapshotReader::new(
                node.provider.clone(),
                proof_chain_id,
                genesis_hash,
            ));
        let observed_snapshots = Arc::new(outbe_radicle::integration::ObservedSnapshotReader::new(
            raw_snapshots,
            publisher.clone(),
        ));
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

        let (endpoint, resolver, evidence) = outbe_radicle::integration::EndpointNetwork::build(
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
        let repository_status: Arc<dyn outbe_radicle::manager::RepositoryStatus> =
            Arc::new(outbe_radicle::manager::HttpRepositoryStatus::new(
                args.radicle_status_address
                    .expect("validated Radicle status address"),
                std::time::Duration::from_secs(5),
            )?);
        let repository_status =
            Arc::new(outbe_radicle::integration::ObservedRepositoryStatus::new(
                repository_status,
                publisher.clone(),
            ));
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
            Some((
                endpoint,
                local_endpoint,
                radicle_status.clone(),
                endpoint_owner,
            )),
            Some(observer),
            Some(drained_rx),
        )
    } else {
        (None, None, None)
    };

    Ok((radicle_consensus, radicle_observer, radicle_drained))
}
