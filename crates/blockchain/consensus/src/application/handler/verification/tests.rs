use super::*;
use crate::application::handler::{ApplicationEpochFence, PayloadBuilder, UnixTimeSource};
use crate::executor::ingress::{Message, VerificationOutcome};
use alloy_primitives::B256;
use commonware_consensus::types::{Epoch, View, ViewDelta};
use commonware_cryptography::Signer as _;
use commonware_runtime::{deterministic, Runner as _, Supervisor as _};
use futures::StreamExt as _;
use metrics_util::debugging::{DebugValue, DebuggingRecorder};
use reth_ethereum::primitives::SealedBlock;
use std::{
    num::{NonZeroU16, NonZeroU64, NonZeroUsize},
    sync::Arc,
    time::Duration,
};

struct TestClock(Option<u64>);

impl UnixTimeSource for TestClock {
    fn now_millis(&self) -> eyre::Result<u64> {
        self.0.ok_or_else(|| eyre::eyre!("clock unavailable"))
    }
}

fn shared_for_test(
    clock: &deterministic::Context,
    marshal_mailbox: crate::marshal_types::MarshalMailbox,
    executor_tx: futures::channel::mpsc::UnboundedSender<Message>,
) -> ApplicationShared {
    let (engine_tx, _) = tokio::sync::mpsc::unbounded_channel();
    ApplicationShared {
        self_proposed_digests: Default::default(),
        unix_time_source: Arc::new(TestClock(Some(3000))),
        engine: crate::application::handler::EngineHandle::new(engine_tx),
        executor_mailbox: crate::executor::Mailbox::from_sender(executor_tx),
        marshal_mailbox,
        payload_builder: PayloadBuilder::noop(),
        genesis_hash: B256::ZERO,
        chain_id: outbe_primitives::chain::CHAIN_ID,
        validators: crate::validators::ValidatorSet {
            public_keys: Vec::new(),
            addresses: Vec::new(),
            p2p_addresses: Vec::new(),
        },
        leader_policy: crate::test_fixtures::test_leader_policy(),
        proposer_evm_address: None,
        certificate_scheme_provider: HybridSchemeProvider::new(),
        committee_provider: CommitteeProvider::new(),
        elector_config_provider: crate::hybrid::election::HybridElectorConfigProvider::new(),
        epoch_fence: ApplicationEpochFence::new(Epoch::new(0)),
        dkg_manager: crate::dkg_manager::Mailbox::new(),
        ancestry_readiness: crate::ancestry_readiness::AncestryReadiness::new(0, 0),
        vrf_safety: crate::vrf_safety::VrfSafetyGate::new(4, 0, 10_000, 100),
        block_cache: crate::finalization::block_cache::BlockCache::new(),
        finalization_view: crate::finalization::state::new_finalization_view(B256::ZERO, 0, None),
        finalization_selector: crate::finalization::selection::ParentProofSelector::new(
            crate::finalization::parent_cert_store::FinalizedParentCertStore::new(),
            crate::config::DEFAULT_PROPOSAL_TIMEOUT,
        ),
        publication: crate::application::publication::ProposalPublication::new(
            clock.child("publication"),
        ),
        repository_publication: Default::default(),
        repository_publication_enabled: false,
        payload_resolve_time: Duration::from_millis(1),
        trust_el_head: false,
        late_sig_store: crate::finalization::late_sig_store::shared(
            outbe_primitives::consensus::LATE_FINALIZE_WINDOW_K,
        ),
        proposal_failure_log_limiter: Arc::new(crate::util::rate_limit::LogRateLimiter::new(
            crate::application::handler::PROPOSAL_FAILURE_LOG_WINDOW,
        )),
    }
}

async fn with_application(
    context: deterministic::Context,
    run: impl AsyncFnOnce(
        ApplicationShared,
        futures::channel::mpsc::UnboundedReceiver<Message>,
        deterministic::Context,
    ),
) {
    use commonware_consensus::marshal::{self, Start};
    use commonware_runtime::buffer::paged::CacheRef;
    let page_cache = CacheRef::from_pooler(
        &context,
        NonZeroU16::new(1024).unwrap(),
        NonZeroUsize::new(10).unwrap(),
    );
    let items_per_section = NonZeroU64::new(10).unwrap();
    let buffer = NonZeroUsize::new(1024).unwrap();
    let archives = crate::test_fixtures::marshal::MarshalArchiveFixture {
        partition_prefix: "verify-telemetry",
        page_cache: &page_cache,
        items_per_section,
        replay_buffer: buffer,
        write_buffer: buffer,
    }
    .open(&context)
    .await
    .unwrap();
    let (actor, mailbox, _) = crate::marshal_types::MarshalActor::<deterministic::Context>::init(
        context.child("marshal"),
        archives.0,
        archives.1,
        marshal::Config {
            provider: HybridSchemeProvider::<MinSig>::new(),
            epocher: crate::follow::FollowerEpocher::new(10_000, 0),
            start: Start::Genesis(crate::test_fixtures::block_with_number(0)),
            partition_prefix: "verify-telemetry".into(),
            page_cache,
            prunable_items_per_section: items_per_section,
            replay_buffer: buffer,
            key_write_buffer: buffer,
            value_write_buffer: buffer,
            mailbox_size: NonZeroUsize::new(32).unwrap(),
            view_retention: ViewDelta::new(10_000),
            block_codec_config: (),
            max_repair: NonZeroUsize::new(10).unwrap(),
            max_pending_acks: NonZeroUsize::new(1).unwrap(),
            strategy: commonware_parallel::Sequential,
        },
    )
    .await;
    // Keep the mailbox open but idle: cache misses resolve through the real timeout.
    let (tx, rx) = futures::channel::mpsc::unbounded();
    let shared = shared_for_test(&context, mailbox, tx);
    run(shared, rx, context).await;
    drop(actor);
}

fn request(block: &ConsensusBlock) -> VerifyRequest {
    let (keys, _) = crate::test_fixtures::participants();
    VerifyRequest {
        payload_digest: block.digest(),
        context: SimplexContext {
            round: Round::new(Epoch::new(0), View::new(2)),
            parent: (View::new(1), Digest::ZERO),
            leader: keys[0].public_key(),
        },
    }
}

fn candidate() -> ConsensusBlock {
    let mut block = crate::test_fixtures::block_with_number(1)
        .into_inner()
        .into_block();
    block.header.inner.timestamp = 3;
    ConsensusBlock::from_sealed(SealedBlock::seal_slow(block))
}

fn snapshot(recorder: &DebuggingRecorder) -> std::collections::BTreeMap<String, DebugValue> {
    recorder
        .snapshotter()
        .snapshot()
        .into_vec()
        .into_iter()
        .map(|(key, _, _, value)| (key.key().name().to_owned(), value))
        .collect()
}

fn assert_withheld(
    recorder: &DebuggingRecorder,
    reason: VoteWithheldReason,
    count_delta: u64,
    in_flight_delta: f64,
) {
    let snapshot = recorder.snapshotter().snapshot().into_vec();
    let records: Vec<_> = snapshot
        .iter()
        .filter(|(key, _, _, _)| key.key().name() == "outbe_consensus_vote_withheld_total")
        .collect();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].3, DebugValue::Counter(count_delta));
    assert_eq!(
        records[0].0.key().labels().next().unwrap().value(),
        reason.as_label()
    );
    let gauge = snapshot
        .iter()
        .find(|(key, _, _, _)| key.key().name() == "outbe_consensus_verify_in_flight_blocks")
        .unwrap();
    assert_eq!(gauge.3, DebugValue::Gauge(in_flight_delta.into()));
}

#[test]
fn signer_clock_telemetry_records_ahead_ms_when_candidate_leads_local_clock() {
    use commonware_runtime::Clock as _;
    for (base_ms, ahead) in [(2999, true), (3000, false), (3001, false)] {
        let recorder = DebuggingRecorder::new();
        metrics::with_local_recorder(&recorder, || {
            deterministic::Runner::default().start(|context| async {
                with_application(context, async |mut shared, mut executor, clock| {
                    let start = clock.current();
                    shared.proposer_evm_address = Some(Address::repeat_byte(0x11));
                    shared.leader_policy = permissive_test_leader_policy();
                    shared.unix_time_source = Arc::new(DeterministicUnixTimeSource {
                        clock: clock.child("time_source"),
                        start,
                        base_ms,
                    });
                    let block = signer_candidate();
                    let request = request(&block);
                    shared.block_cache.insert_bounded(block.digest(), block);
                    let (response, receiver) = oneshot::channel();
                    let mut verify = Box::pin(shared.handle_verify(
                        &clock,
                        VerifyTask {
                            context: request.context,
                            payload_digest: request.payload_digest,
                            response,
                            execution_read_budget: ExecutionReadBudget::new(),
                        },
                    ));
                    assert!(futures::poll!(verify.as_mut()).is_pending());
                    assert!(matches!(
                        executor.next().await,
                        Some(Message::PendingParent(_))
                    ));
                    let Some(Message::VerifyBlock(execution)) = executor.next().await else {
                        panic!("missing execution request");
                    };
                    let after_exec = clock.current();
                    execution.response.send(VerificationOutcome::Valid).unwrap();
                    verify.await;
                    assert!(receiver.await.unwrap());
                    assert_eq!(
                        clock.current().duration_since(after_exec).unwrap(),
                        Duration::from_millis(u64::from(ahead))
                    );
                })
                .await;
            });
        });
        let snapshot = snapshot(&recorder);
        let expected = ahead.then(|| DebugValue::Histogram(vec![1.0.into()]));
        assert_eq!(
            snapshot.get("outbe_consensus_candidate_timestamp_ahead_ms"),
            expected.as_ref()
        );
        assert_eq!(snapshot.get("outbe_consensus_vote_withheld_total"), None);
        assert_eq!(
            snapshot.get("outbe_consensus_verify_in_flight_blocks"),
            Some(&DebugValue::Gauge(0.0.into()))
        );
        let Some(DebugValue::Histogram(decisions)) =
            snapshot.get("outbe_consensus_block_verify_decision_ms")
        else {
            panic!("missing decision timer");
        };
        assert_eq!(decisions.len(), 1);
    }
}

#[test]
fn unavailable_is_counted_before_pending_and_not_again_on_cancellation() {
    for drop_future in [false, true] {
        let recorder = DebuggingRecorder::new();
        metrics::with_local_recorder(&recorder, || {
            deterministic::Runner::default().start(|context| async {
                with_application(context, async |shared, mut executor, clock| {
                    let block = candidate();
                    let request = request(&block);
                    shared.block_cache.insert_bounded(block.digest(), block);
                    let budget = ExecutionReadBudget::new();
                    let (response, mut receiver) = oneshot::channel();
                    let mut verify = Box::pin(shared.handle_verify(
                        &clock,
                        VerifyTask {
                            context: request.context,
                            payload_digest: request.payload_digest,
                            response,
                            execution_read_budget: budget.clone(),
                        },
                    ));
                    assert!(futures::poll!(verify.as_mut()).is_pending());
                    assert!(matches!(
                        executor.next().await,
                        Some(Message::PendingParent(_))
                    ));
                    let Some(Message::VerifyBlock(execution)) = executor.next().await else {
                        panic!("missing execution request");
                    };
                    execution
                        .response
                        .send(VerificationOutcome::Unavailable)
                        .unwrap();
                    assert!(futures::poll!(verify.as_mut()).is_pending());
                    assert!(futures::poll!(&mut receiver).is_pending());
                    assert_withheld(&recorder, VoteWithheldReason::Unavailable, 1, 1.0);
                    if drop_future {
                        drop(verify);
                        assert!(receiver.await.is_err());
                    } else {
                        drop(receiver);
                        verify.await;
                        assert!(budget.is_cancelled());
                    }
                    // Snapshots consume deltas: cancellation adds no count and removes the active task.
                    assert_withheld(&recorder, VoteWithheldReason::Unavailable, 0, -1.0);
                })
                .await;
            });
        });
    }
}

#[test]
fn execution_fence_and_mailbox_errors_keep_their_first_reason() {
    for reason in [
        VoteWithheldReason::EpochFence,
        VoteWithheldReason::VerifyError,
    ] {
        let recorder = DebuggingRecorder::new();
        metrics::with_local_recorder(&recorder, || {
            deterministic::Runner::default().start(|context| async {
                with_application(context, async |shared, mut executor, clock| {
                    let block = candidate();
                    let request = request(&block);
                    shared.block_cache.insert_bounded(block.digest(), block);
                    let (response, receiver) = oneshot::channel();
                    let mut verify = Box::pin(shared.handle_verify(
                        &clock,
                        VerifyTask {
                            context: request.context,
                            payload_digest: request.payload_digest,
                            response,
                            execution_read_budget: ExecutionReadBudget::new(),
                        },
                    ));
                    if reason == VoteWithheldReason::VerifyError {
                        drop(executor);
                    } else {
                        assert!(futures::poll!(verify.as_mut()).is_pending());
                        assert!(matches!(
                            executor.next().await,
                            Some(Message::PendingParent(_))
                        ));
                        let Some(Message::VerifyBlock(execution)) = executor.next().await else {
                            panic!("missing execution request");
                        };
                        shared.epoch_fence.advance_epoch(Epoch::new(1));
                        execution.response.send(VerificationOutcome::Valid).unwrap();
                    }
                    assert!(futures::poll!(verify.as_mut()).is_pending());
                    assert_withheld(&recorder, reason, 1, 1.0);
                    drop(receiver);
                    verify.await;
                    assert_withheld(&recorder, reason, 0, -1.0);
                })
                .await;
            });
        });
    }
}

#[test]
fn resolution_timeout_is_distinct_from_early_view_cancellation() {
    use commonware_runtime::Clock as _;
    for cancelled in [false, true] {
        let recorder = DebuggingRecorder::new();
        metrics::with_local_recorder(&recorder, || {
            deterministic::Runner::default().start(|context| async {
                with_application(context, async |shared, _executor, clock| {
                    let request = request(&candidate());
                    let (response, receiver) = oneshot::channel();
                    let mut verify = Box::pin(shared.handle_verify(
                        &clock,
                        VerifyTask {
                            context: request.context,
                            payload_digest: request.payload_digest,
                            response,
                            execution_read_budget: ExecutionReadBudget::new(),
                        },
                    ));
                    if cancelled {
                        drop(receiver);
                        verify.await;
                        assert_withheld(&recorder, VoteWithheldReason::ViewCancelled, 1, 0.0);
                        return;
                    }
                    let wait = clock
                        .sleep(crate::config::VERIFY_RESOLUTION_TIMEOUT + Duration::from_millis(1));
                    assert!(matches!(
                        futures::future::select(verify.as_mut(), Box::pin(wait)).await,
                        futures::future::Either::Right(_)
                    ));
                    assert_withheld(
                        &recorder,
                        VoteWithheldReason::ResolveTimeoutOrMissing,
                        1,
                        1.0,
                    );
                    drop(receiver);
                    verify.await;
                    assert_withheld(
                        &recorder,
                        VoteWithheldReason::ResolveTimeoutOrMissing,
                        0,
                        -1.0,
                    );
                })
                .await;
            });
        });
    }
}

#[test]
fn publish_fence_drops_response_without_changing_rejections() {
    deterministic::Runner::default().start(|context| async {
        with_application(context, async |shared, _executor, _clock| {
            let request = request(&candidate());
            for (outcome, fenced, expected) in [
                (VerificationOutcome::Valid, false, Some(true)),
                (VerificationOutcome::Invalid, true, Some(false)),
                (VerificationOutcome::Valid, true, None),
            ] {
                let recorder = DebuggingRecorder::new();
                if fenced {
                    shared.epoch_fence.advance_epoch(Epoch::new(1));
                }
                let (response, receiver) = oneshot::channel();
                metrics::with_local_recorder(&recorder, || {
                    shared.publish_verify_verdict(
                        &request,
                        response,
                        outcome,
                        &WithheldVoteRecord::default(),
                    );
                });
                assert_eq!(receiver.await.ok(), expected);
                let snapshot = recorder.snapshotter().snapshot().into_vec();
                if expected.is_some() {
                    assert!(snapshot.is_empty());
                } else {
                    assert_eq!(snapshot.len(), 1);
                    assert_eq!(snapshot[0].3, DebugValue::Counter(1));
                    assert_eq!(
                        snapshot[0].0.key().labels().next().unwrap().value(),
                        "publish_epoch_fence"
                    );
                }
            }
        })
        .await;
    });
}

struct DeterministicUnixTimeSource {
    clock: deterministic::Context,
    start: std::time::SystemTime,
    base_ms: u64,
}

impl UnixTimeSource for DeterministicUnixTimeSource {
    fn now_millis(&self) -> eyre::Result<u64> {
        use commonware_runtime::Clock as _;
        let elapsed_ms = u64::try_from(
            self.clock
                .current()
                .duration_since(self.start)
                .unwrap_or_default()
                .as_millis(),
        )
        .unwrap_or(u64::MAX);
        Ok(self.base_ms.saturating_add(elapsed_ms))
    }
}

fn permissive_test_leader_policy() -> outbe_primitives::system_tx::binding::SystemTxLeaderPolicy {
    fn allow_empty_layout<'a>(
        body: &'a outbe_primitives::OutbeBlockBody,
        header: &outbe_primitives::OutbeHeader,
    ) -> Result<
        (
            outbe_primitives::system_tx::SystemTxLayout<'a>,
            outbe_primitives::reshare_artifact::OutbeBlockArtifacts,
        ),
        outbe_primitives::system_tx::binding::LayoutBindingError,
    > {
        use alloy_consensus::BlockHeader as _;
        let artifacts =
            outbe_primitives::reshare_artifact::decode_outbe_block_artifacts(header.extra_data())
                .map_err(outbe_primitives::system_tx::binding::LayoutBindingError::Artifacts)?;
        let layout = outbe_primitives::system_tx::split_system_layout(&body.transactions)
            .map_err(outbe_primitives::system_tx::binding::LayoutBindingError::Layout)?;
        Ok((layout, artifacts))
    }

    outbe_primitives::system_tx::binding::SystemTxLeaderPolicy {
        validate_layout: allow_empty_layout,
        step_name_or_stage: outbe_primitives::system_tx::SystemTx::stage_name,
    }
}

fn signer_candidate() -> ConsensusBlock {
    let mut block = crate::test_fixtures::block_with_number(1)
        .into_inner()
        .into_block();
    block.header.inner.timestamp = 3;
    block.header.inner.beneficiary = outbe_primitives::addresses::REWARDS_ADDRESS;
    block.header.inner.gas_limit = outbe_primitives::system_tx::protocol_block_gas_limit(1);
    ConsensusBlock::from_sealed(SealedBlock::seal_slow(block))
}

#[test]
fn signer_waits_until_candidate_timestamp_before_voting_valid() {
    use commonware_runtime::Clock as _;
    deterministic::Runner::default().start(|context| async {
        with_application(context, async |mut shared, mut executor, clock| {
            let start = clock.current();
            shared.proposer_evm_address = Some(Address::repeat_byte(0x11));
            shared.leader_policy = permissive_test_leader_policy();
            shared.unix_time_source = Arc::new(DeterministicUnixTimeSource {
                clock: clock.child("time_source"),
                start,
                base_ms: 2500,
            });
            let block = signer_candidate();
            let request = request(&block);
            shared.block_cache.insert_bounded(block.digest(), block);
            let (response, receiver) = oneshot::channel();
            let verify = shared.handle_verify(
                &clock,
                VerifyTask {
                    context: request.context,
                    payload_digest: request.payload_digest,
                    response,
                    execution_read_budget: ExecutionReadBudget::new(),
                },
            );
            let execute = async {
                assert!(matches!(
                    executor.next().await,
                    Some(Message::PendingParent(_))
                ));
                let Some(Message::VerifyBlock(execution)) = executor.next().await else {
                    panic!("missing execution request");
                };
                execution.response.send(VerificationOutcome::Valid).unwrap();
            };
            let ((), ()) = futures::join!(verify, execute);
            assert!(receiver.await.unwrap());
            assert_eq!(
                clock.current().duration_since(start).unwrap(),
                Duration::from_millis(500)
            );
        })
        .await;
    });
}

#[test]
fn signer_rejects_invalid_future_candidate_without_sleeping() {
    use commonware_runtime::Clock as _;
    deterministic::Runner::default().start(|context| async {
        with_application(context, async |mut shared, mut executor, clock| {
            let start = clock.current();
            shared.proposer_evm_address = Some(Address::repeat_byte(0x11));
            shared.leader_policy = permissive_test_leader_policy();
            shared.unix_time_source = Arc::new(DeterministicUnixTimeSource {
                clock: clock.child("time_source"),
                start,
                base_ms: 2500,
            });
            let block = signer_candidate();
            let request = request(&block);
            shared.block_cache.insert_bounded(block.digest(), block);
            let (response, receiver) = oneshot::channel();
            let mut verify = Box::pin(shared.handle_verify(
                &clock,
                VerifyTask {
                    context: request.context,
                    payload_digest: request.payload_digest,
                    response,
                    execution_read_budget: ExecutionReadBudget::new(),
                },
            ));
            assert!(futures::poll!(verify.as_mut()).is_pending());
            assert!(matches!(
                executor.next().await,
                Some(Message::PendingParent(_))
            ));
            let Some(Message::VerifyBlock(execution)) = executor.next().await else {
                panic!("missing execution request");
            };
            let after_exec = clock.current();
            execution
                .response
                .send(VerificationOutcome::Invalid)
                .unwrap();
            verify.await;
            assert!(!receiver.await.unwrap());
            assert_eq!(
                clock.current().duration_since(after_exec).unwrap(),
                Duration::ZERO
            );
        })
        .await;
    });
}

#[test]
fn signer_waiting_on_future_candidate_timestamp_cancels_on_dropped_receiver() {
    use commonware_runtime::Clock as _;
    let recorder = DebuggingRecorder::new();
    metrics::with_local_recorder(&recorder, || {
        deterministic::Runner::default().start(|context| async {
            with_application(context, async |mut shared, mut executor, clock| {
                let start = clock.current();
                shared.proposer_evm_address = Some(Address::repeat_byte(0x11));
                shared.leader_policy = permissive_test_leader_policy();
                shared.unix_time_source = Arc::new(DeterministicUnixTimeSource {
                    clock: clock.child("time_source"),
                    start,
                    base_ms: 2000,
                });
                let block = signer_candidate();
                let request = request(&block);
                shared.block_cache.insert_bounded(block.digest(), block);
                let (response, receiver) = oneshot::channel();
                let mut verify = Box::pin(shared.handle_verify(
                    &clock,
                    VerifyTask {
                        context: request.context,
                        payload_digest: request.payload_digest,
                        response,
                        execution_read_budget: ExecutionReadBudget::new(),
                    },
                ));
                assert!(futures::poll!(verify.as_mut()).is_pending());
                assert!(matches!(
                    executor.next().await,
                    Some(Message::PendingParent(_))
                ));
                let Some(Message::VerifyBlock(execution)) = executor.next().await else {
                    panic!("missing execution request");
                };
                let after_exec = clock.current();
                execution.response.send(VerificationOutcome::Valid).unwrap();
                assert!(futures::poll!(verify.as_mut()).is_pending());
                drop(receiver);
                verify.await;
                assert_eq!(
                    clock.current().duration_since(after_exec).unwrap(),
                    Duration::ZERO
                );
                assert_withheld(&recorder, VoteWithheldReason::ViewCancelled, 1, 0.0);
            })
            .await;
        });
    });
}

struct PanickingUnixTimeSource;

impl UnixTimeSource for PanickingUnixTimeSource {
    fn now_millis(&self) -> eyre::Result<u64> {
        panic!("VerifierOnly must not read local clock");
    }
}

#[test]
fn verifier_only_does_not_sleep_and_signer_with_broken_clock_withholds_verify_error() {
    use commonware_runtime::Clock as _;
    let verifier_recorder = DebuggingRecorder::new();
    metrics::with_local_recorder(&verifier_recorder, || {
        deterministic::Runner::default().start(|context| async {
            with_application(context, async |mut shared, mut executor, clock| {
                shared.proposer_evm_address = None;
                shared.unix_time_source = Arc::new(PanickingUnixTimeSource);
                let block = candidate();
                let request = request(&block);
                shared.block_cache.insert_bounded(block.digest(), block);
                let (response, receiver) = oneshot::channel();
                let mut verify = Box::pin(shared.handle_verify(
                    &clock,
                    VerifyTask {
                        context: request.context,
                        payload_digest: request.payload_digest,
                        response,
                        execution_read_budget: ExecutionReadBudget::new(),
                    },
                ));
                assert!(futures::poll!(verify.as_mut()).is_pending());
                assert!(matches!(
                    executor.next().await,
                    Some(Message::PendingParent(_))
                ));
                let Some(Message::VerifyBlock(execution)) = executor.next().await else {
                    panic!("missing execution request");
                };
                let after_exec = clock.current();
                execution.response.send(VerificationOutcome::Valid).unwrap();
                verify.await;
                assert!(receiver.await.unwrap());
                assert_eq!(
                    clock.current().duration_since(after_exec).unwrap(),
                    Duration::ZERO
                );
            })
            .await;
        });
    });
    assert_eq!(
        snapshot(&verifier_recorder).get("outbe_consensus_candidate_timestamp_ahead_ms"),
        None
    );

    let recorder = DebuggingRecorder::new();
    metrics::with_local_recorder(&recorder, || {
        deterministic::Runner::default().start(|context| async {
            with_application(context, async |mut shared, mut executor, clock| {
                shared.proposer_evm_address = Some(Address::repeat_byte(0x11));
                shared.leader_policy = permissive_test_leader_policy();
                shared.unix_time_source = Arc::new(TestClock(None));
                let block = signer_candidate();
                let request = request(&block);
                shared.block_cache.insert_bounded(block.digest(), block);
                let (response, receiver) = oneshot::channel();
                let mut verify = Box::pin(shared.handle_verify(
                    &clock,
                    VerifyTask {
                        context: request.context,
                        payload_digest: request.payload_digest,
                        response,
                        execution_read_budget: ExecutionReadBudget::new(),
                    },
                ));
                assert!(futures::poll!(verify.as_mut()).is_pending());
                assert!(matches!(
                    executor.next().await,
                    Some(Message::PendingParent(_))
                ));
                let Some(Message::VerifyBlock(execution)) = executor.next().await else {
                    panic!("missing execution request");
                };
                execution.response.send(VerificationOutcome::Valid).unwrap();
                assert!(futures::poll!(verify.as_mut()).is_pending());
                assert_withheld(&recorder, VoteWithheldReason::VerifyError, 1, 1.0);
                drop(receiver);
                verify.await;
                assert_withheld(&recorder, VoteWithheldReason::VerifyError, 0, -1.0);
            })
            .await;
        });
    });
}
