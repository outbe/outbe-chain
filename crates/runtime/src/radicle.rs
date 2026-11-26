use super::*;
use crate::errors::LaunchError;

pub(crate) type Preflight = (
    alloy_primitives::Address,
    outbe_radicle::integration::SidecarInfo,
    outbe_radicle::integration::RadicleStatusPublisher,
);
pub(crate) type ConsensusEndpoint = (
    outbe_radicle::integration::EndpointNetworkService,
    outbe_radicle::integration::LocalEndpointIdentityHandle,
    outbe_radicle::integration::RadicleStatusHandle,
    outbe_radicle::integration::EndpointTaskOwner,
);
pub(crate) type RadicleLaunch = (
    Option<ConsensusEndpoint>,
    Option<tokio::task::JoinHandle<()>>,
    Option<oneshot::Receiver<()>>,
);

pub(crate) async fn preflight(
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
            .ok_or(LaunchError::MissingValidatorEvmKeyForRadicle)?;
        let backend = args.evm_key_backend()?;
        let validator = outbe_consensus::bls::load_validator_evm_signer(&evm_key, &backend)
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

pub(crate) fn start(
    node: &OutbeFullNode,
    args: &ConsensusArgs,
    protocol: Arc<dyn outbe_radicle::protocol_state::RadicleProtocolState>,
    (radicle_preflight, radicle_status): (
        Option<Preflight>,
        outbe_radicle::integration::RadicleStatusHandle,
    ),
    (radicle_shutdown_token, shutdown, publication_mailbox): (
        tokio_util::sync::CancellationToken,
        outbe_execution::shutdown::NodeShutdown,
        outbe_primitives::radicle_publication::RepositoryPublicationMailbox,
    ),
) -> eyre::Result<RadicleLaunch> {
    let proof_chain_id = node.chain_spec().chain().id();
    let genesis_hash = node.chain_spec().genesis_hash();
    let (radicle_consensus, radicle_observer, radicle_drained) =
        if let Some((validator, sidecar, publisher)) = radicle_preflight {
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
            let repository_snapshots = Arc::new(outbe_radicle::manager::RethSnapshotReader::new(
                node.provider.clone(),
                proof_chain_id,
                genesis_hash,
                protocol,
            ));
            let raw_snapshots: Arc<dyn outbe_radicle::manager::SnapshotReader> =
                repository_snapshots.clone();
            let observed_snapshots =
                Arc::new(outbe_radicle::integration::ObservedSnapshotReader::new(
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
                    return Err(LaunchError::ActiveValidatorMissingRadicleNodeId.into());
                }
                Some(candidate) if candidate.node_id != Some(sidecar.node_id) => {
                    return Err(LaunchError::LocalRadicleNodeIdMismatch.into());
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
            let endpoint = endpoint
                .with_repository_authority_reader(repository_snapshots.clone())
                .with_repository_publication_mailbox(publication_mailbox);
            let local_receipts = if node
                .chain_spec()
                .genesis
                .config
                .extra_fields
                .contains_key(outbe_primitives::radicle_genesis::GENESIS_GOVERNMENTS_FIELD)
            {
                let control = args
                    .radicle_control_socket
                    .as_ref()
                    .expect("validated Radicle control socket");
                let publications =
                    outbe_radicle::integration::publication::store::ReviewPackageStore::open(
                        outbe_radicle::integration::local::publication_store_path(control),
                    )
                    .wrap_err("open protected Radicle publication store")?;
                Some(
                    outbe_radicle::integration::local::LocalReceiptServer::bind(
                        control,
                        repository_snapshots.clone(),
                    )
                    .wrap_err("bind protected Radicle receipt socket")?
                    .with_authority_reader(repository_snapshots)
                    .with_material_resolver(resolver.clone())
                    .with_native_node_id(sidecar.node_id)
                    .with_publication_store(publications),
                )
            } else {
                None
            };
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
            let manager_status = manager.subscribe();
            let (drained_tx, drained_rx) = oneshot::channel();
            // Register with Reth's graceful drain before returning to its
            // cancellable launcher. Dropping the launcher must not abort cleanup.
            let observer =
                node.task_executor
                    .spawn_with_graceful_shutdown_signal(async move |guard| {
                        observer_tracker
                            .track_task("Radicle observer", async move {
                                let manager = manager;
                                ObserverLoop {
                                    shutdown: observer_shutdown,
                                    outcome: observer_outcome.clone(),
                                    local_receipts,
                                    manager_status,
                                    evidence,
                                    publisher: observer_publisher,
                                    status: observer_status,
                                    control_socket: radicle_control_socket,
                                    pinned_node_id,
                                    local_endpoint_publisher,
                                }
                                .run()
                                .await;
                                if let Err(error) = outbe_radicle::integration::shutdown_bounded(
                                    std::time::Duration::from_secs(5),
                                    manager,
                                    observer_endpoint_owner.shutdown(&observer_resolver),
                                )
                                .await
                                {
                                    tracing::error!(%error, "Radicle integration shutdown failed");
                                    observer_outcome.record_failure(
                                        eyre::Report::new(error)
                                            .wrap_err("Radicle integration shutdown failed"),
                                    );
                                }
                                let _ = drained_tx.send(());
                            })
                            .await;
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

struct ObserverLoop {
    shutdown: tokio_util::sync::CancellationToken,
    outcome: outbe_execution::shutdown::NodeShutdown,
    local_receipts: Option<outbe_radicle::integration::local::LocalReceiptServer>,
    manager_status: tokio::sync::watch::Receiver<outbe_radicle::manager::ManagerStatus>,
    evidence: outbe_radicle::integration::EndpointEvidenceHandle,
    publisher: outbe_radicle::integration::RadicleStatusPublisher,
    status: outbe_radicle::integration::RadicleStatusHandle,
    control_socket: PathBuf,
    pinned_node_id: [u8; 32],
    local_endpoint_publisher: outbe_radicle::integration::LocalEndpointIdentityPublisher,
}

impl ObserverLoop {
    async fn run(mut self) {
        let receipt_stop = self.shutdown.clone();
        let local_receipts = self.local_receipts.take();
        let mut local_receipts = Box::pin(async move {
            match local_receipts {
                Some(server) => server.run(receipt_stop.cancelled_owned()).await,
                None => std::future::pending().await,
            }
        });
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        let mut local_endpoint_interval =
            tokio::time::interval(outbe_radicle::integration::PRODUCTION_REPAIR_INTERVAL);
        local_endpoint_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut metrics = outbe_radicle::integration::RadicleMetrics::default();
        let mut manager_status_open = true;
        // Sample the initial manager state and mark it seen before entering select.
        let initial_manager = self.manager_status.borrow_and_update().clone();
        self.publisher.observe_manager(initial_manager);
        metrics.record(&self.status.snapshot());
        loop {
            tokio::select! {
                _ = self.shutdown.cancelled() => {
                    break;
                }
                result = &mut local_receipts => {
                    match result {
                        Err(error) => self.outcome.record_failure(
                            eyre::Report::new(error).wrap_err("Radicle receipt service failed"),
                        ),
                        Ok(()) if !self.shutdown.is_cancelled() => self.outcome.record_failure(
                            eyre::eyre!("Radicle receipt service stopped unexpectedly"),
                        ),
                        Ok(()) => {},
                    }
                    break;
                }
                changed = self.manager_status.changed(), if manager_status_open => {
                    match changed {
                        Ok(()) => {
                            let manager = self.manager_status.borrow_and_update().clone();
                            self.publisher.observe_manager(manager);
                            metrics.record(&self.status.snapshot());
                        }
                        Err(_) => {
                            // Apply the final manager state before disabling this branch.
                            let manager = self.manager_status.borrow_and_update().clone();
                            self.publisher.observe_manager(manager);
                            metrics.record(&self.status.snapshot());
                            manager_status_open = false;
                        }
                    }
                }
                _ = interval.tick() => {
                    self.publisher.observe_evidence(self.evidence.snapshot());
                    metrics.record(&self.status.snapshot());
                }
                _ = local_endpoint_interval.tick() => {
                    let sidecar = tokio::select! {
                        _ = self.shutdown.cancelled() => continue,
                        result = outbe_radicle::integration::query_sidecar(
                            &self.control_socket,
                            std::time::Duration::from_secs(5),
                        ) => result,
                    };
                    match sidecar {
                        Ok(sidecar) if sidecar.node_id == self.pinned_node_id => {
                            let _ = self.local_endpoint_publisher.update(
                                sidecar.node_id,
                                sidecar.addresses,
                            );
                        }
                        Ok(sidecar) => {
                            self.local_endpoint_publisher.unavailable();
                            tracing::error!(
                                expected_node_id = ?self.pinned_node_id,
                                actual_node_id = ?sidecar.node_id,
                                "Radicle sidecar NodeId changed; endpoint publication suppressed"
                            );
                        }
                        Err(error) => {
                            self.local_endpoint_publisher.unavailable();
                            tracing::warn!(
                                %error,
                                "Radicle sidecar identity refresh failed; endpoint publication suppressed"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, B256};
    use commonware_cryptography::{bls12381, Signer as _};
    use metrics_util::debugging::{DebugValue, DebuggingRecorder};
    use outbe_primitives::code_registry::RepoId;
    use outbe_radicle::{
        endpoint::{EndpointAddress, PeerId, VerifiedEndpoint},
        integration::{
            EndpointEvidenceHandle, LocalEndpointIdentity, LocalEndpointIdentityChannel,
            ObservedSnapshotReader, RadicleStatusChannel, RadicleVotingGate,
            RadicleVotingGateError, PRODUCTION_REPAIR_INTERVAL,
        },
        manager::{
            BoxFuture, ControlSession, DisconnectDisposition, EndpointResolver, FinalizedBlock,
            FinalizedFeed, FinalizedSnapshot, FinalizedValidator, HeartwoodControl, ManagerConfig,
            ManagerDependencies, ManagerError, ManagerPhase, ManagerStatus, PhaseError,
            RadicleManager, RepositoryStatus, RetryPolicy, SnapshotReader,
        },
    };
    use std::{
        collections::BTreeMap,
        os::unix::net::{UnixListener, UnixStream},
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc, Mutex,
        },
        thread::JoinHandle,
        time::Duration,
    };
    use tokio::sync::{mpsc, watch};

    const LOCAL_NODE: [u8; 32] = [0x42; 32];

    fn validator_address() -> Address {
        Address::repeat_byte(0x11)
    }

    fn test_peer_id() -> PeerId {
        let key = bls12381::PrivateKey::from_seed(7).public_key();
        PeerId::from_public_key(&key)
    }

    fn block(number: u64) -> FinalizedBlock {
        FinalizedBlock {
            number,
            hash: B256::repeat_byte(number as u8),
        }
    }

    fn active_snapshot(number: u64) -> FinalizedSnapshot {
        FinalizedSnapshot {
            block: block(number),
            validators: vec![FinalizedValidator {
                address: validator_address(),
                peer: test_peer_id(),
                node_id: Some(LOCAL_NODE),
            }],
            registry_generation: 1,
            repositories: Vec::new(),
        }
    }

    struct InstantEofSocket {
        path: PathBuf,
        probed: Arc<AtomicBool>,
        stop: Arc<AtomicBool>,
        worker: Option<JoinHandle<()>>,
        _dir: tempfile::TempDir,
    }

    impl InstantEofSocket {
        fn start() -> Self {
            let dir = tempfile::tempdir().expect("create test socket tempdir");
            let path = dir.path().join("control.sock");
            let listener = UnixListener::bind(&path).expect("bind test control socket");
            let probed = Arc::new(AtomicBool::new(false));
            let stop = Arc::new(AtomicBool::new(false));
            let worker_probed = Arc::clone(&probed);
            let worker_stop = Arc::clone(&stop);
            let worker = std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else {
                        break;
                    };
                    if worker_stop.load(Ordering::SeqCst) {
                        break;
                    }
                    worker_probed.store(true, Ordering::SeqCst);
                    drop(stream);
                }
            });
            Self {
                path,
                probed,
                stop,
                worker: Some(worker),
                _dir: dir,
            }
        }
    }

    impl Drop for InstantEofSocket {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            let _ = UnixStream::connect(&self.path);
            if let Some(worker) = self.worker.take() {
                worker.join().expect("join test control socket worker");
            }
        }
    }

    struct FakeFinalizedFeed {
        sample: Mutex<Option<FinalizedBlock>>,
        tx: Mutex<Option<mpsc::UnboundedSender<FinalizedBlock>>>,
    }

    impl FakeFinalizedFeed {
        fn new(initial: Option<FinalizedBlock>) -> Self {
            Self {
                sample: Mutex::new(initial),
                tx: Mutex::new(None),
            }
        }

        fn push(&self, next: FinalizedBlock) {
            *self.sample.lock().expect("lock sample") = Some(next);
            if let Some(tx) = self.tx.lock().expect("lock tx").as_ref() {
                let _ = tx.send(next);
            }
        }
    }

    impl FinalizedFeed for FakeFinalizedFeed {
        fn subscribe(&self) -> Result<mpsc::UnboundedReceiver<FinalizedBlock>, ManagerError> {
            let (tx, rx) = mpsc::unbounded_channel();
            *self.tx.lock().expect("lock tx") = Some(tx);
            Ok(rx)
        }

        fn sample(&self) -> BoxFuture<'_, Result<Option<FinalizedBlock>, ManagerError>> {
            let value = *self.sample.lock().expect("lock sample");
            Box::pin(async move { Ok(value) })
        }
    }

    struct FakeSnapshotReader {
        snapshots: Mutex<BTreeMap<u64, FinalizedSnapshot>>,
    }

    impl FakeSnapshotReader {
        fn new(initial: FinalizedSnapshot) -> Self {
            let mut snapshots = BTreeMap::new();
            snapshots.insert(initial.block.number, initial);
            Self {
                snapshots: Mutex::new(snapshots),
            }
        }

        fn insert(&self, snapshot: FinalizedSnapshot) {
            self.snapshots
                .lock()
                .expect("lock snapshots")
                .insert(snapshot.block.number, snapshot);
        }
    }

    impl SnapshotReader for FakeSnapshotReader {
        fn read_exact(&self, block: FinalizedBlock) -> Result<FinalizedSnapshot, ManagerError> {
            self.snapshots
                .lock()
                .expect("lock snapshots")
                .get(&block.number)
                .cloned()
                .ok_or_else(|| ManagerError::Snapshot(format!("missing block {}", block.number)))
        }
    }

    struct FakeEndpointResolver;

    impl EndpointResolver for FakeEndpointResolver {
        fn refresh<'a>(
            &'a self,
            _snapshot: &'a FinalizedSnapshot,
        ) -> BoxFuture<'a, Result<Vec<VerifiedEndpoint>, ManagerError>> {
            Box::pin(async { Ok(Vec::new()) })
        }
    }

    struct FakeHeartwoodControl {
        node_id: [u8; 32],
        fail_sessions: Arc<AtomicBool>,
    }

    impl HeartwoodControl for FakeHeartwoodControl {
        fn node_id(&self) -> BoxFuture<'_, Result<[u8; 32], ManagerError>> {
            let id = self.node_id;
            Box::pin(async move { Ok(id) })
        }

        fn sessions(&self) -> BoxFuture<'_, Result<Vec<ControlSession>, ManagerError>> {
            let fail = self.fail_sessions.load(Ordering::SeqCst);
            Box::pin(async move {
                if fail {
                    Err(ManagerError::Control("sidecar unavailable".into()))
                } else {
                    Ok(Vec::new())
                }
            })
        }

        fn connect<'a>(
            &'a self,
            _node_id: [u8; 32],
            addresses: &'a [EndpointAddress],
        ) -> BoxFuture<'a, Result<EndpointAddress, ManagerError>> {
            let first = addresses[0].clone();
            Box::pin(async move { Ok(first) })
        }

        fn disconnect<'a>(
            &'a self,
            _node_id: [u8; 32],
            _expected: &'a EndpointAddress,
        ) -> BoxFuture<'a, Result<DisconnectDisposition, ManagerError>> {
            Box::pin(async { Ok(DisconnectDisposition::AlreadyAbsent) })
        }

        fn seed(&self, _repo: RepoId) -> BoxFuture<'_, Result<(), ManagerError>> {
            Box::pin(async { Ok(()) })
        }
    }

    struct FakeRepositoryStatus;

    impl RepositoryStatus for FakeRepositoryStatus {
        fn available(&self, _repo: RepoId) -> BoxFuture<'_, Result<bool, ManagerError>> {
            Box::pin(async { Ok(true) })
        }
    }

    fn spawn_observer(
        manager_status: watch::Receiver<ManagerStatus>,
        publisher: outbe_radicle::integration::RadicleStatusPublisher,
        status: outbe_radicle::integration::RadicleStatusHandle,
        control_socket: PathBuf,
    ) -> (
        tokio_util::sync::CancellationToken,
        tokio::task::JoinHandle<()>,
    ) {
        let shutdown = tokio_util::sync::CancellationToken::new();
        let (local_endpoint_publisher, _local_endpoint) =
            LocalEndpointIdentityChannel::create(LocalEndpointIdentity {
                validator: validator_address(),
                node_id: LOCAL_NODE,
                addresses: Vec::new(),
            });
        let handle = tokio::spawn(
            ObserverLoop {
                shutdown: shutdown.clone(),
                outcome: outbe_execution::shutdown::NodeShutdown::default(),
                local_receipts: None,
                manager_status,
                evidence: EndpointEvidenceHandle::default(),
                publisher,
                status,
                control_socket,
                pinned_node_id: LOCAL_NODE,
                local_endpoint_publisher,
            }
            .run(),
        );
        (shutdown, handle)
    }

    #[tokio::test(start_paused = true)]
    async fn observer_opens_joiner_gate_without_advancing_one_second_timer() {
        let socket = InstantEofSocket::start();
        let (publisher, status) = RadicleStatusChannel::enabled(validator_address(), LOCAL_NODE);
        let (manager_tx, manager_rx) = watch::channel(ManagerStatus {
            phase: ManagerPhase::JoiningUnbound,
            last_seen_finalized: Some(block(10)),
            ..ManagerStatus::default()
        });
        let (shutdown, observer) = spawn_observer(
            manager_rx,
            publisher.clone(),
            status.clone(),
            socket.path.clone(),
        );

        // Wait until t = 0 ticks and spawn_blocking sidecar probe finish at virtual t = 0.5s.
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(socket.probed.load(Ordering::SeqCst));
        assert_eq!(
            status.snapshot().manager.last_seen_finalized,
            Some(block(10))
        );
        assert_eq!(status.snapshot().voting_gate, RadicleVotingGate::Verifier);

        let mut status_rx = status.subscribe();
        let measured_start = tokio::time::Instant::now();
        publisher.observe_snapshot(&active_snapshot(11));
        assert_eq!(status.snapshot().voting_gate, RadicleVotingGate::Verifier);
        manager_tx.send_replace(ManagerStatus {
            phase: ManagerPhase::Ready,
            last_seen_finalized: Some(block(11)),
            last_converged_finalized: Some(block(11)),
            ..ManagerStatus::default()
        });

        status_rx
            .wait_for(|snapshot| snapshot.voting_gate == RadicleVotingGate::SignerAllowed)
            .await
            .expect("status channel stays open");
        assert_eq!(measured_start.elapsed(), Duration::ZERO);

        shutdown.cancel();
        observer.await.expect("observer exits cleanly");
    }

    #[tokio::test(start_paused = true)]
    async fn observer_preserves_first_readiness_sessions_failure_refusal() {
        let socket = InstantEofSocket::start();
        let (publisher, status) = RadicleStatusChannel::enabled(validator_address(), LOCAL_NODE);
        let feed = Arc::new(FakeFinalizedFeed::new(Some(block(10))));
        let raw_snapshots = Arc::new(FakeSnapshotReader::new(active_snapshot(10)));
        let observed_snapshots = Arc::new(ObservedSnapshotReader::new(
            raw_snapshots.clone() as Arc<dyn SnapshotReader>,
            publisher.clone(),
        ));
        let fail_sessions = Arc::new(AtomicBool::new(true));
        let manager = RadicleManager::start(
            ManagerConfig {
                self_validator: validator_address(),
                local_node_id: LOCAL_NODE,
                repair_interval: PRODUCTION_REPAIR_INTERVAL,
                retry: RetryPolicy::default(),
            },
            ManagerDependencies {
                finality: feed.clone(),
                snapshots: observed_snapshots,
                endpoints: Arc::new(FakeEndpointResolver),
                control: Arc::new(FakeHeartwoodControl {
                    node_id: LOCAL_NODE,
                    fail_sessions: Arc::clone(&fail_sessions),
                }),
                repository_status: Arc::new(FakeRepositoryStatus),
            },
        );
        let (shutdown, observer) = spawn_observer(
            manager.subscribe(),
            publisher.clone(),
            status.clone(),
            socket.path.clone(),
        );
        let mut status_rx = status.subscribe();
        feed.push(block(10));

        let observed = status_rx
            .wait_for(|snapshot| {
                snapshot.voting_gate
                    == RadicleVotingGate::Fatal(RadicleVotingGateError::SidecarUnavailable)
            })
            .await
            .expect("status channel stays open")
            .clone();
        assert_eq!(observed.manager.phase, ManagerPhase::JoiningUnbound);
        assert_eq!(
            observed.manager.phase_error,
            Some(PhaseError::SidecarUnavailable)
        );

        // Fatal proves ever_ready stayed false through the initial sessions() failure.
        // Recover on the next finalized block through manager.subscribe() and the observer.
        fail_sessions.store(false, Ordering::SeqCst);
        raw_snapshots.insert(active_snapshot(11));
        feed.push(block(11));
        let recovered = status_rx
            .wait_for(|snapshot| snapshot.voting_gate == RadicleVotingGate::SignerAllowed)
            .await
            .expect("status channel stays open")
            .clone();
        assert_eq!(recovered.manager.phase, ManagerPhase::Ready);
        assert_eq!(recovered.manager.last_converged_finalized, Some(block(11)));

        shutdown.cancel();
        observer.await.expect("observer exits cleanly");
        manager.shutdown().await.expect("manager exits cleanly");

        // Startup-ready latch stays open when the observer applies JoiningUnbound without errors.
        let startup_socket = InstantEofSocket::start();
        let (startup_pub, startup_status) =
            RadicleStatusChannel::enabled(validator_address(), LOCAL_NODE);
        startup_pub.observe_snapshot(&active_snapshot(20));
        startup_pub.mark_startup_ready();
        let (_startup_tx, startup_rx) = watch::channel(ManagerStatus {
            phase: ManagerPhase::JoiningUnbound,
            last_seen_finalized: Some(block(20)),
            ..ManagerStatus::default()
        });
        let (startup_shutdown, startup_observer) = spawn_observer(
            startup_rx,
            startup_pub,
            startup_status.clone(),
            startup_socket.path.clone(),
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(
            startup_status.snapshot().voting_gate,
            RadicleVotingGate::SignerAllowed
        );
        startup_shutdown.cancel();
        startup_observer
            .await
            .expect("startup observer exits cleanly");
    }

    #[tokio::test(start_paused = true)]
    async fn observer_handles_coalesced_updates_transient_ready_degradation_and_closed_watch() {
        // Post-ready degradation preserves SignerAllowed and loop metrics stay delta-idempotent.
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        let recorder_guard = metrics::set_default_local_recorder(&recorder);

        let socket = InstantEofSocket::start();
        let (publisher, status) = RadicleStatusChannel::enabled(validator_address(), LOCAL_NODE);
        let (manager_tx, manager_rx) = watch::channel(ManagerStatus {
            phase: ManagerPhase::JoiningUnbound,
            last_seen_finalized: Some(block(10)),
            ..ManagerStatus::default()
        });
        let (shutdown, observer) = spawn_observer(
            manager_rx,
            publisher.clone(),
            status.clone(),
            socket.path.clone(),
        );
        let mut status_rx = status.subscribe();
        publisher.observe_snapshot(&active_snapshot(11));
        manager_tx.send_replace(ManagerStatus {
            phase: ManagerPhase::Ready,
            last_seen_finalized: Some(block(11)),
            last_converged_finalized: Some(block(11)),
            ..ManagerStatus::default()
        });
        status_rx
            .wait_for(|snapshot| snapshot.voting_gate == RadicleVotingGate::SignerAllowed)
            .await
            .expect("status channel stays open");

        let ready_values = snapshotter.snapshot().into_vec();
        let signer_allowed = ready_values
            .iter()
            .find(|(key, _, _, _)| key.key().name() == "outbe_radicle_voting_gate_signer_allowed")
            .expect("missing outbe_radicle_voting_gate_signer_allowed");
        match &signer_allowed.3 {
            DebugValue::Gauge(value) => assert_eq!(value.into_inner(), 1.0),
            other => panic!("expected gauge, got {other:?}"),
        }

        manager_tx.send_replace(ManagerStatus {
            phase: ManagerPhase::RuntimeDegraded,
            phase_error: None,
            last_seen_finalized: Some(block(12)),
            last_converged_finalized: Some(block(11)),
            uds_failures: 1,
            ..ManagerStatus::default()
        });
        let degraded = status_rx
            .wait_for(|snapshot| snapshot.manager.phase == ManagerPhase::RuntimeDegraded)
            .await
            .expect("status channel stays open")
            .clone();
        assert_eq!(degraded.voting_gate, RadicleVotingGate::SignerAllowed);
        assert_eq!(degraded.manager.uds_failures, 1);

        // Advance virtual time across two 1s evidence ticks to verify delta-idempotent loop metrics.
        tokio::time::sleep(Duration::from_millis(2500)).await;
        let degraded_values = snapshotter.snapshot().into_vec();
        let uds_counter = degraded_values
            .iter()
            .find(|(key, _, _, _)| key.key().name() == "outbe_radicle_uds_failures_total")
            .expect("missing outbe_radicle_uds_failures_total");
        match &uds_counter.3 {
            DebugValue::Counter(value) => assert_eq!(*value, 1),
            other => panic!("expected counter, got {other:?}"),
        }
        drop(recorder_guard);

        // A closed watch disables the changed() branch. Virtual sleep completes only when the
        // observer parks instead of spinning on an always-ready branch. The C4 Err(_) final
        // sample is defensive and is not separately proven from Ok(()) delivery here.
        manager_tx.send_replace(ManagerStatus {
            phase: ManagerPhase::RuntimeDegraded,
            last_seen_finalized: Some(block(13)),
            provider_failures: 2,
            ..ManagerStatus::default()
        });
        drop(manager_tx);
        status_rx
            .wait_for(|snapshot| snapshot.manager.provider_failures == 2)
            .await
            .expect("status channel stays open");
        tokio::time::sleep(Duration::from_millis(50)).await;
        shutdown.cancel();
        observer
            .await
            .expect("observer exits cleanly after closed watch");

        // Coalesced Ready -> RuntimeDegraded before observation keeps Verifier until Ready is observed.
        let coalesced_socket = InstantEofSocket::start();
        let (coalesced_pub, coalesced_status) =
            RadicleStatusChannel::enabled(validator_address(), LOCAL_NODE);
        let (coalesced_tx, coalesced_rx) = watch::channel(ManagerStatus {
            phase: ManagerPhase::JoiningUnbound,
            last_seen_finalized: Some(block(10)),
            ..ManagerStatus::default()
        });
        coalesced_tx.send_replace(ManagerStatus {
            phase: ManagerPhase::Ready,
            last_seen_finalized: Some(block(11)),
            ..ManagerStatus::default()
        });
        coalesced_tx.send_replace(ManagerStatus {
            phase: ManagerPhase::RuntimeDegraded,
            phase_error: None,
            last_seen_finalized: Some(block(12)),
            ..ManagerStatus::default()
        });
        coalesced_pub.observe_snapshot(&active_snapshot(12));
        let (coalesced_shutdown, coalesced_observer) = spawn_observer(
            coalesced_rx,
            coalesced_pub,
            coalesced_status.clone(),
            coalesced_socket.path.clone(),
        );
        let mut coalesced_status_rx = coalesced_status.subscribe();
        let coalesced_snapshot = coalesced_status_rx
            .wait_for(|snapshot| snapshot.manager.last_seen_finalized == Some(block(12)))
            .await
            .expect("status channel stays open")
            .clone();
        assert_eq!(coalesced_snapshot.voting_gate, RadicleVotingGate::Verifier);
        coalesced_shutdown.cancel();
        coalesced_observer
            .await
            .expect("coalesced observer exits cleanly");
    }
}
