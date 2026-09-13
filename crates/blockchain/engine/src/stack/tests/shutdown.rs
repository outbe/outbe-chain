use super::*;

/// Real lookup transport, endpoint service and manager, under the same retained
/// supervision task as production. Only the external finalized feed is gated.
#[test]
fn application_drain_retains_transport_on_terminal_startup_and_panic_paths() {
    use outbe_radicle::{integration::*, manager::*};

    struct GatedFeed {
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }
    impl FinalizedFeed for GatedFeed {
        fn subscribe(
            &self,
        ) -> Result<tokio::sync::mpsc::UnboundedReceiver<FinalizedBlock>, ManagerError> {
            Ok(tokio::sync::mpsc::unbounded_channel().1)
        }
        fn sample(&self) -> BoxFuture<'_, Result<Option<FinalizedBlock>, ManagerError>> {
            Box::pin(async {
                self.entered.notify_one();
                self.release.notified().await;
                Ok(None)
            })
        }
    }
    struct NoSnapshot;
    impl SnapshotReader for NoSnapshot {
        fn read_exact(&self, _: FinalizedBlock) -> Result<FinalizedSnapshot, ManagerError> {
            panic!("feed has no finalized snapshot");
        }
    }

    for outcome in 0..5 {
        commonware_tokio::Runner::new(commonware_tokio::Config::default().with_worker_threads(2))
            .start(|context| async move {
                let entered = Arc::new(tokio::sync::Notify::new());
                let release = Arc::new(tokio::sync::Notify::new());
                let (_, status) = RadicleStatusChannel::enabled(Address::ZERO, [1; 32]);
                let (endpoint, resolver, _) = EndpointNetwork::build(
                    outbe_radicle::endpoint::ChainIdentity {
                        chain_id: 1,
                        genesis_hash: B256::ZERO,
                    },
                    status,
                );
                let manager = RadicleManager::start(
                    ManagerConfig {
                        self_validator: Address::ZERO,
                        local_node_id: [1; 32],
                        repair_interval: Duration::from_secs(30),
                        retry: RetryPolicy::default(),
                    },
                    ManagerDependencies {
                        finality: Arc::new(GatedFeed {
                            entered: entered.clone(),
                            release: release.clone(),
                        }),
                        snapshots: Arc::new(NoSnapshot),
                        endpoints: Arc::new(resolver.clone()),
                        control: Arc::new(NativeHeartwoodControl::new(
                            "/unused-radicle-test-control".into(),
                            Duration::from_secs(1),
                        )),
                        repository_status: Arc::new(
                            HttpRepositoryStatus::new(
                                "127.0.0.1:1".parse().unwrap(),
                                Duration::from_secs(1),
                            )
                            .unwrap(),
                        ),
                    },
                );
                entered.notified().await;
                let endpoint_owner = EndpointTaskOwner::default();
                let drain_owner = endpoint_owner.clone();
                let drain_resolver = resolver.clone();
                let (draining_tx, draining_rx) = tokio::sync::oneshot::channel();
                let application = crate::application_shutdown::ApplicationDrain::new(async move {
                    draining_tx.send(()).unwrap();
                    shutdown_bounded(
                        Duration::from_secs(5),
                        manager,
                        drain_owner.shutdown(&drain_resolver),
                    )
                    .await
                    .map_err(Into::into)
                });
                let owner_drain = application.clone();
                let inner_drain = application.clone();
                let (network_tx, network_rx) = tokio::sync::oneshot::channel();
                let mut stack =
                    context
                        .child("retained_application_owner")
                        .spawn(move |ctx| async move {
                            owner_drain
                                .finish(async move {
                                    let signer = PrivateKey::from_seed(11);
                                    let cfg = lookup::Config::local(
                                        signer.clone(),
                                        b"radicle-shutdown-test",
                                        "127.0.0.1:0".parse().unwrap(),
                                        NZUsize!(32),
                                        1024 * 1024,
                                    );
                                    let (mut network, _oracle) =
                                        lookup::Network::new(ctx.child("network"), cfg);
                                    let (sender, receiver) =
                                        network.register(42, Quota::per_second(NZU32!(64)));
                                    assert!(network_tx.send(network.start()).is_ok());
                                    let (_, local) = LocalEndpointIdentityChannel::create(
                                        LocalEndpointIdentity {
                                            validator: Address::ZERO,
                                            node_id: [1; 32],
                                            addresses: vec![
                                                outbe_radicle::endpoint::EndpointAddress::dns(
                                                    "local.example",
                                                    8776,
                                                )
                                                .unwrap(),
                                            ],
                                        },
                                    );
                                    assert!(endpoint_owner
                                        .start(endpoint.run(sender, receiver, signer, local))
                                        .unwrap());
                                    match outcome {
                                        0 => {
                                            let mut engine =
                                                ctx.child("engine").spawn(|engine| async move {
                                                    let _ = engine.stopped().await;
                                                });
                                            supervise_epoch_loop_result(
                                                &ctx,
                                                Err(eyre::eyre!("VRF expiry witness")),
                                                &mut engine,
                                                &inner_drain,
                                            )
                                            .await?;
                                            Ok(())
                                        }
                                        1 => Err(eyre::eyre!("startup failure witness")),
                                        2 => panic!("protocol panic witness"),
                                        4 => {
                                            let _ = ctx.stopped().await;
                                            Ok(())
                                        }
                                        _ => Ok(()),
                                    }
                                })
                                .await
                        });
                let mut network = network_rx.await.unwrap();
                let signal_stop = if outcome == 4 {
                    let application = application.clone();
                    let shutdown = context.child("external_stop");
                    Some(tokio::spawn(async move {
                        application.drain().await.unwrap();
                        shutdown
                            .stop(0, Some(Duration::from_secs(5)))
                            .await
                            .unwrap();
                    }))
                } else {
                    None
                };
                tokio::time::timeout(Duration::from_secs(2), draining_rx)
                    .await
                    .unwrap()
                    .unwrap();
                assert!(
                    context.stopped().now_or_never().is_none(),
                    "transport stopped before manager drain"
                );
                // The real endpoint is still servicing its interface while the
                // manager has an accepted in-flight provider operation.
                resolver
                    .refresh(&FinalizedSnapshot {
                        block: FinalizedBlock {
                            number: 0,
                            hash: B256::ZERO,
                        },
                        validators: vec![],
                        registry_generation: 0,
                        repositories: vec![],
                    })
                    .await
                    .unwrap();
                assert!((&mut stack).now_or_never().is_none());
                assert!(
                    (&mut network).now_or_never().is_none(),
                    "network ended while manager was draining"
                );
                release.notify_one();
                let result = tokio::time::timeout(Duration::from_secs(8), &mut stack)
                    .await
                    .unwrap()
                    .unwrap();
                application
                    .drain()
                    .await
                    .expect("endpoint must ACK and join before transport closes");
                match outcome {
                    0 => {
                        assert!(format!("{:#}", result.unwrap_err()).contains("VRF expiry witness"))
                    }
                    1 => {
                        assert!(format!("{:#}", result.unwrap_err())
                            .contains("startup failure witness"))
                    }
                    2 => assert!(
                        format!("{:#}", result.unwrap_err()).contains("protocol panic witness")
                    ),
                    _ => result.unwrap(),
                }
                // The retained owner may return after global-stop guards are
                // acknowledged but before the network publishes its result.
                // Its supervision tree then cancels that task. Both outcomes
                // prove termination; a panic remains an error. Crucially, the
                // endpoint ACK/join was already required to succeed above.
                let transport = tokio::time::timeout(Duration::from_secs(2), &mut network)
                    .await
                    .unwrap();
                assert!(matches!(
                    transport,
                    Ok(()) | Err(commonware_runtime::Error::Closed)
                ));
                if let Some(stop) = signal_stop {
                    stop.await.unwrap();
                }
            });
    }
}

#[derive(Clone)]
struct ShutdownNullSender<P> {
    participants: Vec<P>,
    votes: Option<mpsc::Sender<()>>,
}

struct ShutdownNullCheckedSender<P> {
    recipients: Vec<P>,
    votes: Option<mpsc::Sender<()>>,
}

impl<P> CheckedSender for ShutdownNullCheckedSender<P>
where
    P: commonware_cryptography::PublicKey,
{
    type PublicKey = P;

    fn recipients(&self) -> Vec<Self::PublicKey> {
        self.recipients.clone()
    }

    fn send(self, _message: impl Into<IoBufs> + Send, _priority: bool) -> Unreliable<Feedback> {
        if let Some(votes) = self.votes {
            let _ = votes.send(());
        }
        Unreliable::Outcome(Feedback::Ok)
    }
}

impl<P> LimitedSender for ShutdownNullSender<P>
where
    P: commonware_cryptography::PublicKey,
{
    type PublicKey = P;
    type Checked<'a>
        = ShutdownNullCheckedSender<P>
    where
        Self: 'a;

    fn check(
        &mut self,
        recipients: Recipients<Self::PublicKey>,
    ) -> Result<Self::Checked<'_>, SystemTime> {
        let recipients = match recipients {
            Recipients::All => self.participants.clone(),
            Recipients::Some(recipients) => recipients,
            Recipients::One(recipient) => vec![recipient],
        };
        Ok(ShutdownNullCheckedSender {
            recipients,
            votes: self.votes.clone(),
        })
    }
}

#[derive(Debug)]
struct ShutdownNullReceiver<P>(PhantomData<P>);

impl<P> Receiver for ShutdownNullReceiver<P>
where
    P: commonware_cryptography::PublicKey,
{
    type Error = Infallible;
    type PublicKey = P;

    async fn recv(&mut self) -> Result<Message<Self::PublicKey>, Self::Error> {
        std::future::pending().await
    }
}

#[derive(Clone)]
struct ShutdownNullBlocker<P>(PhantomData<P>);

impl<P> Blocker for ShutdownNullBlocker<P>
where
    P: commonware_cryptography::PublicKey,
{
    type PublicKey = P;

    fn block(&mut self, _peer: Self::PublicKey) -> Feedback {
        Feedback::Ok
    }
    fn blocked(&mut self) -> commonware_p2p::BlockedSubscription<Self::PublicKey> {
        let (_, receiver) = commonware_utils::channel::ring::channel(NZUsize!(1));
        receiver
    }
}

#[test]
fn global_stop_completes_with_a_stopping_sibling_and_real_voter() {
    let storage = tempfile::tempdir().expect("stack shutdown test storage");
    let config = commonware_tokio::Config::default()
        .with_worker_threads(1)
        .with_max_blocking_threads(1)
        .with_catch_panics(true)
        .with_storage_directory(storage.path());
    let runner = commonware_tokio::Runner::new(config);

    runner.start(|context| async move {
        let epoch = Epoch::new(1);
        let signing_key = PrivateKey::from_seed(7);
        let public_key = signing_key.public_key();
        let participants = Set::from_iter_dedup([public_key.clone()]);
        let dkg = bootstrap_dkg(1).expect("single-validator DKG fixture");
        let scheme = HybridScheme::<MinSig>::signer(
            &outbe_consensus::config::outbe_app_namespace(),
            participants,
            signing_key,
            dkg.polynomial,
            dkg.shares[0].clone(),
        )
        .expect("single-validator hybrid signer");

        let (votes_tx, votes_rx) = mpsc::channel();
        let sender = ShutdownNullSender {
            participants: vec![public_key.clone()],
            votes: None,
        };
        let vote_network = (
            ShutdownNullSender { votes: Some(votes_tx), ..sender.clone() },
            ShutdownNullReceiver::<commonware_cryptography::bls12381::PublicKey>(PhantomData),
        );
        let certificate_network = (
            sender.clone(),
            ShutdownNullReceiver::<commonware_cryptography::bls12381::PublicKey>(PhantomData),
        );
        let resolver_network = (
            sender,
            ShutdownNullReceiver::<commonware_cryptography::bls12381::PublicKey>(PhantomData),
        );

        let engine_config = SimplexConfig {
            scheme,
            elector: RoundRobin::<Sha256>::default(),
            blocker: ShutdownNullBlocker(PhantomData),
            automaton: MockAutomaton::new(public_key),
            relay: MockRelay::new(),
            reporter: MockReporter::new(),
            strategy: Sequential,
            forward: ForwardPolicy::Disabled,
            partition: "stack_shutdown_voter_journal".to_owned(),
            epoch,
            floor: Floor::Genesis(mock_genesis(epoch)),
            mailbox_size: NZUsize!(64),
            leader_timeout: Duration::from_millis(10),
            certification_timeout: Duration::from_millis(20),
            timeout_retry: Duration::from_millis(40),
            view_retention: ViewDelta::new(16),
            skip: commonware_consensus::simplex::SkipPolicy::Disabled,
            track_historical_votes: true,
            fetch_timeout: Duration::from_millis(20),

            replay_buffer: NZUsize!(64 * 1024),
            write_buffer: NZUsize!(4 * 1024),
            page_cache: CacheRef::from_pooler(
                &context,
                NonZeroU16::new(1024).unwrap(),
                NZUsize!(10),
            ),
        };
        let engine = SimplexEngine::new(context.child("engine"), engine_config);
        let engine_handle = engine.start(vote_network, certificate_network, resolver_network);

        votes_rx.recv_timeout(Duration::from_secs(2)).expect("real voter emits a durable vote");

        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let blocking_handle =
            context
                .child("blocking_gate")
                .shared(true)
                .spawn(move |_| async move {
                    started_tx.send(()).expect("report blocking worker start");
                    release_rx.recv_timeout(Duration::from_secs(3)).expect("release blocking worker");
                });
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("sole blocking worker must be occupied");

        let network_handle = context.child("network").spawn(|network| async move {
            let _ = network.stopped().await;
        });
        let mut stack_owner = context.child("stack_owner").spawn(move |owner| async move {
            let mut shutdown = owner.stopped();
            let mut network_handle = network_handle;
            let mut engine_handle = engine_handle;
            commonware_macros::select! {
                _ = &mut shutdown => {
                    let action = supervise_epoch_loop_result(
                        &owner,
                        Ok(EpochLoopOutcome::GlobalStop),
                        &mut engine_handle,
                        &crate::application_shutdown::ApplicationDrain::default(),
                    )
                    .await
                    .expect("simplex engine must stop successfully");
                    assert_eq!(action, EpochLoopAction::ExitStack);
                },
                _ = &mut network_handle => panic!("global stop must take precedence over its stopping sibling")
            }
        });

        let stop_handle = context
            .child("shutdown")
            .spawn(|shutdown| async move { shutdown.stop(0, Some(Duration::from_secs(1))).await });

        let (result, ()) = futures::join!(&mut stack_owner, async {
            context.sleep(Duration::from_millis(50)).await;
            release_tx.send(()).expect("release blocking worker");
        });
        blocking_handle.await.expect("blocking worker completes");
        result.expect("stack owner must finish on global stop");
        stop_handle
            .await
            .expect("shutdown driver must finish")
            .expect("global shutdown must complete");
    });
}

#[test]
fn fatal_stack_exit_preserves_error_and_voter_journal_can_resume() {
    let storage = tempfile::tempdir().expect("fatal stack exit test storage");
    let epoch = Epoch::new(1);
    let signing_key = PrivateKey::from_seed(13);
    let public_key = signing_key.public_key();
    let participants = Set::from_iter_dedup([public_key.clone()]);
    let dkg = bootstrap_dkg(1).expect("single-validator DKG fixture");
    let scheme = HybridScheme::<MinSig>::signer(
        &outbe_consensus::config::outbe_app_namespace(),
        participants,
        signing_key,
        dkg.polynomial,
        dkg.shares[0].clone(),
    )
    .expect("single-validator hybrid signer");
    let first_scheme = scheme.clone();
    let first_public_key = public_key.clone();
    let config = commonware_tokio::Config::default()
        .with_worker_threads(1)
        .with_max_blocking_threads(1)
        .with_catch_panics(true)
        .with_storage_directory(storage.path());
    let runner = commonware_tokio::Runner::new(config);

    runner.start(|context| async move {
        let (votes_tx, votes_rx) = mpsc::channel();
        let vote_sender = ShutdownNullSender {
            participants: vec![first_public_key.clone()],
            votes: Some(votes_tx),
        };
        let certificate_sender = ShutdownNullSender {
            participants: vec![first_public_key.clone()],
            votes: None,
        };
        let resolver_sender = ShutdownNullSender {
            participants: vec![first_public_key.clone()],
            votes: None,
        };
        let vote_network = (
            vote_sender,
            ShutdownNullReceiver::<commonware_cryptography::bls12381::PublicKey>(PhantomData),
        );
        let certificate_network = (
            certificate_sender,
            ShutdownNullReceiver::<commonware_cryptography::bls12381::PublicKey>(PhantomData),
        );
        let resolver_network = (
            resolver_sender,
            ShutdownNullReceiver::<commonware_cryptography::bls12381::PublicKey>(PhantomData),
        );

        let engine_config = SimplexConfig {
            scheme: first_scheme,
            elector: RoundRobin::<Sha256>::default(),
            blocker: ShutdownNullBlocker(PhantomData),
            automaton: MockAutomaton::new(first_public_key),
            relay: MockRelay::new(),
            reporter: MockReporter::new(),
            strategy: Sequential,
            forward: ForwardPolicy::Disabled,
            partition: "fatal_stack_exit_voter_journal".to_owned(),
            epoch,
            floor: Floor::Genesis(mock_genesis(epoch)),
            mailbox_size: NZUsize!(64),
            leader_timeout: Duration::from_millis(200),
            certification_timeout: Duration::from_millis(400),
            timeout_retry: Duration::from_millis(800),
            view_retention: ViewDelta::new(16),
            skip: commonware_consensus::simplex::SkipPolicy::Disabled,
            track_historical_votes: true,
            fetch_timeout: Duration::from_millis(20),

            replay_buffer: NZUsize!(64 * 1024),
            write_buffer: NZUsize!(4 * 1024),
            page_cache: CacheRef::from_pooler(
                &context,
                NonZeroU16::new(1024).unwrap(),
                NZUsize!(10),
            ),
        };

        let (engine_ready_tx, engine_ready_rx) = mpsc::sync_channel(1);
        let (fatal_tx, fatal_rx) = tokio::sync::oneshot::channel::<()>();
        let mut stack_owner = context
            .child("fatal_stack_owner")
            .spawn(move |owner| async move {
                let engine = SimplexEngine::new(owner.child("engine"), engine_config);
                let mut engine_handle =
                    engine.start(vote_network, certificate_network, resolver_network);
                engine_ready_tx
                    .send(())
                    .expect("report initialized voter journal");
                fatal_rx.await.expect("drive fatal stack exit");
                supervise_epoch_loop_result(
                    &owner,
                    Err(eyre::eyre!("synthetic fatal stack cause")),
                    &mut engine_handle,
                    &crate::application_shutdown::ApplicationDrain::default(),
                )
                .await
            });
        engine_ready_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("real voter engine must start");
        votes_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("real voter emits a durable vote");

        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let blocking_handle = context
            .child("fatal_stack_blocking_gate")
            .shared(true)
            .spawn(move |_| async move {
                started_tx.send(()).expect("report blocking worker start");
                release_rx
                    .recv_timeout(Duration::from_secs(3))
                    .expect("release blocking worker");
            });
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("sole blocking worker must be occupied");

        // Stop with the I/O worker occupied; do not assume that the upstream
        // Engine handle joins its final journal sync.
        context.sleep(Duration::from_millis(250)).await;
        fatal_tx.send(()).expect("trigger fatal stack exit");
        let (result, ()) = futures::join!(&mut stack_owner, async {
            context.sleep(Duration::from_millis(50)).await;
            release_tx.send(()).expect("release blocking worker");
        });
        blocking_handle.await.expect("blocking worker completes");
        let result = result
            .expect("stack owner task finishes")
            .expect_err("the original fatal stack result must be preserved");
        assert!(
            result.to_string().contains("synthetic fatal stack cause"),
            "fatal result: {result:#}"
        );
    });

    let reopen_config = commonware_tokio::Config::default()
        .with_worker_threads(1)
        .with_max_blocking_threads(1)
        .with_catch_panics(true)
        .with_storage_directory(storage.path());
    commonware_tokio::Runner::new(reopen_config).start(|context| async move {
        let (votes_tx, votes_rx) = mpsc::channel();
        let sender = ShutdownNullSender {
            participants: vec![public_key.clone()],
            votes: None,
        };
        let vote_network = (
            ShutdownNullSender {
                votes: Some(votes_tx),
                ..sender.clone()
            },
            ShutdownNullReceiver::<commonware_cryptography::bls12381::PublicKey>(PhantomData),
        );
        let certificate_network = (
            sender.clone(),
            ShutdownNullReceiver::<commonware_cryptography::bls12381::PublicKey>(PhantomData),
        );
        let resolver_network = (
            sender,
            ShutdownNullReceiver::<commonware_cryptography::bls12381::PublicKey>(PhantomData),
        );
        let engine_config = SimplexConfig {
            scheme,
            elector: RoundRobin::<Sha256>::default(),
            blocker: ShutdownNullBlocker(PhantomData),
            automaton: MockAutomaton::new(public_key),
            relay: MockRelay::new(),
            reporter: MockReporter::new(),
            strategy: Sequential,
            forward: ForwardPolicy::Disabled,
            partition: "fatal_stack_exit_voter_journal".to_owned(),
            epoch,
            floor: Floor::Genesis(mock_genesis(epoch)),
            mailbox_size: NZUsize!(64),
            leader_timeout: Duration::from_millis(200),
            certification_timeout: Duration::from_millis(400),
            timeout_retry: Duration::from_millis(800),
            view_retention: ViewDelta::new(16),
            skip: commonware_consensus::simplex::SkipPolicy::Disabled,
            track_historical_votes: true,
            fetch_timeout: Duration::from_millis(20),

            replay_buffer: NZUsize!(64 * 1024),
            write_buffer: NZUsize!(4 * 1024),
            page_cache: CacheRef::from_pooler(
                &context,
                NonZeroU16::new(1024).unwrap(),
                NZUsize!(10),
            ),
        };
        let engine = SimplexEngine::new(context.child("reopened_engine"), engine_config);
        let engine_handle = engine.start(vote_network, certificate_network, resolver_network);

        votes_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("reopened journal must resume outbound voting");

        let stop_handle = context
            .child("reopened_shutdown")
            .spawn(|shutdown| async move { shutdown.stop(0, Some(Duration::from_secs(1))).await });
        engine_handle
            .await
            .expect("reopened engine must stop normally");
        stop_handle
            .await
            .expect("reopened shutdown driver must finish")
            .expect("reopened runtime shutdown must complete");
    });
}

#[test]
fn completed_engine_outcome_is_not_polled_twice() {
    commonware_tokio::Runner::default().start(|context| async move {
        let mut engine_handle = context.child("completed_engine").spawn(|_| async {});
        (&mut engine_handle)
            .await
            .expect("test engine task must complete once");

        let action = supervise_epoch_loop_result(
            &context,
            Ok(EpochLoopOutcome::EngineExit(Ok(()))),
            &mut engine_handle,
            &crate::application_shutdown::ApplicationDrain::default(),
        )
        .await
        .expect("an already observed engine exit must stop the stack without repolling the handle");

        assert_eq!(action, EpochLoopAction::ExitStack);
    });
}

#[test]
fn terminal_drain_diagnostic_preserves_the_original_stack_error_chain() {
    let error = preserve_stack_result_after_drain::<()>(
        Err(eyre::eyre!("original engine exit")),
        Err(eyre::eyre!("secondary global stop timeout")),
    )
    .expect_err("both failures must remain observable");

    assert!(
        error.to_string().contains("secondary global stop timeout"),
        "outer diagnostic: {error:#}"
    );
    assert!(
        error
            .chain()
            .any(|cause| cause.to_string().contains("original engine exit")),
        "original source chain: {error:#}"
    );
}

#[derive(Default)]
struct MockBlockHashProvider {
    hashes: BTreeMap<u64, B256>,
}

impl BlockHashReader for MockBlockHashProvider {
    fn block_hash(&self, number: u64) -> ProviderResult<Option<B256>> {
        Ok(self.hashes.get(&number).copied())
    }

    fn canonical_hashes_range(&self, start: u64, end: u64) -> ProviderResult<Vec<B256>> {
        Ok((start..end)
            .filter_map(|height| self.hashes.get(&height).copied())
            .collect())
    }
}

#[test]
fn provider_matches_consensus_tip_checks_height_and_hash() {
    let digest = outbe_consensus::digest::Digest(B256::repeat_byte(0x11));
    let tip = crate::marshal_update_reporter::ConsensusTip {
        round: Round::new(Epoch::new(0), View::new(7)),
        height: Height::new(42),
        digest,
    };

    let mut provider = MockBlockHashProvider::default();
    provider.hashes.insert(42, digest.0);

    assert!(provider_matches_consensus_tip(&provider, tip, 41).unwrap());
    assert!(provider_matches_consensus_tip(&provider, tip, 42).unwrap());
    assert!(!provider_matches_consensus_tip(&provider, tip, 43).unwrap());

    provider.hashes.insert(42, B256::repeat_byte(0x22));
    assert!(!provider_matches_consensus_tip(&provider, tip, 42).unwrap());

    provider.hashes.clear();
    assert!(!provider_matches_consensus_tip(&provider, tip, 42).unwrap());
}

#[test]
fn execution_watchdog_decision_covers_core_states() {
    let started = SystemTime::UNIX_EPOCH;
    let after_startup = started
        + Duration::from_secs(config::EXECUTION_WATCHDOG_STARTUP_GRACE_SEC)
        + Duration::from_secs(1);
    let after_fatal_grace = after_startup + config::EXECUTION_WATCHDOG_GRACE;

    let (decision, next_unhealthy_since) = execution_watchdog_decision(
        ExecutionWatchdogObservation::ProviderState {
            consensus_tip_height: 100,
            reth_head_height: 100,
            hash_match: true,
        },
        after_startup,
        started,
        Some(started),
    );
    assert_eq!(decision, ExecutionWatchdogDecision::Healthy);
    assert_eq!(next_unhealthy_since, None);

    let (decision, next_unhealthy_since) = execution_watchdog_decision(
        ExecutionWatchdogObservation::ProviderState {
            consensus_tip_height: 100,
            reth_head_height: 0,
            hash_match: true,
        },
        after_startup,
        started,
        Some(started),
    );
    assert_eq!(decision, ExecutionWatchdogDecision::Healthy);
    assert_eq!(next_unhealthy_since, None);

    let (decision, next_unhealthy_since) = execution_watchdog_decision(
        ExecutionWatchdogObservation::ProviderState {
            consensus_tip_height: 100,
            reth_head_height: 0,
            hash_match: false,
        },
        started + Duration::from_secs(1),
        started,
        None,
    );
    assert_eq!(decision, ExecutionWatchdogDecision::StartupGrace);
    assert_eq!(next_unhealthy_since, None);

    let (decision, next_unhealthy_since) = execution_watchdog_decision(
        ExecutionWatchdogObservation::ProviderState {
            consensus_tip_height: config::EXECUTION_WATCHDOG_LAG_BLOCKS + 2,
            reth_head_height: 0,
            hash_match: false,
        },
        after_startup,
        started,
        None,
    );
    assert_eq!(
        decision,
        ExecutionWatchdogDecision::Unhealthy {
            unhealthy_for: Duration::ZERO,
        }
    );
    assert_eq!(next_unhealthy_since, Some(after_startup));

    let (decision, next_unhealthy_since) = execution_watchdog_decision(
        ExecutionWatchdogObservation::ProviderState {
            consensus_tip_height: 100,
            reth_head_height: 100,
            hash_match: false,
        },
        after_fatal_grace,
        started,
        Some(after_startup),
    );
    assert_eq!(
        decision,
        ExecutionWatchdogDecision::Fatal {
            unhealthy_for: config::EXECUTION_WATCHDOG_GRACE,
        }
    );
    assert_eq!(next_unhealthy_since, Some(after_startup));

    let (decision, next_unhealthy_since) = execution_watchdog_decision(
        ExecutionWatchdogObservation::ProviderReadError,
        after_fatal_grace,
        started,
        Some(after_startup),
    );
    assert_eq!(
        decision,
        ExecutionWatchdogDecision::Fatal {
            unhealthy_for: config::EXECUTION_WATCHDOG_GRACE,
        }
    );
    assert_eq!(next_unhealthy_since, Some(after_startup));
}
