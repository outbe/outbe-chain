use std::{
    collections::btree_map::Entry,
    collections::{BTreeMap, VecDeque},
    fmt,
    future::Future,
    num::NonZeroU32,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};

use alloy_primitives::{keccak256, Address, Bytes, B256};
use commonware_codec::Read as _;
use commonware_consensus::types::Epoch;
use commonware_cryptography::bls12381::{
    self,
    dkg::feldman_desmedt::{observe, DealerLog, Info, Logs, Output, SignedDealerLog},
    primitives::{
        sharing::{Mode, Sharing},
        variant::MinSig,
    },
};
use commonware_parallel::Sequential;
use commonware_utils::{
    ordered::{Quorum, Set},
    N3f1,
};
use eyre::{ensure, Result, WrapErr};
use outbe_primitives::{
    consensus::{DkgBoundaryArtifact, ReshareResult, TeeReshareRegistration},
    reshare_artifact::{encode_boundary_artifact, ConsensusHeaderArtifact},
};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::{
    block::ConsensusBlock, config, finalization::util::extract_header_artifact_from_block,
    util::rate_limit::LogRateLimiter, validators::ValidatorSet,
};

/// Boxed, `Send` future returned by [`AncestryReader`] lookups. Mirrors the
/// marshal-backed block lookup that the application handler provides; the trait
/// methods carry no async context, so each returns an owned future.
pub type BlockLookupFuture<'a> = Pin<Box<dyn Future<Output = Option<ConsensusBlock>> + Send + 'a>>;

/// Read-only ancestry access used by [`Mailbox::resolve_boundary`] to walk a
/// proposal/verification parent chain looking for an already-committed DKG
/// boundary. The production implementation (`MarshalAncestryReader`) lives in
/// the application handler — `dkg_manager` is the sole consumer and defines the
/// contract it needs.
pub trait AncestryReader: Send + Sync {
    fn get_block_by_height<'a>(&'a self, height: u64) -> BlockLookupFuture<'a>;
    fn get_block_by_hash<'a>(&'a self, hash: B256) -> BlockLookupFuture<'a>;
    fn is_ready(&self) -> bool;
}

/// Outcome of [`Mailbox::resolve_boundary`]: whether the proposer/verifier must
/// emit the pending DKG `BoundaryOutcome`, the parent ancestry already committed
/// it, or there is no pending boundary at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundaryRequirement {
    NoPending,
    AlreadyCommitted,
    MustEmit,
}

/// Failure modes of [`Mailbox::resolve_boundary`]. `Unavailable` means the
/// ancestry could not be read (retry/forfeit), `Conflict` means the ancestry
/// carries a contradictory boundary (deterministic reject).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundaryRequirementError {
    Unavailable(String),
    Conflict(String),
}

impl BoundaryRequirementError {
    pub fn is_unavailable(&self) -> bool {
        matches!(self, Self::Unavailable(_))
    }
}

impl fmt::Display for BoundaryRequirementError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(message) | Self::Conflict(message) => f.write_str(message),
        }
    }
}

fn boundary_scan_floor(pending: &DkgBoundaryArtifact) -> u64 {
    if pending.freeze_height <= pending.planned_activation_height {
        pending.freeze_height
    } else {
        pending
            .planned_activation_height
            .saturating_sub(config::DEFAULT_DKG_ACTIVATION_GRACE_BLOCKS)
    }
}

fn block_boundary_artifact(block: &ConsensusBlock) -> Result<Option<DkgBoundaryArtifact>, String> {
    match extract_header_artifact_from_block(block)? {
        Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary)) => Ok(Some(boundary)),
        _ => Ok(None),
    }
}

#[derive(Clone, Debug)]
pub struct Mailbox {
    inner: Arc<Mutex<State>>,
    duplicate_dealer_log_limiter: Arc<LogRateLimiter>,
}

pub const BOUNDARY_STATUS_CACHE_SIZE: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommittedDkgBoundary {
    pub artifact: DkgBoundaryArtifact,
    pub artifact_hash: B256,
    pub block_number: u64,
    pub block_hash: B256,
}

// `BoundaryCommitted` carries the full committed boundary; the other variants are
// unit. Boxing it would ripple through every match/construct site for a status
// enum that is held briefly per epoch — not worth it for the stack-size delta.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BoundaryStatus {
    NoBoundarySeen,
    BoundaryCommitted(CommittedDkgBoundary),
    Conflict,
}

#[derive(Clone, Debug)]
struct BoundaryStatusCacheEntry {
    query_artifact_hash: B256,
    status: BoundaryStatus,
}

#[derive(Clone, Debug)]
struct CeremonyState {
    epoch: Epoch,
    info: Info<MinSig, bls12381::PublicKey>,
    max_players: NonZeroU32,
    dealers: Set<bls12381::PublicKey>,
    local_dealer_log: Option<Bytes>,
    pending_dealer_logs: BTreeMap<bls12381::PublicKey, Bytes>,
    finalized_dealer_log_tx: Option<mpsc::UnboundedSender<Bytes>>,
    finalized_dealer_logs: BTreeMap<bls12381::PublicKey, DealerLog<MinSig, bls12381::PublicKey>>,
    canonical_output: Option<Output<MinSig, bls12381::PublicKey>>,
}

#[derive(Clone, Debug, Default)]
struct State {
    pending_boundary: Option<DkgBoundaryArtifact>,
    committed_boundary: Option<CommittedDkgBoundary>,
    boundary_status_cache: BTreeMap<B256, BoundaryStatusCacheEntry>,
    boundary_status_lru: VecDeque<B256>,
    ceremony: Option<CeremonyState>,
}

#[derive(Debug)]
struct VerifiedDealerLog {
    dealer: bls12381::PublicKey,
    log: DealerLog<MinSig, bls12381::PublicKey>,
}

enum PendingDealerLogOutcome {
    Stored,
    DuplicateSame,
    DuplicateDifferent { dealer: bls12381::PublicKey },
    IgnoredCanonical,
}

impl Mailbox {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn boundary_artifact_hash(artifact: &DkgBoundaryArtifact) -> Result<B256> {
        let bytes = encode_boundary_artifact(artifact)
            .map_err(|error| eyre::eyre!("failed to encode DKG boundary artifact: {error}"))?;
        Ok(keccak256(bytes.as_ref()))
    }

    fn cache_boundary_status(
        state: &mut State,
        parent_hash: B256,
        query_artifact_hash: B256,
        status: BoundaryStatus,
    ) {
        state
            .boundary_status_lru
            .retain(|hash| *hash != parent_hash);
        state.boundary_status_lru.push_back(parent_hash);
        state.boundary_status_cache.insert(
            parent_hash,
            BoundaryStatusCacheEntry {
                query_artifact_hash,
                status,
            },
        );

        while state.boundary_status_cache.len() > BOUNDARY_STATUS_CACHE_SIZE {
            let Some(victim) = state.boundary_status_lru.pop_front() else {
                break;
            };
            state.boundary_status_cache.remove(&victim);
        }
    }

    fn clear_boundary_status_cache_inner(state: &mut State) {
        state.boundary_status_cache.clear();
        state.boundary_status_lru.clear();
    }

    fn cached_boundary_status(
        &self,
        parent_hash: B256,
        query_artifact_hash: B256,
    ) -> Option<BoundaryStatus> {
        self.with_state(|state| {
            let status = state
                .boundary_status_cache
                .get(&parent_hash)
                .filter(|entry| entry.query_artifact_hash == query_artifact_hash)
                .map(|entry| entry.status.clone());
            if status.is_some() {
                state
                    .boundary_status_lru
                    .retain(|hash| *hash != parent_hash);
                state.boundary_status_lru.push_back(parent_hash);
            }
            status
        })
    }

    fn record_boundary_status(
        &self,
        parent_hash: B256,
        query_artifact_hash: B256,
        status: BoundaryStatus,
    ) {
        self.with_state(|state| {
            Self::cache_boundary_status(state, parent_hash, query_artifact_hash, status);
        });
    }

    fn evict_boundary_status(&self, parent_hash: B256) -> bool {
        self.with_state(|state| {
            state
                .boundary_status_lru
                .retain(|hash| *hash != parent_hash);
            state.boundary_status_cache.remove(&parent_hash).is_some()
        })
    }

    pub fn clear_boundary_status_cache(&self) {
        self.with_state(Self::clear_boundary_status_cache_inner);
    }

    /// Decide whether a proposer/verifier must emit the pending DKG
    /// `BoundaryOutcome`, whether the parent ancestry already committed it, or
    /// whether there is no pending boundary.
    ///
    /// The boundary-status cache (process-local memoization keyed by
    /// `(parent_hash, pending_artifact_hash)`) is consulted first; on a miss the
    /// parent chain is walked via `ancestry` down to `boundary_scan_floor`, and
    /// the resolved verdict is cached. Each cache touch is a discrete
    /// `with_state` call — no lock guard is ever held across an `.await`.
    ///
    /// Both the propose path (`build_block`) and the verify path
    /// (`validate_header_consensus_artifacts`) call this, so the result must be
    /// deterministic for a given `(parent, pending)` pair.
    pub async fn resolve_boundary<R: AncestryReader>(
        &self,
        parent: Option<&ConsensusBlock>,
        pending: Option<&DkgBoundaryArtifact>,
        ancestry: &R,
    ) -> Result<BoundaryRequirement, BoundaryRequirementError> {
        let Some(pending) = pending else {
            return Ok(BoundaryRequirement::NoPending);
        };
        let Some(parent) = parent else {
            return Ok(BoundaryRequirement::MustEmit);
        };
        let original_parent_hash = parent.block_hash();
        let pending_hash = Self::boundary_artifact_hash(pending)
            .map_err(|error| BoundaryRequirementError::Unavailable(error.to_string()))?;

        if let Some(status) = self.cached_boundary_status(original_parent_hash, pending_hash) {
            return match status {
                BoundaryStatus::NoBoundarySeen => Ok(BoundaryRequirement::MustEmit),
                BoundaryStatus::BoundaryCommitted(committed) => {
                    if committed.artifact_hash == pending_hash && committed.artifact == *pending {
                        Ok(BoundaryRequirement::AlreadyCommitted)
                    } else {
                        Err(BoundaryRequirementError::Conflict(
                            "cached DKG BoundaryOutcome conflicts with pending boundary"
                                .to_string(),
                        ))
                    }
                }
                BoundaryStatus::Conflict => Err(BoundaryRequirementError::Conflict(
                    "cached parent ancestry carries conflicting DKG BoundaryOutcome".to_string(),
                )),
            };
        }

        if !ancestry.is_ready() {
            return Err(BoundaryRequirementError::Unavailable(
                "DKG boundary ancestry unavailable: marshal ancestry reader is not ready"
                    .to_string(),
            ));
        }

        let mut current = parent.clone();
        let scan_floor = boundary_scan_floor(pending);
        loop {
            if let Some(boundary) =
                block_boundary_artifact(&current).map_err(BoundaryRequirementError::Unavailable)?
            {
                let boundary_hash = Self::boundary_artifact_hash(&boundary)
                    .map_err(|error| BoundaryRequirementError::Unavailable(error.to_string()))?;
                if boundary_hash == pending_hash && boundary == *pending {
                    let committed = CommittedDkgBoundary {
                        artifact: boundary,
                        artifact_hash: boundary_hash,
                        block_number: current.number(),
                        block_hash: current.block_hash(),
                    };
                    self.record_boundary_status(
                        original_parent_hash,
                        pending_hash,
                        BoundaryStatus::BoundaryCommitted(committed),
                    );
                    return Ok(BoundaryRequirement::AlreadyCommitted);
                }
                if boundary.epoch == pending.epoch {
                    self.record_boundary_status(
                        original_parent_hash,
                        pending_hash,
                        BoundaryStatus::Conflict,
                    );
                    return Err(BoundaryRequirementError::Conflict(
                        // Outbe has one DKG boundary artifact per epoch. Same
                        // epoch with different bytes means a local state bug or a
                        // conflicting proposal, not an alternate valid activation.
                        "parent ancestry carries conflicting DKG BoundaryOutcome".to_string(),
                    ));
                }
            }

            if current.number() == 0 || current.number() <= scan_floor {
                self.record_boundary_status(
                    original_parent_hash,
                    pending_hash,
                    BoundaryStatus::NoBoundarySeen,
                );
                return Ok(BoundaryRequirement::MustEmit);
            }

            let expected_hash = current.parent_hash();
            let expected_height = current.number().saturating_sub(1);
            let mut next = ancestry.get_block_by_height(expected_height).await;
            let needs_hash_lookup = match next.as_ref() {
                Some(block) if block.block_hash() == expected_hash => false,
                Some(block) => {
                    let stale_hash = block.block_hash();
                    if self.evict_boundary_status(stale_hash) {
                        debug!(
                            expected_height,
                            stale_hash = %stale_hash,
                            expected_hash = %expected_hash,
                            "evicted stale DKG boundary status after non-canonical ancestry height hit"
                        );
                    }
                    true
                }
                None => true,
            };
            if needs_hash_lookup {
                next = ancestry.get_block_by_hash(expected_hash).await;
            }
            let Some(next) = next else {
                return Err(BoundaryRequirementError::Unavailable(format!(
                    "DKG boundary ancestry unavailable before seeing pending boundary: missing parent {expected_hash} at height {expected_height}",
                )));
            };
            if next.number() != expected_height {
                return Err(BoundaryRequirementError::Unavailable(format!(
                    "DKG boundary ancestry unavailable: parent {expected_hash} resolved at height {}, expected {expected_height}",
                    next.number()
                )));
            };
            current = next;
        }
    }

    pub fn note_bootstrap_outcome(&self, artifact: DkgBoundaryArtifact) {
        self.with_state(|state| {
            state.pending_boundary = Some(artifact);
            state.committed_boundary = None;
            Self::clear_boundary_status_cache_inner(state);
        });
    }

    pub fn note_ceremony_started(
        &self,
        epoch: Epoch,
        round: u64,
        previous_output: Option<Output<MinSig, bls12381::PublicKey>>,
        participants: Set<bls12381::PublicKey>,
    ) -> Result<()> {
        self.note_ceremony_started_with_finalized_log_tx(
            epoch,
            round,
            previous_output,
            participants,
            None,
        )
    }

    pub fn note_ceremony_started_with_finalized_log_tx(
        &self,
        epoch: Epoch,
        round: u64,
        previous_output: Option<Output<MinSig, bls12381::PublicKey>>,
        participants: Set<bls12381::PublicKey>,
        finalized_dealer_log_tx: Option<mpsc::UnboundedSender<Bytes>>,
    ) -> Result<()> {
        let max_players = NonZeroU32::new(participants.len() as u32)
            .ok_or_else(|| eyre::eyre!("DKG ceremony requires at least one participant"))?;
        let dealers = previous_output
            .as_ref()
            .map(|output| output.players().clone())
            .unwrap_or_else(|| participants.clone());
        let info = Info::<MinSig, bls12381::PublicKey>::new::<N3f1>(
            &config::outbe_app_namespace(),
            round,
            previous_output,
            Mode::NonZeroCounter,
            dealers.clone(),
            participants.clone(),
        )
        .wrap_err("failed to build DKG ceremony info")?;

        self.with_state(|state| {
            state.ceremony = Some(CeremonyState {
                epoch,
                info,
                max_players,
                dealers,
                local_dealer_log: None,
                pending_dealer_logs: BTreeMap::new(),
                finalized_dealer_log_tx,
                finalized_dealer_logs: BTreeMap::new(),
                canonical_output: None,
            });
        });
        Ok(())
    }

    pub fn note_local_dealer_log(&self, epoch: Epoch, bytes: Bytes) -> Result<()> {
        let dealer = self.verify_dealer_log_sync(epoch, bytes.as_ref())?;
        self.with_state(|state| {
            if let Some(ceremony) = state.ceremony.as_mut() {
                if ceremony.epoch == epoch {
                    ceremony.pending_dealer_logs.remove(&dealer);
                    ceremony.local_dealer_log = Some(bytes);
                }
            }
        });
        Ok(())
    }

    pub fn note_ceremony_completed(&self, artifact: DkgBoundaryArtifact) {
        self.with_state(|state| {
            state.pending_boundary = Some(artifact);
            state.committed_boundary = None;
            Self::clear_boundary_status_cache_inner(state);
            if let Some(ceremony) = state.ceremony.as_mut() {
                ceremony.local_dealer_log = None;
                ceremony.pending_dealer_logs.clear();
            }
        });
    }

    pub fn note_recovered_pending_boundary(&self, artifact: DkgBoundaryArtifact) {
        self.with_state(|state| {
            state.pending_boundary = Some(artifact);
            state.committed_boundary = None;
            Self::clear_boundary_status_cache_inner(state);
        });
    }

    /// Return the pending boundary for `epoch`.
    ///
    /// Proposer/verifier validity uses this together with parent-chain
    /// evidence. The pending artifact alone must not decide whether a block is
    /// valid; the caller must derive emission/duplication from the parent
    /// ancestry snapshot.
    pub async fn pending_boundary_artifact(&self, epoch: Epoch) -> Option<DkgBoundaryArtifact> {
        self.with_state(|state| {
            state
                .pending_boundary
                .clone()
                .filter(|artifact| artifact.epoch == epoch.get())
        })
    }

    pub async fn take_committed_boundary_artifact(&self) -> Option<DkgBoundaryArtifact> {
        self.with_state(|state| {
            let pending = state.pending_boundary.as_ref()?;
            let committed = state.committed_boundary.as_ref()?;
            if &committed.artifact != pending {
                return None;
            }
            let committed = state.committed_boundary.take()?;
            state.pending_boundary = None;
            Some(committed.artifact)
        })
    }

    pub async fn get_dealer_log(&self, epoch: Epoch) -> Option<Bytes> {
        self.with_state(|state| {
            let ceremony = state
                .ceremony
                .as_ref()
                .filter(|ceremony| ceremony.epoch == epoch)?;
            ceremony.local_dealer_log.clone().or_else(|| {
                ceremony
                    .pending_dealer_logs
                    .iter()
                    .next()
                    .map(|(_, bytes)| bytes.clone())
            })
        })
    }

    pub fn canonical_output(&self, epoch: Epoch) -> Option<Output<MinSig, bls12381::PublicKey>> {
        self.with_state(|state| {
            state
                .ceremony
                .as_ref()
                .filter(|ceremony| ceremony.epoch == epoch)
                .and_then(|ceremony| ceremony.canonical_output.clone())
        })
    }

    /// Verify a carried boundary against the pending boundary only. This does
    /// not decide whether the boundary is required; callers must separately
    /// derive that from the parent chain snapshot.
    pub async fn verify_pending_boundary_artifact(
        &self,
        epoch: Epoch,
        artifact: &DkgBoundaryArtifact,
    ) -> Result<()> {
        let expected = self
            .with_state(|state| state.pending_boundary.clone())
            .ok_or_else(|| eyre::eyre!("no pending DKG boundary artifact"))?;
        ensure!(
            epoch.get() == artifact.epoch,
            "boundary artifact epoch {} does not match verify epoch {}",
            artifact.epoch,
            epoch.get()
        );
        ensure!(
            expected == *artifact,
            "carried DKG boundary artifact does not match local expectation"
        );
        Ok(())
    }

    pub async fn verify_dealer_log(
        &self,
        epoch: Epoch,
        bytes: Vec<u8>,
    ) -> Result<bls12381::PublicKey> {
        self.verify_dealer_log_sync(epoch, &bytes)
    }

    pub fn note_pending_dealer_log(&self, epoch: Epoch, bytes: Bytes) -> Result<()> {
        let dealer = self.verify_dealer_log_sync(epoch, bytes.as_ref())?;
        let outcome = self.with_state(|state| {
            let Some(ceremony) = state
                .ceremony
                .as_mut()
                .filter(|ceremony| ceremony.epoch == epoch)
            else {
                return PendingDealerLogOutcome::IgnoredCanonical;
            };

            if ceremony.finalized_dealer_logs.contains_key(&dealer) {
                return PendingDealerLogOutcome::IgnoredCanonical;
            }
            if ceremony.local_dealer_log.as_ref() == Some(&bytes) {
                return PendingDealerLogOutcome::DuplicateSame;
            }

            match ceremony.pending_dealer_logs.entry(dealer.clone()) {
                Entry::Vacant(entry) => {
                    entry.insert(bytes);
                    PendingDealerLogOutcome::Stored
                }
                Entry::Occupied(entry) if entry.get() == &bytes => {
                    PendingDealerLogOutcome::DuplicateSame
                }
                Entry::Occupied(_) => PendingDealerLogOutcome::DuplicateDifferent {
                    dealer: dealer.clone(),
                },
            }
        });

        match outcome {
            PendingDealerLogOutcome::Stored => {
                debug!(?dealer, "recorded pending P2P DKG dealer log candidate");
            }
            PendingDealerLogOutcome::DuplicateSame => {
                debug!(?dealer, "ignoring duplicate pending P2P DKG dealer log");
            }
            PendingDealerLogOutcome::DuplicateDifferent { dealer } => {
                if let Some(suppressed_since_last) = self.duplicate_dealer_log_limiter.check() {
                    warn!(
                        ?dealer,
                        suppressed_since_last, "rejecting conflicting pending P2P DKG dealer log"
                    );
                }
            }
            PendingDealerLogOutcome::IgnoredCanonical => {
                debug!(
                    ?dealer,
                    "ignoring pending P2P DKG dealer log for already-finalized dealer"
                );
            }
        }
        Ok(())
    }

    pub fn note_finalized_header_artifact(&self, artifact: Option<&ConsensusHeaderArtifact>) {
        self.note_finalized_header_artifact_at(0, B256::ZERO, artifact);
    }

    pub fn note_finalized_header_artifact_at(
        &self,
        block_number: u64,
        block_hash: B256,
        artifact: Option<&ConsensusHeaderArtifact>,
    ) {
        self.with_state(|state| match artifact {
            Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary)) => {
                match Self::boundary_artifact_hash(boundary) {
                    Ok(artifact_hash) => {
                        let committed = CommittedDkgBoundary {
                            artifact: boundary.clone(),
                            artifact_hash,
                            block_number,
                            block_hash,
                        };
                        if state.pending_boundary.as_ref() == Some(boundary) {
                            state.committed_boundary = Some(committed.clone());
                        }
                        if block_hash != B256::ZERO {
                            Self::cache_boundary_status(
                                state,
                                block_hash,
                                artifact_hash,
                                BoundaryStatus::BoundaryCommitted(committed),
                            );
                        }
                    }
                    Err(error) => {
                        warn!(%error, "failed to record finalized DKG boundary status");
                    }
                }
                if let Some(ceremony) = state.ceremony.as_mut() {
                    ceremony.local_dealer_log = None;
                    ceremony.pending_dealer_logs.clear();
                }
            }
            Some(ConsensusHeaderArtifact::DealerLog(bytes)) => {
                if let Some(ceremony) = state.ceremony.as_mut() {
                    let verified = match verify_dealer_log_for_ceremony(ceremony, bytes.as_ref()) {
                        Ok(verified) => verified,
                        Err(error) => {
                            warn!(%error, "ignoring finalized DKG dealer log");
                            return;
                        }
                    };
                    if ceremony.local_dealer_log.as_ref() == Some(bytes) {
                        ceremony.local_dealer_log = None;
                    }
                    ceremony.pending_dealer_logs.remove(&verified.dealer);
                    if ceremony
                        .finalized_dealer_logs
                        .contains_key(&verified.dealer)
                    {
                        debug!(
                            dealer = ?verified.dealer,
                            "ignoring duplicate chain-finalized DKG dealer log"
                        );
                        return;
                    }
                    ceremony
                        .finalized_dealer_logs
                        .insert(verified.dealer.clone(), verified.log);
                    debug!(
                        dealer = ?verified.dealer,
                        logs = ceremony.finalized_dealer_logs.len(),
                        "recorded finalized DKG dealer log"
                    );
                    if let Some(tx) = &ceremony.finalized_dealer_log_tx {
                        if tx.send(bytes.clone()).is_err() {
                            debug!("active DKG actor is no longer accepting finalized dealer logs");
                        }
                    }
                    if ceremony.canonical_output.is_none() {
                        let mut logs =
                            Logs::<MinSig, bls12381::PublicKey, N3f1>::new(ceremony.info.clone());
                        for (dealer, log) in ceremony.finalized_dealer_logs.clone() {
                            logs.record(dealer, log);
                        }
                        match observe::<
                            MinSig,
                            bls12381::PublicKey,
                            N3f1,
                            commonware_cryptography::bls12381::Batch,
                        >(&mut rand_core::OsRng, logs, &Sequential)
                        {
                            Ok(output) => {
                                let output_hash = dkg_output_hash(&output);
                                let polynomial_hash = public_polynomial_hash(output.public());
                                info!(
                                    %output_hash,
                                    %polynomial_hash,
                                    logs = ceremony.finalized_dealer_logs.len(),
                                    dealers = output.dealers().len(),
                                    players = output.players().len(),
                                    "canonical DKG output reconstructed from finalized dealer logs"
                                );
                                ceremony.canonical_output = Some(output);
                            }
                            Err(error) => {
                                debug!(
                                    %error,
                                    logs = ceremony.finalized_dealer_logs.len(),
                                    "finalized DKG dealer logs do not yet produce an output"
                                );
                            }
                        }
                    }
                }
            }
            None => {}
        });
    }

    fn verify_dealer_log_sync(&self, epoch: Epoch, bytes: &[u8]) -> Result<bls12381::PublicKey> {
        self.with_state(|state| {
            let ceremony = state
                .ceremony
                .as_ref()
                .filter(|ceremony| ceremony.epoch == epoch)
                .ok_or_else(|| eyre::eyre!("no active DKG ceremony for epoch {}", epoch.get()))?;
            verify_dealer_log_for_ceremony(ceremony, bytes).map(|verified| verified.dealer)
        })
    }

    fn with_state<T>(&self, f: impl FnOnce(&mut State) -> T) -> T {
        let mut state = match self.inner.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        f(&mut state)
    }
}

impl Default for Mailbox {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(State::default())),
            duplicate_dealer_log_limiter: Arc::new(LogRateLimiter::new(Duration::from_secs(5))),
        }
    }
}

fn verify_dealer_log_for_ceremony(
    ceremony: &CeremonyState,
    bytes: &[u8],
) -> Result<VerifiedDealerLog> {
    let mut reader = bytes;
    let signed_log = SignedDealerLog::<MinSig, bls12381::PrivateKey>::read_cfg(
        &mut reader,
        &ceremony.max_players,
    )
    .wrap_err("failed decoding signed dealer log")?;
    ensure!(reader.is_empty(), "trailing bytes after signed dealer log");

    let (dealer, log) = signed_log
        .check(&ceremony.info)
        .ok_or_else(|| eyre::eyre!("signed dealer log failed cryptographic verification"))?;
    ensure!(
        ceremony.dealers.index(&dealer).is_some(),
        "signed dealer log dealer is not in ceremony committee"
    );
    Ok(VerifiedDealerLog { dealer, log })
}

pub struct BoundaryArtifactInput<'a> {
    pub epoch: Epoch,
    pub validator_set: &'a ValidatorSet,
    pub output: &'a Output<MinSig, bls12381::PublicKey>,
    pub is_full_dkg: bool,
    pub dkg_cycle: u64,
    pub freeze_height: u64,
    pub planned_activation_height: u64,
    pub vrf_material_version: u64,
    pub is_validator_set_change: bool,
    /// New-committee per-validator TEE key re-registrations to carry in this
    /// boundary outcome after a tribute-offer reshare (R5). Empty for a
    /// non-reshare boundary; populated by the epoch loop from the TEE reshare
    /// result. The offer key is preserved across the reshare.
    pub tee_reshare_registrations: Vec<TeeReshareRegistration>,
}

pub fn build_boundary_artifact(input: BoundaryArtifactInput<'_>) -> Result<DkgBoundaryArtifact> {
    let BoundaryArtifactInput {
        epoch,
        validator_set,
        output,
        is_full_dkg,
        dkg_cycle,
        freeze_height,
        planned_activation_height,
        vrf_material_version,
        is_validator_set_change,
        tee_reshare_registrations,
    } = input;

    let participants = output.players();
    let polynomial = output.public();
    let mut new_active_set = Vec::with_capacity(participants.len());
    for bls_pk in participants.iter() {
        let Some(idx) = validator_set.public_keys.iter().position(|pk| pk == bls_pk) else {
            return Err(eyre::eyre!(
                "DKG output contains a player absent from the validator set"
            ));
        };
        new_active_set.push(validator_set.addresses[idx]);
    }

    let group_pk_bytes_vec = commonware_codec::Encode::encode(polynomial.public()).to_vec();
    let vrf_group_public_key = alloy_primitives::keccak256(&group_pk_bytes_vec);
    let active_set_hash = hash_active_set(&new_active_set);
    let target_set_hash =
        hash_target_set(&new_active_set, freeze_height, planned_activation_height);
    let outcome = encode_outcome(epoch, output, is_full_dkg);

    // V2 canonical committee snapshot identit.
    //
    // Per-entry MinPk pubkeys are encoded from the DKG `participants` list, in
    // the same Commonware participant-index order as `new_active_set` (we built
    // `new_active_set` by iterating `participants` above). Length must be 48
    // bytes — `bls12381::PublicKey` is MinPk-compressed.
    let encoded_pubkeys: Vec<Vec<u8>> = participants
        .iter()
        .map(|bls_pk| commonware_codec::Encode::encode(bls_pk).to_vec())
        .collect();
    // Single canonical builder (shared with the finalization actor/resolver and
    // the reporter). `new_active_set[i]` was built by iterating `participants`
    // above, so it is in the same order as `encoded_pubkeys[i]`. The proposer
    // carries the full polynomial commitment hash; it is not folded into
    // committee_set_hash_v2 (the executor re-derives it from the boundary
    // `outcome`).
    let committee_snapshot = crate::proof::build_committee_snapshot(
        &new_active_set,
        &encoded_pubkeys,
        vrf_material_version,
        group_pk_bytes_vec.clone(),
        public_polynomial_hash(output.public()),
    )
    .map_err(|e| eyre::eyre!("DKG boundary committee snapshot build failed: {e}"))?;
    let committee_set_hash = crate::proof::committee_set_hash_v2(epoch.get(), &committee_snapshot);

    Ok(DkgBoundaryArtifact {
        epoch: epoch.get(),
        dkg_cycle,
        freeze_height,
        planned_activation_height,
        target_set_hash,
        vrf_material_version,
        vrf_group_public_key,
        vrf_group_public_key_bytes: Bytes::from(group_pk_bytes_vec),
        committee_set_hash,
        is_validator_set_change,
        outcome,
        is_full_dkg,
        tee_recipient_pubkeys: Vec::new(),
        tee_reshare_registrations,
        reshare: ReshareResult {
            new_active_set,
            active_set_hash,
        },
    })
}

fn hash_active_set(addresses: &[Address]) -> B256 {
    let mut bytes = Vec::with_capacity(8 + addresses.len() * 20);
    bytes.extend_from_slice(&(addresses.len() as u64).to_be_bytes());
    for address in addresses {
        bytes.extend_from_slice(address.as_slice());
    }
    alloy_primitives::keccak256(bytes)
}

fn hash_target_set(
    addresses: &[Address],
    freeze_height: u64,
    planned_activation_height: u64,
) -> B256 {
    let mut bytes = Vec::with_capacity(24 + addresses.len() * 20);
    bytes.extend_from_slice(&freeze_height.to_be_bytes());
    bytes.extend_from_slice(&planned_activation_height.to_be_bytes());
    bytes.extend_from_slice(&(addresses.len() as u64).to_be_bytes());
    for address in addresses {
        bytes.extend_from_slice(address.as_slice());
    }
    alloy_primitives::keccak256(bytes)
}

fn encode_outcome(
    epoch: Epoch,
    output: &Output<MinSig, bls12381::PublicKey>,
    is_full_dkg: bool,
) -> Bytes {
    let mut buf = Vec::new();
    buf.extend_from_slice(b"ODKO");
    buf.push(0x02);
    buf.extend_from_slice(&epoch.get().to_be_bytes());
    buf.push(u8::from(is_full_dkg));

    let output_bytes = commonware_codec::Encode::encode(output);
    buf.extend_from_slice(&(output_bytes.len() as u32).to_be_bytes());
    buf.extend_from_slice(output_bytes.as_ref());

    Bytes::from(buf)
}

pub fn dkg_output_hash(output: &Output<MinSig, bls12381::PublicKey>) -> B256 {
    alloy_primitives::keccak256(commonware_codec::Encode::encode(output))
}

/// Enforce the consensus-critical invariant that a locally-computed DKG `local`
/// output matches the chain-`canonical` output (the one the chain reconstructs
/// from finalized dealer logs).
///
/// The DKG actor's output is advisory; the authority is the canonical output.
/// Activating a VRF key from a local output that disagrees with canonical would
/// diverge this node's randomness from the network, so a mismatch is fatal. This
/// is the single definition of the check shared by every activation and recovery
/// path: callers resolve `canonical` from their own authoritative source (the
/// live manager via [`Mailbox::canonical_output`] vs a decoded boundary
/// artifact) and pass both outputs in; `context` names the call site for the
/// error.
pub fn assert_canonical_output(
    local: &Output<MinSig, bls12381::PublicKey>,
    canonical: &Output<MinSig, bls12381::PublicKey>,
    context: &str,
) -> eyre::Result<()> {
    if local != canonical {
        return Err(eyre::eyre!(
            "local DKG output does not match canonical finalized-log output ({context}): \
             local {}, canonical {}",
            dkg_output_hash(local),
            dkg_output_hash(canonical),
        ));
    }
    Ok(())
}

pub fn public_polynomial_hash(polynomial: &Sharing<MinSig>) -> B256 {
    alloy_primitives::keccak256(commonware_codec::Encode::encode(polynomial))
}

/// Decode the ODKO-wrapped boundary `outcome` and return
/// `keccak256(Encode(full public polynomial))` of the carried DKG output.
///
/// Returns `B256::ZERO` when the outcome is not a decodable full-output ODKO
/// record (e.g. a group-key-only bootstrap outcome) — in that case the
/// committee's "invalid seed partial" slash offense is simply unavailable for
/// that epoch. Deterministic and panic-free; safe to call in the executor over
/// the already-consensus-validated boundary `outcome`.
pub fn boundary_outcome_polynomial_hash(outcome: &[u8]) -> B256 {
    use commonware_cryptography::bls12381::primitives::sharing::ModeVersion;
    // ODKO || version(1) || epoch(8) || is_full_dkg(1) || len(4 BE) || Output
    const HEADER_LEN: usize = 4 + 1 + 8 + 1 + 4;
    if outcome.len() < HEADER_LEN || &outcome[0..4] != b"ODKO" {
        return B256::ZERO;
    }
    let Ok(len_bytes) = <[u8; 4]>::try_from(&outcome[14..18]) else {
        return B256::ZERO;
    };
    let len = u32::from_be_bytes(len_bytes) as usize;
    let Some(body) = outcome.get(HEADER_LEN..HEADER_LEN + len) else {
        return B256::ZERO;
    };
    let Some(max) = NonZeroU32::new(crate::bls::MAX_VALIDATORS) else {
        return B256::ZERO;
    };
    let cfg = (max, ModeVersion::v0());
    match Output::<MinSig, bls12381::PublicKey>::read_cfg(&mut &body[..], &cfg) {
        Ok(output) => public_polynomial_hash(output.public()),
        Err(_) => B256::ZERO,
    }
}

#[cfg(test)]
mod tests;
