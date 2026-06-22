//! Application handler — processes messages from the Automaton/Relay side.
//!
//! This is the "server side" that reads from the mpsc channel populated by
//! [`OutbeApplication`](super::actor::OutbeApplication). It bridges Simplex
//! consensus with Reth's execution layer via `beacon_engine_handle` and
//! `payload_builder_handle`.
//!
//! Block availability uses Commonware's marshal actor (Tempo-style):
//! - Proposer disseminates blocks via `buffered::Engine` (broadcast)
//! - Non-proposers resolve blocks via `marshal::resolver` (on-demand P2P)
//! - No ad-hoc block propagation channel or local cache admission

use std::{
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, Instant},
};

/// Maximum retry attempts for marshal block resolution before structured application failure.
pub(crate) const FINALIZE_MAX_RETRIES: u32 = 5;
/// Delay between retry attempts for marshal block resolution.
pub(crate) const FINALIZE_RETRY_DELAY: Duration = Duration::from_secs(2);
/// Maximum time to wait for marshal block resolution during verify.
pub(crate) const VERIFY_RESOLUTION_TIMEOUT: Duration = crate::config::DEFAULT_PEER_RESPONSE_TIMEOUT;
/// Delay between Engine API retries while execution reports temporary SYNCING.
pub(crate) const VERIFY_SYNCING_RETRY_DELAY: Duration = Duration::from_millis(100);
/// Maximum time to wait for marshal parent resolution during proposal.
pub(crate) const PROPOSE_RESOLUTION_TIMEOUT: Duration =
    crate::config::DEFAULT_PEER_RESPONSE_TIMEOUT;
/// Per-attempt time budget for marshal block resolution during finalization.
///
/// Without this bound, a `subscribe_by_digest` waiter that never completes can
/// wedge the application handler's serial event loop, blocking propose/verify
/// indefinitely (see `retry_with_backoff` comment for the wedge mechanism).
/// Exhaustion is surfaced as a structured application failure, not a direct
/// process kill from inside the handler.
pub(crate) const FINALIZE_RESOLUTION_TIMEOUT: Duration = Duration::from_secs(10);
/// Log-rate window for repeated critical proposal failures.
pub(crate) const PROPOSAL_FAILURE_LOG_WINDOW: Duration = Duration::from_secs(5);
/// epoch boundary: bounded wait inside `handle_genesis` for the
/// finalization view to expose a continuity anchor for the new epoch.
///
/// If Commonware Simplex queries `Automaton::genesis(epoch>0)` faster than the
/// finalization actor publishes the boundary block's anchor into
/// `FinalizationView`, we wait up to this deadline before declaring the
/// terminal failure path. The companion `stack.rs` pre-restart guard should
/// normally make sure this never trips in practice.
pub(crate) const GENESIS_ANCHOR_WAIT_TIMEOUT: Duration = Duration::from_secs(5);
/// Poll interval used by the bounded waits in `handle_genesis` and the
/// `stack.rs` pre-restart preconditions.
pub(crate) const GENESIS_ANCHOR_POLL_INTERVAL: Duration = Duration::from_millis(50);
struct MarshalAncestryReader<C: commonware_runtime::Clock> {
    marshal: crate::marshal_types::MarshalMailbox,
    block_cache: BlockCacheHandle,
    readiness: AncestryReadiness,
    round: Option<Round>,
    timeout: Duration,
    // Owned runtime clock used to bound the marshal lookups. Cloned cheaply from
    // the spawn's context so the trait methods (which carry no context) can apply
    // a runtime-agnostic timeout without pulling in the tokio reactor.
    clock: C,
}

impl<C: commonware_runtime::Clock> MarshalAncestryReader<C> {
    fn new(
        marshal: crate::marshal_types::MarshalMailbox,
        block_cache: BlockCacheHandle,
        readiness: AncestryReadiness,
        round: Option<Round>,
        timeout: Duration,
        clock: C,
    ) -> Self {
        Self {
            marshal,
            block_cache,
            readiness,
            round,
            timeout,
            clock,
        }
    }
}

impl<C: commonware_runtime::Clock> AncestryReader for MarshalAncestryReader<C> {
    fn get_block_by_height<'a>(&'a self, height: u64) -> BlockLookupFuture<'a> {
        let cached = match self.block_cache.lock() {
            Ok(cache) => cache
                .values()
                .find(|block| block.number() == height)
                .cloned(),
            Err(error) => {
                warn!(%error, height, "block cache unavailable while resolving ancestry by height");
                None
            }
        };
        if cached.is_some() {
            return Box::pin(async move { cached });
        }
        let marshal = self.marshal.clone();
        // `Clock::sleep` returns an owned `'static` future, so we build it from the
        // borrowed clock here and move it into the lookup future — no clone of the
        // (non-`Clone`) runtime context needed.
        let sleep = self.clock.sleep(self.timeout);
        Box::pin(async move {
            // `marshal.get_block(..)` borrows `marshal`, so it is not `'static` and
            // cannot use `Clock::timeout`. Inline the same biased race the default
            // `Clock::timeout` uses: prefer the resolved block over the timeout.
            let lookup = marshal.get_block(Height::new(height));
            let mut lookup = std::pin::pin!(lookup);
            let mut sleep = std::pin::pin!(sleep);
            commonware_macros::select! {
                block = &mut lookup => block,
                _ = &mut sleep => None,
            }
        })
    }

    fn get_block_by_hash<'a>(&'a self, hash: B256) -> BlockLookupFuture<'a> {
        let digest = Digest(hash);
        let cached = match self.block_cache.lock() {
            Ok(cache) => cache.get(&digest).cloned(),
            Err(error) => {
                warn!(%error, %hash, "block cache unavailable while resolving ancestry by hash");
                None
            }
        };
        if cached.is_some() {
            return Box::pin(async move { cached });
        }
        let marshal = self.marshal.clone();
        let round = self.round;
        // Owned `'static` sleep future built from the borrowed clock (no clone).
        let sleep = self.clock.sleep(self.timeout);
        Box::pin(async move {
            let fallback = match round {
                Some(round) => {
                    commonware_consensus::marshal::core::DigestFallback::FetchByRound { round }
                }
                None => commonware_consensus::marshal::core::DigestFallback::Wait,
            };
            let block_future = marshal.subscribe_by_digest(digest, fallback);
            // Biased race, preferring the resolved block over the timeout
            // (the `block_future` borrows `marshal`, so it is not `'static`).
            let mut block_future = std::pin::pin!(block_future);
            let mut sleep = std::pin::pin!(sleep);
            commonware_macros::select! {
                result = &mut block_future => result.ok(),
                _ = &mut sleep => None,
            }
        })
    }

    fn is_ready(&self) -> bool {
        self.readiness.is_ready()
    }
}

fn unix_now_millis() -> eyre::Result<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| eyre::eyre!("system clock before UNIX_EPOCH: {e}"))?
        .as_millis()
        .try_into()
        .map_err(|_| eyre::eyre!("system clock millis does not fit in u64"))
}

/// Clamp a proposer's block timestamp (ms) into the deterministic drift band
/// `[parent + min_advance, parent + band]`, with the genesis-child exception.
///
/// When `parent_timestamp_millis == 0` there is no finalized parent yet (the
/// `finalization_view` is unseeded at genesis — it does NOT carry the genesis
/// header timestamp), so the band is meaningless: capping at `0 + band` would
/// clamp the real wall-clock time far below the genesis timestamp and reth
/// would reject the payload as a past timestamp, stalling at block 0. In that
/// case only monotonicity is enforced (`max(now, parent + 1)`), mirroring the
/// validator-side genesis exemption (`parent.number() == 0`). For every real
/// parent the full two-sided band applies, mirroring the validator-side
/// `validate_against_parent_timestamp_millis`:
/// - lower bound `parent + min_advance`: if the proposer's clock has not
///   advanced `min_advance` past the parent, the timestamp is clamped *up* so
///   the block still satisfies the validator minimum-advance rule and is never
///   rejected; this is what denies a colluding leader majority the
///   `parent + 1 ms` timestamp freeze.
/// - upper bound `parent + band` (C-01): an honest proposer never emits an
///   over-drifted block, and a long stall self-heals by ratcheting forward at
///   most one band per block.
fn clamp_proposed_timestamp_millis(
    parent_timestamp_millis: u64,
    now_millis: u64,
    band_millis: u64,
    min_advance_millis: u64,
) -> u64 {
    if parent_timestamp_millis == 0 {
        return std::cmp::max(now_millis, parent_timestamp_millis.saturating_add(1));
    }
    let min_timestamp_millis = parent_timestamp_millis.saturating_add(min_advance_millis);
    let max_timestamp_millis = parent_timestamp_millis.saturating_add(band_millis);
    std::cmp::max(now_millis, min_timestamp_millis).min(max_timestamp_millis)
}

fn finalized_parent_attestation_from_phase1_system_tx(
    block: &ConsensusBlock,
) -> eyre::Result<Option<FinalizedParentAttestation>> {
    if block.number() < 2 {
        return Ok(None);
    }

    let raw_block = block.clone().into_inner().into_block();
    let layout = outbe_primitives::system_tx::split_system_layout(&raw_block.body.transactions)
        .map_err(|error| {
            eyre::eyre!("invalid system tx layout while extracting Phase 1: {error}")
        })?;
    let phase1 = *layout.begin.first().ok_or_else(|| {
        eyre::eyre!(
            "missing Phase 1 finalization system transaction for block {}",
            block.number()
        )
    })?;
    let input = outbe_primitives::system_tx::SystemTxInputV2::decode(phase1.input().as_ref())
        .map_err(|error| eyre::eyre!("decode Phase 1 system transaction input: {error}"))?;
    let outbe_primitives::system_tx::SystemTxInputV2::CertifiedParentAccounting { metadata } =
        input
    else {
        return Err(eyre::eyre!(
            "expected Phase 1 finalization system transaction at begin ordinal 0"
        ));
    };

    Ok(Some(FinalizedParentAttestation {
        finalized_block_number: metadata.finalized_block_number,
        finalized_block_hash: metadata.finalized_block_hash,
        finalized_epoch: metadata.finalized_epoch,
        finalized_view: metadata.finalized_view,
        parent_view: metadata.parent_view,
        ordered_committee: metadata.ordered_committee,
        signer_bitmap: metadata.signer_bitmap,
        certificate: metadata.proof,
        // V2 `missed_proposers: Vec<MissedProposerEvent>` always
        // empty under the verifier rule; the V1 attestation surface
        // keeps the legacy `Vec<Address>` shape for backwards-compat callers.
        missed_proposers: metadata
            .missed_proposers
            .into_iter()
            .map(|ev| ev.validator)
            .collect(),
    }))
}

use alloy_consensus::Transaction as _;
use alloy_primitives::{Address, Bytes, B256};
use commonware_consensus::types::{Epoch, Height, Round, View};
use commonware_cryptography::{
    bls12381::{primitives::variant::MinSig, PublicKey},
    certificate::Provider as _,
};
use commonware_utils::channel::oneshot;
use futures::StreamExt;
use outbe_primitives::{
    addresses::REWARDS_ADDRESS,
    reshare_artifact::{
        encode_outbe_block_artifacts, ConsensusHeaderArtifact, FinalizedParentAttestation,
        OutbeBlockArtifacts,
    },
    OutbeExecutionData, OutbePayloadAttributes, OutbePayloadTypes,
};
use reth_node_builder::{BuiltPayload as _, ConsensusEngineHandle};
use reth_payload_builder::PayloadBuilderHandle;
use tracing::{debug, error, info, warn};

use crate::{
    ancestry_readiness::AncestryReadiness,
    block::ConsensusBlock,
    committee_provider::CommitteeProvider,
    digest::Digest,
    dkg_manager::{AncestryReader, BlockLookupFuture, BoundaryRequirement},
    executor,
    finalization::{
        actor::BlockCacheHandle,
        parent_cert_store::{CertifiedParentProofKey, CertifiedParentProofRecord},
        state::{FinalizationViewAccess, FinalizationViewHandle},
    },
    hybrid::{election::HybridElectorConfigProvider, HybridSchemeProvider},
    validators::ValidatorSet,
    vrf_safety::VrfSafetyGate,
};

use super::ingress::Message;

/// Type alias for engine types used in Outbe.
type EngineHandle = ConsensusEngineHandle<OutbePayloadTypes>;
type PayloadBuilder = PayloadBuilderHandle<OutbePayloadTypes>;

/// The application handler that processes consensus messages.
///
/// Reads from the mpsc channel and calls `beacon_engine_handle` / `payload_builder_handle`
/// to propose and verify blocks. Finalization side effects are owned by
/// `FinalizationActor`; this handler only observes the finalized view while
/// preparing proposals.
///
/// Block resolution uses marshal's digest-bound model:
/// - `handle_verify()` resolves blocks via `marshal_mailbox.subscribe_by_digest()`
/// - `FinalizationActor` resolves finalized blocks via marshal (or proposer's local cache)
/// - No separate block propagation channel or raw block admission path
///
// `ReplayClassification`, `classify_finalization`,
// `extract_consensus_metadata_from_block`,
// `extract_header_artifact_from_block`, `retry_with_backoff`,
// `RetryFailure`, and `RetryFailureKind` live in
// `crate::finalization::util` (relocated in step 17). After step 21 the
// application handler no longer runs the finalization side effects, so
// it only consumes the metadata + header-artifact extractors on the
// verify path.
use crate::finalization::util::extract_header_artifact_from_block;

use crate::application::validation::{
    validate_context_parent_binding, validate_rewards_beneficiary,
    validate_system_tx_leader_binding,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EpochFenceRejection {
    StaleEpoch { active_epoch: Epoch },
    FutureEpoch { active_epoch: Epoch },
    BeyondBoundary { max_block_height: u64 },
}

#[derive(Debug, Clone, Copy)]
struct EpochFenceState {
    active_epoch: Epoch,
    boundary: Option<EpochBoundaryFence>,
}

#[derive(Debug, Clone, Copy)]
struct EpochBoundaryFence {
    epoch: Epoch,
    max_block_height: u64,
}

/// epoch continuity anchor for the first proposal of `epoch > 0`.
///
/// Built by [`ApplicationShared::resolve_epoch_boundary_parent`] and consumed
/// by both `handle_propose` and `handle_verify` to bypass the `parent_view = 0`
/// chain-genesis path for non-zero epochs.
#[derive(Debug, Clone)]
pub(crate) struct EpochBoundaryParent {
    pub(crate) height: Height,
    pub(crate) block: ConsensusBlock,
    pub(crate) proof_key: CertifiedParentProofKey,
}

/// Typed error returned by [`ApplicationShared::resolve_epoch_boundary_parent`].
///
/// The variants distinguish *invalid proposal* (the proposer chose a parent
/// that does not match the canonical anchor) from *local infrastructure issue*
/// (the validator cannot decide locally because the finalization view or the
/// marshal store has not caught up). Verify path votes `false` only in the
/// first case; the rest bubble up as `Err` and drop the response channel, to
/// match the existing `resolve_for_verify` semantics for local timeouts.
#[derive(Debug)]
pub(crate) enum EpochBoundaryParentError {
    /// Simplex parent does not match the committed continuity anchor.
    ParentMismatch {
        expected: B256,
        got: B256,
        epoch: u64,
    },
    /// `FinalizationView` has no anchor for `epoch > 0`. Caller waited as long
    /// as it could; this is a local-infrastructure failure, not a vote.
    MissingAnchor { epoch: u64 },
    /// Marshal store cannot return the anchor block.
    MissingMarshalBlock { height: u64 },
    /// Marshal returned a block whose digest does not match the anchor.
    MarshalHashMismatch {
        height: u64,
        expected: B256,
        got: B256,
    },
}

impl std::fmt::Display for EpochBoundaryParentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ParentMismatch {
                expected,
                got,
                epoch,
            } => write!(
                f,
                "epoch boundary parent mismatch: simplex parent {got} != finalized anchor {expected} (epoch={epoch})",
            ),
            Self::MissingAnchor { epoch } => write!(
                f,
                "broken epoch continuity: epoch={epoch} but no finalized anchor in finalization_view",
            ),
            Self::MissingMarshalBlock { height } => write!(
                f,
                "epoch boundary parent block at height {height} not found in marshal",
            ),
            Self::MarshalHashMismatch {
                height,
                expected,
                got,
            } => write!(
                f,
                "marshal block at height {height} has digest {got} != simplex parent {expected}",
            ),
        }
    }
}

impl std::error::Error for EpochBoundaryParentError {}

/// Guards application work during DKG activation so an old Simplex epoch cannot
/// submit Engine API work past the activation boundary while the epoch restarts.
#[derive(Debug, Clone)]
pub struct ApplicationEpochFence {
    state: Arc<StdMutex<EpochFenceState>>,
}

impl ApplicationEpochFence {
    pub fn new(active_epoch: Epoch) -> Self {
        Self {
            state: Arc::new(StdMutex::new(EpochFenceState {
                active_epoch,
                boundary: None,
            })),
        }
    }

    pub fn arm_activation_boundary(&self, epoch: Epoch, max_block_height: u64) {
        let mut state = self.lock_state();
        state.boundary = Some(EpochBoundaryFence {
            epoch,
            max_block_height,
        });
    }

    pub fn advance_epoch(&self, next_epoch: Epoch) {
        let mut state = self.lock_state();
        state.active_epoch = next_epoch;
        state.boundary = state.boundary.filter(|fence| fence.epoch >= next_epoch);
    }

    fn check(&self, round: Round, candidate_block_height: u64) -> Result<(), EpochFenceRejection> {
        let state = self.lock_state();
        let round_epoch = round.epoch();
        if round_epoch < state.active_epoch {
            return Err(EpochFenceRejection::StaleEpoch {
                active_epoch: state.active_epoch,
            });
        }
        if round_epoch > state.active_epoch {
            return Err(EpochFenceRejection::FutureEpoch {
                active_epoch: state.active_epoch,
            });
        }
        if let Some(fence) = state.boundary {
            if fence.epoch == round_epoch && candidate_block_height > fence.max_block_height {
                return Err(EpochFenceRejection::BeyondBoundary {
                    max_block_height: fence.max_block_height,
                });
            }
        }
        Ok(())
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, EpochFenceState> {
        match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProposeOutcome {
    Proposed(Digest),
    ParentProofUnavailable,
    EpochStale,
    BoundaryUnavailable,
}

/// Pure min-block-time floor arithmetic: remaining pad = `min ⊖ elapsed`
/// (`saturating_sub`). A zero result means the floor is already met — send the
/// digest immediately with no wait (case C / heavy block).
fn floor_remaining(
    min_block_time: std::time::Duration,
    elapsed: std::time::Duration,
) -> std::time::Duration {
    min_block_time.saturating_sub(elapsed)
}

/// Proposer-side minimum block-time pacing.
///
/// Holds the already-sealed `digest` until the floor (`min_block_time`) elapses,
/// then hands it to Simplex via `response`. If the view is cancelled first
/// (Simplex drops the proposal receiver), the `select!` aborts on
/// `response.closed()` and nothing is sent. Liveness pacing only — it never
/// touches block bytes/hash/validation, so it is invisible to validators.
///
/// `propose_start` is the closure-level instant captured before `handle_propose`;
/// `elapsed` therefore subsumes the whole build + marshal path, making the floor
/// a total ceiling (`max(floor, build)`), not an additive delay.
async fn pace_and_send<C>(
    ctx: &C,
    mut response: oneshot::Sender<Digest>,
    digest: Digest,
    min_block_time: std::time::Duration,
    propose_start: std::time::SystemTime,
) where
    C: commonware_runtime::Clock,
{
    let elapsed = ctx
        .current()
        .duration_since(propose_start)
        .unwrap_or_default();
    let remaining = floor_remaining(min_block_time, elapsed);
    crate::metrics::record_block_build_time(elapsed);
    crate::metrics::record_block_wait_time(remaining);
    if remaining.is_zero() {
        // Case C (elapsed >= min): send now, control-flow identical to pre-pacing.
        let _ = response.send(digest);
    } else {
        // Biased select!: cancellation wins over a near-simultaneous sleep
        // completion, so we never send into a closed channel.
        commonware_macros::select! {
            () = response.closed() => {
                debug!("view cancelled during min-block-time pacing; dropping proposal");
            },
            _ = ctx.sleep(remaining) => {
                let _ = response.send(digest);
            },
        }
    }
}

// `Built` carries the full `ConsensusBlock`; the other variants are unit. This is
// an internal result returned once per propose and consumed immediately — boxing
// the block would only add an allocation on the hot proposer path.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
enum BuildBlockOutcome {
    Built(Digest, ConsensusBlock),
    ParentProofUnavailable,
    EpochStale,
    BoundaryUnavailable,
}

// Like `BuildBlockOutcome` above: an internal direct-parent proof lookup result,
// produced once per proposal in `select_parent_proof_for_proposal` and consumed
// immediately at the single match site. `Found` is the common case, so boxing the
// record would only add a heap allocation on the hot proposer path for no benefit.
#[allow(clippy::large_enum_variant)]
enum ParentProofLookup {
    NoProofNeeded,
    Found(CertifiedParentProofRecord),
    Unavailable,
}

pub struct ApplicationHandler {
    /// Receiver for messages from the Automaton/Relay side.
    rx: futures::channel::mpsc::Receiver<Message>,

    pub(crate) shared: ApplicationShared,
}

#[derive(Clone)]
pub(crate) struct ApplicationShared {
    /// Engine handle for new_payload / fork_choice_updated.
    engine: EngineHandle,

    /// Payload builder handle for building new blocks.
    payload_builder: PayloadBuilder,

    /// Executor mailbox for forwarding finalized blocks.
    executor_mailbox: executor::Mailbox,

    /// Genesis block hash (block 0).
    genesis_hash: B256,

    /// Current active validator set.
    #[allow(dead_code)]
    validators: ValidatorSet,

    /// Active EVM chain id used to rebuild deterministic system-tx envelopes
    /// during consensus prechecks before Engine status is trusted.
    chain_id: u64,

    /// Marshal mailbox for digest-bound block resolution.
    pub(crate) marshal_mailbox: crate::marshal_types::MarshalMailbox,

    /// Epoch-scoped verifier schemes for carried finalized-parent certificates.
    certificate_scheme_provider: HybridSchemeProvider<MinSig>,

    /// Epoch-scoped leader elector configs. removed the
    /// stateful verify-time elector use; kept on the surface for ctor
    /// stability with `stack.rs` and for the upcoming V2 verifier hook.
    #[allow(dead_code)]
    elector_config_provider: HybridElectorConfigProvider<MinSig>,

    /// Epoch-scoped ordered committee snapshots.
    committee_provider: CommitteeProvider,

    /// DKG artifact manager for boundary outcomes and dealer logs.
    dkg_manager: crate::dkg_manager::Mailbox,

    /// Fail-closed guard for VRF/DKG freshness.
    vrf_safety: VrfSafetyGate,

    /// DKG activation boundary guard shared with the stack epoch loop.
    epoch_fence: ApplicationEpochFence,

    /// Fast local readiness bit for startup/crash backfill. When false,
    /// ancestry checks fail before opening marshal subscriptions that would
    /// otherwise wait until timeout while the executor is replaying durable
    /// consensus blocks into Reth.
    ancestry_readiness: AncestryReadiness,

    /// Time to give the payload builder to execute transactions before resolving.
    payload_resolve_time: std::time::Duration,

    /// Retained on the ctor surface for ABI stability
    /// with `stack.rs`. The proposer no longer gates the parent-proof
    /// lookup on this budget (the new selector is non-blocking); the field
    /// will be removed in a follow-up cleanup once `stack.rs` no longer
    /// supplies it.
    #[allow(dead_code)]
    payload_return_time: std::time::Duration,

    /// Proposer-side minimum block-time floor (liveness pacing only; never
    /// affects block contents or validation). Read in the `Message::Propose`
    /// closure via `shared.min_block_time`. Always > 0 (validated at startup).
    min_block_time: std::time::Duration,

    /// Proposer EVM identity used to sign system transaction artifacts.
    proposer_evm_address: Option<Address>,

    /// Rate limiter for repeated critical proposal failure logs.
    proposal_failure_log_limiter: Arc<crate::util::rate_limit::LogRateLimiter>,

    /// Shared canonical view of the last finalization (forkchoice,
    /// `last_finalized_*`, `prev_randao`, monotonic clock floor).
    /// Written by the FinalizationActor; read here for `build_block`.
    finalization_view: FinalizationViewHandle,

    /// Shared block cache: proposer inserts on local build, the
    /// FinalizationActor evicts entries below the new finalized height.
    block_cache: BlockCacheHandle,

    /// Proposer-side exact-parent certificate selector.
    ///
    /// It waits for the finalized-parent certificate record matching the
    /// Simplex context parent, then carries the metadata in the Phase 1
    /// begin-zone system transaction rather than in `header.extra_data`.
    finalization_selector: crate::finalization::selection::ParentProofSelector,

    /// Disaster-recovery flag (`--testnet.trust-el-head`). When true and
    /// `FinalizationView` has a non-zero execution head, `handle_genesis`
    /// uses the execution head as the Simplex anchor instead of the chain
    /// genesis hash. This allows blocks to resume from the existing
    /// execution state after a force-DKG restart.
    trust_el_head: bool,

    /// shared late-finalize signature store. On proposal the
    /// handler reads it (`build_artifact`) to pack the in-window
    /// `LateFinalizeCreditsArtifact` into `header.extra_data`. The reporter
    /// writes votes into it and the `FinalizationActor` resolves them. Best-
    /// effort, process-local — the resulting artifact is re-verified by every
    /// validator, so it never affects determinism.
    late_sig_store: crate::finalization::late_sig_store::SharedLateFinalizeStore,
}

/// Named dependencies for [`ApplicationHandler::new`].
///
/// Replaces a 25-positional-argument constructor (mirrors `FinalizationActorDeps`,
/// used a few lines later in `stack.rs`), so the single production caller and the
/// test fixtures cannot transpose arguments — the wiring order lives in the type
/// system rather than in a call-site convention.
pub struct ApplicationDeps {
    pub rx: futures::channel::mpsc::Receiver<Message>,
    pub engine: EngineHandle,
    pub payload_builder: PayloadBuilder,
    pub executor_mailbox: executor::Mailbox,
    pub genesis_hash: B256,
    pub validators: ValidatorSet,
    pub chain_id: u64,
    pub marshal_mailbox: crate::marshal_types::MarshalMailbox,
    pub certificate_scheme_provider: HybridSchemeProvider<MinSig>,
    pub elector_config_provider: HybridElectorConfigProvider<MinSig>,
    pub committee_provider: CommitteeProvider,
    pub dkg_manager: crate::dkg_manager::Mailbox,
    pub vrf_safety: VrfSafetyGate,
    pub epoch_fence: ApplicationEpochFence,
    pub ancestry_readiness: AncestryReadiness,
    pub finalization_view: FinalizationViewHandle,
    pub block_cache: BlockCacheHandle,
    pub finalization_selector: crate::finalization::selection::ParentProofSelector,
    pub payload_resolve_time: std::time::Duration,
    pub payload_return_time: std::time::Duration,
    pub min_block_time: std::time::Duration,
    pub proposer_evm_address: Option<Address>,
    pub trust_el_head: bool,
    pub late_sig_store: crate::finalization::late_sig_store::SharedLateFinalizeStore,
}

impl ApplicationHandler {
    /// Create a new handler.
    ///
    /// The application no longer owns a private finalization view copy.
    /// Forkchoice / last-finalized / prev-randao / monotonic-clock-floor
    /// state lives in the shared [`FinalizationViewHandle`], written by
    /// the `FinalizationActor` and read here under a short-lived guard.
    /// Recovery is performed by `new_finalization_view(...)` at the call
    /// site (`stack.rs`); this constructor takes the already-initialized
    /// handle.
    pub fn new(deps: ApplicationDeps) -> Self {
        let ApplicationDeps {
            rx,
            engine,
            payload_builder,
            executor_mailbox,
            genesis_hash,
            validators,
            chain_id,
            marshal_mailbox,
            certificate_scheme_provider,
            elector_config_provider,
            committee_provider,
            dkg_manager,
            vrf_safety,
            epoch_fence,
            ancestry_readiness,
            finalization_view,
            block_cache,
            finalization_selector,
            payload_resolve_time,
            payload_return_time,
            min_block_time,
            proposer_evm_address,
            trust_el_head,
            late_sig_store,
        } = deps;
        Self {
            rx,
            shared: ApplicationShared {
                engine,
                payload_builder,
                executor_mailbox,
                genesis_hash,
                validators,
                chain_id,
                marshal_mailbox,
                certificate_scheme_provider,
                elector_config_provider,
                committee_provider,
                dkg_manager,
                vrf_safety,
                epoch_fence,
                ancestry_readiness,
                payload_resolve_time,
                payload_return_time,
                min_block_time,
                proposer_evm_address,
                proposal_failure_log_limiter: Arc::new(
                    crate::util::rate_limit::LogRateLimiter::new(PROPOSAL_FAILURE_LOG_WINDOW),
                ),
                finalization_view,
                block_cache,
                finalization_selector,
                trust_el_head,
                late_sig_store,
            },
        }
    }

    /// Run the handler event loop.
    pub async fn run<E>(mut self, context: E) -> eyre::Result<()>
    where
        E: commonware_runtime::Metrics
            + commonware_runtime::Spawner
            + commonware_runtime::Clock
            + Send
            + Sync
            + 'static,
    {
        info!("application handler started");

        // Step 21: the per-finalization side effects no longer run in
        // this handler. Finalization events flow voter → OutbeReporter →
        // FinalizationActor (via the unbounded
        // `finalization::ingress::Mailbox`); the application handler's
        // mailbox handles only Genesis / Propose / Verify / Broadcast.
        loop {
            let msg = match self.rx.next().await {
                Some(m) => m,
                None => {
                    info!("application handler mailbox closed, exiting");
                    return Ok(());
                }
            };
            match msg {
                Message::Genesis(genesis) => {
                    context.child("genesis").spawn({
                        let shared = self.shared.clone();
                        move |ctx| async move {
                            shared.handle_genesis(&ctx, genesis).await;
                        }
                    });
                }
                Message::Propose(propose) => {
                    context.child("propose").spawn({
                        let shared = self.shared.clone();
                        move |ctx| async move {
                            let propose = *propose;
                            let response = propose.response;
                            // Closure-level instant covering the whole build + marshal path,
                            // used only for proposer-side min-block-time pacing.
                            let propose_start = ctx.current();
                            match shared.handle_propose(&ctx, propose.context).await {
                                Ok(ProposeOutcome::Proposed(digest)) => {
                                    // Proposer-side liveness pacing only: hold the already-sealed
                                    // digest until the min-block-time floor elapses, then hand it
                                    // to Simplex (or abort if the view is cancelled first). Never
                                    // touches block bytes/hash/validation.
                                    pace_and_send(
                                        &ctx,
                                        response,
                                        digest,
                                        shared.min_block_time,
                                        propose_start,
                                    )
                                    .await;
                                }
                                Ok(ProposeOutcome::ParentProofUnavailable) => {
                                    debug!(
                                        "proposal task completed without response: exact parent proof unavailable"
                                    );
                                }
                                Ok(ProposeOutcome::EpochStale) => {
                                    debug!(
                                        "proposal task completed without response for stale epoch work"
                                    );
                                }
                                Ok(ProposeOutcome::BoundaryUnavailable) => {
                                    debug!(
                                        "proposal task completed without response: DKG boundary requirement unavailable"
                                    );
                                }
                                Err(error) => {
                                    if let Some(suppressed_since_last) =
                                        shared.proposal_failure_log_limiter.check()
                                    {
                                        tracing::error!(
                                            %error,
                                            suppressed_since_last,
                                            "critical proposal failure; stopping proposal task"
                                        );
                                    }
                                }
                            }
                        }
                    });
                }
                Message::Verify(verify) => {
                    context.child("verify").spawn({
                        let shared = self.shared.clone();
                        move |ctx| async move {
                            let verify = *verify;
                            let response = verify.response;
                            match shared
                                .handle_verify(&ctx, verify.context, verify.payload, response)
                                .await
                            {
                                Ok(()) => {}
                                Err(error) => {
                                    info!(
                                        %error,
                                        "could not decide proposal validity; dropping verify response channel"
                                    );
                                }
                            }
                        }
                    });
                }
            }
        }
    }
}

impl ApplicationShared {
    /// Handle genesis request — return the parent digest for `view = 1` of
    /// `genesis.epoch`.
    ///
    /// ** epoch continuity:**
    /// - `epoch == 0` — return the chain genesis hash, as before.
    /// - `epoch > 0` — return the last finalized block's hash from
    ///   `FinalizationView`. That value is the *continuity anchor*: the
    ///   first block produced in the new epoch must extend it.
    ///
    /// Commonware Simplex caches the value returned here as the parent of
    /// `view = 1` for the duration of the engine instance. Returning a stale
    /// or default value (e.g. `B256::ZERO`) would permanently lock the
    /// engine on a non-existent parent and cause every proposal to be
    /// rejected. To avoid that:
    ///   1. We bound-wait up to `GENESIS_ANCHOR_WAIT_TIMEOUT` for the
    ///      finalization view to publish the anchor.
    ///   2. The `stack.rs` pre-restart guard ensures the anchor is already
    ///      present before `engine.start(...)` runs, so the wait below
    ///      should resolve immediately in steady-state operation.
    ///   3. If the wait expires we log `error!` and respond with
    ///      `B256::ZERO` as a terminal-failure signal. The node operator
    ///      is expected to investigate; the bounded wait keeps the
    ///      handler responsive instead of stalling Simplex indefinitely.
    async fn handle_genesis(
        &self,
        clock: &impl commonware_runtime::Clock,
        genesis: super::ingress::Genesis,
    ) {
        debug!(epoch = %genesis.epoch, "genesis requested");
        let epoch = genesis.epoch;
        if epoch.get() == 0 {
            if self.trust_el_head {
                let anchor = self.finalization_view.finalized_anchor();
                if anchor.number > 0 && anchor.finalized_head_hash != B256::ZERO {
                    debug!(
                        finalized_number = anchor.number,
                        finalized_hash = %anchor.finalized_head_hash,
                        "handle_genesis(epoch=0): using execution head as anchor (--testnet.trust-el-head)"
                    );
                    let _ = genesis.response.send(Digest(anchor.finalized_head_hash));
                    return;
                }
            }
            let _ = genesis.response.send(Digest(self.genesis_hash));
            return;
        }

        // Bounded wait driven by the runtime clock so the deadline and the poll
        // sleep below share one time source (works on both the tokio and the
        // deterministic runtimes; no wall-clock on the consensus path).
        let deadline = clock.current() + GENESIS_ANCHOR_WAIT_TIMEOUT;
        loop {
            let anchor = self.finalization_view.finalized_anchor();
            let (height, hash) = (anchor.number, anchor.finalized_head_hash);
            if height > 0 && hash != B256::ZERO {
                debug!(
                    %epoch,
                    finalized_number = height,
                    finalized_hash = %hash,
                    "handle_genesis: continuity anchor"
                );
                let _ = genesis.response.send(Digest(hash));
                return;
            }
            if clock.current() >= deadline {
                error!(
                    %epoch,
                    timeout_ms = GENESIS_ANCHOR_WAIT_TIMEOUT.as_millis(),
                    "handle_genesis: epoch>0 without finalized continuity anchor after timeout; \
                     terminal failure — Simplex will lock parent_view=0 to B256::ZERO. \
                     Investigate why FinalizationView lacks last_finalized_number/hash; \
                     stack.rs pre-restart guard should have prevented this."
                );
                let _ = genesis.response.send(Digest(B256::ZERO));
                return;
            }
            clock.sleep(GENESIS_ANCHOR_POLL_INTERVAL).await;
        }
    }

    /// epoch continuity: resolve the parent for the very first
    /// proposal of a fresh epoch (`epoch > 0`, `parent_view = 0`).
    ///
    /// Simplex restarts at every epoch advance and treats the digest
    /// returned by [`Self::handle_genesis`] as the parent of `view = 1`.
    /// For `epoch > 0` that digest is the canonical last-finalized block's
    /// hash (the *continuity anchor*), not chain genesis. The application
    /// must therefore translate this synthetic `parent_view = 0` context
    /// back into the real block 120 (or whatever the boundary block is),
    /// not into the chain genesis special case.
    ///
    /// `Ok(None)` means "this is not the epoch boundary" — caller falls
    /// back to its existing resolution path (chain genesis check followed
    /// by `block_cache → subscribe_by_digest`).
    ///
    /// `Ok(Some(EpochBoundaryParent))` carries the resolved finalized
    /// block and is fed straight into `engine.new_payload` / `build_block`.
    ///
    /// `Err(EpochBoundaryParentError)` is split into two categories:
    /// * `ParentMismatch` — the Simplex parent does not match the
    ///   committed anchor. This is an *invalid proposal*; verify path
    ///   votes false, propose path forfeits the slot.
    /// * `MissingAnchor` / `MissingMarshalBlock` / `MarshalHashMismatch` —
    ///   local infrastructure issue; verify path drops the response
    ///   channel (matching the existing `resolve_for_verify` behaviour)
    ///   rather than voting false on a valid block.
    async fn resolve_epoch_boundary_parent(
        &self,
        clock: &impl commonware_runtime::Clock,
        round: Round,
        parent_view: View,
        parent_digest: Digest,
    ) -> Result<Option<EpochBoundaryParent>, EpochBoundaryParentError> {
        if round.epoch().get() == 0 || parent_view != View::new(0) {
            return Ok(None);
        }

        let anchor = self.finalization_view.finalized_anchor();
        let (expected_height, expected_hash, finalized_round) =
            (anchor.number, anchor.finalized_head_hash, anchor.round);
        let Some(finalized_round) = finalized_round else {
            return Err(EpochBoundaryParentError::MissingAnchor {
                epoch: round.epoch().get(),
            });
        };
        if expected_height == 0 || expected_hash == B256::ZERO {
            return Err(EpochBoundaryParentError::MissingAnchor {
                epoch: round.epoch().get(),
            });
        }
        if parent_digest.0 != expected_hash {
            return Err(EpochBoundaryParentError::ParentMismatch {
                expected: expected_hash,
                got: parent_digest.0,
                epoch: round.epoch().get(),
            });
        }

        // Marshal exposes only digest-based lookup. Since we just confirmed
        // `parent_digest == expected_hash`, looking up by digest yields the
        // committed anchor block; we then sanity-check the height to catch
        // a corrupted local store.
        let block_future = self.marshal_mailbox.clone().subscribe_by_digest(
            parent_digest,
            commonware_consensus::marshal::core::DigestFallback::Wait,
        );
        // `Clock::timeout` returns `Err(Error::Timeout)` on expiry; the inner
        // `Ok`/`Err` is the marshal waiter's own result, unchanged.
        let block = match clock
            .timeout(PROPOSE_RESOLUTION_TIMEOUT, block_future)
            .await
        {
            Ok(Ok(block)) => block,
            Ok(Err(_)) | Err(_) => {
                return Err(EpochBoundaryParentError::MissingMarshalBlock {
                    height: expected_height,
                });
            }
        };
        if block.number() != expected_height {
            return Err(EpochBoundaryParentError::MarshalHashMismatch {
                height: expected_height,
                expected: parent_digest.0,
                got: block.digest().0,
            });
        }
        Ok(Some(EpochBoundaryParent {
            height: Height::new(expected_height),
            block,
            proof_key: CertifiedParentProofKey::new(
                finalized_round.epoch().get(),
                finalized_round.view().get(),
                expected_hash,
            ),
        }))
    }

    /// Handle propose request strict wait.
    ///
    /// 1. Resolve parent block (local cache or marshal)
    /// 2. Send parent to execution layer via new_payload (ensure Reth knows it)
    /// 3. Canonicalize parent as head (FCU)
    /// 4. Build next block
    async fn handle_propose(
        &self,
        clock: &(impl commonware_runtime::Clock + commonware_runtime::Supervisor),
        context: super::ingress::SimplexContext,
    ) -> eyre::Result<ProposeOutcome> {
        // Runtime-clock timestamp so the payload-build budget below tracks the same
        // time source the proposer's sleep uses (works on the deterministic runtime).
        let propose_start = clock.current();
        let (parent_view, parent) = context.parent;
        let parent_digest = Digest(parent.0);
        let round = context.round;
        debug!(%round, %parent_view, parent = %parent_digest.0, "propose requested");

        // epoch continuity: special-case the first proposal of a
        // new Simplex epoch (`epoch > 0`, `parent_view = 0`) before the chain
        // genesis path. `Ok(None)` means "not an epoch boundary"; caller falls
        // through to the chain genesis / cache / marshal-by-digest branches.
        let maybe_epoch_anchor = match self
            .resolve_epoch_boundary_parent(clock, round, parent_view, parent_digest)
            .await
        {
            Ok(opt) => opt,
            Err(error) => {
                warn!(
                    %round,
                    parent = %parent_digest.0,
                    %error,
                    "propose: epoch boundary parent resolution failed; forfeiting slot"
                );
                return Ok(ProposeOutcome::ParentProofUnavailable);
            }
        };
        debug_assert!(
            !(round.epoch().get() > 0
                && parent_view == View::new(0)
                && maybe_epoch_anchor.is_none()),
            "resolve_epoch_boundary_parent invariant: epoch>0 && parent_view=0 must \
             resolve to Some(EpochBoundaryParent) or return an explicit error"
        );

        let (parent_height, parent_block, parent_proof_key) =
            if let Some(anchor) = maybe_epoch_anchor {
                (anchor.height, Some(anchor.block), Some(anchor.proof_key))
            } else if parent_digest.0 == self.genesis_hash {
                (Height::zero(), None, None)
            } else {
                let cached_parent = self
                    .block_cache
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .remove(&parent_digest);
                let parent_block = if let Some(block) = cached_parent {
                    block
                } else {
                    // Parent from another proposer — resolve via marshal.
                    let marshal = self.marshal_mailbox.clone();
                    let block_future = marshal.subscribe_by_digest(
                        parent_digest,
                        commonware_consensus::marshal::core::DigestFallback::FetchByRound {
                            round: parent_round(round, parent_view),
                        },
                    );
                    match clock
                        .timeout(PROPOSE_RESOLUTION_TIMEOUT, block_future)
                        .await
                    {
                        Ok(Ok(block)) => block,
                        Ok(Err(_)) => {
                            return Err(eyre::eyre!(
                                "failed to resolve parent block {} for proposal",
                                parent_digest.0
                            ));
                        }
                        Err(_) => {
                            return Err(eyre::eyre!(
                                "timed out resolving parent block {} for proposal",
                                parent_digest.0
                            ));
                        }
                    }
                };

                let parent_height = Height::new(parent_block.number());
                (
                    parent_height,
                    Some(parent_block),
                    Some(CertifiedParentProofKey::new(
                        parent_round(round, parent_view).epoch().get(),
                        parent_view.get(),
                        parent_digest.0,
                    )),
                )
            };

        let next_block_number = parent_height.get().saturating_add(1);
        if let Err(rejection) = self.epoch_fence.check(round, next_block_number) {
            debug!(
                %round,
                parent = %parent_digest.0,
                next_block_number,
                ?rejection,
                "dropping stale proposal before Engine API work"
            );
            return Ok(ProposeOutcome::EpochStale);
        }

        if let Some(parent_block) = parent_block.as_ref() {
            // Step 2: Send parent to execution layer via new_payload.
            let execution_data = OutbeExecutionData {
                block: std::sync::Arc::new(parent_block.clone().into_inner()),
            };

            if crate::test_faults::should_drop_new_payload_for_test(parent_height) {
                warn!(
                    height = %parent_height,
                    parent = %parent_digest.0,
                    "test-marshal-drop: skipping propose parent new_payload"
                );
            } else {
                match self.engine.new_payload(execution_data).await {
                    Ok(status) if status.is_valid() || status.is_syncing() => {
                        debug!(parent = %parent_digest.0, ?status, "parent verified by execution layer");
                    }
                    Ok(status) => {
                        return Err(eyre::eyre!(
                            "parent {} rejected by execution layer: {status:?}",
                            parent_digest.0
                        ));
                    }
                    Err(e) => {
                        return Err(eyre::eyre!(
                            "new_payload failed for parent {}: {e}",
                            parent_digest.0
                        ));
                    }
                }
            }

            self.finalization_view
                .advance_timestamp_floor(parent_block.timestamp_millis());
        }

        self.vrf_safety
            .ensure_block_allowed(next_block_number)
            .map_err(|error| eyre::eyre!("refusing proposal above VRF expiry: {error}"))?;

        // Steps 3+4: Canonicalize parent as head and build next block.
        // Uses FCU-based flow: canonicalize_and_build sends
        // FCU with payload attributes so the engine starts building a payload
        // on the correct canonical state with access to the txpool.
        match self
            .build_block(
                clock,
                round,
                parent_height,
                parent_digest,
                parent_block.clone(),
                parent_proof_key,
                propose_start,
            )
            .await
        {
            Ok(BuildBlockOutcome::Built(digest, block)) => {
                // Cache the proposed block into marshal NOW, at propose time, so
                // the proposer can always SERVE it on demand (verifiers pull via
                // subscribe_by_digest) — independent of whether the later
                // `Relay::broadcast` wire-push succeeds. commonware 2026.5.0
                // split dissemination: `proposed` caches + stashes locally;
                // `forward` (driven from `Relay::broadcast`) does the wire-push.
                // Decoupling cache from push makes a dropped push recoverable via
                // pull instead of losing the view (bp-1).
                let durable = self.marshal_mailbox.proposed(round, block).await;
                if !durable {
                    // `proposed()` returns false only when the marshal actor's ack
                    // channel is closed — i.e. marshal is gone/shutting down. The
                    // block is then NOT durably cached (not servable on pull, not
                    // stashed for `forward`), so this proposal cannot be resolved by
                    // verifiers (bp-1 pull-recovery does not help — nothing to serve).
                    // Surface it loudly rather than silently treating the proposal as
                    // durable. A persistent marshal failure is the supervisor's
                    // concern: the marshal handle is monitored (SSA-8) and a dead
                    // marshal fails the node fast.
                    warn!(
                        %round,
                        digest = %digest.0,
                        "marshal did not acknowledge proposed block (mailbox closed); \
                         proposal is not durably cached"
                    );
                }
                Ok(ProposeOutcome::Proposed(digest))
            }
            Ok(BuildBlockOutcome::ParentProofUnavailable) => {
                Ok(ProposeOutcome::ParentProofUnavailable)
            }
            Ok(BuildBlockOutcome::EpochStale) => Ok(ProposeOutcome::EpochStale),
            Ok(BuildBlockOutcome::BoundaryUnavailable) => Ok(ProposeOutcome::BoundaryUnavailable),
            Err(e) => Err(eyre::eyre!("failed to build block for proposal: {e}")),
        }
    }

    /// Canonicalize parent and build a block on top of it.
    ///
    /// recover the direct parent's canonical Finalization parent-proof
    /// record from marshal's durable finalization archive when the in-process
    /// selection store missed it (restart / late-join / brief finalization lag).
    ///
    /// `get_finalization` is a LOCAL archive read — it never triggers a network
    /// fetch, so this cannot block consensus on a peer; the archive is the same
    /// durable store marshal repopulates during sync. The rebuilt record mirrors
    /// the live [`FinalizationActor`](crate::finalization::actor) writer
    /// field-for-field (pinned by the `record_builder_parity` test), so the
    /// proposer's Phase 1 metadata stays canonical and every validator accepts
    /// it. Returns `None` when the archive has no finalization for `parent_height`,
    /// the recovered finalization does not finalize this exact parent, or the
    /// finalized epoch's committee scheme / ordered addresses are not registered.
    // `pub(crate)` for the regression test in `handler_tests` (a sibling
    // module): exercises the selection-store-miss recovery branch directly.
    pub(crate) async fn recover_parent_proof_from_marshal(
        &self,
        parent_proof_key: crate::finalization::parent_cert_store::CertifiedParentProofKey,
        parent_height: u64,
    ) -> Option<crate::finalization::parent_cert_store::CertifiedParentProofRecord> {
        use commonware_codec::Encode as _;

        let finalization = self
            .marshal_mailbox
            .get_finalization(Height::new(parent_height))
            .await?;
        // Hash-exact: the recovered finalization must finalize THIS parent.
        if finalization.proposal.payload.0 != parent_proof_key.block_hash {
            return None;
        }
        let epoch = finalization.proposal.round.epoch();
        let scheme = self.certificate_scheme_provider.scoped(epoch)?;
        let ordered = self.committee_provider.ordered_committee(epoch)?;
        let encoded: alloy_primitives::Bytes = finalization.encode().into();
        match crate::finalization::resolver::build_finalization_record_from_recovered(
            epoch.get(),
            finalization.proposal.round.view().get(),
            finalization.proposal.parent.get(),
            parent_height,
            finalization.proposal.payload.0,
            ordered.as_ref(),
            &finalization.certificate,
            encoded,
            scheme.as_ref(),
        ) {
            Ok(record) => Some(record),
            Err(error) => {
                // Encode-invariant violation on the marshal-recovery path: no
                // canonical record can be produced, so recovery is unavailable
                // (deterministic; never a wrong proof). Logged, not fatal.
                tracing::warn!(
                    target: "outbe::application",
                    epoch = epoch.get(),
                    parent_height,
                    %error,
                    "marshal-recovered finalization record build failed; \
                     no Phase 1 recovery record available"
                );
                None
            }
        }
    }

    async fn select_parent_proof_for_proposal(
        &self,
        clock: &(impl commonware_runtime::Clock + commonware_runtime::Supervisor),
        round: Round,
        parent_digest: Digest,
        parent_height: Height,
        parent_proof_key: Option<CertifiedParentProofKey>,
    ) -> ParentProofLookup {
        if parent_height.get() == 0 {
            return ParentProofLookup::NoProofNeeded;
        }
        let Some(parent_proof_key) = parent_proof_key else {
            warn!(
                %round,
                parent = %parent_digest.0,
                parent_height = parent_height.get(),
                "non-genesis proposal missing exact parent proof key"
            );
            crate::metrics::record_parent_cert_missing();
            crate::metrics::record_parent_proof_unavailable_forfeit();
            crate::metrics::record_phase1_parent_proof_unavailable();
            return ParentProofLookup::Unavailable;
        };
        match self
            .finalization_selector
            .select_direct_parent_proof_by_key_with_wait(
                clock,
                parent_proof_key,
                parent_height.get(),
                crate::finalization::selection::PHASE1_FINALIZATION_WAIT_DEFAULT,
            )
            .await
        {
            Some(record) => ParentProofLookup::Found(record),
            None => {
                // The in-process selection store missed the direct parent's proof
                // (post-restart, late-joining validator, or brief finalization lag),
                // but marshal's DURABLE finalization archive may still hold the
                // parent's finalization locally. Recover it and rebuild the
                // canonical Finalization parent-proof record before forfeiting the
                // slot.
                match self
                    .recover_parent_proof_from_marshal(parent_proof_key, parent_height.get())
                    .await
                {
                    Some(record) => {
                        crate::metrics::record_parent_proof_recovered_from_marshal();
                        info!(
                            %round,
                            parent = %parent_digest.0,
                            parent_height = parent_height.get(),
                            "recovered direct-parent proof from marshal archive; slot not forfeited"
                        );
                        ParentProofLookup::Found(record)
                    }
                    None => {
                        crate::metrics::record_parent_cert_missing();
                        crate::metrics::record_parent_proof_unavailable_forfeit();
                        crate::metrics::record_phase1_parent_proof_unavailable();
                        ParentProofLookup::Unavailable
                    }
                }
            }
        }
    }

    /// Uses FCU-based flow (like tempo): sends fork_choice_updated with payload
    /// attributes through the executor actor, so the engine starts building
    /// a payload on the correct canonical state with txpool access.
    #[allow(clippy::too_many_arguments)]
    async fn build_block(
        &self,
        clock: &(impl commonware_runtime::Clock + commonware_runtime::Supervisor),
        round: Round,
        parent_height: Height,
        parent_digest: Digest,
        parent_block: Option<ConsensusBlock>,
        parent_proof_key: Option<CertifiedParentProofKey>,
        propose_start: std::time::SystemTime,
    ) -> eyre::Result<BuildBlockOutcome> {
        let next_block_number = parent_height.get().saturating_add(1);
        if let Err(rejection) = self.epoch_fence.check(round, next_block_number) {
            debug!(
                %round,
                parent = %parent_digest.0,
                next_block_number,
                ?rejection,
                "dropping stale proposal before payload build"
            );
            return Ok(BuildBlockOutcome::EpochStale);
        }

        if crate::test_faults::should_drop_new_payload_for_test(Height::new(next_block_number)) {
            warn!(
                %round,
                parent = %parent_digest.0,
                next_block_number,
                "test-marshal-drop: skipping local proposal for dropped height"
            );
            return Ok(BuildBlockOutcome::EpochStale);
        }

        let parent_timestamp_millis = self.finalization_view.timestamp_floor();
        let now_millis = unix_now_millis()?;
        // Clamp the proposed timestamp into the deterministic two-sided drift band
        // `[parent + MIN_BLOCK_TIMESTAMP_ADVANCE_MILLIS,
        // parent + MAX_BLOCK_TIMESTAMP_DRIFT_MILLIS]`. The lower bound
        // forces each block to advance chain time, denying a colluding leader
        // majority the `parent + 1 ms` timestamp freeze that stalls emission and
        // unbonding maturity; the upper bound (C-01) mirrors the validator check
        // in `outbe-node`'s `validate_against_parent_timestamp_millis`, so an
        // honest proposer never emits a block validators would reject as
        // over-drifted. Both bounds match the validator rule exactly, so the
        // clamp only ever shifts the timestamp into the accepted band — never out
        // of it. After a long stall `now_millis` may exceed the cap; the chain
        // self-heals, ratcheting time forward by at most one band per block until
        // it catches up to real time.
        //
        // Exception — the genesis child: before any block has finalized,
        // `finalization_view.last_timestamp_millis` is 0 (it is NOT seeded with
        // the genesis header timestamp), so the band is meaningless and only
        // monotonicity is enforced. The validator side uses the genuine genesis
        // header timestamp and exempts the genesis parent (`parent.number() == 0`)
        // from both band bounds, so block 1 (≈ genesis + now) always validates and
        // no unbonding-lock bypass is possible at the first block.
        let timestamp_millis = clamp_proposed_timestamp_millis(
            parent_timestamp_millis,
            now_millis,
            outbe_primitives::consensus::MAX_BLOCK_TIMESTAMP_DRIFT_MILLIS,
            outbe_primitives::consensus::MIN_BLOCK_TIMESTAMP_ADVANCE_MILLIS,
        );
        let prev_randao = self
            .finalization_view
            .advance_floor_and_read_prev_randao(timestamp_millis);

        // build header.extra_data only from consensus header
        // artifacts that affect block hashing (DKG boundary/dealer-log).
        // Exact-parent finalization facts are carried in the begin-zone
        // Phase 1 system transaction body, not as a header attestation
        // backlog tag.
        //
        //(proposed_height == 1,
        // parent_height == 0) MUST carry `ConsensusHeaderArtifact::BoundaryOutcome`
        // in `extra_data`. If the epoch has no pending boundary for block 1,
        // the proposer forfeits the slot deterministically with the
        // `genesis_dkg_boundary_not_ready` reason — never propose block 1
        // without a real boundary artifact.
        let proposed_height = parent_height.get().saturating_add(1);
        let pending_boundary = self
            .dkg_manager
            .pending_boundary_artifact(round.epoch())
            .await;
        let ancestry = MarshalAncestryReader::new(
            self.marshal_mailbox.clone(),
            self.block_cache.clone(),
            self.ancestry_readiness.clone(),
            Some(round),
            PROPOSE_RESOLUTION_TIMEOUT,
            clock.child("ancestry"),
        );
        let consensus_header_artifact = match self
            .dkg_manager
            .resolve_boundary(parent_block.as_ref(), pending_boundary.as_ref(), &ancestry)
            .await
        {
            Ok(BoundaryRequirement::AlreadyCommitted) => {
                crate::metrics::record_dkg_boundary_requirement("already_committed");
                None
            }
            Ok(BoundaryRequirement::MustEmit) => {
                let Some(boundary) = pending_boundary else {
                    return Err(eyre::eyre!(
                        "boundary requirement requested emission without pending artifact"
                    ));
                };
                crate::metrics::record_dkg_boundary_requirement("must_emit");
                Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary))
            }
            Ok(BoundaryRequirement::NoPending) if proposed_height == 1 => {
                debug!(
                    %round,
                    proposed_height,
                    "block 1 proposal forfeited: DKG boundary artifact for epoch 0 not ready"
                );
                crate::metrics::record_genesis_dkg_boundary_not_ready_forfeit();
                crate::metrics::record_dkg_boundary_unavailable("genesis_boundary_not_ready");
                return Ok(BuildBlockOutcome::BoundaryUnavailable);
            }
            Ok(BoundaryRequirement::NoPending) => {
                crate::metrics::record_dkg_boundary_requirement("no_pending");
                self.dkg_manager
                    .get_dealer_log(round.epoch())
                    .await
                    .map(ConsensusHeaderArtifact::DealerLog)
            }
            Err(error) => {
                warn!(
                    %round,
                    proposed_height,
                    %error,
                    "block proposal forfeited: DKG boundary requirement unavailable"
                );
                if error.is_unavailable() {
                    crate::metrics::record_dkg_boundary_unavailable("ancestry_unavailable");
                }
                return Ok(BuildBlockOutcome::BoundaryUnavailable);
            }
        };

        // Non-blocking direct-parent proof selection
        // (finalization first → certified-notarization → marshal-archive
        // recovery → forfeit). The `payload_return_time` budget no longer gates
        // the lookup — the selector returns synchronously. On a selection-store
        // miss the None branch recovers the parent's finalization from marshal's
        // durable archive (, `recover_parent_proof_from_marshal`); only if
        // that also misses does the slot forfeit deterministically with the
        // parent-proof-unavailable metric.
        let parent_proof_record = match self
            .select_parent_proof_for_proposal(
                clock,
                round,
                parent_digest,
                parent_height,
                parent_proof_key,
            )
            .await
        {
            ParentProofLookup::NoProofNeeded => None,
            ParentProofLookup::Found(record) => Some(record),
            ParentProofLookup::Unavailable => return Ok(BuildBlockOutcome::ParentProofUnavailable),
        };
        // V2 wire-format swap landed. Build the V2
        // `CertifiedParentAccountingMetadata` directly from the proof record
        // via [`CertifiedParentProofRecord::to_v2_metadata`]. Both
        // finalization and certified-notarization records project into V2
        // metadata's `ParentProofSelector::select_direct_parent_proof`
        // is the upstream caller that decides which record (if any) to feed
        // into Phase 1.
        let parent_consensus_metadata = parent_proof_record
            .as_ref()
            .map(CertifiedParentProofRecord::to_v2_metadata);
        if parent_consensus_metadata.is_some() {
            crate::metrics::record_parent_cert_included();
        }

        // pack the in-window late-finalize credits this node has
        // locally observed for blocks `proposed_height − K ..= proposed_height − 1`.
        // Best-effort and process-local: every validator re-verifies each batch
        // (pre-exec FATAL) and re-derives the same artifact via header↔calldata
        // parity, so the contents never affect determinism — an empty store just
        // credits nobody. A poisoned lock degrades to no credits.
        let late_finalize_credits = match self.late_sig_store.lock() {
            Ok(store) => {
                let artifact = store.build_artifact(proposed_height);
                if artifact.batches.is_empty() {
                    None
                } else {
                    Some(artifact)
                }
            }
            Err(_) => None,
        };

        let header_extra_data =
            if consensus_header_artifact.is_none() && late_finalize_credits.is_none() {
                Bytes::new()
            } else {
                encode_outbe_block_artifacts(&OutbeBlockArtifacts {
                    execution_summary: None,
                    consensus_header_artifact,
                    // The sub-second timestamp part is recomputed by the
                    // payload builder from `OutbeBlockExecutionCtx` and
                    // re-encoded into `extra_data` before sealing; we
                    // intentionally leave it at 0 here.
                    timestamp_millis_part: 0,
                    late_finalize_credits,
                })
                .map_err(|e| eyre::eyre!(e.to_string()))?
            };

        let attrs = OutbePayloadAttributes::new(
            REWARDS_ADDRESS,
            timestamp_millis,
            prev_randao,
            Some(B256::ZERO),
            header_extra_data,
            parent_consensus_metadata,
            self.proposer_evm_address,
        );

        if let Err(rejection) = self.epoch_fence.check(round, next_block_number) {
            debug!(
                %round,
                parent = %parent_digest.0,
                next_block_number,
                ?rejection,
                "dropping stale proposal before FCU payload build"
            );
            return Ok(BuildBlockOutcome::EpochStale);
        }

        // FCU-based payload building: canonicalize parent and start building
        // in one atomic operation via the executor actor.
        let payload_id = self
            .executor_mailbox
            .canonicalize_and_build(parent_height, parent_digest, attrs)
            .await
            .map_err(|e| eyre::eyre!("canonicalize_and_build failed: {e}"))?;

        debug!(%payload_id, "payload building started via FCU");

        // Give the payload builder a bounded chance to execute transactions before
        // resolving. Elapsed is measured against the runtime clock (same source as
        // the sleep below), so it is correct on the deterministic runtime too.
        let elapsed = clock
            .current()
            .duration_since(propose_start)
            .unwrap_or_default();
        let remaining_resolve = self.payload_resolve_time.saturating_sub(elapsed);

        clock.sleep(remaining_resolve).await;

        if let Err(rejection) = self.epoch_fence.check(round, next_block_number) {
            debug!(
                %round,
                parent = %parent_digest.0,
                next_block_number,
                ?rejection,
                "dropping stale proposal after payload build started"
            );
            return Ok(BuildBlockOutcome::EpochStale);
        }

        let payload = self
            .payload_builder
            .resolve_kind(
                payload_id,
                reth_payload_builder::PayloadKind::WaitForPending,
            )
            .await
            .ok_or_else(|| eyre::eyre!("payload resolution returned None"))?
            .map_err(|e| eyre::eyre!("payload resolution failed: {e}"))?;

        let sealed_block = payload.block().clone();

        let consensus_block = ConsensusBlock::from_sealed(sealed_block);
        let digest = consensus_block.digest();
        let block_number = consensus_block.number();
        debug!(%digest, number = block_number, "block built");

        crate::metrics::record_block_proposed(block_number);

        {
            let mut guard = self
                .block_cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            crate::finalization::actor::insert_block_cache_bounded(
                &mut guard,
                digest,
                consensus_block.clone(),
            );
        }

        Ok(BuildBlockOutcome::Built(digest, consensus_block))
    }

    /// Handle verify request.
    ///
    /// 1. Resolve proposed block (cache or marshal)
    /// 2. Resolve parent block and send new_payload to Reth
    /// 3. Canonicalize parent
    /// 4. Send new_payload for proposed block
    /// 5. Respond with execution validity
    /// 6. If valid, canonicalize proposed block
    async fn handle_verify(
        &self,
        clock: &(impl commonware_runtime::Clock + commonware_runtime::Supervisor),
        context: super::ingress::SimplexContext,
        payload_digest: Digest,
        response: oneshot::Sender<bool>,
    ) -> eyre::Result<()> {
        let round = context.round;
        let (parent_view, parent) = context.parent;
        let parent_digest = Digest(parent.0);

        debug!(
            %round,
            %parent_view,
            digest = %payload_digest.0,
            parent = %parent_digest.0,
            "verify requested"
        );

        if let Err(rejection) = self.epoch_fence.check(round, 0) {
            debug!(
                %round,
                digest = %payload_digest.0,
                ?rejection,
                "dropping stale verify before block resolution"
            );
            let _ = response.send(false);
            return Ok(());
        }

        // epoch continuity: special-case epoch boundary parent
        // before falling back to chain-genesis / generic verify resolution.
        let maybe_epoch_anchor = match self
            .resolve_epoch_boundary_parent(clock, round, parent_view, parent_digest)
            .await
        {
            Ok(opt) => opt,
            Err(EpochBoundaryParentError::ParentMismatch { .. }) => {
                // Invalid proposal: proposer chose a parent that does not match
                // the committed continuity anchor. Deterministic reject.
                warn!(
                    %round,
                    parent = %parent_digest.0,
                    "verify: epoch boundary parent mismatch with finalized anchor"
                );
                let _ = response.send(false);
                return Ok(());
            }
            Err(error) => {
                // Local infrastructure issue (missing anchor / marshal miss / hash mismatch).
                // Do NOT vote false — a validator with a temporarily lagging finalization view
                // or marshal store must not reject a block that is in fact valid. Bubble Err
                // so the response channel drops, matching existing `resolve_for_verify`
                // behaviour for local timeouts.
                return Err(eyre::eyre!(
                    "could not resolve epoch boundary parent: {error}"
                ));
            }
        };
        debug_assert!(
            !(round.epoch().get() > 0
                && parent_view == View::new(0)
                && maybe_epoch_anchor.is_none()),
            "resolve_epoch_boundary_parent invariant: epoch>0 && parent_view=0 must \
             resolve to Some(EpochBoundaryParent) or return an explicit error"
        );

        let block_resolution =
            self.resolve_for_verify(clock, round, payload_digest, VerifyResolveTarget::Block);
        let parent_resolution = async {
            if let Some(anchor) = maybe_epoch_anchor {
                Ok(Some(anchor.block))
            } else if parent_digest.0 == self.genesis_hash {
                Ok(None)
            } else {
                self.resolve_for_verify(
                    clock,
                    parent_round(round, parent_view),
                    parent_digest,
                    VerifyResolveTarget::Parent,
                )
                .await
                .map(Some)
            }
        };

        // `futures::try_join!` is runtime-agnostic (no tokio reactor needed); it polls
        // both resolutions concurrently and short-circuits on the first `Err`,
        // identical to the prior `tokio::try_join!`.
        let (block, parent_block) = match futures::try_join!(block_resolution, parent_resolution) {
            Ok(result) => result,
            Err(error) => {
                return Err(eyre::eyre!(
                    "failed to resolve verify payload or parent: error={error:?} round={round} digest={} parent={}",
                    payload_digest.0,
                    parent_digest.0
                ));
            }
        };

        if let Err(error) = validate_context_parent_binding(
            &block,
            parent_block.as_ref(),
            parent_digest,
            self.genesis_hash,
        ) {
            warn!(
                digest = %payload_digest.0,
                round = %round,
                block_number = block.number(),
                parent = %parent_digest.0,
                %error,
                "proposed block does not extend Simplex context parent"
            );
            let _ = response.send(false);
            return Ok(());
        }

        if let Err(rejection) = self.epoch_fence.check(round, block.number()) {
            debug!(
                %round,
                digest = %payload_digest.0,
                block_number = block.number(),
                ?rejection,
                "dropping stale verify before Engine API work"
            );
            let _ = response.send(false);
            return Ok(());
        }

        if let Err(error) = self.vrf_safety.ensure_block_allowed(block.number()) {
            warn!(
                digest = %payload_digest.0,
                round = %round,
                block_number = block.number(),
                %error,
                "proposed block is above VRF expiry"
            );
            let _ = response.send(false);
            return Ok(());
        }

        let ancestry = MarshalAncestryReader::new(
            self.marshal_mailbox.clone(),
            self.block_cache.clone(),
            self.ancestry_readiness.clone(),
            Some(round),
            VERIFY_RESOLUTION_TIMEOUT,
            clock.child("ancestry"),
        );
        if let Err(error) = validate_header_consensus_artifacts(
            &block,
            parent_block.as_ref(),
            round,
            &context.leader,
            self.chain_id,
            self.proposer_evm_address.is_none(),
            &self.certificate_scheme_provider,
            &self.committee_provider,
            &self.dkg_manager,
            &ancestry,
        )
        .await
        {
            if error.contains("DKG boundary ancestry unavailable")
                || error.contains("DKG boundary ancestry scan exceeded")
            {
                crate::metrics::record_dkg_boundary_unavailable("ancestry_unavailable");
                return Err(eyre::eyre!("DKG boundary requirement unavailable: {error}"));
            }
            warn!(
                digest = %payload_digest.0,
                round = %round,
                %error,
                "proposed block carries invalid header consensus artifact"
            );
            let _ = response.send(false);
            return Ok(());
        }

        // `handle_verify` performs ONLY structural
        // checks — Phase 1 system tx decode succeeds, header artifacts well-
        // formed, parent binding correct, VRF window not expired. It does NOT
        // perform BLS decode/verify on the carried certificate, does not
        // perform accounting checks, and does not look up committee snapshots.
        // The full V2 cryptographic verify is delegated to the EVM-side V2
        // verifier (`outbe-consensus-proof::verify_v2_proof`, consumed by
        // class verifier wiring), keeping `handle_verify` cheap and
        // stateless across every validator.
        if let Err(error) = finalized_parent_attestation_from_phase1_system_tx(&block) {
            warn!(
                digest = %payload_digest.0,
                round = %round,
                %error,
                "failed to decode Phase 1 finalized-parent metadata structure during verify"
            );
            let _ = response.send(false);
            return Ok(());
        }

        if let Some(parent_block) = parent_block {
            let parent_height = Height::new(parent_block.number());
            let execution_data = OutbeExecutionData {
                block: std::sync::Arc::new(parent_block.clone().into_inner()),
            };

            let mut parent_saw_syncing = false;
            if crate::test_faults::should_drop_new_payload_for_test(parent_height) {
                warn!(
                    height = %parent_height,
                    parent = %parent_digest.0,
                    "test-marshal-drop: skipping verify parent new_payload"
                );
            } else {
                loop {
                    if response.is_closed() {
                        debug!(
                            parent = %parent_digest.0,
                            "verify response channel closed while waiting for parent execution validation"
                        );
                        return Ok(());
                    }
                    match self.engine.new_payload(execution_data.clone()).await {
                        Ok(status) if status.is_valid() => {
                            debug!(parent = %parent_digest.0, ?status, "parent accepted during verify");
                            break;
                        }
                        Ok(status) if status.is_syncing() => {
                            parent_saw_syncing = true;
                            warn!(
                                parent = %parent_digest.0,
                                ?status,
                                "parent new_payload returned SYNCING during verify; keeping verification pending"
                            );
                            clock.sleep(VERIFY_SYNCING_RETRY_DELAY).await;
                        }
                        Ok(status) => {
                            warn!(parent = %parent_digest.0, ?status, "parent rejected during verify");
                            let _ = response.send(false);
                            return Ok(());
                        }
                        Err(e) => {
                            return Err(eyre::eyre!(
                                "new_payload for parent failed in verify: parent={} error={e}",
                                parent_digest.0
                            ));
                        }
                    }
                }
            }

            if response.is_closed() || parent_saw_syncing {
                debug!(
                    parent = %parent_digest.0,
                    parent_saw_syncing,
                    "skipping verify parent side effects after pending/cancelable execution validation"
                );
            } else if let Err(rejection) = self.epoch_fence.check(round, block.number()) {
                debug!(
                    %round,
                    parent = %parent_digest.0,
                    block_number = block.number(),
                    ?rejection,
                    "skipping verify parent side effects after stale epoch transition"
                );
            } else {
                if let Err(e) = self
                    .executor_mailbox
                    .canonicalize_head(parent_height, parent_digest)
                    .await
                {
                    return Err(eyre::eyre!(
                        "canonicalize_head failed for parent during verify: parent={} error={e}",
                        parent_digest.0
                    ));
                }

                self.finalization_view
                    .advance_timestamp_floor(parent_block.timestamp_millis());
            }
        }

        // Step 3: new_payload for proposed block.
        let execution_data = OutbeExecutionData {
            block: std::sync::Arc::new(block.clone().into_inner()),
        };

        let block_height = Height::new(block.number());
        let mut block_saw_syncing = false;
        let valid = if crate::test_faults::should_drop_new_payload_for_test(block_height) {
            warn!(
                height = %block_height,
                digest = %payload_digest.0,
                "test-marshal-drop: skipping verify block new_payload"
            );
            true
        } else {
            loop {
                if response.is_closed() {
                    debug!(
                        digest = %payload_digest.0,
                        "verify response channel closed while waiting for execution validation"
                    );
                    return Ok(());
                }
                match self.engine.new_payload(execution_data.clone()).await {
                    Ok(status) if status.is_valid() => break true,
                    Ok(status) if status.is_syncing() => {
                        block_saw_syncing = true;
                        warn!(
                            digest = %payload_digest.0,
                            ?status,
                            "new_payload returned SYNCING during verify; keeping verification pending until execution validates"
                        );
                        clock.sleep(VERIFY_SYNCING_RETRY_DELAY).await;
                    }
                    Ok(status) => {
                        debug!(digest = %payload_digest.0, ?status, "block invalid in verify");
                        break false;
                    }
                    Err(e) => {
                        return Err(eyre::eyre!(
                            "new_payload failed in verify: digest={} error={e}",
                            payload_digest.0
                        ));
                    }
                }
            }
        };

        // Step 4: If valid, canonicalize the proposed block only while the
        // single-shot Simplex verify request is still live. A SYNCING retry can
        // outlive the view timeout; once the receiver is gone, all side effects
        // for this verify request must be suppressed.
        if !valid {
            let _ = response.send(false);
            return Ok(());
        }
        if let Err(rejection) = self.epoch_fence.check(round, block.number()) {
            debug!(
                %round,
                digest = %payload_digest.0,
                block_number = block.number(),
                ?rejection,
                "skipping verify block side effects after stale epoch transition"
            );
            return Ok(());
        }
        if response.is_closed() {
            debug!(
                digest = %payload_digest.0,
                "verify response channel closed before execution-valid side effects"
            );
            return Ok(());
        }
        if response.send(true).is_err() {
            debug!(
                digest = %payload_digest.0,
                "verify response receiver dropped before execution-valid side effects"
            );
            return Ok(());
        }
        if block_saw_syncing {
            debug!(
                digest = %payload_digest.0,
                "skipping verify block side effects after pending/cancelable execution validation"
            );
            return Ok(());
        }
        let _ = self.marshal_mailbox.verified(round, block.clone()).await;
        let _ = self
            .executor_mailbox
            .canonicalize_head(block_height, payload_digest)
            .await;
        self.finalization_view
            .advance_timestamp_floor(block.timestamp_millis());

        Ok(())
    }

    async fn resolve_for_verify(
        &self,
        clock: &impl commonware_runtime::Clock,
        round: Round,
        digest: Digest,
        target: VerifyResolveTarget,
    ) -> Result<ConsensusBlock, VerifyResolveError> {
        let started_at = Instant::now();
        debug!(
            %round,
            digest = %digest.0,
            target = target.as_str(),
            "verify resolve started"
        );
        let cached = self
            .block_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&digest)
            .cloned();
        if let Some(block) = cached {
            debug!(
                %round,
                digest = %digest.0,
                target = target.as_str(),
                source = "cache",
                result = "Resolved",
                elapsed_ms = started_at.elapsed().as_millis(),
                "verify resolve finished"
            );
            return Ok(block);
        }

        let marshal = self.marshal_mailbox.clone();
        let block_future = marshal.subscribe_by_digest(
            digest,
            commonware_consensus::marshal::core::DigestFallback::FetchByRound { round },
        );
        match clock.timeout(VERIFY_RESOLUTION_TIMEOUT, block_future).await {
            Ok(Ok(block)) => {
                debug!(
                    %round,
                    digest = %digest.0,
                    target = target.as_str(),
                    source = "marshal",
                    result = "Resolved",
                    elapsed_ms = started_at.elapsed().as_millis(),
                    "verify resolve finished"
                );
                Ok(block)
            }
            Ok(Err(_)) => {
                debug!(
                    %round,
                    digest = %digest.0,
                    target = target.as_str(),
                    source = "marshal",
                    result = "Unavailable",
                    elapsed_ms = started_at.elapsed().as_millis(),
                    "verify resolve finished"
                );
                Err(VerifyResolveError::Unavailable)
            }
            Err(_) => {
                debug!(
                    %round,
                    digest = %digest.0,
                    target = target.as_str(),
                    source = "marshal",
                    result = "Timeout",
                    elapsed_ms = started_at.elapsed().as_millis(),
                    "verify resolve finished"
                );
                Err(VerifyResolveError::Timeout)
            }
        }
    }
}

// Marshal-based block resolution tests are in `crate::marshal_tests`.

pub(crate) fn parent_round(round: Round, parent_view: View) -> Round {
    Round::new(round.epoch(), parent_view)
}

// `retry_with_backoff`, `RetryFailure`, `RetryFailureKind` moved to
// `crate::finalization::util` in step 17. Imported at the top of this file.

#[derive(Debug, Clone, Copy)]
enum VerifyResolveError {
    Timeout,
    Unavailable,
}

#[derive(Debug, Clone, Copy)]
enum VerifyResolveTarget {
    Block,
    Parent,
}

impl VerifyResolveTarget {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::Parent => "parent",
        }
    }
}

// `extract_consensus_metadata_from_block` and
// `extract_header_artifact_from_block` moved to
// `crate::finalization::util` in step 17. Imported at the top of this file.

#[allow(clippy::too_many_arguments)]
async fn validate_header_consensus_artifacts(
    block: &ConsensusBlock,
    parent_block: Option<&ConsensusBlock>,
    round: Round,
    proposer: &PublicKey,
    chain_id: u64,
    is_verifier: bool,
    certificate_scheme_provider: &HybridSchemeProvider<MinSig>,
    committee_provider: &CommitteeProvider,
    dkg_manager: &crate::dkg_manager::Mailbox,
    ancestry: &impl AncestryReader,
) -> Result<(), String> {
    // Finalized-follower rule: a share-less verifier (a TEE full-node, no
    // `proposer_evm_address`) does NOT validate live proposals — it follows
    // FINALIZED blocks, whose threshold certificate is verified by the reporter
    // against the GROUP public key (preserved across reshares). The leader-binding
    // and DKG-boundary checks below are polynomial/DKG-view-dependent and would
    // diverge on a verifier's stale post-rotation state, so they are skipped here;
    // consensus safety for the follower comes from the finalization certificate, not
    // from re-deriving the live proposal's leader. The verifier never votes (`me()`
    // is None), so accepting the proposal here cannot affect the committee's quorum.
    if is_verifier {
        return Ok(());
    }
    validate_rewards_beneficiary(block)?;
    validate_system_tx_leader_binding(
        block,
        round,
        proposer,
        chain_id,
        certificate_scheme_provider,
        committee_provider,
    )?;

    let expected_boundary = dkg_manager.pending_boundary_artifact(round.epoch()).await;
    let artifact = extract_header_artifact_from_block(block)?;

    match dkg_manager
        .resolve_boundary(parent_block, expected_boundary.as_ref(), ancestry)
        .await
        .map_err(|error| error.to_string())?
    {
        BoundaryRequirement::NoPending => {}
        BoundaryRequirement::AlreadyCommitted => {
            if matches!(artifact, Some(ConsensusHeaderArtifact::BoundaryOutcome(_))) {
                crate::metrics::record_dkg_boundary_duplicate_rejected();
                return Err(
                    "duplicate DKG BoundaryOutcome after parent ancestry already committed it"
                        .to_string(),
                );
            }
            crate::metrics::record_dkg_boundary_requirement("already_committed");
            return Ok(());
        }
        BoundaryRequirement::MustEmit => {
            let Some(expected_boundary) = expected_boundary else {
                return Err(
                    "boundary requirement requested emission without pending artifact".to_string(),
                );
            };

            let Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary)) = artifact else {
                return Err("block omitted pending DKG BoundaryOutcome".to_string());
            };
            if boundary != expected_boundary {
                return Err("block BoundaryOutcome does not match pending DKG boundary".to_string());
            }
            return dkg_manager
                .verify_pending_boundary_artifact(round.epoch(), &boundary)
                .await
                .map_err(|error| error.to_string());
        }
    }

    let Some(artifact) = artifact else {
        return Ok(());
    };

    match artifact {
        ConsensusHeaderArtifact::BoundaryOutcome(_) => {
            crate::metrics::record_dkg_boundary_duplicate_rejected();
            Err("block carried DKG BoundaryOutcome without pending boundary".to_string())
        }
        ConsensusHeaderArtifact::DealerLog(bytes) => {
            dkg_manager
                .verify_dealer_log(round.epoch(), bytes.to_vec())
                .await
                .map_err(|error| error.to_string())?;
            Ok(())
        }
    }
}

#[cfg(test)]
#[path = "handler_tests.rs"]
mod handler_tests;

#[cfg(test)]
mod clamp_tests {
    use super::clamp_proposed_timestamp_millis;

    const BAND: u64 = 60 * 60 * 1_000; // 1h, matches MAX_BLOCK_TIMESTAMP_DRIFT_MILLIS
    const MIN: u64 = 1_000; // matches MIN_BLOCK_TIMESTAMP_ADVANCE_MILLIS

    #[test]
    fn genesis_child_uses_wall_clock_not_band() {
        // Regression: at genesis the finalization_view is unseeded (parent==0).
        // The real wall-clock (≈1.78e12 ms) must NOT be clamped to 0+band, which
        // would put block 1 before the genesis timestamp and stall the chain. The
        // min-advance lower bound is also skipped at genesis (monotonic-only).
        let now = 1_781_255_987_000u64;
        assert_eq!(clamp_proposed_timestamp_millis(0, now, BAND, MIN), now);
    }

    #[test]
    fn real_parent_applies_band() {
        let parent = 1_781_255_987_000u64;
        // within band, above min advance → wall clock used
        assert_eq!(
            clamp_proposed_timestamp_millis(parent, parent + 2_000, BAND, MIN),
            parent + 2_000
        );
        // far-future now → capped at parent + band
        assert_eq!(
            clamp_proposed_timestamp_millis(parent, parent + 10 * BAND, BAND, MIN),
            parent + BAND
        );
    }

    #[test]
    fn lagging_clock_clamps_up_to_min_advance() {
        // when the proposer's clock has not advanced `MIN` past the parent
        // (or is in the past), the timestamp is clamped UP to `parent + MIN` so
        // the block satisfies the validator minimum-advance rule and is accepted,
        // rather than emitting `parent + 1` which validators would now reject.
        let parent = 1_781_255_987_000u64;
        // now in the past → parent + MIN (not parent + 1).
        assert_eq!(
            clamp_proposed_timestamp_millis(parent, parent - 5, BAND, MIN),
            parent + MIN
        );
        // now between parent+1 and parent+MIN → clamped up to parent + MIN.
        assert_eq!(
            clamp_proposed_timestamp_millis(parent, parent + 500, BAND, MIN),
            parent + MIN
        );
        // now exactly at the min-advance boundary → unchanged.
        assert_eq!(
            clamp_proposed_timestamp_millis(parent, parent + MIN, BAND, MIN),
            parent + MIN
        );
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{address, Address, Bytes, B256};
    use commonware_codec::Encode as _;
    use commonware_consensus::{
        simplex::types::{Finalization, Proposal, Subject},
        types::{Epoch, Round, View},
    };
    use commonware_cryptography::{
        bls12381::{self, primitives::variant::MinSig},
        certificate::Scheme as _,
        Hasher, Sha256, Signer as _,
    };
    use commonware_parallel::Sequential;
    use commonware_utils::{ordered::Quorum as _, N3f1};
    use outbe_primitives::consensus_metadata::CertifiedParentAccountingMetadata;
    use outbe_primitives::reshare_artifact::ConsensusHeaderArtifact;

    use crate::dkg_manager::{self, Mailbox as DkgManagerMailbox};
    use crate::finalization::util::{
        build_signer_bitmap, validate_consensus_metadata, AttestationVerdict,
    };
    use crate::hybrid::{HybridScheme, HybridSchemeProvider};

    use super::{
        validate_header_consensus_artifacts, ApplicationEpochFence, CommitteeProvider, Digest,
        EpochFenceRejection,
    };
    use crate::test_fixtures::*;

    const OUTSIDER: Address = address!("0xdeaddeaddeaddeaddeaddeaddeaddeaddeaddead");

    #[test]
    fn epoch_fence_allows_old_epoch_at_boundary_height() {
        let fence = ApplicationEpochFence::new(Epoch::new(2));
        fence.arm_activation_boundary(Epoch::new(2), 360);

        assert_eq!(
            fence.check(Round::new(Epoch::new(2), View::new(120)), 360),
            Ok(())
        );
    }

    #[test]
    fn epoch_fence_rejects_old_epoch_above_boundary_height() {
        let fence = ApplicationEpochFence::new(Epoch::new(2));
        fence.arm_activation_boundary(Epoch::new(2), 360);

        assert_eq!(
            fence.check(Round::new(Epoch::new(2), View::new(121)), 361),
            Err(EpochFenceRejection::BeyondBoundary {
                max_block_height: 360,
            })
        );
    }

    #[test]
    fn epoch_fence_rejects_old_epoch_after_advance() {
        let fence = ApplicationEpochFence::new(Epoch::new(2));
        fence.arm_activation_boundary(Epoch::new(2), 360);
        fence.advance_epoch(Epoch::new(3));

        assert_eq!(
            fence.check(Round::new(Epoch::new(2), View::new(1)), 361),
            Err(EpochFenceRejection::StaleEpoch {
                active_epoch: Epoch::new(3),
            })
        );
        assert_eq!(
            fence.check(Round::new(Epoch::new(3), View::new(1)), 361),
            Ok(())
        );
    }

    // `valid_metadata_with_supplemental_finalize_vote` and the
    // V1 supplemental-finalize-vote tests below are retired. V2 contract
    // uses the certificate's own signer bitmap as the sole participation
    // input; there is no supplemental-vote bitmap-extension path to test.

    fn valid_metadata() -> (
        CertifiedParentAccountingMetadata,
        HybridSchemeProvider<MinSig>,
        CommitteeProvider,
    ) {
        let (keys, participants) = participants();
        let dkg = crate::bls::bootstrap_dkg(3).expect("bootstrap dkg should succeed");
        let schemes: Vec<HybridScheme<MinSig>> = keys
            .iter()
            .map(|key| {
                let pk = bls12381::PublicKey::from(key.clone());
                let idx = participants.index(&pk).expect("participant index");
                HybridScheme::signer(
                    &crate::config::outbe_app_namespace(),
                    participants.clone(),
                    key.clone(),
                    dkg.polynomial.clone(),
                    dkg.shares[idx.get() as usize].clone(),
                )
                .expect("signer scheme should build")
            })
            .collect();
        let verifier = HybridScheme::<MinSig>::verifier(
            &crate::config::outbe_app_namespace(),
            participants.clone(),
            dkg.polynomial,
        )
        .expect("verifier scheme should build");

        let proposal = Proposal::new(
            Round::new(Epoch::new(0), View::new(5)),
            View::new(4),
            Digest(B256::from_slice(Sha256::hash(b"handler-finalize").as_ref())),
        );
        let subject = Subject::Finalize {
            proposal: &proposal,
        };
        let attestations: Vec<_> = schemes
            .iter()
            .map(|scheme| {
                scheme
                    .sign::<Digest>(subject)
                    .expect("finalize attestation")
            })
            .collect();
        let certificate = verifier
            .assemble::<_, N3f1>(attestations, &Sequential)
            .expect("certificate should assemble");

        let scheme_provider = HybridSchemeProvider::new();
        let committee_provider = CommitteeProvider::new();
        let committee = vec![V1, V2, V3];
        let _ = scheme_provider.register(Epoch::new(0), verifier);
        let _ = committee_provider.register(Epoch::new(0), committee.clone());

        let envelope = Finalization::<HybridScheme<MinSig>, Digest> {
            proposal: proposal.clone(),
            certificate: certificate.clone(),
        };

        (
            CertifiedParentAccountingMetadata {
                finalized_block_number: 5,
                finalized_block_hash: proposal.payload.0,
                finalized_epoch: 0,
                finalized_view: 5,
                parent_view: 4,
                ordered_committee: committee,
                signer_bitmap: build_signer_bitmap(&certificate, 3),
                proof: Bytes::from(envelope.encode()),
                ..Default::default()
            },
            scheme_provider,
            committee_provider,
        )
    }

    #[test]
    fn metadata_is_optional_for_verify() {
        let scheme_provider = HybridSchemeProvider::<MinSig>::new();
        let committee_provider = CommitteeProvider::new();
        assert_eq!(
            validate_consensus_metadata(None, &scheme_provider, &committee_provider),
            AttestationVerdict::AcceptNone
        );
    }

    #[test]
    fn valid_finalized_parent_certificate_is_accepted() {
        let (metadata, scheme_provider, committee_provider) = valid_metadata();
        assert_eq!(
            validate_consensus_metadata(Some(&metadata), &scheme_provider, &committee_provider),
            AttestationVerdict::AcceptValid
        );
    }

    #[test]
    fn finalized_parent_sentinel_metadata_is_rejected_when_present() {
        let (mut metadata, scheme_provider, committee_provider) = valid_metadata();
        metadata.finalized_block_number = 0;
        assert_eq!(
            validate_consensus_metadata(Some(&metadata), &scheme_provider, &committee_provider),
            AttestationVerdict::RejectStructural
        );

        let (mut metadata, scheme_provider, committee_provider) = valid_metadata();
        metadata.finalized_block_hash = B256::ZERO;
        assert_eq!(
            validate_consensus_metadata(Some(&metadata), &scheme_provider, &committee_provider),
            AttestationVerdict::RejectStructural
        );
    }

    // `supplemental_finalize_vote_extends_signer_bitmap` and
    // `supplemental_finalize_vote_is_required_for_extended_bitmap` were
    // V1-only tests of the legacy `build_signer_bitmap_with_finalize_votes`
    // reconciliation. Under V2 the certificate's own bitmap is authoritative;
    // these tests are retired in lockstep with the helper they exercised.

    #[test]
    fn mismatched_ordered_committee_is_rejected() {
        let (mut metadata, scheme_provider, committee_provider) = valid_metadata();
        metadata.ordered_committee.swap(0, 1);
        assert_eq!(
            validate_consensus_metadata(Some(&metadata), &scheme_provider, &committee_provider),
            AttestationVerdict::RejectStructural
        );
    }

    #[test]
    fn outsider_missed_proposer_is_rejected() {
        let (mut metadata, scheme_provider, committee_provider) = valid_metadata();
        metadata
            .missed_proposers
            .push(outbe_primitives::consensus_metadata::MissedProposerEvent {
                view: 1,
                validator: OUTSIDER,
            });
        assert_eq!(
            validate_consensus_metadata(Some(&metadata), &scheme_provider, &committee_provider),
            AttestationVerdict::RejectStructural
        );
    }

    #[test]
    fn tampered_signer_bitmap_is_rejected() {
        let (mut metadata, scheme_provider, committee_provider) = valid_metadata();
        metadata.signer_bitmap[0] = 0;
        assert_eq!(
            validate_consensus_metadata(Some(&metadata), &scheme_provider, &committee_provider),
            AttestationVerdict::RejectCertificate
        );
    }

    #[tokio::test]
    async fn boundary_header_artifact_must_match_dkg_manager() {
        let (keys, _participants, output, _polynomial, _dealer_log) = dkg_runtime_artifacts();
        let validator_set = validator_set_from_keys(&keys);
        let artifact = dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
            epoch: Epoch::new(0),
            validator_set: &validator_set,
            output: &output,
            is_full_dkg: true,
            dkg_cycle: 0,
            freeze_height: 0,
            planned_activation_height: 0,
            vrf_material_version: 0,
            is_validator_set_change: true,
            tee_reshare_registrations: Vec::new(),
        })
        .unwrap();
        let (scheme_provider, committee_provider) =
            leader_binding_providers(Epoch::new(0), &validator_set);
        let manager = DkgManagerMailbox::new();
        manager.note_bootstrap_outcome(artifact.clone());
        let block = block_with_header_artifact(&ConsensusHeaderArtifact::BoundaryOutcome(artifact));
        let ancestry = TestAncestryReader::ready();

        assert!(validate_header_consensus_artifacts(
            &block,
            None,
            Round::new(Epoch::new(0), View::new(1)),
            &keys[0].public_key(),
            outbe_primitives::chain::CHAIN_ID,
            false,
            &scheme_provider,
            &committee_provider,
            &manager,
            &ancestry,
        )
        .await
        .is_ok());
    }

    #[tokio::test]
    async fn pending_boundary_must_be_included_exactly() {
        let (keys, _participants, output, _polynomial, dealer_log) = dkg_runtime_artifacts();
        let validator_set = validator_set_from_keys(&keys);
        let artifact = dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
            epoch: Epoch::new(0),
            validator_set: &validator_set,
            output: &output,
            is_full_dkg: true,
            dkg_cycle: 0,
            freeze_height: 0,
            planned_activation_height: 0,
            vrf_material_version: 0,
            is_validator_set_change: true,
            tee_reshare_registrations: Vec::new(),
        })
        .unwrap();
        let (scheme_provider, committee_provider) =
            leader_binding_providers(Epoch::new(0), &validator_set);
        let manager = DkgManagerMailbox::new();
        manager.note_bootstrap_outcome(artifact);
        let ancestry = TestAncestryReader::ready();

        let missing = validate_header_consensus_artifacts(
            &block_with_number(0),
            None,
            Round::new(Epoch::new(0), View::new(1)),
            &keys[0].public_key(),
            outbe_primitives::chain::CHAIN_ID,
            false,
            &scheme_provider,
            &committee_provider,
            &manager,
            &ancestry,
        )
        .await
        .unwrap_err();
        assert!(missing.contains("omitted pending DKG BoundaryOutcome"));

        let dealer_log_block =
            block_with_header_artifact(&ConsensusHeaderArtifact::DealerLog(dealer_log));
        let wrong_kind = validate_header_consensus_artifacts(
            &dealer_log_block,
            None,
            Round::new(Epoch::new(0), View::new(1)),
            &keys[0].public_key(),
            outbe_primitives::chain::CHAIN_ID,
            false,
            &scheme_provider,
            &committee_provider,
            &manager,
            &ancestry,
        )
        .await
        .unwrap_err();
        assert!(wrong_kind.contains("omitted pending DKG BoundaryOutcome"));
    }

    #[tokio::test]
    async fn dealer_log_header_artifact_allows_foreign_valid_dealer() {
        let (keys, participants, _output, _polynomial, dealer_log) = dkg_runtime_artifacts();
        let validator_set = validator_set_from_keys(&keys);
        let (scheme_provider, committee_provider) =
            leader_binding_providers(Epoch::new(0), &validator_set);
        let manager = DkgManagerMailbox::new();
        manager
            .note_ceremony_started(Epoch::new(0), 7, None, participants)
            .unwrap();
        let block = block_with_header_artifact(&ConsensusHeaderArtifact::DealerLog(dealer_log));
        let ancestry = TestAncestryReader::ready();

        assert!(validate_header_consensus_artifacts(
            &block,
            None,
            Round::new(Epoch::new(0), View::new(2)),
            &keys[0].public_key(),
            outbe_primitives::chain::CHAIN_ID,
            false,
            &scheme_provider,
            &committee_provider,
            &manager,
            &ancestry,
        )
        .await
        .is_ok());
        assert!(validate_header_consensus_artifacts(
            &block,
            None,
            Round::new(Epoch::new(0), View::new(2)),
            &keys[1].public_key(),
            outbe_primitives::chain::CHAIN_ID,
            false,
            &scheme_provider,
            &committee_provider,
            &manager,
            &ancestry,
        )
        .await
        .is_ok());
    }

    #[tokio::test]
    async fn dealer_log_header_artifact_rejects_wrong_ceremony() {
        let (keys, participants, _output, _polynomial, dealer_log) = dkg_runtime_artifacts();
        let validator_set = validator_set_from_keys(&keys);
        let (scheme_provider, committee_provider) =
            leader_binding_providers(Epoch::new(0), &validator_set);
        let manager = DkgManagerMailbox::new();
        manager
            .note_ceremony_started(Epoch::new(0), 8, None, participants)
            .unwrap();
        let block = block_with_header_artifact(&ConsensusHeaderArtifact::DealerLog(dealer_log));
        let ancestry = TestAncestryReader::ready();

        assert!(validate_header_consensus_artifacts(
            &block,
            None,
            Round::new(Epoch::new(0), View::new(2)),
            &keys[1].public_key(),
            outbe_primitives::chain::CHAIN_ID,
            false,
            &scheme_provider,
            &committee_provider,
            &manager,
            &ancestry,
        )
        .await
        .is_err());
    }

    // Finalizer fatal forwarding and `ReplayClassification` tests moved.
    // - The forwarding tests are gone with the deleted finalizer worker.
    // - The replay-classification tests were ported alongside the helper
    //   into `crate::finalization::util` (step 17). See
    //   `finalization/util.rs::tests` for the same coverage.
}
