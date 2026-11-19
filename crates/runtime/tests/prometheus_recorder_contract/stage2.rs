//! Stage 2 production emitters and their explicit Prometheus wire contract.

#[path = "../../src/tee_canary_probe/test_support.rs"]
mod resident_fixture;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use alloy_consensus::{SignableTransaction as _, TxLegacy};
use alloy_primitives::{Address, Bytes, Signature, TxKind};
use commonware_actor::{Feedback, Unreliable};
use commonware_cryptography::bls12381;
use commonware_p2p::{CheckedSender, LimitedSender, Recipients};
use commonware_runtime::IoBufs;
use outbe_consensus::{
    follow::{metrics as follower, upstream::UpstreamTipError},
    metrics::{executor, p2p, propose, verify},
};
use outbe_execution::payload_builder::{
    record_payload_build_outcome, record_payload_rlp_size, record_payload_sparse_trie_fallback,
    record_payload_state_root_duration, PayloadTxSkipReason,
};
use outbe_primitives::{
    consensus::{ConsensusExecutionBridge, ConsensusOperatingMode},
    OutbeBlock, OutbeBlockBody, OutbeHeader, OutbePrimitives,
};
use outbe_tee::metrics::*;
use outbe_txpool::{
    maintain::{
        maintain_outbe_pool, run_staleness_tick, OutbePoolMaintainConfig, PendingStalenessTracker,
    },
    record_txpool_rejection, record_txpool_validation_duration, TxpoolRejectionReason,
    TxpoolRejectionReason::*,
};
use reth_basic_payload_builder::BuildOutcome;
use reth_primitives_traits::RecoveredBlock;
use reth_provider::{CanonStateNotification, CanonStateSubscriptions, Chain};
use reth_transaction_pool::{
    test_utils::{testing_pool, MockTransactionFactory},
    TransactionOrigin, TransactionPool,
};

pub async fn emit() {
    canary::emit_ticks().await;
    emit_tee_client();
    emit_staleness().await;
    emit_consensus();
    emit_equivocation();
    emit_p2p();
    emit_follower();
    emit_payload();
    emit_txpool().await;
    let _in_flight = outbe_tee::metrics::ExecutionInFlightGuard::new();
    outbe_tee::metrics::record_tee_execution_lock_wait_ms(Duration::from_millis(3));
}

fn emit_tee_client() {
    record_request_duration("get_public_keys", Duration::from_millis(5));
    record_request_error("get_public_keys", "codec");
}

async fn emit_staleness() {
    let pool = testing_pool();
    let transaction = MockTransactionFactory::default().create_eip1559();
    pool.add_transaction(TransactionOrigin::External, transaction.transaction)
        .await
        .expect("admit pending transaction");
    let mut tracker = PendingStalenessTracker::new(10);
    assert!(run_staleness_tick(&pool, &mut tracker, 100).is_empty());
    assert_eq!(run_staleness_tick(&pool, &mut tracker, 110).len(), 1);
}

fn emit_consensus() {
    let bridge = ConsensusExecutionBridge::new();
    for mode in [
        ConsensusOperatingMode::UpstreamBootstrap,
        ConsensusOperatingMode::Verifier,
        ConsensusOperatingMode::Signer,
        ConsensusOperatingMode::Follower,
    ] {
        bridge.set_operating_mode(mode);
    }
    for reason in [
        verify::VoteWithheldReason::Unavailable,
        verify::VoteWithheldReason::VerifyError,
        verify::VoteWithheldReason::EpochFence,
        verify::VoteWithheldReason::PublishEpochFence,
        verify::VoteWithheldReason::ViewCancelled,
        verify::VoteWithheldReason::ResolveTimeoutOrMissing,
    ] {
        verify::WithheldVoteRecord::default().record(reason);
    }
    let _in_flight = verify::VerifyInFlightGuard::new();
    executor::record_block_verify_execution_ms(Duration::from_millis(4));
    verify::record_block_verify_decision_ms(Duration::from_millis(6));
    verify::record_candidate_timestamp_ahead_ms(8);
    propose::record_block_build_resolve_pad_ms(Duration::from_millis(3));
    for outcome in [
        propose::ProposalOutcomeLabel::Proposed,
        propose::ProposalOutcomeLabel::Cancelled,
        propose::ProposalOutcomeLabel::PaceCancelled,
        propose::ProposalOutcomeLabel::ParentProofUnavailable,
        propose::ProposalOutcomeLabel::EpochStale,
        propose::ProposalOutcomeLabel::BoundaryUnavailable,
        propose::ProposalOutcomeLabel::ExecutionUnavailable,
        propose::ProposalOutcomeLabel::RoundAlreadyProposed,
        propose::ProposalOutcomeLabel::Failed,
    ] {
        propose::record_proposal_outcome(outcome);
    }
    propose::record_parent_ahead_of_local_clock();
    propose::record_self_finalized_block();
    executor::record_executor_queue_entries(2);
    executor::record_executor_queue_wait_ms(Duration::from_millis(5));
    executor::record_executor_finalization_lag_blocks(1);
    executor::record_executor_convergence_depth_blocks(2);
}

fn emit_equivocation() {
    use outbe_consensus::metrics::{self, EquivocationKind::*};
    use outbe_runtime::equivocation_slasher as slasher;
    metrics::record_equivocation_verify_admission("enqueued");
    let labels = [("result", "enqueued")];
    assert_counter("consensus_equivocation_verify_admission", &labels);
    for (kind, label) in [
        (ConflictingNotarize, "conflicting_notarize"),
        (ConflictingFinalize, "conflicting_finalize"),
        (NullifyFinalize, "nullify_finalize"),
    ] {
        slasher::record_equivocation_submitted(kind);
        assert_counter("slashing_equivocation_submitted", &[("kind", label)]);
        for result in ["verified", "invalid_signature"] {
            metrics::record_equivocation_verify(kind, result);
            let labels = [("kind", label), ("result", result)];
            assert_counter("consensus_equivocation_verify", &labels);
        }
    }
    for reason in [
        "already_processed",
        "already_penalized",
        "expired_epoch",
        "unregistered_signer",
        "not_slashable",
        "submitter_not_active",
        "sender_busy",
        "rejected",
    ] {
        slasher::record_equivocation_skipped(reason);
        assert_counter("slashing_equivocation_skipped", &[("reason", reason)]);
    }
    for reason in ["state_read", "submit"] {
        slasher::record_equivocation_submit_failed(reason);
        assert_counter("slashing_equivocation_submit_failed", &[("reason", reason)]);
    }
}

fn assert_counter(metric: &str, labels: &[(&str, &str)]) {
    let recorder = outbe_runtime::install_outbe_prometheus_recorder().expect("recorder");
    let rendered = recorder.handle().render();
    let name = format!("reth_outbe_{metric}_total");
    assert!(rendered.contains(&format!("# TYPE {name} counter\n")));
    assert!(has_labeled_sample(&rendered, &name, labels));
}

#[derive(Clone, Copy)]
struct TestSender(Option<Unreliable<Feedback>>);

impl LimitedSender for TestSender {
    type PublicKey = bls12381::PublicKey;
    type Checked<'a> = Self;

    fn check(&mut self, _: Recipients<Self::PublicKey>) -> Result<Self, std::time::SystemTime> {
        self.0.ok_or(std::time::SystemTime::UNIX_EPOCH)?;
        Ok(*self)
    }
}

impl CheckedSender for TestSender {
    type PublicKey = bls12381::PublicKey;

    fn recipients(&self) -> Vec<Self::PublicKey> {
        Vec::new()
    }

    fn send(self, _: impl Into<IoBufs> + Send, _: bool) -> Unreliable<Feedback> {
        self.0.expect("checked sender has feedback")
    }
}

fn emit_p2p() {
    for channel in [
        p2p::P2pChannel::Dkg,
        p2p::P2pChannel::TeeDkg,
        p2p::P2pChannel::TeeBootstrap,
    ] {
        for feedback in [None, Some(Unreliable::Rejected)] {
            assert!(p2p::send_observed(
                &mut TestSender(feedback),
                p2p::ObservedSend {
                    channel,
                    recipients: Recipients::All,
                    expected_peers: Some(1),
                    bytes: Bytes::from_static(b"recorder contract"),
                    priority: false,
                },
            )
            .is_empty());
        }
    }
    p2p::record_tee_bootstrap_pending(2, 64);
    outbe_radicle::integration::record_p2p_send_rate_limited();
    outbe_radicle::integration::record_p2p_send_skipped_peer();
    outbe_radicle::integration::record_p2p_send_rejected();
}

fn emit_follower() {
    for err in [
        UpstreamTipError::HttpRateLimited {
            source: jsonrpsee::core::client::Error::Custom("rate limited".into()),
        },
        UpstreamTipError::HttpStatus {
            status_code: 503,
            source: jsonrpsee::core::client::Error::Custom("bad gateway".into()),
        },
        UpstreamTipError::Transport {
            source: jsonrpsee::core::client::Error::Custom("transport".into()),
        },
        UpstreamTipError::InvalidResponse {
            source: jsonrpsee::core::client::Error::Custom("invalid tip".into()),
        },
    ] {
        follower::record_upstream_tip_err(&err);
    }
    follower::record_upstream_tip_ok(100, 105.0);
    follower::record_follower_heights(Some(98), Some(100));
    for round in [
        follower::FollowerRoundResult::Idle,
        follower::FollowerRoundResult::Pulled,
        follower::FollowerRoundResult::CeilingClamped,
        follower::FollowerRoundResult::TipUnreachable,
    ] {
        follower::record_follower_round(round);
    }
    for (kind, reason) in [
        (
            follower::FollowerResolverKind::Block,
            follower::FollowerResolverFailureReason::Timeout,
        ),
        (
            follower::FollowerResolverKind::Block,
            follower::FollowerResolverFailureReason::PermitSaturated,
        ),
        (
            follower::FollowerResolverKind::Block,
            follower::FollowerResolverFailureReason::MissingOrError,
        ),
        (
            follower::FollowerResolverKind::Block,
            follower::FollowerResolverFailureReason::HashMismatch,
        ),
        (
            follower::FollowerResolverKind::Block,
            follower::FollowerResolverFailureReason::Oversized,
        ),
        (
            follower::FollowerResolverKind::Block,
            follower::FollowerResolverFailureReason::UnannotatedBlock,
        ),
        (
            follower::FollowerResolverKind::Block,
            follower::FollowerResolverFailureReason::DeliveryRejected,
        ),
        (
            follower::FollowerResolverKind::Finalized,
            follower::FollowerResolverFailureReason::ProofUnauthenticated,
        ),
    ] {
        follower::record_resolver_failure(kind, reason);
    }
}

fn emit_payload() {
    outbe_evm::executor::record_zero_fee_soft_failure_metric();
    outbe_evm::executor::record_zero_fee_cap_exceeded_metric();
    for outcome in [
        BuildOutcome::Better {
            payload: (),
            cached_reads: Default::default(),
        },
        BuildOutcome::Aborted {
            fees: Default::default(),
            cached_reads: Default::default(),
        },
        BuildOutcome::Cancelled,
    ] {
        record_payload_build_outcome(&Ok(outcome), Duration::from_millis(7));
    }
    record_payload_state_root_duration(Duration::from_millis(2));
    record_payload_sparse_trie_fallback();
    record_payload_rlp_size(128);
    for reason in [
        PayloadTxSkipReason::GasLimit,
        PayloadTxSkipReason::RlpSizeLimit,
        PayloadTxSkipReason::BlobRejected,
        PayloadTxSkipReason::GasAboveAvailable,
        PayloadTxSkipReason::NonceTooLow,
        PayloadTxSkipReason::SponsoredBudgetDeferred,
        PayloadTxSkipReason::EnclaveWitnessBudgetDeferred,
        PayloadTxSkipReason::InvalidTx,
    ] {
        reason.record();
    }
}

struct Subscription(
    Mutex<Option<tokio::sync::broadcast::Receiver<CanonStateNotification<OutbePrimitives>>>>,
);

impl CanonStateSubscriptions for Subscription {
    type Primitives = OutbePrimitives;

    fn subscribe_to_canonical_state(
        &self,
    ) -> reth_chain_state::CanonStateNotifications<OutbePrimitives> {
        self.0
            .lock()
            .expect("subscription lock")
            .take()
            .expect("one subscriber")
    }
}

const TXPOOL_REJECTIONS: &[(TxpoolRejectionReason, &str)] = &[
    (UpstreamInvalid, "upstream_invalid"),
    (UpstreamError, "upstream_error"),
    (ReservedSystemTx, "reserved_system_tx"),
    (Overdraft, "overdraft"),
    (StateReadFailed, "state_read_failed"),
    (UnknownHook, "unknown_hook"),
    (NonZeroValue, "non_zero_value"),
    (CalldataTooLarge, "calldata_too_large"),
    (GasLimitTooHigh, "gas_limit_too_high"),
    (FeeCapTooLow, "fee_cap_too_low"),
    (MalformedCalldata, "malformed_calldata"),
    (UnauthorizedSigner, "unauthorized_signer"),
    (HookConflict, "hook_conflict"),
    (Storage, "storage"),
    (FreeTxDailyExhausted, "free_tx_daily_exhausted"),
    (
        FreeTxDailyContractCreationForbidden,
        "free_tx_daily_contract_creation_forbidden",
    ),
    (FreeTxDailyValueNotZero, "free_tx_daily_value_not_zero"),
    (
        FreeTxDailyGasLimitExceeded,
        "free_tx_daily_gas_limit_exceeded",
    ),
    (
        FreeTxDailyCalldataTooLarge,
        "free_tx_daily_calldata_too_large",
    ),
    (
        FreeTxDailyTargetNotWhitelisted,
        "free_tx_daily_target_not_whitelisted",
    ),
];

async fn emit_txpool() {
    for &(reason, _) in TXPOOL_REJECTIONS {
        record_txpool_rejection(reason);
    }
    record_txpool_validation_duration(Duration::from_millis(2));
    for queued_only in [false, true] {
        let pool = testing_pool();
        let mut tx = MockTransactionFactory::default()
            .create_eip1559()
            .transaction;
        if queued_only {
            tx.set_nonce(64);
        }
        pool.add_transaction(TransactionOrigin::External, tx)
            .await
            .expect("admit transaction for inclusion alarm");
        let (sender, receiver) = tokio::sync::broadcast::channel(1);
        let block = RecoveredBlock::new_unhashed(
            OutbeBlock {
                header: OutbeHeader::new(alloy_consensus::Header {
                    number: 2,
                    timestamp: 100,
                    ..Default::default()
                }),
                body: OutbeBlockBody {
                    transactions: vec![TxLegacy {
                        to: TxKind::Call(outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS),
                        ..Default::default()
                    }
                    .into_signed(Signature::test_signature())
                    .into()],
                    ..Default::default()
                },
            },
            vec![Address::ZERO],
        );
        sender
            .send(CanonStateNotification::Commit {
                new: Arc::new(Chain::new([block], Default::default(), Default::default())),
            })
            .expect("queue canonical block");
        drop(sender);
        maintain_outbe_pool(
            Subscription(Mutex::new(Some(receiver))),
            pool,
            OutbePoolMaintainConfig {
                staleness_interval_secs: 4,
                queued_lifetime: Duration::from_secs(4),
                ..Default::default()
            },
        )
        .await;
    }
}

fn has_labeled_sample(rendered: &str, name: &str, expected: &[(&str, &str)]) -> bool {
    let prefix = format!("{name}{{");
    rendered.lines().any(|line| {
        !line.starts_with('#')
            && line
                .strip_prefix(&prefix)
                .and_then(|rest| rest.split_once("} "))
                .is_some_and(|(raw_labels, _)| {
                    let mut parsed: Vec<(&str, &str)> = raw_labels
                        .split(',')
                        .filter_map(|pair| pair.split_once('='))
                        .map(|(k, v)| (k, v.trim_matches('"')))
                        .collect();
                    parsed.sort_unstable();
                    let mut want = expected.to_vec();
                    want.sort_unstable();
                    parsed == want
                })
    })
}

fn assert_single_label_set(rendered: &str, name: &str, key: &str, values: &[&str]) {
    for value in values {
        assert!(
            has_labeled_sample(rendered, name, &[(key, value)]),
            "missing rendered label {name}{{{key}=\"{value}\"}}"
        );
    }
}

pub fn assert_stage2(rendered: &str, types: &HashMap<&str, &str>) {
    for (from, to) in [
        ("follower", "upstream_bootstrap"),
        ("upstream_bootstrap", "verifier"),
        ("verifier", "signer"),
        ("signer", "follower"),
    ] {
        assert!(
            has_labeled_sample(
                rendered,
                "reth_outbe_consensus_mode_transitions_total",
                &[("from", from), ("to", to)],
            ),
            "missing mode transition {from} -> {to}"
        );
    }
    assert_single_label_set(
        rendered,
        "reth_outbe_consensus_vote_withheld_total",
        "reason",
        &[
            "unavailable",
            "verify_error",
            "epoch_fence",
            "publish_epoch_fence",
            "view_cancelled",
            "resolve_timeout_or_missing",
        ],
    );
    assert_single_label_set(
        rendered,
        "reth_outbe_consensus_proposal_outcomes_total",
        "result",
        &[
            "proposed",
            "cancelled",
            "pace_cancelled",
            "parent_proof_unavailable",
            "epoch_stale",
            "boundary_unavailable",
            "execution_unavailable",
            "round_already_proposed",
            "failed",
        ],
    );
    for metric in [
        "reth_outbe_p2p_send_rate_limited_total",
        "reth_outbe_p2p_send_skipped_peers_total",
        "reth_outbe_p2p_send_rejected_total",
    ] {
        assert_single_label_set(
            rendered,
            metric,
            "channel",
            &["dkg", "tee_dkg", "tee_bootstrap", "radicle"],
        );
    }
    assert_single_label_set(
        rendered,
        "reth_outbe_follower_upstream_poll_errors_total",
        "reason",
        &["http_429", "http_status", "transport", "invalid_response"],
    );
    assert_single_label_set(
        rendered,
        "reth_outbe_follower_rounds_total",
        "result",
        &["idle", "pulled", "ceiling_clamped", "tip_unreachable"],
    );
    for (kind, reason) in [
        ("block", "timeout"),
        ("block", "permit_saturated"),
        ("block", "missing_or_error"),
        ("block", "hash_mismatch"),
        ("block", "oversized"),
        ("block", "unannotated_block"),
        ("block", "delivery_rejected"),
        ("finalized", "proof_unauthenticated"),
    ] {
        assert!(
            has_labeled_sample(
                rendered,
                "reth_outbe_follower_resolver_failures_total",
                &[("kind", kind), ("reason", reason)],
            ),
            "missing resolver failure {kind}/{reason}"
        );
    }
    assert_single_label_set(
        rendered,
        "reth_outbe_txpool_inclusion_alarms_total",
        "kind",
        &["pending_not_included", "queued_only"],
    );
    for (_, label) in TXPOOL_REJECTIONS {
        assert_single_label_set(
            rendered,
            "reth_outbe_txpool_rejected_total",
            "reason",
            &[label],
        );
    }
    assert_single_label_set(
        rendered,
        "reth_outbe_evm_payload_build_stop_total",
        "reason",
        &["cancelled", "candidates_exhausted", "aborted_not_better"],
    );
    assert_single_label_set(
        rendered,
        "reth_outbe_evm_payload_tx_skipped_total",
        "reason",
        &[
            "gas_limit",
            "rlp_size_limit",
            "blob_rejected",
            "gas_above_available",
            "nonce_too_low",
            "sponsored_budget_deferred",
            "enclave_witness_budget_deferred",
            "invalid_tx",
        ],
    );
    for (kind, names) in [
        (
            "gauge",
            &[
                "reth_outbe_consensus_operating_mode",
                "reth_outbe_consensus_verify_in_flight_blocks",
                "reth_outbe_consensus_executor_queue_entries",
                "reth_outbe_consensus_executor_finalization_lag_blocks",
                "reth_outbe_consensus_executor_convergence_depth_blocks",
                "reth_outbe_p2p_tee_bootstrap_pending_messages",
                "reth_outbe_p2p_tee_bootstrap_pending_bytes",
                "reth_outbe_follower_upstream_tip_known",
                "reth_outbe_follower_upstream_tip_height",
                "reth_outbe_follower_upstream_last_ok_timestamp_seconds",
                "reth_outbe_follower_processed_height",
                "reth_outbe_follower_supported_ceiling_height",
                "reth_outbe_txpool_canonical_tip_age_seconds",
                "reth_outbe_txpool_queued_txs",
                "reth_outbe_txpool_block_user_txs",
                "reth_outbe_txpool_pending_snapshot_txs",
                "reth_outbe_evm_payload_rlp_size_bytes",
                "reth_outbe_tee_client_execution_in_flight_requests",
            ][..],
        ),
        (
            "counter",
            &[
                "reth_outbe_consensus_mode_transitions_total",
                "reth_outbe_consensus_vote_withheld_total",
                "reth_outbe_consensus_proposal_outcomes_total",
                "reth_outbe_consensus_parent_ahead_of_local_clock_total",
                "reth_outbe_consensus_self_finalized_blocks_total",
                "reth_outbe_p2p_send_rate_limited_total",
                "reth_outbe_p2p_send_skipped_peers_total",
                "reth_outbe_p2p_send_rejected_total",
                "reth_outbe_follower_upstream_poll_errors_total",
                "reth_outbe_follower_rounds_total",
                "reth_outbe_follower_resolver_failures_total",
                "reth_outbe_txpool_inclusion_alarms_total",
                "reth_outbe_txpool_rejected_total",
                "reth_outbe_txpool_stale_evicted_total",
                "reth_outbe_evm_zero_fee_soft_failures_total",
                "reth_outbe_evm_zero_fee_cap_exceeded_total",
                "reth_outbe_evm_payload_build_stop_total",
                "reth_outbe_evm_payload_tx_skipped_total",
                "reth_outbe_evm_payload_sparse_trie_fallback_total",
            ][..],
        ),
        (
            "histogram",
            &[
                "reth_outbe_consensus_block_verify_execution_ms",
                "reth_outbe_consensus_block_verify_decision_ms",
                "reth_outbe_consensus_candidate_timestamp_ahead_ms",
                "reth_outbe_consensus_block_build_resolve_pad_ms",
                "reth_outbe_consensus_executor_queue_wait_ms",
                "reth_outbe_txpool_validation_duration_ms",
                "reth_outbe_evm_payload_build_duration_ms",
                "reth_outbe_evm_payload_state_root_duration_ms",
                "reth_outbe_tee_client_execution_lock_wait_ms",
            ][..],
        ),
    ] {
        for name in names {
            assert_eq!(types.get(name), Some(&kind), "{name}");
            let sample = if kind == "histogram" {
                format!("{name}_count")
            } else {
                (*name).to_owned()
            };
            assert!(
                super::has_sample(rendered, &sample),
                "missing sample {sample}"
            );
        }
    }
}

mod canary {
    use super::*;
    use ed25519_dalek::SigningKey;
    use outbe_execution::tee_canary::{
        run_tee_canary_worker, EnclaveRequester, TeeCanaryConfig, TeeCanaryTarget,
    };
    use outbe_runtime::TributeCanaryProbe;
    use outbe_tee::{
        canary::{TeeEnclaveHealthChannel, TeeEnclaveHealthState},
        errors::TransportError,
        protocol::{EnclaveRequest, EnclaveResponse},
    };
    use tokio_util::sync::CancellationToken;

    pub(super) async fn emit_ticks() {
        emit_resident_ticks().await;
        for expected in [TeeEnclaveHealthState::Degraded] {
            let resident = resident_fixture::context(4);
            resident_fixture::activate(&resident);
            assert_ne!(
                resident.network_encryption_public().expect("offer key"),
                TRIBUTE_OFFER_PUB
            );
            let status = TeeEnclaveHealthChannel::default();
            let shutdown = CancellationToken::new();
            let period = Duration::from_millis(5);
            let worker = tokio::spawn(run_tee_canary_worker(
                TeeCanaryTarget::new(
                    FakeEnclaveRequester {
                        key: SigningKey::from_bytes(&[9; 32]),
                        resident,
                    },
                    TributeCanaryProbe,
                ),
                TeeCanaryConfig {
                    interval: period,
                    failure_threshold: 1,
                },
                status.clone(),
                shutdown.clone(),
            ));
            let observed = observe_tick(&status, expected, period).await;
            shutdown.cancel();
            worker.await.expect("canary worker stopped");
            observed.expect("canary published expected tick");
        }
    }

    async fn emit_resident_ticks() {
        let context = resident_fixture::context(8);
        let status = TeeEnclaveHealthChannel::default();
        let shutdown = CancellationToken::new();
        let period = Duration::from_millis(5);
        let worker = tokio::spawn(TributeCanaryProbe::run_in_process_worker(
            Arc::clone(&context),
            TeeCanaryConfig {
                interval: period,
                failure_threshold: 1,
            },
            status.clone(),
            shutdown.clone(),
        ));
        tokio::time::timeout(Duration::from_secs(10), async {
            while status.snapshot().health_probe_supported != Some(false) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("pre-key resident tick");
        let before_key = status.snapshot();
        assert_eq!(before_key.state, TeeEnclaveHealthState::Starting);
        assert!(!before_key.offer_key_ready);
        assert_eq!(before_key.consecutive_failures, 0);
        assert!(before_key.last_failure.is_none());
        resident_fixture::activate(&context);
        let observed = observe_tick(
            &status,
            TeeEnclaveHealthState::Ready,
            Duration::from_secs(10),
        )
        .await;
        shutdown.cancel();
        worker.await.expect("resident worker stopped");
        observed.expect("real resident decrypt tick");
        let ready = status.snapshot();
        assert!(ready.offer_key_ready);
        assert_eq!(ready.health_probe_supported, Some(false));
        assert!(ready.enclave.is_none());
        assert!(ready.last_canary_latency_ms.is_some());
    }

    async fn observe_tick(
        status: &TeeEnclaveHealthChannel,
        expected: TeeEnclaveHealthState,
        period: Duration,
    ) -> Result<(), tokio::time::error::Elapsed> {
        // One period is the budget for observing the immediate first tick.
        tokio::time::timeout(period, async {
            while status.snapshot().state != expected {
                tokio::task::yield_now().await;
            }
        })
        .await
    }

    const TRIBUTE_OFFER_PUB: [u8; 32] = [0x11; 32];

    std::thread_local! {
        static PROBE_RESIDENT: std::cell::RefCell<
            Option<outbe_tee::in_process_dispatch::InProcessTestGuard>,
        > = const { std::cell::RefCell::new(None) };
    }

    /// The canary probe runs on the blocking pool. Install the override there.
    fn bind_mismatched_resident(context: &Arc<outbe_tee::in_process::InProcessTeeContext>) {
        PROBE_RESIDENT.with(|slot| {
            if slot.borrow().is_some() {
                return;
            }
            *slot.borrow_mut() = Some(
                outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test(Some(
                    Arc::clone(context),
                )),
            );
        });
    }

    struct FakeEnclaveRequester {
        key: SigningKey,
        resident: Arc<outbe_tee::in_process::InProcessTeeContext>,
    }

    impl EnclaveRequester for FakeEnclaveRequester {
        fn request(&self, request: &EnclaveRequest) -> Result<EnclaveResponse, TransportError> {
            bind_mismatched_resident(&self.resident);
            match request {
                EnclaveRequest::GetPublicKeys => Ok(EnclaveResponse::PublicKeys {
                    offer_key_ready: true,
                    recipient_x25519_pub: TRIBUTE_OFFER_PUB,
                    attestation_pub: self.key.verifying_key().to_bytes(),
                    noise_static_pub: [0x0C; 32],
                    tee_bls_pub: Vec::new(),
                    dkg_enc_pub: [0x0D; 32],
                    dkg_enc_sig: Vec::new(),
                }),
                _ => panic!("unexpected canary request: {request:?}"),
            }
        }

        fn attestation_pub(&self) -> Option<[u8; 32]> {
            Some(self.key.verifying_key().to_bytes())
        }
    }
}
