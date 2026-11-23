//! `FinalizationActor` - single-task consumer of finalization events.
//!
//! Drains its unbounded mailbox and applies certified ancestry in height order.
//! The actor also retries missing local archive material without blocking the
//! voter task. It owns the production per-finalization side effects:
//!
//! - Stale-round / duplicate / inconsistency replay classification
//! - One pending target and bounded unresolved finalization identities
//! - A verified tip, a backward ancestry check, and forward block application
//! - Exact Phase 1 child-witness verification (preserving CN vs F proof kinds)
//! - Durable exact-parent certificate record persistence for Phase 1
//! - VRF seed propagation, forkchoice update, last_finalized bookkeeping
//! - Bridge consensus-status publication
//! - DKG header artifact recording and boundary observation (once per block)
//! - Block-cache eviction below the new finalized height
//!
//! A new authenticated `(height, hash)` advances once after bounded attempts
//! to write its local parent certificate, even if that handoff fails. A missing
//! verifier parks before authentication. For a same-hash new-round (or repaired)
//! `Finalization`, `view.last_finalized_round` advances only after the exact
//! proof is stored; per-block effects are never repeated.

mod anchor;
mod apply;
mod catch_up;
mod events;
mod reconcile;
mod retry;
mod stall;

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use commonware_consensus::types::{Epoch, Round};
use commonware_runtime::{Clock, Spawner};
use futures::{channel::mpsc, StreamExt};
use outbe_primitives::{consensus::ConsensusExecutionBridge, error::Result};
use tracing::{info, warn};

use crate::block::ConsensusBlock;
use crate::digest::Digest;
use crate::finalization::block_cache::BlockCache;
use crate::finalization::evidence::{EventWorkBudget, VerifiedDirectFinalization};
#[cfg(test)]
use crate::finalization::evidence::{EventWorkStats, VerifiedStep};
use crate::finalization::ingress::{Finalized, Mailbox, Message};
use crate::finalization::late_sig_store::LateFinalizeCommittee;
use crate::finalization::parent_cert_store::{CertifiedParentProofStore, FinalizedParentCertStore};
use crate::finalization::state::FinalizationViewHandle;
use crate::marshal_types::MarshalMailbox;
use crate::marshal_update_reporter::ConsensusTip;
use crate::vrf_safety::VrfSafetyGate;

/// Retry only when a block or verifier is unavailable.
pub const DEFAULT_PENDING_RETRY_INTERVAL: Duration = Duration::from_secs(1);

const MARSHAL_LOCAL_READ_TIMEOUT: Duration = Duration::from_secs(1);

/// Fail-closed ceiling on unresolved live finalizations awaiting block resolution.
pub const MAX_UNRESOLVED_LIVE_TARGETS: usize = 1_024;

/// Bound on consensus-owned exact-parent certificate handoff retention.
///
/// The store uses the finalized block hash as its key. Only the Simplex context
/// parent is eligible for Phase 1. Retention remains bounded so stale local
/// recovery data cannot grow unbounded across restarts or missed slots.
pub const PARENT_CERT_KEEP_DEPTH: u64 = 256;

/// Synchronous attempts for one parent-cert store write or prune.
///
/// After this many failures the actor records a degraded metric. A new
/// authenticated block still advances; a duplicate head F retains its proof
/// for a later retry.
pub const FINALIZE_PERSIST_MAX_ATTEMPTS: usize = 3;

/// Constructor inputs for the finalization actor. One struct holds them
/// so the spawn site in `outbe-engine` (`stack::epoch::run`) stays ergonomic.
pub struct FinalizationActorDeps {
    pub view: FinalizationViewHandle,
    pub block_cache: BlockCache,
    /// Marshal mailbox for resolving a finalized block not in the local cache.
    /// `Some` in production. Only unit tests that drive `process_finalization`
    /// directly construct the actor with `None`.
    pub marshal_mailbox: Option<MarshalMailbox>,
    pub bridge: Option<ConsensusExecutionBridge>,
    pub dkg_manager: crate::dkg_manager::Mailbox,
    pub vrf_safety: VrfSafetyGate,
    /// Hash-keyed per-finalized-block exact-parent certificate store.
    pub parent_cert_store: FinalizedParentCertStore,
    /// Per-epoch verifier scheme provider.
    pub certificate_scheme_provider: crate::hybrid::HybridSchemeProvider<
        commonware_cryptography::bls12381::primitives::variant::MinSig,
    >,
    pub committee_provider: crate::committee_provider::CommitteeProvider,
    /// Shared late-finalize signature store.
    pub late_sig_store: crate::finalization::late_sig_store::SharedLateFinalizeStore,
}

/// Result of one finalization-slot persist attempt.
struct PersistFinalizationOutcome {
    /// Committee for the late-finalize rekey. `None` when the snapshot build
    /// failed or the verifier scheme was missing.
    rekey_committee: Option<LateFinalizeCommittee>,
    /// `true` only when `put_finalization` finished inside the retry budget.
    put_succeeded: bool,
    failure: Option<ProofPersistFailure>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProofPersistFailure {
    MissingVerifier,
    Metadata,
    Store,
}

/// Run `op` up to [`FINALIZE_PERSIST_MAX_ATTEMPTS`] times.
///
/// Returns the successful value. Does not clear the persist-degraded gauge.
/// The caller clears that gauge once, after the whole store cycle succeeds.
fn persist_with_budget<T, E>(
    op_name: &'static str,
    mut op: impl FnMut() -> std::result::Result<T, E>,
) -> Option<T>
where
    E: std::fmt::Display,
{
    let mut error = String::new();
    for _attempt in 0..FINALIZE_PERSIST_MAX_ATTEMPTS {
        match op() {
            Ok(value) => return Some(value),
            Err(failure) => error = failure.to_string(),
        }
    }
    crate::metrics::record_finalization_persist_degraded();
    tracing::error!(
        target: "outbe::finalization",
        op = op_name,
        attempts = FINALIZE_PERSIST_MAX_ATTEMPTS,
        %error,
        "finalization store operation failed after bounded retries; continuing in degraded mode"
    );
    None
}

/// Consume finalization events and publish verified block effects.
pub struct FinalizationActor {
    rx: mpsc::UnboundedReceiver<Message>,
    deps: FinalizationActorDeps,
    epocher: Option<crate::follow::FollowerEpocher>,
    finalization_notify: Option<Arc<tokio::sync::Notify>>,
    view_advanced: Option<tokio::sync::mpsc::UnboundedSender<u64>>,
    pending_retry_interval: Duration,
    pending_target: Option<PendingTarget>,
    head_proof_retry: Option<Box<Finalized>>,
    unresolved: BTreeMap<Round, Digest>,
    unresolved_cursor: Option<Round>,
    active_job: Option<CatchUpJob>,
    parked_probe: Option<ParkedProbe>,
    parked_reason: Option<ParkedReason>,
    resolution_stall: Option<ResolutionStall>,
    #[cfg(test)]
    last_event_stats: EventWorkStats,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ApplyOutcome {
    Applied,
    PendingPersist,
    PendingVerifier,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ParkedReason {
    MissingBlock,
    MissingVerifier,
    MissingCertificate,
    Persistence,
}

struct ResolutionStall {
    started: SystemTime,
    cycles: u64,
    alarmed: bool,
}

/// The actor owns one target. A higher certified target covers lower heights.
struct PendingTarget {
    tip: ConsensusTip,
    live: Option<Box<Finalized>>,
}

enum CatchUpPhase {
    LocateAnchor {
        height: u64,
        child: Option<ConsensusBlock>,
    },
    PreflightAncestry {
        height: u64,
        expected: Digest,
    },
    ApplyForward {
        next_height: u64,
        parent_hash: alloy_primitives::B256,
    },
}

struct CatchUpJob {
    base_height: u64,
    base_hash: alloy_primitives::B256,
    anchor_height: u64,
    anchor_digest: Digest,
    anchor_proof: Option<VerifiedDirectFinalization>,
    temporary_anchor: bool,
    missing_epochs: BTreeSet<Epoch>,
    phase: CatchUpPhase,
}

struct ParkedProbe {
    target: ConsensusTip,
    base_height: u64,
    base_hash: alloy_primitives::B256,
    missing_epochs: BTreeSet<Epoch>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReconcileStepOutcome {
    Idle,
    YieldMoreWork,
    Parked,
}

enum MarshalRead<T> {
    Found(T),
    Absent,
    TimedOut,
}

async fn marshal_read_block<C, F>(
    clock: Option<&C>,
    budget: &mut EventWorkBudget,
    future: F,
) -> MarshalRead<ConsensusBlock>
where
    C: Clock,
    F: Future<Output = Option<commonware_cryptography::Digested<ConsensusBlock>>> + Send,
{
    match marshal_read(clock, budget, future).await {
        MarshalRead::Found(block) => MarshalRead::Found(block.inner().clone()),
        MarshalRead::Absent => MarshalRead::Absent,
        MarshalRead::TimedOut => MarshalRead::TimedOut,
    }
}

async fn marshal_read<C, F, T>(
    clock: Option<&C>,
    budget: &mut EventWorkBudget,
    future: F,
) -> MarshalRead<T>
where
    C: Clock,
    F: Future<Output = Option<T>> + Send,
    T: Send,
{
    let result = match clock {
        Some(clock) => {
            let mut read = std::pin::pin!(future);
            let mut timer = std::pin::pin!(clock.sleep(MARSHAL_LOCAL_READ_TIMEOUT));
            commonware_macros::select! {
                value = &mut read => Ok(value),
                _ = &mut timer => Err(()),
            }
        }
        None => {
            return match future.await {
                Some(value) => MarshalRead::Found(value),
                None => MarshalRead::Absent,
            }
        }
    };
    match result {
        Ok(Some(value)) => MarshalRead::Found(value),
        Ok(None) => MarshalRead::Absent,
        Err(_) => {
            budget.marshal_timed_out = true;
            MarshalRead::TimedOut
        }
    }
}

fn canonicalize_live_finalized(
    mut finalized: Finalized,
    verified: &VerifiedDirectFinalization,
) -> Finalized {
    let certificate = &mut finalized.consensus_data.finalized_certificate;
    certificate.epoch = verified.round.epoch().get();
    certificate.view = verified.round.view().get();
    certificate.parent_view = verified.parent_view;
    certificate.ordered_committee = verified.ordered_committee.clone();
    certificate.signer_bitmap = verified.signer_bitmap.clone();
    certificate.encoded_certificate = verified.encoded_certificate.clone();
    finalized.vrf_seed = verified.vrf_seed;
    finalized.consensus_data.vrf_seed = verified.vrf_seed;
    finalized
}

impl FinalizationActor {
    pub fn new(deps: FinalizationActorDeps) -> (Self, Mailbox) {
        let (tx, rx) = mpsc::unbounded::<Message>();
        let mailbox = Mailbox::from_sender(tx);
        (
            Self {
                rx,
                deps,
                epocher: None,
                finalization_notify: None,
                view_advanced: None,
                pending_retry_interval: DEFAULT_PENDING_RETRY_INTERVAL,
                pending_target: None,
                head_proof_retry: None,
                unresolved: BTreeMap::new(),
                unresolved_cursor: None,
                active_job: None,
                parked_probe: None,
                parked_reason: None,
                resolution_stall: None,
                #[cfg(test)]
                last_event_stats: EventWorkStats::default(),
            },
            mailbox,
        )
    }

    #[must_use]
    pub fn with_epocher(mut self, epocher: crate::follow::FollowerEpocher) -> Self {
        self.epocher = Some(epocher);
        self
    }

    #[must_use]
    pub fn with_finalization_notify(mut self, notify: Arc<tokio::sync::Notify>) -> Self {
        self.finalization_notify = Some(notify);
        self
    }

    #[must_use]
    pub fn with_view_advanced(mut self, tx: tokio::sync::mpsc::UnboundedSender<u64>) -> Self {
        self.view_advanced = Some(tx);
        self
    }

    #[cfg(test)]
    #[must_use]
    pub fn with_pending_retry_interval(mut self, interval: Duration) -> Self {
        self.pending_retry_interval = interval;
        self
    }

    pub async fn run<E>(mut self, ctx: E) -> Result<()>
    where
        E: Spawner + Clock + Send + Sync + 'static,
    {
        info!(target: "outbe::finalization", "FinalizationActor started");
        let mut next_delay = self.has_pending_work().then_some(Duration::ZERO);
        loop {
            let message = self.next_event_or_timeout(&ctx, next_delay).await;
            self.update_resolution_stall(&ctx, false);

            let previous_height = self.current_head().0;
            let mut budget = EventWorkBudget::new();
            match message {
                Some(Some(msg)) => {
                    self.handle_message(&ctx, msg, &mut budget).await.map_err(|error| {
                        tracing::error!(target: "outbe::finalization", %error, "fatal finalization error");
                        crate::errors::FinalizationError::ActorFatal(error)
                    })?;
                }
                Some(None) => break,
                None => {}
            }

            let outcome = self.step_reconcile_with_clock(&ctx, &mut budget).await.map_err(|error| {
                tracing::error!(target: "outbe::finalization", %error, "fatal marshal finality reconciliation error");
                crate::errors::FinalizationError::ActorFatal(error)
            })?;
            next_delay = self.finish_event_turn(&ctx, previous_height, &budget, outcome);
        }
        info!(target: "outbe::finalization", "FinalizationActor mailbox closed");
        Ok(())
    }

    fn has_pending_work(&self) -> bool {
        self.active_job.is_some()
            || self.pending_target.is_some()
            || self.head_proof_retry.is_some()
            || !self.unresolved.is_empty()
    }

    async fn next_event_or_timeout<C: Clock>(
        &mut self,
        ctx: &C,
        delay: Option<Duration>,
    ) -> Option<Option<Message>> {
        let Some(delay) = delay else {
            return Some(self.rx.next().await);
        };
        let timer = ctx.sleep(delay);
        let mut timer = std::pin::pin!(timer);
        commonware_macros::select! {
            msg = self.rx.next() => Some(msg),
            _ = &mut timer => None,
        }
    }

    fn finish_event_turn<C: Clock>(
        &mut self,
        ctx: &C,
        previous_height: u64,
        budget: &EventWorkBudget,
        outcome: ReconcileStepOutcome,
    ) -> Option<Duration> {
        let progressed = self.current_head().0 > previous_height;
        self.update_resolution_stall(ctx, progressed);
        crate::metrics::record_finalization_event_work(
            budget.stats.marshal_reads,
            budget.stats.crypto_checks,
            budget.stats.applied_steps,
        );
        #[cfg(test)]
        {
            self.last_event_stats = budget.stats;
        }
        let delay = match outcome {
            ReconcileStepOutcome::Idle => None,
            ReconcileStepOutcome::YieldMoreWork => Some(Duration::ZERO),
            ReconcileStepOutcome::Parked => Some(self.pending_retry_interval),
        };
        delay.map(|delay| self.cap_delay_for_stall(delay, ctx.current()))
    }

    async fn handle_message<C: Clock>(
        &mut self,
        clock: &C,
        message: Message,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<()> {
        match message {
            Message::Finalized(finalized) => {
                self.handle_finalized_event(Some(clock), finalized, budget)
                    .await
            }
            Message::ReconcileMarshalTip(tip) => self.handle_tip(Some(clock), tip, budget).await,
            Message::CertifiedNotarization(pending) => {
                match self
                    .deps
                    .parent_cert_store
                    .put_certified_notarization(pending.record)
                {
                    Ok(()) => crate::metrics::record_certification_persisted(),
                    Err(error) => {
                        crate::metrics::record_certification_dropped(
                            crate::metrics::CertificationDropReason::StoreError,
                        );
                        warn!(target: "outbe::finalization", %error, "certification persist failed");
                    }
                }
                Ok(())
            }
        }
    }

    fn retain_unresolved(&mut self, round: Round, digest: Digest) -> eyre::Result<()> {
        if !self.unresolved.contains_key(&round)
            && self.unresolved.len() >= MAX_UNRESOLVED_LIVE_TARGETS
        {
            return Err(archive_error(0, "unresolved voter finalization ceiling exceeded").into());
        }
        self.unresolved.insert(round, digest);
        Ok(())
    }

    fn retain_head_proof(&mut self, finalized: Finalized) {
        if self
            .head_proof_retry
            .as_ref()
            .is_none_or(|current| finalized.round > current.round)
        {
            self.head_proof_retry = Some(Box::new(finalized));
        }
    }

    fn check_round_identity(&self, round: Round, digest: Digest) -> eyre::Result<()> {
        let view = self.deps.view.read();
        if view.last_finalized_round == Some(round)
            && view.forkchoice.finalized_block_hash != digest.0
        {
            return Err(crate::errors::FinalizationError::SameRoundInconsistency {
                round,
                digest,
                finalized_hash: view.forkchoice.finalized_block_hash,
            }
            .into());
        }
        if self
            .pending_target
            .as_ref()
            .is_some_and(|target| target.tip.round == round && target.tip.digest != digest)
        {
            return Err(archive_error(0, "pending target conflicts at the same round").into());
        }
        if self
            .unresolved
            .get(&round)
            .is_some_and(|known| *known != digest)
        {
            return Err(
                archive_error(0, "unresolved finalization conflicts at the same round").into(),
            );
        }
        if let Some(proof) = self
            .head_proof_retry
            .as_ref()
            .filter(|proof| proof.round == round && proof.digest != digest)
        {
            return Err(crate::errors::FinalizationError::SameRoundInconsistency {
                round,
                digest,
                finalized_hash: proof.digest.0,
            }
            .into());
        }
        Ok(())
    }

    fn record_target(
        &mut self,
        tip: ConsensusTip,
        live: Option<Box<Finalized>>,
    ) -> eyre::Result<()> {
        if self
            .parked_probe
            .as_ref()
            .is_some_and(|probe| probe.target != tip || live.is_some())
        {
            self.parked_probe = None;
        }
        if let Some(current) = &mut self.pending_target {
            if current.tip.height == tip.height && current.tip.digest != tip.digest {
                return Err(crate::errors::FinalizationError::SameHeightInconsistency {
                    block_number: tip.height.get(),
                    digest: tip.digest,
                    finalized_hash: current.tip.digest.0,
                }
                .into());
            }
            if tip.height < current.tip.height {
                return Ok(());
            }
            if tip.height == current.tip.height {
                if tip.round > current.tip.round {
                    current.tip = tip;
                }
                if live.is_some() {
                    current.live = live;
                }
                return Ok(());
            }
        }
        self.pending_target = Some(PendingTarget { tip, live });
        Ok(())
    }

    fn current_head(&self) -> (u64, alloy_primitives::B256) {
        let view = self.deps.view.read();
        (
            view.last_finalized_number,
            view.forkchoice.finalized_block_hash,
        )
    }

    fn marshal(&self) -> eyre::Result<MarshalMailbox> {
        self.deps.marshal_mailbox.clone().ok_or_else(|| {
            crate::errors::FinalizationError::MarshalMailboxRequired(Digest(
                alloy_primitives::B256::ZERO,
            ))
            .into()
        })
    }

    fn epoch_is_registered(&self, epoch: Epoch) -> bool {
        self.deps
            .certificate_scheme_provider
            .scoped(epoch)
            .is_some()
            && self
                .deps
                .committee_provider
                .ordered_committee(epoch)
                .is_some()
    }
}

fn archive_error(height: u64, reason: impl Into<String>) -> crate::errors::FinalizationError {
    crate::errors::FinalizationError::ArchiveReconciliation {
        height,
        reason: reason.into(),
    }
}

#[cfg(test)]
pub(super) fn publish_finalized_wakeup(
    notify: Option<&tokio::sync::Notify>,
    finalized_height: u64,
) {
    outbe_tee::authority::note_finalized_authority_revision(finalized_height);
    notify_finalized_waiters(notify);
}

fn notify_finalized_waiters(notify: Option<&tokio::sync::Notify>) {
    if let Some(notify) = notify {
        notify.notify_waiters();
    }
}

/// Builder methods on the `(actor, mailbox)` pair from [`FinalizationActor::new`].
pub trait FinalizationActorBuilder {
    #[must_use]
    fn with_epocher(self, epocher: crate::follow::FollowerEpocher) -> Self;
    #[must_use]
    fn with_finalization_notify(self, notify: Arc<tokio::sync::Notify>) -> Self;
}

impl FinalizationActorBuilder for (FinalizationActor, Mailbox) {
    fn with_epocher(mut self, epocher: crate::follow::FollowerEpocher) -> Self {
        self.0 = self.0.with_epocher(epocher);
        self
    }

    fn with_finalization_notify(mut self, notify: Arc<tokio::sync::Notify>) -> Self {
        self.0 = self.0.with_finalization_notify(notify);
        self
    }
}

#[cfg(test)]
mod tests;
