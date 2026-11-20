//! Verification stages preserve the distinction between rejection and local unavailability.
use super::ApplicationShared;
use crate::{
    application::{
        ingress::SimplexContext,
        validation::{
            validate_rewards_beneficiary, validate_system_tx_leader_binding_for_activation,
            SystemTxLeaderValidationContext,
        },
    },
    block::ConsensusBlock,
    committee_provider::CommitteeProvider,
    digest::Digest,
    dkg_manager::{AncestryReader, ArtifactAdmissionError, BoundaryRequirement},
    finalization::util::extract_header_artifact_from_block,
    hybrid::HybridSchemeProvider,
    metrics::verify::{
        record_candidate_timestamp_ahead_ms, VerifyInFlightGuard, VoteWithheldReason,
        WithheldVoteRecord,
    },
};
use alloy_primitives::Address;
use commonware_consensus::types::Round;
use commonware_cryptography::bls12381::{primitives::variant::MinSig, PublicKey};
use commonware_utils::channel::oneshot;
use outbe_primitives::projection::ExecutionReadBudget;
use tracing::warn;

mod execution;
mod prechecks;
mod resolution;
mod verdict;

/// Owned inputs and cancellation budget for one spawned verification task.
pub(super) struct VerifyTask {
    pub(super) context: SimplexContext,
    pub(super) payload_digest: Digest,
    pub(super) response: oneshot::Sender<bool>,
    pub(super) execution_read_budget: ExecutionReadBudget,
}

/// Request identity stays in the canonical Simplex context throughout every stage.
struct VerifyRequest {
    context: SimplexContext,
    payload_digest: Digest,
}

impl VerifyRequest {
    fn parent_digest(&self) -> Digest {
        self.context.parent.1
    }
}

struct ResolvedVerifyBlocks {
    block: ConsensusBlock,
    parent_block: Option<ConsensusBlock>,
}

/// Whether the local node validates live proposals. A share-less verifier (a TEE
/// full-node with no proposer EVM address) follows FINALIZED blocks only. It skips
/// the leader-binding / DKG-boundary checks. These checks are
/// polynomial/DKG-view-dependent and would diverge on a verifier's stale
/// post-rotation state. This enum replaces a boolean-blind `is_verifier` flag so
/// the role choice is explicit in the type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ValidatorRole {
    Signer,
    VerifierOnly,
}

impl ValidatorRole {
    /// A node with no proposer EVM address is a share-less verifier-only follower.
    fn from_proposer_evm_address(proposer_evm_address: Option<Address>) -> Self {
        match proposer_evm_address {
            Some(_) => Self::Signer,
            None => Self::VerifierOnly,
        }
    }
}

/// Facts carried by one proposal being checked for header-artifact admission.
pub(super) struct HeaderArtifactRequest<'a> {
    pub(super) block: &'a ConsensusBlock,
    pub(super) parent_block: Option<&'a ConsensusBlock>,
    pub(super) round: Round,
    pub(super) proposer: &'a PublicKey,
    pub(super) role: ValidatorRole,
}

/// Existing chain policy and read-only admission dependencies, borrowed for the request.
pub(super) struct HeaderArtifactValidationDeps<'a, A: AncestryReader> {
    pub(super) chain_id: u64,
    pub(super) leader_policy: outbe_primitives::system_tx::binding::SystemTxLeaderPolicy,
    pub(super) certificate_scheme_provider: &'a HybridSchemeProvider<MinSig>,
    pub(super) committee_provider: &'a CommitteeProvider,
    pub(super) dkg_manager: &'a crate::dkg_manager::Mailbox,
    pub(super) ancestry: &'a A,
}

pub(super) async fn validate_header_consensus_artifacts_for_activation(
    request: HeaderArtifactRequest<'_>,
    deps: HeaderArtifactValidationDeps<'_, impl AncestryReader>,
) -> Result<(), ArtifactAdmissionError> {
    let HeaderArtifactRequest {
        block,
        parent_block,
        round,
        proposer,
        role,
    } = request;
    let HeaderArtifactValidationDeps {
        chain_id,
        leader_policy,
        certificate_scheme_provider,
        committee_provider,
        dkg_manager,
        ancestry,
    } = deps;
    // Finalized-follower rule: a share-less verifier (a TEE full-node, no
    // `proposer_evm_address`) does NOT validate live proposals. It follows
    // FINALIZED blocks. The reporter verifies their threshold certificate against
    // the GROUP public key (preserved across reshares). The leader-binding and
    // DKG-boundary checks below are polynomial/DKG-view-dependent and would
    // diverge on a verifier's stale post-rotation state, so this function skips
    // them. Consensus safety for the follower comes from the finalization
    // certificate, not from re-deriving the live proposal's leader. The verifier
    // never votes (`me()` is None), so accepting the proposal here cannot affect
    // the committee's quorum.
    if role == ValidatorRole::VerifierOnly {
        return Ok(());
    }
    validate_rewards_beneficiary(block)?;
    validate_system_tx_leader_binding_for_activation(
        block,
        SystemTxLeaderValidationContext {
            round,
            proposer,
            chain_id,
            leader_policy,
            certificate_scheme_provider,
            committee_provider,
        },
    )?;

    let artifact = extract_header_artifact_from_block(block)?;
    let requirement = dkg_manager
        .admit_header_artifact(parent_block, round.epoch(), artifact.as_ref(), ancestry)
        .await?;
    if requirement == BoundaryRequirement::AlreadyCommitted {
        crate::metrics::record_dkg_boundary_requirement(
            crate::metrics::DkgBoundaryDecision::AlreadyCommitted,
        );
    }
    Ok(())
}

impl ApplicationShared {
    /// One response owner races the entire decision against consensus cancellation.
    pub(super) async fn handle_verify(
        &self,
        clock: &(impl commonware_runtime::Clock + commonware_runtime::Supervisor),
        task: VerifyTask,
    ) {
        let _in_flight = VerifyInFlightGuard::new();
        let withheld_vote = WithheldVoteRecord::default();
        let VerifyTask {
            context,
            payload_digest,
            mut response,
            execution_read_budget,
        } = task;
        let request = VerifyRequest {
            context,
            payload_digest,
        };
        let decide = async {
            match self
                .decide_verification(clock, &request, &execution_read_budget, &withheld_vote)
                .await
            {
                Ok(crate::executor::ingress::VerificationOutcome::Unavailable) => {
                    withheld_vote.record(VoteWithheldReason::Unavailable);
                    std::future::pending().await
                }
                Err(error) => {
                    warn!(%error, "local verification unavailable; withholding vote until cancellation");
                    withheld_vote.record(VoteWithheldReason::VerifyError);
                    std::future::pending().await
                }
                Ok(outcome) => outcome,
            }
        };
        let outcome =
            match futures::future::select(Box::pin(response.closed()), Box::pin(decide)).await {
                futures::future::Either::Left(_) => {
                    withheld_vote.record(VoteWithheldReason::ViewCancelled);
                    execution_read_budget.cancel();
                    return;
                }
                futures::future::Either::Right((outcome, _)) => outcome,
            };
        self.publish_verify_verdict(&request, response, outcome, &withheld_vote);
    }

    async fn decide_verification(
        &self,
        clock: &(impl commonware_runtime::Clock + commonware_runtime::Supervisor),
        request: &VerifyRequest,
        execution_read_budget: &ExecutionReadBudget,
        withheld_vote: &WithheldVoteRecord,
    ) -> eyre::Result<crate::executor::ingress::VerificationOutcome> {
        use crate::executor::ingress::{PendingParent, VerificationOutcome};
        let round = request.context.round;
        if self.epoch_fence.check(round, 0).is_err() {
            return Ok(VerificationOutcome::Invalid);
        }
        let Some(resolved) = self
            .resolve_verify_blocks(clock, request, withheld_vote)
            .await?
        else {
            return Ok(VerificationOutcome::Invalid);
        };
        let candidate_ts_ms = resolved.block.timestamp_millis();
        if let Ok(local_now_ms) = self.unix_time_source.now_millis() {
            if candidate_ts_ms > local_now_ms {
                record_candidate_timestamp_ahead_ms(candidate_ts_ms - local_now_ms);
            }
        }
        let parent = resolved
            .parent_block
            .as_ref()
            .map(|block| std::sync::Arc::new(block.clone()));
        self.executor_mailbox.report_pending_parent(PendingParent {
            round,
            digest: request.parent_digest(),
            height: commonware_consensus::types::Height::new(
                parent.as_ref().map_or(0, |block| block.number()),
            ),
            block: parent,
            epoch_fence: self.epoch_fence.clone(),
        })?;
        if !self
            .validate_verify_blocks(clock, request, &resolved)
            .await?
        {
            return Ok(VerificationOutcome::Invalid);
        }
        let outcome = self
            .verify_block_execution(request, &resolved, execution_read_budget)
            .await?;
        if matches!(outcome, VerificationOutcome::Valid)
            && ValidatorRole::from_proposer_evm_address(self.proposer_evm_address)
                == ValidatorRole::Signer
        {
            self.wait_until_candidate_timestamp(clock, candidate_ts_ms)
                .await?;
        }
        Ok(
            if self
                .epoch_fence
                .check(round, resolved.block.number())
                .is_ok()
            {
                outcome
            } else {
                withheld_vote.record(VoteWithheldReason::EpochFence);
                VerificationOutcome::Unavailable
            },
        )
    }

    async fn wait_until_candidate_timestamp(
        &self,
        clock: &impl commonware_runtime::Clock,
        candidate_ts_ms: u64,
    ) -> eyre::Result<()> {
        loop {
            let now_ms = self.unix_time_source.now_millis()?;
            if now_ms >= candidate_ts_ms {
                return Ok(());
            }
            clock
                .sleep(std::time::Duration::from_millis(candidate_ts_ms - now_ms))
                .await;
        }
    }
}

#[cfg(test)]
mod tests {
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
            finalization_view: crate::finalization::state::new_finalization_view(
                B256::ZERO,
                0,
                None,
            ),
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
        let (actor, mailbox, _) =
            crate::marshal_types::MarshalActor::<deterministic::Context>::init(
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
    fn clock_telemetry_never_changes_a_valid_vote() {
        for (local_time, ahead) in [
            (Some(2999), true),
            (Some(3000), false),
            (Some(3001), false),
            (None, false),
        ] {
            let recorder = DebuggingRecorder::new();
            metrics::with_local_recorder(&recorder, || {
                deterministic::Runner::default().start(|context| async {
                    with_application(context, async |mut shared, mut executor, clock| {
                        shared.unix_time_source = Arc::new(TestClock(local_time));
                        let block = candidate();
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
                            let Some(Message::VerifyBlock(execution)) = executor.next().await
                            else {
                                panic!("missing execution request");
                            };
                            execution.response.send(VerificationOutcome::Valid).unwrap();
                        };
                        let ((), ()) = futures::join!(verify, execute);
                        assert!(receiver.await.unwrap());
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
                            let Some(Message::VerifyBlock(execution)) = executor.next().await
                            else {
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
                        let wait = clock.sleep(
                            crate::config::VERIFY_RESOLUTION_TIMEOUT + Duration::from_millis(1),
                        );
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

    fn permissive_test_leader_policy() -> outbe_primitives::system_tx::binding::SystemTxLeaderPolicy
    {
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
            let artifacts = outbe_primitives::reshare_artifact::decode_outbe_block_artifacts(
                header.extra_data(),
            )
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

    #[test]
    fn verifier_only_does_not_sleep_and_signer_with_broken_clock_withholds_verify_error() {
        use commonware_runtime::Clock as _;
        deterministic::Runner::default().start(|context| async {
            with_application(context, async |mut shared, mut executor, clock| {
                let start = clock.current();
                shared.proposer_evm_address = None;
                shared.unix_time_source = Arc::new(DeterministicUnixTimeSource {
                    clock: clock.child("time_source"),
                    start,
                    base_ms: 2500,
                });
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
}
