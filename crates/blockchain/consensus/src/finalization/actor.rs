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

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use commonware_consensus::types::{Epoch, Height, Round};
use commonware_runtime::{Clock, Spawner};
use futures::{channel::mpsc, StreamExt};
use outbe_primitives::{
    consensus::{ConsensusData, ConsensusExecutionBridge, ConsensusStatus},
    consensus_metadata::ParentParticipationProof,
    error::Result,
    reshare_artifact::ConsensusHeaderArtifact,
};
use tracing::{debug, info, warn};

use crate::block::ConsensusBlock;
use crate::digest::Digest;
use crate::finalization::block_cache::BlockCache;
use crate::finalization::committee_prelude::build_committee_prelude;
#[cfg(test)]
use crate::finalization::evidence::EventWorkStats;
use crate::finalization::evidence::{
    certified_parent_metadata_from_phase1_system_tx, check_missing_verifier_epoch,
    verify_child_parent_witness, verify_direct_finalization, verify_live_finalized_cert,
    EventWorkBudget, VerifiedChildParentWitness, VerifiedDirectFinalization, VerifiedStep,
};
use crate::finalization::ingress::{Finalized, Mailbox, Message};
use crate::finalization::late_sig_store::{FinalizeVoteTarget, LateFinalizeCommittee};
use crate::finalization::parent_cert_store::{
    CertifiedParentProofKey, CertifiedParentProofRecord, CertifiedParentProofStore,
    FinalizedParentCertStore, ProofKind, CERTIFIED_PARENT_PROOF_RECORD_FORMAT_VERSION,
};
use crate::finalization::state::FinalizationViewHandle;
use crate::finalization::util::{
    classify_finalization, extract_header_artifact_from_block, ReplayClassification,
};
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
        let mut next_delay = if self.active_job.is_some()
            || self.pending_target.is_some()
            || self.head_proof_retry.is_some()
            || !self.unresolved.is_empty()
        {
            Some(Duration::ZERO)
        } else {
            None
        };
        loop {
            let message = if let Some(delay) = next_delay {
                let timer = ctx.sleep(delay);
                let mut timer = std::pin::pin!(timer);
                commonware_macros::select! {
                    msg = self.rx.next() => Some(msg),
                    _ = &mut timer => None,
                }
            } else {
                Some(self.rx.next().await)
            };

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
            let progressed = self.current_head().0 > previous_height;
            self.update_resolution_stall(&ctx, progressed);
            crate::metrics::record_finalization_event_work(
                budget.stats.marshal_reads,
                budget.stats.crypto_checks,
                budget.stats.applied_steps,
            );
            #[cfg(test)]
            {
                self.last_event_stats = budget.stats;
            }
            next_delay = match outcome {
                ReconcileStepOutcome::Idle => None,
                ReconcileStepOutcome::YieldMoreWork => Some(Duration::ZERO),
                ReconcileStepOutcome::Parked => Some(self.pending_retry_interval),
            };
            if let (Some(delay), Some(stall)) = (next_delay, self.resolution_stall.as_ref()) {
                let cycle = Self::resolution_cycle();
                let next_cycle = stall.started
                    + cycle.saturating_mul(
                        (stall.cycles.saturating_add(1)).min(u32::MAX as u64) as u32
                    );
                let next_alarm = (!stall.alarmed)
                    .then_some(stall.started + crate::config::STALL_DIVERGENCE_ALARM);
                let deadline = next_alarm.map_or(next_cycle, |alarm| alarm.min(next_cycle));
                let until_deadline = deadline.duration_since(ctx.current()).unwrap_or_default();
                next_delay = Some(delay.min(until_deadline));
            }
        }
        info!(target: "outbe::finalization", "FinalizationActor mailbox closed");
        Ok(())
    }

    fn resolution_cycle() -> Duration {
        crate::config::FINALIZE_RESOLUTION_TIMEOUT * crate::config::FINALIZE_MAX_RETRIES
            + crate::config::FINALIZE_RETRY_DELAY * (crate::config::FINALIZE_MAX_RETRIES - 1)
    }

    fn update_resolution_stall(&mut self, clock: &impl Clock, progressed: bool) {
        if progressed {
            if self
                .resolution_stall
                .as_ref()
                .is_some_and(|stall| stall.alarmed)
            {
                crate::metrics::record_finalization_divergence_cleared();
            }
            self.resolution_stall = None;
        }
        let missing_block =
            !self.unresolved.is_empty() || self.parked_reason == Some(ParkedReason::MissingBlock);
        if !missing_block {
            if self
                .resolution_stall
                .as_ref()
                .is_some_and(|stall| stall.alarmed)
            {
                crate::metrics::record_finalization_divergence_cleared();
            }
            self.resolution_stall = None;
            return;
        }
        let now = clock.current();
        let stall = self.resolution_stall.get_or_insert(ResolutionStall {
            started: now,
            cycles: 0,
            alarmed: false,
        });
        let elapsed = now.duration_since(stall.started).unwrap_or_default();
        let cycle = Self::resolution_cycle();
        let due_cycles = (elapsed.as_nanos() / cycle.as_nanos()).min(u64::MAX as u128) as u64;
        let (round, digest) = self
            .unresolved
            .iter()
            .next()
            .map(|(round, digest)| (Some(*round), Some(*digest)))
            .or_else(|| {
                self.pending_target
                    .as_ref()
                    .map(|target| (Some(target.tip.round), Some(target.tip.digest)))
            })
            .unwrap_or((None, None));
        if stall.cycles < due_cycles {
            let completed_cycles = due_cycles - stall.cycles;
            stall.cycles = due_cycles;
            crate::metrics::record_finalization_resolution_stalled(completed_cycles);
            warn!(
                target: "outbe::finalization",
                ?round,
                ?digest,
                stall_cycles = stall.cycles,
                cycle_budget_secs = cycle.as_secs(),
                last_finalized_number = self.deps.view.read().last_finalized_number,
                "finalized block remains unavailable after one resolution cycle"
            );
        }
        if !stall.alarmed && elapsed >= crate::config::STALL_DIVERGENCE_ALARM {
            stall.alarmed = true;
            crate::metrics::record_finalization_divergence_suspected();
            tracing::error!(
                target: "outbe::finalization",
                ?round,
                ?digest,
                stall_cycles = stall.cycles,
                elapsed_secs = elapsed.as_secs(),
                "suspected local divergence: marshal resolution exceeded the alarm threshold"
            );
        }
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

    async fn handle_finalized_event<C: Clock>(
        &mut self,
        clock: Option<&C>,
        finalized: Finalized,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<()> {
        let round = finalized.round;
        let digest = finalized.digest;
        let (head_height, head_hash) = self.current_head();
        if self
            .head_proof_retry
            .as_ref()
            .is_some_and(|old| old.digest.0 != head_hash)
        {
            self.head_proof_retry = None;
        }
        self.check_round_identity(round, digest)?;

        if head_height > 0 && digest.0 == head_hash {
            self.retain_head_proof(finalized);
            return Ok(());
        }

        if self
            .parked_probe
            .as_ref()
            .is_some_and(|probe| probe.target.round != round || probe.target.digest != digest)
        {
            self.parked_probe = None;
        }
        let block = if let Some(block) = self.deps.block_cache.get(&digest) {
            Some(block)
        } else {
            self.retain_unresolved(round, digest)?;
            if let Some(clock) = clock {
                self.resolution_stall.get_or_insert(ResolutionStall {
                    started: clock.current(),
                    cycles: 0,
                    alarmed: false,
                });
            }
            if budget.charge_read() {
                let marshal = self.marshal()?;
                match marshal_read(clock, budget, marshal.get_block(&digest)).await {
                    MarshalRead::Found(block) => Some(block),
                    MarshalRead::Absent | MarshalRead::TimedOut => None,
                }
            } else {
                None
            }
        };

        if let Some(block) = block {
            if !budget.charge_crypto() {
                return self.retain_unresolved(round, digest);
            }
            let verified = verify_live_finalized_cert(
                block.number(),
                &finalized,
                &self.deps.certificate_scheme_provider,
                &self.deps.committee_provider,
            )?;
            if verified.is_none() {
                let accepted_epoch = self
                    .deps
                    .view
                    .read()
                    .last_finalized_round
                    .map(|r| r.epoch());
                let _ =
                    check_missing_verifier_epoch(block.number(), round.epoch(), accepted_epoch)?;
            }
            let finalized = match verified.as_ref() {
                Some(verified) => canonicalize_live_finalized(finalized, verified),
                None => finalized,
            };
            self.accept_resolved_live(clock, finalized, block, verified.is_some(), budget)
                .await
        } else {
            self.retain_unresolved(round, digest)
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

    #[cfg(test)]
    async fn handle_finalized(
        &mut self,
        _clock: &impl Clock,
        finalized: Finalized,
    ) -> eyre::Result<()> {
        let mut budget = EventWorkBudget::new();
        self.handle_finalized_event(Some(_clock), finalized, &mut budget)
            .await?;
        let _ = self.step_reconcile_with_clock(_clock, &mut budget).await?;
        Ok(())
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
        if self
            .head_proof_retry
            .as_ref()
            .is_some_and(|proof| proof.round == round && proof.digest != digest)
        {
            return Err(crate::errors::FinalizationError::SameRoundInconsistency {
                round,
                digest,
                finalized_hash: self.head_proof_retry.as_ref().expect("checked").digest.0,
            }
            .into());
        }
        Ok(())
    }

    async fn accept_resolved_live<C: Clock>(
        &mut self,
        clock: Option<&C>,
        finalized: Finalized,
        block: ConsensusBlock,
        verified: bool,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<()> {
        let height = block.number();
        let round = finalized.round;
        let digest = finalized.digest;
        if block.digest() != digest {
            return Err(archive_error(height, "resolved voter block digest differs from F").into());
        }
        let (head_height, head_hash) = self.current_head();
        if height <= head_height {
            if height == head_height && digest.0 != head_hash {
                return Err(crate::errors::FinalizationError::SameHeightInconsistency {
                    block_number: height,
                    digest,
                    finalized_hash: head_hash,
                }
                .into());
            }
            if height < head_height {
                if !budget.charge_read() {
                    return self.retain_unresolved(round, digest);
                }
                let marshal = self.marshal()?;
                let archived =
                    marshal_read(clock, budget, marshal.get_block(Height::new(height))).await;
                let MarshalRead::Found(archived) = archived else {
                    return self.retain_unresolved(round, digest);
                };
                if archived.digest() != digest {
                    return Err(archive_error(
                        height,
                        "historical voter F conflicts with finalized archive",
                    )
                    .into());
                }
            }
            if height == head_height {
                self.retain_head_proof(finalized);
            }
            self.unresolved.remove(&round);
            return Ok(());
        }

        if !budget.charge_read() {
            return self.retain_unresolved(round, digest);
        }
        let marshal = self.marshal()?;
        match marshal_read(clock, budget, marshal.get_block(Height::new(height))).await {
            MarshalRead::Found(archived) if archived.digest() != digest => {
                return Err(
                    archive_error(height, "voter F conflicts with finalized archive").into(),
                );
            }
            MarshalRead::Absent
                if verified
                    && height == head_height.saturating_add(1)
                    && self.active_job.is_none() => {}
            MarshalRead::Absent | MarshalRead::TimedOut => {
                return self.retain_unresolved(round, digest);
            }
            MarshalRead::Found(_) => {}
        }
        if self
            .pending_target
            .as_ref()
            .is_some_and(|target| target.tip.height.get() == height && target.tip.digest != digest)
        {
            return Err(archive_error(height, "voter F conflicts with same-height target").into());
        }

        if height == head_height.saturating_add(1) && block.parent_hash() != head_hash {
            return Err(
                archive_error(height, "voter F block does not extend accepted head").into(),
            );
        }

        if verified && height == head_height.saturating_add(1) && self.active_job.is_none() {
            let retry_finalized = Finalized {
                round: finalized.round,
                digest,
                vrf_seed: finalized.vrf_seed,
                consensus_data: finalized.consensus_data.clone(),
            };
            match self.process_finalization(finalized, block).await? {
                ApplyOutcome::Applied => {
                    self.unresolved.remove(&retry_finalized.round);
                    return Ok(());
                }
                ApplyOutcome::PendingPersist | ApplyOutcome::PendingVerifier => {
                    self.record_target(
                        ConsensusTip {
                            round: retry_finalized.round,
                            height: Height::new(height),
                            digest,
                        },
                        Some(Box::new(retry_finalized)),
                    )?;
                    return Ok(());
                }
            }
        }

        self.record_target(
            ConsensusTip {
                round: finalized.round,
                height: Height::new(height),
                digest,
            },
            Some(Box::new(finalized)),
        )?;
        self.unresolved.remove(&round);
        Ok(())
    }

    async fn handle_tip<C: Clock>(
        &mut self,
        clock: Option<&C>,
        tip: ConsensusTip,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<()> {
        self.check_round_identity(tip.round, tip.digest)?;
        if let Some(current) = &self.pending_target {
            if current.tip.height == tip.height && current.tip.digest != tip.digest {
                return Err(crate::errors::FinalizationError::SameHeightInconsistency {
                    block_number: tip.height.get(),
                    digest: tip.digest,
                    finalized_hash: current.tip.digest.0,
                }
                .into());
            }
            if current.tip == tip {
                return Ok(());
            }
        }
        let (head_height, head_hash) = self.current_head();
        if tip.height.get() == head_height && tip.digest.0 != head_hash {
            return Err(crate::errors::FinalizationError::SameHeightInconsistency {
                block_number: head_height,
                digest: tip.digest,
                finalized_hash: head_hash,
            }
            .into());
        }
        if !budget.charge_read() {
            return self.record_target(tip, None);
        }
        let marshal = self.marshal()?;
        let archived_f = marshal_read(clock, budget, marshal.get_finalization(tip.height)).await;
        if let MarshalRead::TimedOut = archived_f {
            return self.record_target(tip, None);
        }
        if let MarshalRead::Found(f) = archived_f {
            if f.proposal.payload != tip.digest || f.proposal.round != tip.round {
                return Err(
                    archive_error(tip.height.get(), "marshal tip differs from archived F").into(),
                );
            }
        }
        if !budget.charge_read() {
            return self.record_target(tip, None);
        }
        let marshal = self.marshal()?;
        let archived_block = marshal_read(clock, budget, marshal.get_block(tip.height)).await;
        if let MarshalRead::TimedOut = archived_block {
            return self.record_target(tip, None);
        }
        if let MarshalRead::Found(block) = archived_block {
            if block.number() != tip.height.get() || block.digest() != tip.digest {
                return Err(archive_error(
                    tip.height.get(),
                    "marshal tip differs from archived block",
                )
                .into());
            }
        }
        if tip.height.get() <= head_height {
            return Ok(());
        }
        self.parked_probe = None;
        self.record_target(tip, None)
    }

    #[cfg(test)]
    async fn reconcile_marshal_tip(
        &mut self,
        tip: ConsensusTip,
    ) -> eyre::Result<ReconcileStepOutcome> {
        let mut budget = EventWorkBudget::new();
        self.handle_tip(
            None::<&commonware_runtime::deterministic::Context>,
            tip,
            &mut budget,
        )
        .await?;
        self.step_reconcile(&mut budget).await
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

    async fn retry_unresolved<C: Clock>(
        &mut self,
        clock: Option<&C>,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<()> {
        if self.unresolved.is_empty() {
            self.unresolved_cursor = None;
            return Ok(());
        }
        let after = self.unresolved_cursor;
        let mut keys: Vec<Round> = self
            .unresolved
            .keys()
            .copied()
            .filter(|round| after.is_none_or(|last| *round > last))
            .take(16)
            .collect();
        if keys.is_empty() {
            keys = self.unresolved.keys().copied().take(16).collect();
        }
        let marshal = self.marshal()?;
        for round in keys {
            if !budget.can_read_count(2) {
                break;
            }
            self.unresolved_cursor = Some(round);
            let digest = self.unresolved[&round];
            let _ = budget.charge_read();
            let block = match marshal_read(clock, budget, marshal.get_block(&digest)).await {
                MarshalRead::Found(block) => block,
                MarshalRead::Absent => continue,
                MarshalRead::TimedOut => return Ok(()),
            };
            let height = block.number();
            if block.digest() != digest {
                return Err(archive_error(height, "unresolved voter block digest mismatch").into());
            }
            let (head_height, head_hash) = self.current_head();
            if height == head_height && digest.0 != head_hash {
                return Err(archive_error(height, "unresolved voter F conflicts with head").into());
            }
            let _ = budget.charge_read();
            let archived =
                marshal_read(clock, budget, marshal.get_block(Height::new(height))).await;
            match archived {
                MarshalRead::Found(archived) => {
                    if archived.number() != height || archived.digest() != digest {
                        return Err(archive_error(
                            height,
                            "unresolved voter F conflicts with finalized archive",
                        )
                        .into());
                    }
                }
                MarshalRead::Absent if height == head_height && digest.0 == head_hash => {}
                MarshalRead::Absent => continue,
                MarshalRead::TimedOut => return Ok(()),
            }
            if self.pending_target.as_ref().is_some_and(|target| {
                target.tip.height.get() == height && target.tip.digest != digest
            }) {
                return Err(
                    archive_error(height, "unresolved voter F conflicts with target").into(),
                );
            }
            if height > head_height {
                self.record_target(
                    ConsensusTip {
                        round,
                        height: Height::new(height),
                        digest,
                    },
                    None,
                )?;
            }
            self.unresolved.remove(&round);
        }
        Ok(())
    }

    async fn retry_head_proof<C: Clock>(
        &mut self,
        clock: Option<&C>,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<Option<ReconcileStepOutcome>> {
        let Some(proof) = self.head_proof_retry.take() else {
            return Ok(None);
        };
        let (height, head_hash) = self.current_head();
        if proof.digest.0 != head_hash {
            return Ok(None);
        }
        let higher_work = self
            .pending_target
            .as_ref()
            .is_some_and(|target| target.tip.height.get() > height)
            || self.active_job.is_some()
            || !self.unresolved.is_empty();
        let block = if let Some(block) = self.deps.block_cache.get(&proof.digest) {
            MarshalRead::Found(block)
        } else if budget.charge_read() {
            let marshal = self.marshal()?;
            marshal_read(clock, budget, marshal.get_block(Height::new(height))).await
        } else {
            self.head_proof_retry = Some(proof);
            return Ok(Some(ReconcileStepOutcome::YieldMoreWork));
        };
        let block = match block {
            MarshalRead::Found(block) => block,
            MarshalRead::Absent => {
                self.head_proof_retry = Some(proof);
                if !higher_work {
                    self.parked_reason = Some(ParkedReason::MissingBlock);
                }
                return Ok((!higher_work).then_some(ReconcileStepOutcome::Parked));
            }
            MarshalRead::TimedOut => {
                self.head_proof_retry = Some(proof);
                if !higher_work {
                    self.parked_reason = Some(ParkedReason::MissingBlock);
                } else {
                    // The timed-out head proof is only a local handoff repair.
                    // Its timeout must not mark the independent higher target
                    // as timed out or stop its work in this event.
                    budget.marshal_timed_out = false;
                }
                return Ok((!higher_work).then_some(ReconcileStepOutcome::Parked));
            }
        };
        if block.number() != height || block.digest() != proof.digest {
            return Err(
                archive_error(height, "head proof block differs from accepted head").into(),
            );
        }
        if !budget.charge_crypto() {
            self.head_proof_retry = Some(proof);
            return Ok(Some(ReconcileStepOutcome::YieldMoreWork));
        }
        let verified = verify_live_finalized_cert(
            height,
            &proof,
            &self.deps.certificate_scheme_provider,
            &self.deps.committee_provider,
        )?;
        let Some(verified) = verified else {
            let accepted_epoch = self
                .deps
                .view
                .read()
                .last_finalized_round
                .map(|r| r.epoch());
            let _ = check_missing_verifier_epoch(height, proof.round.epoch(), accepted_epoch)?;
            self.head_proof_retry = Some(proof);
            if !higher_work {
                self.parked_reason = Some(ParkedReason::MissingVerifier);
            }
            return Ok((!higher_work).then_some(ReconcileStepOutcome::Parked));
        };
        let proof = canonicalize_live_finalized(*proof, &verified);
        let retry = Finalized {
            round: proof.round,
            digest: proof.digest,
            vrf_seed: proof.vrf_seed,
            consensus_data: proof.consensus_data.clone(),
        };
        match self.process_finalization(proof, block).await? {
            ApplyOutcome::Applied => {}
            pending @ (ApplyOutcome::PendingPersist | ApplyOutcome::PendingVerifier) => {
                self.head_proof_retry = Some(Box::new(retry));
                if !higher_work {
                    self.parked_reason = Some(match pending {
                        ApplyOutcome::PendingPersist => ParkedReason::Persistence,
                        ApplyOutcome::PendingVerifier => ParkedReason::MissingVerifier,
                        ApplyOutcome::Applied => unreachable!(),
                    });
                }
                return Ok((!higher_work).then_some(ReconcileStepOutcome::Parked));
            }
        }
        Ok(None)
    }

    async fn step_reconcile_with_clock<C: Clock>(
        &mut self,
        clock: &C,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<ReconcileStepOutcome> {
        self.step_reconcile_with_optional_clock(Some(clock), budget)
            .await
    }

    #[cfg(test)]
    async fn step_reconcile(
        &mut self,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<ReconcileStepOutcome> {
        self.step_reconcile_with_optional_clock(
            None::<&commonware_runtime::deterministic::Context>,
            budget,
        )
        .await
    }

    async fn step_reconcile_with_optional_clock<C: Clock>(
        &mut self,
        clock: Option<&C>,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<ReconcileStepOutcome> {
        self.parked_reason = None;
        let outcome = self.step_reconcile_inner(clock, budget).await;
        #[cfg(test)]
        {
            self.last_event_stats = budget.stats;
        }
        outcome
    }

    async fn step_reconcile_inner<C: Clock>(
        &mut self,
        clock: Option<&C>,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<ReconcileStepOutcome> {
        if let Some(outcome) = self.retry_head_proof(clock, budget).await? {
            return Ok(outcome);
        }
        self.retry_unresolved(clock, budget).await?;
        if budget.marshal_timed_out {
            self.parked_reason = Some(ParkedReason::MissingBlock);
            return Ok(ReconcileStepOutcome::Parked);
        }
        loop {
            let (head_height, head_hash) = self.current_head();
            if self.active_job.is_none() {
                if let Some(probe) = self.parked_probe.as_ref() {
                    let unchanged = self.pending_target.as_ref().is_some_and(|target| {
                        target.tip == probe.target
                            && head_height == probe.base_height
                            && head_hash == probe.base_hash
                    });
                    let registered = probe
                        .missing_epochs
                        .iter()
                        .copied()
                        .any(|epoch| self.epoch_is_registered(epoch));
                    if unchanged && !registered {
                        self.parked_reason = Some(ParkedReason::MissingVerifier);
                        return Ok(ReconcileStepOutcome::Parked);
                    }
                    self.parked_probe = None;
                }
                if let Some(tip) = self
                    .pending_target
                    .as_ref()
                    .filter(|target| target.tip.height.get() <= head_height)
                    .map(|target| target.tip)
                {
                    let block = if tip.height.get() == head_height {
                        if tip.digest.0 != head_hash {
                            return Err(archive_error(
                                tip.height.get(),
                                "replayed target differs from head",
                            )
                            .into());
                        }
                        self.deps.block_cache.get(&tip.digest)
                    } else {
                        None
                    };
                    let block = if let Some(block) = block {
                        Some(block)
                    } else {
                        if !budget.can_read() {
                            return Ok(ReconcileStepOutcome::YieldMoreWork);
                        }
                        let marshal = self.marshal()?;
                        let _ = budget.charge_read();
                        match marshal_read(clock, budget, marshal.get_block(tip.height)).await {
                            MarshalRead::Found(block) => Some(block),
                            MarshalRead::Absent | MarshalRead::TimedOut => None,
                        }
                    };
                    let Some(block) = block else {
                        self.parked_reason = Some(ParkedReason::MissingBlock);
                        return Ok(ReconcileStepOutcome::Parked);
                    };
                    if block.number() != tip.height.get() || block.digest() != tip.digest {
                        return Err(archive_error(
                            tip.height.get(),
                            "replayed target differs from archived block",
                        )
                        .into());
                    }
                    if tip.height.get() == head_height {
                        if let Some(live) = self
                            .pending_target
                            .as_mut()
                            .and_then(|target| target.live.take())
                        {
                            self.retain_head_proof(*live);
                        }
                    }
                    self.pending_target = None;
                }
                let Some(target) = self.pending_target.as_ref() else {
                    return Ok(
                        if self.unresolved.is_empty() && self.head_proof_retry.is_none() {
                            ReconcileStepOutcome::Idle
                        } else {
                            ReconcileStepOutcome::Parked
                        },
                    );
                };
                let required_reads = if target.live.is_some() { 1 } else { 2 };
                if !budget.can_read_count(required_reads) || !budget.can_crypto() {
                    return Ok(ReconcileStepOutcome::YieldMoreWork);
                }
                let tip = target.tip;
                let live = target.live.as_deref();
                let (job, reason) = self.start_job(clock, tip, live, budget).await?;
                let Some(job) = job else {
                    self.parked_reason = reason;
                    return Ok(ReconcileStepOutcome::Parked);
                };
                self.active_job = Some(job);
            }
            match self.step_job(clock, budget).await? {
                ReconcileStepOutcome::Idle => continue,
                other => return Ok(other),
            }
        }
    }

    async fn start_job<C: Clock>(
        &self,
        clock: Option<&C>,
        tip: ConsensusTip,
        live: Option<&Finalized>,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<(Option<CatchUpJob>, Option<ParkedReason>)> {
        let marshal = self.marshal()?;
        let (base_height, base_hash) = self.current_head();
        if tip.height.get() <= base_height {
            return Ok((None, None));
        }
        let _ = budget.charge_read();
        let block = match marshal_read(clock, budget, marshal.get_block(tip.height)).await {
            MarshalRead::Found(block) => block,
            MarshalRead::Absent | MarshalRead::TimedOut => {
                return Ok((None, Some(ParkedReason::MissingBlock)));
            }
        };
        if block.number() != tip.height.get() || block.digest() != tip.digest {
            return Err(archive_error(
                tip.height.get(),
                "target differs from finalized archive block",
            )
            .into());
        }

        let verified = if let Some(live) = live {
            let _ = budget.charge_crypto();
            verify_live_finalized_cert(
                tip.height.get(),
                live,
                &self.deps.certificate_scheme_provider,
                &self.deps.committee_provider,
            )?
        } else {
            let _ = budget.charge_read();
            let f = match marshal_read(clock, budget, marshal.get_finalization(tip.height)).await {
                MarshalRead::Found(f) => f,
                MarshalRead::Absent => return Ok((None, Some(ParkedReason::MissingCertificate))),
                MarshalRead::TimedOut => return Ok((None, Some(ParkedReason::MissingBlock))),
            };
            if f.proposal.payload != tip.digest || f.proposal.round != tip.round {
                return Err(
                    archive_error(tip.height.get(), "target differs from archived F").into(),
                );
            }
            if !self.epoch_is_registered(tip.round.epoch()) {
                None
            } else {
                let _ = budget.charge_crypto();
                verify_direct_finalization(
                    tip.height.get(),
                    &f,
                    &self.deps.certificate_scheme_provider,
                    &self.deps.committee_provider,
                )?
            }
        };
        let phase = if verified.is_some() {
            CatchUpPhase::PreflightAncestry {
                height: tip.height.get(),
                expected: tip.digest,
            }
        } else {
            let accepted_epoch = self
                .deps
                .view
                .read()
                .last_finalized_round
                .map(|r| r.epoch());
            let _ =
                check_missing_verifier_epoch(tip.height.get(), tip.round.epoch(), accepted_epoch)?;
            CatchUpPhase::LocateAnchor {
                height: tip.height.get(),
                child: None,
            }
        };
        let mut missing_epochs = BTreeSet::new();
        if verified.is_none() {
            missing_epochs.insert(tip.round.epoch());
        }
        Ok((
            Some(CatchUpJob {
                base_height,
                base_hash,
                anchor_height: tip.height.get(),
                anchor_digest: tip.digest,
                anchor_proof: verified,
                temporary_anchor: false,
                missing_epochs,
                phase,
            }),
            None,
        ))
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

    async fn step_job<C: Clock>(
        &mut self,
        clock: Option<&C>,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<ReconcileStepOutcome> {
        let Some(mut job) = self.active_job.take() else {
            return Ok(ReconcileStepOutcome::Idle);
        };
        let marshal = self.marshal()?;
        let outcome = loop {
            match &mut job.phase {
                CatchUpPhase::LocateAnchor { height, child } => {
                    if *height <= job.base_height {
                        if let Some(target) = self.pending_target.as_ref().map(|target| target.tip)
                        {
                            self.parked_probe = Some(ParkedProbe {
                                target,
                                base_height: job.base_height,
                                base_hash: job.base_hash,
                                missing_epochs: job.missing_epochs,
                            });
                            self.parked_reason = Some(ParkedReason::MissingVerifier);
                            return Ok(ReconcileStepOutcome::Parked);
                        }
                        self.parked_reason = Some(ParkedReason::MissingVerifier);
                        return Ok(ReconcileStepOutcome::Idle);
                    }
                    if !budget.can_read() {
                        break ReconcileStepOutcome::YieldMoreWork;
                    }
                    let _ = budget.charge_read();
                    let block =
                        match marshal_read(clock, budget, marshal.get_block(Height::new(*height)))
                            .await
                        {
                            MarshalRead::Found(block) => block,
                            MarshalRead::Absent | MarshalRead::TimedOut => {
                                self.parked_reason = Some(ParkedReason::MissingBlock);
                                break ReconcileStepOutcome::Parked;
                            }
                        };
                    if block.number() != *height {
                        return Err(archive_error(*height, "anchor search height mismatch").into());
                    }
                    if let Some(upper) = child.as_ref() {
                        if upper.parent_hash() != block.digest().0 {
                            return Err(
                                archive_error(*height, "anchor search branch mismatch").into()
                            );
                        }
                    } else if block.digest() != job.anchor_digest {
                        return Err(
                            archive_error(*height, "anchor search target digest mismatch").into(),
                        );
                    }

                    if !budget.can_read() {
                        break ReconcileStepOutcome::YieldMoreWork;
                    }
                    let _ = budget.charge_read();
                    let archive_f = match marshal_read(
                        clock,
                        budget,
                        marshal.get_finalization(Height::new(*height)),
                    )
                    .await
                    {
                        MarshalRead::Found(f) => Some(f),
                        MarshalRead::Absent => None,
                        MarshalRead::TimedOut => {
                            self.parked_reason = Some(ParkedReason::MissingBlock);
                            break ReconcileStepOutcome::Parked;
                        }
                    };
                    let mut verified = None;
                    if let Some(f) = archive_f {
                        if f.proposal.payload != block.digest() {
                            return Err(archive_error(
                                *height,
                                "anchor F payload differs from branch block",
                            )
                            .into());
                        }
                        if self.epoch_is_registered(f.proposal.round.epoch()) {
                            if !budget.can_crypto() {
                                break ReconcileStepOutcome::YieldMoreWork;
                            }
                            let _ = budget.charge_crypto();
                            verified = verify_direct_finalization(
                                *height,
                                &f,
                                &self.deps.certificate_scheme_provider,
                                &self.deps.committee_provider,
                            )?;
                        } else {
                            job.missing_epochs.insert(f.proposal.round.epoch());
                        }
                    }

                    if verified.is_none() {
                        if let Some(upper) = child.as_ref() {
                            let metadata = certified_parent_metadata_from_phase1_system_tx(upper)?;
                            if let Some(metadata) = metadata {
                                if metadata.finalized_block_number != *height
                                    || metadata.finalized_block_hash != block.digest().0
                                {
                                    return Err(archive_error(
                                        *height,
                                        "embedded F parent binding mismatch",
                                    )
                                    .into());
                                }
                                if metadata.proof_kind == ParentParticipationProof::Finalization {
                                    let candidate_epoch = Epoch::new(metadata.finalized_epoch);
                                    if !self.epoch_is_registered(candidate_epoch) {
                                        job.missing_epochs.insert(candidate_epoch);
                                    } else {
                                        if !budget.can_crypto_count(2) {
                                            break ReconcileStepOutcome::YieldMoreWork;
                                        }
                                        let _ = budget.charge_crypto();
                                        let witness = verify_child_parent_witness(
                                            &block,
                                            upper,
                                            &self.deps.certificate_scheme_provider,
                                            &self.deps.committee_provider,
                                        )?
                                        .ok_or_else(|| {
                                            archive_error(
                                                *height,
                                                "embedded anchor verifier vanished",
                                            )
                                        })?;
                                        let f = witness.finalization.ok_or_else(|| {
                                            archive_error(*height, "embedded anchor has no F")
                                        })?;
                                        let _ = budget.charge_crypto();
                                        verified = verify_direct_finalization(
                                            *height,
                                            &f,
                                            &self.deps.certificate_scheme_provider,
                                            &self.deps.committee_provider,
                                        )?;
                                    }
                                }
                            }
                        }
                    }
                    if let Some(verified) = verified {
                        job.anchor_height = *height;
                        job.anchor_digest = block.digest();
                        job.anchor_proof = Some(verified);
                        job.temporary_anchor = self
                            .pending_target
                            .as_ref()
                            .is_some_and(|target| target.tip.height.get() > *height);
                        job.phase = CatchUpPhase::PreflightAncestry {
                            height: *height,
                            expected: block.digest(),
                        };
                        continue;
                    }
                    *child = Some(block);
                    *height -= 1;
                }
                CatchUpPhase::PreflightAncestry { height, expected } => {
                    if !budget.can_read() {
                        break ReconcileStepOutcome::YieldMoreWork;
                    }
                    let _ = budget.charge_read();
                    let block =
                        match marshal_read(clock, budget, marshal.get_block(Height::new(*height)))
                            .await
                        {
                            MarshalRead::Found(block) => block,
                            MarshalRead::Absent | MarshalRead::TimedOut => {
                                self.parked_reason = Some(ParkedReason::MissingBlock);
                                break ReconcileStepOutcome::Parked;
                            }
                        };
                    if block.number() != *height || block.digest() != *expected {
                        return Err(
                            archive_error(*height, "preflight ancestry block mismatch").into()
                        );
                    }
                    let parent = block.parent_hash();
                    if *height == job.base_height.saturating_add(1) {
                        if parent != job.base_hash {
                            return Err(archive_error(
                                *height,
                                "preflight ancestry does not reach head",
                            )
                            .into());
                        }
                        job.phase = CatchUpPhase::ApplyForward {
                            next_height: job.base_height.saturating_add(1),
                            parent_hash: job.base_hash,
                        };
                    } else {
                        *height -= 1;
                        *expected = Digest(parent);
                    }
                }
                CatchUpPhase::ApplyForward {
                    next_height,
                    parent_hash,
                } => {
                    if !budget.can_apply() || !budget.can_read() || !budget.can_crypto() {
                        break ReconcileStepOutcome::YieldMoreWork;
                    }
                    let height = *next_height;
                    let _ = budget.charge_read();
                    let block =
                        match marshal_read(clock, budget, marshal.get_block(Height::new(height)))
                            .await
                        {
                            MarshalRead::Found(block) => block,
                            MarshalRead::Absent | MarshalRead::TimedOut => {
                                self.parked_reason = Some(ParkedReason::MissingBlock);
                                break ReconcileStepOutcome::Parked;
                            }
                        };
                    if block.number() != height || block.parent_hash() != *parent_hash {
                        return Err(archive_error(
                            height,
                            "forward archive block does not extend accepted chain",
                        )
                        .into());
                    }
                    if height == job.anchor_height && block.digest() != job.anchor_digest {
                        return Err(archive_error(height, "forward archive anchor changed").into());
                    }

                    let step = if height == job.anchor_height && !job.temporary_anchor {
                        let verified = job.anchor_proof.clone().expect("verified top F");
                        VerifiedStep::from_direct_f(height, verified)
                    } else {
                        if !budget.can_read() {
                            break ReconcileStepOutcome::YieldMoreWork;
                        }
                        let _ = budget.charge_read();
                        let child = match marshal_read(
                            clock,
                            budget,
                            marshal.get_block(Height::new(height + 1)),
                        )
                        .await
                        {
                            MarshalRead::Found(child) => child,
                            MarshalRead::Absent | MarshalRead::TimedOut => {
                                self.parked_reason = Some(ParkedReason::MissingBlock);
                                break ReconcileStepOutcome::Parked;
                            }
                        };
                        let _ = budget.charge_crypto();
                        let witness = verify_child_parent_witness(
                            &block,
                            &child,
                            &self.deps.certificate_scheme_provider,
                            &self.deps.committee_provider,
                        )?;
                        let Some(witness) = witness else {
                            self.parked_reason = Some(ParkedReason::MissingVerifier);
                            break ReconcileStepOutcome::Parked;
                        };
                        VerifiedStep::from_child_witness(height, block.digest(), witness)
                    };
                    let (head_height, head_hash) = self.current_head();
                    if head_height.saturating_add(1) != height || head_hash != *parent_hash {
                        return Err(archive_error(height, "forward apply head changed").into());
                    }
                    let apply = match step.proof_kind {
                        ParentParticipationProof::Finalization => {
                            self.process_finalization(step.to_finalized(), block.clone())
                                .await?
                        }
                        ParentParticipationProof::CertifiedNotarization => {
                            self.apply_implicit_cn(step.to_cn_witness(), block.clone())
                                .await?
                        }
                    };
                    match apply {
                        ApplyOutcome::Applied => {}
                        ApplyOutcome::PendingPersist => {
                            self.parked_reason = Some(ParkedReason::Persistence);
                            break ReconcileStepOutcome::Parked;
                        }
                        ApplyOutcome::PendingVerifier => {
                            self.parked_reason = Some(ParkedReason::MissingVerifier);
                            break ReconcileStepOutcome::Parked;
                        }
                    }
                    let _ = budget.charge_apply();
                    *parent_hash = block.digest().0;
                    *next_height += 1;
                    if height == job.anchor_height {
                        break ReconcileStepOutcome::Idle;
                    }
                }
            }
        };
        if outcome == ReconcileStepOutcome::Idle {
            self.active_job = None;
        } else {
            self.active_job = Some(job);
        }
        Ok(outcome)
    }
    /// Process a genuine `Finalization` for which we have the full block.
    ///
    /// Splits per-block effects from per-proof effects.
    /// - `ReplayClassification::New`: attempts the local proof write, then
    ///   publishes the authenticated block even if the local handoff fails.
    /// - `ReplayClassification::DuplicateReplay` (same height & hash, e.g.
    ///   same-hash `F(v2)` after `F(v1)` or repair of a missing proof): persists
    ///   the proof and advances `view.last_finalized_round` ONLY when the proof
    ///   is actually available in `parent_cert_store`. Never repeats per-block
    ///   effects (`DealerLog`, `BoundaryOutcome`, `view_advanced`, status, eviction).
    async fn process_finalization(
        &self,
        finalized: Finalized,
        block: ConsensusBlock,
    ) -> eyre::Result<ApplyOutcome> {
        let digest = finalized.digest;
        let block_number = block.number();

        let mut view = self.deps.view.write();

        let is_duplicate = match classify_finalization(
            block_number,
            digest.0,
            view.last_finalized_number,
            view.forkchoice.finalized_block_hash,
        ) {
            ReplayClassification::HistoricalReplay => {
                debug!(
                    block_number,
                    last_finalized = view.last_finalized_number,
                    %digest,
                    "dropping replayed finalization (historical)"
                );
                return Ok(ApplyOutcome::Applied);
            }
            ReplayClassification::DuplicateReplay => {
                let proof_key = CertifiedParentProofKey::new(
                    finalized.round.epoch().get(),
                    finalized.round.view().get(),
                    digest.0,
                );
                if self
                    .deps
                    .parent_cert_store
                    .get_finalization(proof_key)
                    .is_some()
                {
                    view.last_finalized_round = Some(match view.last_finalized_round {
                        Some(last_round) => std::cmp::max(last_round, finalized.round),
                        None => finalized.round,
                    });
                    debug!(
                        block_number,
                        %digest,
                        "dropping replayed finalization (duplicate)"
                    );
                    return Ok(ApplyOutcome::Applied);
                }
                debug!(
                    block_number,
                    %digest,
                    "persisting same-height finalization proof without repeating block effects"
                );
                true
            }
            ReplayClassification::FatalInconsistency => {
                tracing::error!(
                    block_number,
                    %digest,
                    finalized_hash = %view.forkchoice.finalized_block_hash,
                    "fatal same-height finalization inconsistency; stopping FinalizationActor"
                );
                return Err(crate::errors::FinalizationError::SameHeightInconsistency {
                    block_number,
                    digest,
                    finalized_hash: view.forkchoice.finalized_block_hash,
                }
                .into());
            }
            ReplayClassification::New => false,
        };

        let consensus_data = finalized.consensus_data.clone();
        let outcome =
            self.persist_finalization_record(&finalized, &consensus_data, digest, block_number);
        if outcome.failure == Some(ProofPersistFailure::MissingVerifier) {
            return Ok(ApplyOutcome::PendingVerifier);
        }
        if is_duplicate && !outcome.put_succeeded {
            return Ok(match outcome.failure {
                Some(ProofPersistFailure::Store) => ApplyOutcome::PendingPersist,
                Some(ProofPersistFailure::Metadata) => ApplyOutcome::Applied,
                Some(ProofPersistFailure::MissingVerifier) | None => unreachable!(),
            });
        }
        if let Some(committee) = outcome.rekey_committee {
            self.rekey_late_finalize_votes(
                FinalizeVoteTarget {
                    epoch: finalized.round.epoch().get(),
                    view: finalized.round.view().get(),
                    parent_view: consensus_data.finalized_certificate.parent_view,
                    fb_hash: digest.0,
                },
                block_number,
                committee,
            );
        }
        let prune_succeeded = self.prune_parent_cert_store(
            block_number,
            view.last_finalized_round.unwrap_or(finalized.round),
        );
        if outcome.put_succeeded && prune_succeeded {
            crate::metrics::record_finalization_persist_recovered();
        }

        if is_duplicate {
            view.last_finalized_round = Some(match view.last_finalized_round {
                Some(last_round) => std::cmp::max(last_round, finalized.round),
                None => finalized.round,
            });
            if let Some(seed) = finalized.vrf_seed {
                view.prev_randao = seed;
            }
            return Ok(ApplyOutcome::Applied);
        }

        view.last_finalized_round = Some(match view.last_finalized_round {
            Some(last_round) => std::cmp::max(last_round, finalized.round),
            None => finalized.round,
        });

        if let Some(seed) = finalized.vrf_seed {
            view.prev_randao = seed;
        } else {
            self.deps.vrf_safety.mark_degraded();
        }

        view.forkchoice.finalized_block_hash = digest.0;
        view.forkchoice.safe_block_hash = digest.0;
        view.forkchoice.head_block_hash = digest.0;

        view.last_finalized_number = block_number;
        view.last_timestamp_millis =
            std::cmp::max(view.last_timestamp_millis, block.timestamp_millis());
        outbe_tee::authority::note_finalized_authority_revision(block_number);

        drop(view);

        self.apply_block_effects(
            &block,
            digest,
            block_number,
            finalized.round,
            Some(&consensus_data),
            finalized.vrf_seed,
        )?;
        Ok(ApplyOutcome::Applied)
    }

    /// Apply an implicit finalized height whose verified immediate child Phase 1
    /// witness is a `CertifiedNotarization`.
    async fn apply_implicit_cn(
        &self,
        witness: VerifiedChildParentWitness,
        block: ConsensusBlock,
    ) -> eyre::Result<ApplyOutcome> {
        let digest = block.digest();
        let block_number = block.number();

        let mut view = self.deps.view.write();
        match classify_finalization(
            block_number,
            digest.0,
            view.last_finalized_number,
            view.forkchoice.finalized_block_hash,
        ) {
            ReplayClassification::HistoricalReplay | ReplayClassification::DuplicateReplay => {
                return Ok(ApplyOutcome::Applied);
            }
            ReplayClassification::FatalInconsistency => {
                tracing::error!(
                    block_number,
                    %digest,
                    finalized_hash = %view.forkchoice.finalized_block_hash,
                    "fatal same-height finalization inconsistency; stopping FinalizationActor"
                );
                return Err(crate::errors::FinalizationError::SameHeightInconsistency {
                    block_number,
                    digest,
                    finalized_hash: view.forkchoice.finalized_block_hash,
                }
                .into());
            }
            ReplayClassification::New => {}
        }

        let cn_record = CertifiedParentProofRecord {
            format_version: CERTIFIED_PARENT_PROOF_RECORD_FORMAT_VERSION,
            kind: ProofKind::CertifiedNotarization,
            finalized_epoch: witness.round.epoch().get(),
            finalized_view: witness.round.view().get(),
            parent_view: witness.parent_view,
            finalized_block_hash: digest.0,
            committee_set_hash: witness.metadata.committee_set_hash,
            vrf_material_version: witness.metadata.vrf_material_version,
            vrf_group_public_key_hash: witness.metadata.vrf_group_public_key_hash,
            ordered_committee: witness.metadata.ordered_committee,
            signer_bitmap: witness.metadata.signer_bitmap,
            encoded_proof: witness.metadata.proof,
        };
        let put_succeeded = persist_with_budget("put_certified_notarization", || {
            self.deps
                .parent_cert_store
                .put_certified_notarization(cn_record.clone())
        })
        .is_some();
        let prune_succeeded = self.prune_parent_cert_store(
            block_number,
            view.last_finalized_round.unwrap_or(witness.round),
        );
        if put_succeeded && prune_succeeded {
            crate::metrics::record_finalization_persist_recovered();
        }

        view.last_finalized_round = Some(match view.last_finalized_round {
            Some(last_round) => std::cmp::max(last_round, witness.round),
            None => witness.round,
        });
        view.prev_randao = witness.prev_randao;
        view.forkchoice.finalized_block_hash = digest.0;
        view.forkchoice.safe_block_hash = digest.0;
        view.forkchoice.head_block_hash = digest.0;
        view.last_finalized_number = block_number;
        view.last_timestamp_millis =
            std::cmp::max(view.last_timestamp_millis, block.timestamp_millis());
        outbe_tee::authority::note_finalized_authority_revision(block_number);

        drop(view);

        self.apply_block_effects(
            &block,
            digest,
            block_number,
            witness.round,
            None,
            Some(witness.prev_randao),
        )?;
        Ok(ApplyOutcome::Applied)
    }

    /// Per-height block effects that must execute at most once per `(height, hash)`.
    fn apply_block_effects(
        &self,
        block: &ConsensusBlock,
        digest: Digest,
        block_number: u64,
        round: Round,
        consensus_data: Option<&ConsensusData>,
        vrf_seed: Option<alloy_primitives::B256>,
    ) -> eyre::Result<()> {
        self.publish_consensus_status(round, consensus_data, vrf_seed, block_number);
        self.note_finalized_dkg_artifact(block, digest, block_number);
        self.observe_finalized_boundary(block, block_number)?;
        notify_finalized_waiters(self.finalization_notify.as_deref());
        if let Some(tx) = &self.view_advanced {
            let _ = tx.send(block_number);
        }
        self.evict_finalized_block_cache();
        Ok(())
    }

    /// Build and persist the canonical V2 finalization parent-proof record.
    fn persist_finalization_record(
        &self,
        finalized: &Finalized,
        consensus_data: &ConsensusData,
        digest: Digest,
        block_number: u64,
    ) -> PersistFinalizationOutcome {
        let encoded_certificate = consensus_data
            .finalized_certificate
            .encoded_certificate
            .clone();
        let finalized_epoch = finalized.round.epoch().get();
        let ordered_committee = consensus_data
            .finalized_certificate
            .ordered_committee
            .clone();
        let committee_size = ordered_committee.len();
        let (committee_set_hash, vrf_material_version, vrf_group_public_key_hash) = match self
            .deps
            .certificate_scheme_provider
            .scoped(finalized.round.epoch())
        {
            Some(scheme) => {
                match build_committee_prelude(&scheme, &ordered_committee, finalized_epoch) {
                    Ok(prelude) => (
                        prelude.committee_set_hash,
                        prelude.vrf_material_version,
                        prelude.vrf_group_public_key_hash,
                    ),
                    Err(error) => {
                        crate::metrics::record_finalization_snapshot_build_degraded();
                        warn!(
                            target: "outbe::finalization",
                            epoch = finalized_epoch,
                            block_number,
                            %error,
                            "committee prelude build failed on finalization; skipping finalization parent-cert write in degraded mode"
                        );
                        return PersistFinalizationOutcome {
                            rekey_committee: None,
                            put_succeeded: false,
                            failure: Some(ProofPersistFailure::Metadata),
                        };
                    }
                }
            }
            None => {
                crate::metrics::record_finalization_snapshot_build_degraded();
                warn!(
                    target: "outbe::finalization",
                    epoch = finalized_epoch,
                    block_number,
                    "no verifier scheme registered for finalized epoch; skipping finalization \
                     parent-cert write and keeping target pending"
                );
                return PersistFinalizationOutcome {
                    rekey_committee: None,
                    put_succeeded: false,
                    failure: Some(ProofPersistFailure::MissingVerifier),
                };
            }
        };
        let snap = CertifiedParentProofRecord {
            format_version: CERTIFIED_PARENT_PROOF_RECORD_FORMAT_VERSION,
            kind: ProofKind::Finalization {
                finalized_block_number: block_number,
            },
            finalized_block_hash: digest.0,
            finalized_epoch,
            finalized_view: finalized.round.view().get(),
            parent_view: consensus_data.finalized_certificate.parent_view,
            ordered_committee,
            signer_bitmap: consensus_data.finalized_certificate.signer_bitmap.clone(),
            encoded_proof: encoded_certificate,
            committee_set_hash,
            vrf_material_version,
            vrf_group_public_key_hash,
        };
        let put_succeeded = persist_with_budget("put_finalization", || {
            self.deps.parent_cert_store.put_finalization(snap.clone())
        })
        .is_some();
        PersistFinalizationOutcome {
            rekey_committee: Some(LateFinalizeCommittee {
                set_hash: committee_set_hash,
                size: committee_size,
            }),
            put_succeeded,
            failure: (!put_succeeded).then_some(ProofPersistFailure::Store),
        }
    }

    fn rekey_late_finalize_votes(
        &self,
        target: FinalizeVoteTarget,
        block_number: u64,
        committee: LateFinalizeCommittee,
    ) {
        if let Ok(mut store) = self.deps.late_sig_store.lock() {
            store.resolve_finalized_target(target, block_number, committee);
        }
    }

    fn prune_parent_cert_store(&self, block_number: u64, processed_round: Round) -> bool {
        let height_floor = block_number.saturating_sub(PARENT_CERT_KEEP_DEPTH);
        let pruned_height = persist_with_budget("prune_below_height", || {
            self.deps.parent_cert_store.prune_below_height(height_floor)
        });
        let keep_views = u64::from(crate::config::ACTIVITY_TIMEOUT)
            .saturating_mul(crate::config::VIEW_RETENTION_MULTIPLIER);
        let round_floor = Round::new(
            processed_round.epoch(),
            processed_round
                .view()
                .saturating_sub(commonware_consensus::types::ViewDelta::new(keep_views)),
        );
        let pruned_cn = persist_with_budget("prune_certified_notarizations_below_round", || {
            self.deps
                .parent_cert_store
                .prune_certified_notarizations_below_round(round_floor)
        });
        let pruned = pruned_height.unwrap_or(0) + pruned_cn.unwrap_or(0);
        crate::metrics::record_parent_cert_store_size(self.deps.parent_cert_store.len());
        crate::metrics::record_parent_cert_record_pruned(pruned);
        if let Some(oldest) = self.deps.parent_cert_store.oldest_stored_height() {
            crate::metrics::record_parent_cert_retained_depth(block_number.saturating_sub(oldest));
        } else {
            crate::metrics::record_parent_cert_retained_depth(0);
        }
        pruned_height.is_some() && pruned_cn.is_some()
    }

    fn publish_consensus_status(
        &self,
        round: Round,
        consensus_data: Option<&ConsensusData>,
        vrf_seed: Option<alloy_primitives::B256>,
        block_number: u64,
    ) {
        if let Some(ref bridge) = self.deps.bridge {
            let vrf_safety = self.deps.vrf_safety.snapshot();
            let connected_peers = consensus_data
                .map(|data| {
                    u32::try_from(
                        data.finalized_certificate
                            .signer_bitmap
                            .iter()
                            .filter(|signed| **signed != 0)
                            .count(),
                    )
                    .unwrap_or(u32::MAX)
                })
                .unwrap_or(0);
            bridge.set_consensus_status(ConsensusStatus {
                current_view: round.view().get(),
                connected_peers,
                last_finalized_block: block_number,
                last_vrf_seed: vrf_seed,
                randomness_status: vrf_safety.randomness_status,
                vrf_material_version: vrf_safety.vrf_material_version,
                last_dkg_activation_height: vrf_safety.last_dkg_activation_height,
                next_planned_activation_height: vrf_safety.next_planned_activation_height,
                vrf_expiry_height: vrf_safety.vrf_expiry_height,
            });
        }
    }

    fn observe_finalized_boundary(
        &self,
        block: &ConsensusBlock,
        height: u64,
    ) -> std::result::Result<(), crate::errors::FinalizationError> {
        let Some(epocher) = &self.epocher else {
            return Ok(());
        };
        let Ok(Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary))) =
            extract_header_artifact_from_block(block)
        else {
            return Ok(());
        };
        if boundary.epoch == 0 {
            return Ok(());
        }
        epocher
            .observe_boundary(Epoch::new(boundary.epoch), Height::new(height))
            .map_err(
                |source| crate::errors::FinalizationError::BoundaryObservationFailed {
                    epoch: boundary.epoch,
                    height,
                    source,
                },
            )
    }

    fn note_finalized_dkg_artifact(
        &self,
        block: &ConsensusBlock,
        digest: Digest,
        block_number: u64,
    ) {
        match extract_header_artifact_from_block(block) {
            Ok(artifact) => {
                self.deps.dkg_manager.note_finalized_header_artifact_at(
                    block_number,
                    digest.0,
                    artifact.as_ref(),
                );
            }
            Err(error) => {
                warn!(
                    %digest,
                    %error,
                    "finalized block carries invalid DKG header artifact"
                );
            }
        }
    }

    fn evict_finalized_block_cache(&self) {
        let finalized_num = self.deps.view.read().last_finalized_number;
        self.deps.block_cache.evict_at_or_below(finalized_num);
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
