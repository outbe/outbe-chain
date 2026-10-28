//! Consensus-execution bridge types.
//!
//! Shared data structures for passive consensus status and execution-summary
//! cache handoff. Finalized-parent certificate facts travel through Phase 1
//! system transaction input, not through this bridge. This module lives in
//! `outbe-primitives` to avoid circular dependencies between `outbe-consensus`
//! and `outbe-evm`.

use alloy_primitives::{Address, Bytes, B256};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc, Mutex, MutexGuard,
    },
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};

use crate::reshare_artifact::ExecutionSummaryArtifact;

const EXECUTION_SUMMARY_CACHE_LIMIT: usize = 1024;

/// Maximum supported block `extra_data` size for Outbe payloads.
///
/// Outbe reserves `header.extra_data` for DKG boundary outcomes and dealer
/// logs. Those artifacts can be much larger than Ethereum's default 32-byte
/// budget. Thus the node validator and the consensus runtime share a larger
/// explicit cap.
pub const OUTBE_MAX_EXTRA_DATA_SIZE: usize = 64 * 1_024;

/// Maximum allowed RLP-encoded size of a block, in bytes.
///
/// The full block travels in a single consensus P2P message (`ConsensusBlock`,
/// and the marshal's `(notarization, block)` resolver co-transport).
/// Commonware caps that message at `MAX_P2P_MESSAGE_SIZE` (2 MiB) and
/// **panics** on overflow. The execution-layer gas limit permits far larger
/// blocks (~7.5 MB of zero-byte calldata at 30M gas). Without this cap, an
/// honest proposer could build a valid block that the network can never
/// disseminate. Verifiers could never pull it, and the view would stall. This
/// bound keeps a margin below the 2 MiB transport cap for the message envelope
/// and the attached certificate.
///
/// Two paths enforce this bound:
/// - Block build: the payload builder skips txs / rejects a sealed block over
///   this size.
/// - Verify: `validate_block_pre_execution` measures `sealed_block.rlp_length()`
///   and re-checks the bound deterministically.
///
/// Thus the validator rejects a byzantine proposer that ignores the build cap,
/// and the transport does not crash. This is a hard-fork-governed protocol
/// constant. Both the builder and the validator read it from here. A workspace
/// test in `outbe-consensus` guards `OUTBE_MAX_BLOCK_SIZE < MAX_P2P_MESSAGE_SIZE`.
pub const OUTBE_MAX_BLOCK_SIZE: usize = 2 * 1024 * 1024 - 128 * 1024;

/// Maximum allowed forward drift, in milliseconds, between a block's
/// `timestamp_millis` and its parent's.
///
/// The proposer supplies block timestamps. The stock Ethereum rules check them
/// only for parent monotonicity. Without an upper bound, a single byzantine
/// leader can ratchet chain time arbitrarily far forward in one block. That
/// jump has two effects:
/// - (a) It instantly matures every pending unbonding entry and the
///   slashed-withdrawal delay. The proposer's own stake can then escape the
///   unbonding lock and the slashing window.
/// - (b) It skips the day-indexed emission schedule.
///
/// This bound is the deterministic, chain-state-only drift band. The proposer
/// and every validator share it:
/// - The proposer caps its assigned timestamp at `parent + this`.
/// - Every validator rejects a block whose delta exceeds `this`.
///
/// See the validator check `validate_against_parent_timestamp_millis` in the
/// `outbe-node` consensus policy, and the proposer clamp
/// (`proposal_timestamp_millis`) in the `outbe-consensus` proposal handler.
///
/// 1 hour is >400x the certification timeout. Thus honest operation, including
/// view-nullification bursts and DKG-reshare pauses, never trips it. A
/// genuinely long outage self-heals: the proposer caps at `parent + this`, and
/// chain time ratchets forward in bounded steps until it reaches real time.
/// Thus the band never turns a recoverable stall into a permanent halt.
/// The bound is ~504x smaller than the 21-day default unbonding period. This
/// eliminates the single-block unbonding-lock bypass. Any residual time ratchet
/// by a sustained byzantine leader is slow and visible on chain. This is a
/// hard-fork-governed protocol constant. Both paths read it from here.
pub const MAX_BLOCK_TIMESTAMP_DRIFT_MILLIS: u64 = 60 * 60 * 1_000;

/// Minimum advance, in milliseconds, that a block's `timestamp_millis` must add
/// over its parent's. This is the lower bound of the two-sided drift band.
///
/// Stock monotonicity requires only `+1 ms`. Thus a colluding leader majority
/// can keep `timestamp = parent + 1 ms` while real time advances. This freezes
/// every time-driven settlement that depends on the block timestamp:
/// - The day-indexed emission schedule never crosses a UTC boundary, so
///   emission stalls.
/// - Unbonding maturity (`complete_time`) is never reached, so stake never
///   unlocks.
///
/// This bound forces each block to advance chain time by at least `this`, and
/// so neutralizes the freeze. The only way to slow chain time below
/// `this`-per-block is to withhold blocks. The view-timeout / leader rotation
/// machinery already bounds that case.
///
/// Like the maximum bound, the rule is deterministic and chain-state-only
/// (header + parent, no wall clock). Thus the proposer and every validator
/// agree. The proposer clamps its assigned timestamp *up* to `parent + this`
/// when its clock has not advanced that far (see the consensus handler build
/// path). Thus validators never reject an honest block. The clamp has an
/// effect only under clock lag. [`MAX_BLOCK_TIMESTAMP_DRIFT_MILLIS`] bounds any
/// resulting forward inflation, and the inflation self-corrects once real time
/// reaches chain time. The validator skips only this minimum when the parent
/// block number is 0. The proposer skips both bands only when the parent
/// timestamp is 0. A parent timestamp of 0, a parent block number of 0, and a
/// missing parent block are not the same input.
///
/// 1 s is half the 2 s default block-time floor (`DEFAULT_MIN_BLOCK_TIME_MS`).
/// Honest pacing leaves real intervals `>=` the floor between consecutive block
/// timestamps. Thus honest pacing never trips the clamp at the default cadence,
/// while a sub-second freeze is impossible. Operators who configure a
/// block-time floor below `this` accept proportionally more bounded forward
/// inflation. This is a hard-fork-governed protocol constant. Both paths read
/// it from here.
pub const MIN_BLOCK_TIMESTAMP_ADVANCE_MILLIS: u64 = 1_000;

/// Numeric relation of a child timestamp to its parent timestamp.
///
/// This comparison does not know the parent block number or whether a parent
/// block was resolved. Callers keep those exemptions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimestampAdvance {
    /// `timestamp` is not strictly after `parent`.
    NotAfterParent,
    /// Positive drift is below `min_advance`.
    BelowMinAdvance { drift: u64 },
    /// Positive drift is above `max_drift`.
    ExceedsMaxDrift { drift: u64 },
    /// Positive drift is inside both bounds.
    Inside { drift: u64 },
}

/// Classify `timestamp_millis` against `parent_timestamp_millis`.
///
/// A drift above `max_drift_millis` is reported before a drift below
/// `min_advance_millis`. The live constants keep the minimum below the maximum.
#[must_use]
pub fn classify_timestamp_advance(
    parent_timestamp_millis: u64,
    timestamp_millis: u64,
    max_drift_millis: u64,
    min_advance_millis: u64,
) -> TimestampAdvance {
    let Some(drift) = timestamp_millis.checked_sub(parent_timestamp_millis) else {
        return TimestampAdvance::NotAfterParent;
    };
    if drift == 0 {
        return TimestampAdvance::NotAfterParent;
    }
    if drift > max_drift_millis {
        return TimestampAdvance::ExceedsMaxDrift { drift };
    }
    if drift < min_advance_millis {
        return TimestampAdvance::BelowMinAdvance { drift };
    }
    TimestampAdvance::Inside { drift }
}

/// Clamp `now_millis` into `[parent + min_advance, parent + max_drift]`.
///
/// Addition saturates at `u64::MAX`. A parent with no room for `min_advance`
/// has no in-range successor. This function does not apply the absent-parent
/// exception.
#[must_use]
pub fn clamp_timestamp_into_band(
    parent_timestamp_millis: u64,
    now_millis: u64,
    max_drift_millis: u64,
    min_advance_millis: u64,
) -> u64 {
    let min_timestamp_millis = parent_timestamp_millis.saturating_add(min_advance_millis);
    let max_timestamp_millis = parent_timestamp_millis.saturating_add(max_drift_millis);
    now_millis
        .max(min_timestamp_millis)
        .min(max_timestamp_millis)
}

/// Inclusion-window depth `K` for the late-finalize-credits mechanism.
///
/// Block `N`'s fees go into escrow. At `N+K` they are split across everyone whose
/// finalize signature for `N` was gathered within `K` blocks. Inclusion distance
/// is `k = inclusion_block - N`, `k in {0..=K}`. The window closes / settles at
/// `N+K`, and per-block state is freed at `N+K+1`. This is a hard-fork-set
/// protocol constant. The executor (settle timing) and the rewards module
/// (decay weights) share it.
pub const LATE_FINALIZE_WINDOW_K: u64 = 3;

/// Decoded participation data from a finalized block.
#[derive(Debug, Clone, Default)]
pub struct ParticipationData {
    /// Validators who signed the finalization.
    pub voters: Vec<Address>,
    /// Validators in the committee who did NOT sign.
    pub absent: Vec<Address>,
}

/// Canonical finalized-parent certificate artifact carried in block history.
///
/// The proposer chooses this data, but the chain carries it. Once a block
/// includes it, it becomes the canonical execution input for participation
/// and missed-vote accounting for the finalized parent it references.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FinalizedParentCertificateData {
    /// Finalized proposal epoch.
    pub epoch: u64,
    /// Finalized proposal view.
    pub view: u64,
    /// Parent view recorded in the finalized proposal.
    pub parent_view: u64,
    /// Active committee in participant index order for this finalized proposal.
    pub ordered_committee: Vec<Address>,
    /// One byte per participant in `ordered_committee`: `1` if the validator
    /// signed the finalization, `0` otherwise.
    pub signer_bitmap: Vec<u8>,
    /// Encoded `HybridCertificate<MinSig>` bytes for cryptographic verification.
    pub encoded_certificate: Bytes,
}

/// Result of a DKG/reshare ceremony, consumed by the executor's certified
/// ValidatorSet boundary hook in the first block after the ceremony completes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReshareResult {
    /// EVM addresses of validators who participated in the ceremony.
    pub new_active_set: Vec<Address>,
    /// Deterministic hash of the active consensus set snapshot.
    pub active_set_hash: B256,
}

/// Canonical DKG boundary artifact carried in block `header.extra_data`.
///
/// This artifact is the execution-facing chain record of a DKG outcome at an
/// epoch boundary. Consensus verifies the carried `outcome` bytes against the
/// locally expected DKG outcome, while execution uses the nested
/// [`ReshareResult`] to activate the new validator set deterministically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DkgBoundaryArtifact {
    /// Epoch activated by this artifact.
    pub epoch: u64,
    /// Monotonic DKG/VRF rotation cycle.
    pub dkg_cycle: u64,
    /// Block height where the target validator set was frozen.
    pub freeze_height: u64,
    /// Planned activation height for this frozen target.
    pub planned_activation_height: u64,
    /// Hash of the frozen target set and boundary metadata.
    pub target_set_hash: B256,
    /// Version of the VRF material activated by this outcome.
    pub vrf_material_version: u64,
    /// Keccak256 hash of the serialized VRF group BLS public key.
    pub vrf_group_public_key: B256,
    /// Raw `commonware_codec::Encode(polynomial.public())` bytes for the VRF
    /// group public key. The boundary artifact carries them so that the
    /// executor can build the incoming `CommitteeSnapshotStore` entry (slot 39)
    /// at the activation block. Thus the executor does not need a new DKG run.
    /// Empty before V2 wiring.
    pub vrf_group_public_key_bytes: Bytes,
    /// Canonical V2 `committee_set_hash`
    /// (`outbe-validatorset::state::committee_set_hash_v2`). Binds addresses,
    /// MinPk pubkeys, `vrf_material_version`, and the encoded VRF group key
    /// for the activated committee. `B256::ZERO` before V2 wiring.
    pub committee_set_hash: B256,
    /// Whether this outcome changes the active consensus set.
    pub is_validator_set_change: bool,
    /// Canonical encoded full DKG output bytes for consensus-side verification.
    pub outcome: Bytes,
    /// Whether the carried outcome comes from a full DKG instead of a reshare.
    pub is_full_dkg: bool,
    /// Execution-facing reshared-set activation payload.
    pub reshare: ReshareResult,
    /// List, in canonical storage order, of ValidatorSet target members
    /// excluded at the exact DKG freeze block because their DcapRequired lease
    /// was not ready. This is the only authority for the narrow replayable
    /// expiry demotion.
    pub tee_expired_target_exclusions: Vec<Address>,
    /// Domain-separated commitment to [`Self::tee_expired_target_exclusions`].
    /// Honest voters compare the complete locally frozen artifact, including
    /// both this commitment and the ordered list.
    pub tee_expired_target_exclusions_hash: B256,
}

/// A single validator entry for genesis initialization.
#[derive(Debug, Clone)]
pub struct GenesisValidator {
    /// Ethereum address of the validator.
    pub address: Address,
    /// 48-byte BLS12-381 MinPk consensus public key.
    pub consensus_pubkey: [u8; 48],
}

/// Initial validator set expected in genesis.
///
/// The executor does not write these validators. It verifies the canonical
/// genesis state against this local consensus bootstrap config on fresh start.
#[derive(Debug, Clone)]
pub struct GenesisValidators {
    pub validators: Vec<GenesisValidator>,
    pub epoch_length_blocks: u32,
}

/// Data produced by the consensus layer for a finalized block.
#[derive(Debug, Clone, Default)]
pub struct ConsensusData {
    /// Finalized block number corresponding to this metadata, when known.
    pub finalized_block_number: u64,
    /// Finalized block hash corresponding to this metadata.
    pub finalized_block_hash: B256,
    /// Canonical finalized-parent certificate artifact used for exact
    /// participation/slashing derivation.
    pub finalized_certificate: FinalizedParentCertificateData,
    /// VRF seed derived from the BLS threshold signature (if available).
    pub vrf_seed: Option<B256>,
    /// V2 requires this list to be empty. Proof verification rejects a nonempty
    /// list. The field stays so the canonical header layout does not change.
    pub missed_proposers: Vec<Address>,
}

/// Execution summary decoded from a locally executed block header.
///
/// This is a bounded live cache for the handoff window between the Reth
/// in-memory tree and the provider DB. The authoritative data remains the
/// canonical block header. Callers must use this cache only after they check
/// that the provider cannot read the header yet. Callers must not use it to
/// override a readable canonical header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CachedExecutionSummary {
    pub summary: ExecutionSummaryArtifact,
    pub timestamp: u64,
    /// State root committed by the same exact block header as `summary`.
    ///
    /// `None` is retained only for legacy/test cache writers. Consensus paths that need
    /// to authenticate native state require `Some` and fail closed otherwise.
    pub state_root: Option<B256>,
}

/// VRF/DKG safety state visible to operators and RPC clients.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RandomnessStatus {
    #[default]
    Unknown,
    Healthy,
    Preparing,
    PendingActivation,
    Grace,
    Degraded,
    Expired,
}

impl RandomnessStatus {
    /// Whether validator-mode consensus should be reported as active for this
    /// randomness state.
    pub const fn is_consensus_active(self) -> bool {
        !matches!(self, Self::Unknown | Self::Expired)
    }

    /// Whether this network-level VRF state permits use of a locally present
    /// threshold share. This does not prove that the local process owns one.
    pub const fn has_threshold_shares(self) -> bool {
        matches!(
            self,
            Self::Healthy | Self::Preparing | Self::PendingActivation | Self::Grace
        )
    }
}

/// Ontological consensus operating mode of this node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[repr(u8)]
pub enum ConsensusOperatingMode {
    /// Pure certified follower node (`--upstream` without `--validator`).
    #[default]
    Follower = 0,
    /// Validator node in Phase 1: syncing via `--upstream` until `ValidatorBootstrapReady`.
    UpstreamBootstrap = 1,
    /// Validator node in Phase 2: running `EpochSupervisor` with `HybridScheme::verifier` (`share = None`).
    Verifier = 2,
    /// Validator node in Phase 3: running `EpochSupervisor` with `HybridScheme::signer` (`share = Some`).
    Signer = 3,
}

impl ConsensusOperatingMode {
    /// Whether this operating mode belongs to the validator lifecycle.
    #[must_use]
    pub const fn is_validator(self) -> bool {
        matches!(
            self,
            Self::UpstreamBootstrap | Self::Verifier | Self::Signer
        )
    }

    const fn from_u8(raw: u8) -> Self {
        match raw {
            1 => Self::UpstreamBootstrap,
            2 => Self::Verifier,
            3 => Self::Signer,
            _ => Self::Follower,
        }
    }
}

/// Finalized consensus status snapshot, updated by the reporter on each finalization.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ConsensusStatus {
    /// Current Simplex view number.
    pub current_view: u64,
    /// Number of connected consensus peers.
    pub connected_peers: u32,
    /// Last finalized block number.
    pub last_finalized_block: u64,
    /// Last VRF seed (from the most recent finalized certificate).
    pub last_vrf_seed: Option<B256>,
    /// Current VRF/DKG safety state.
    pub randomness_status: RandomnessStatus,
    /// Active VRF material version used by consensus votes.
    pub vrf_material_version: u64,
    /// Block height at which the active VRF material was activated.
    pub last_dkg_activation_height: u64,
    /// Planned height for the next VRF/DKG activation.
    pub next_planned_activation_height: u64,
    /// Last block height at which old VRF material may still be used.
    pub vrf_expiry_height: u64,
}

impl ConsensusStatus {
    /// Whether the node is synced and participating in consensus. This value
    /// comes from `randomness_status` (single source of truth) and is not
    /// stored. Thus it can never drift from the VRF/DKG safety state it is
    /// computed from.
    pub fn is_active(&self) -> bool {
        self.randomness_status.is_consensus_active()
    }

    /// Whether the network-level randomness state permits threshold-share use.
    ///
    /// This compatibility helper does not prove that the local process owns a
    /// private share. RPC callers must use
    /// [`ConsensusExecutionBridge::consensus_status_with_threshold_shares`].
    pub fn has_threshold_shares(&self) -> bool {
        self.randomness_status.has_threshold_shares()
    }
}

/// Thread-safe bridge for passive consensus/execution status and caches.
///
/// Finalized-parent attestations no longer travel through this bridge or
/// through `header.extra_data`. Consensus writes exact-parent records into the
/// consensus-owned parent certificate store. It carries the selected record in
/// the successor block's Phase 1 system transaction. The bridge keeps only
/// bootstrap/status data and the short-lived execution-summary cache. That
/// cache is in use while Reth's generic provider has not yet reached the
/// recently executed headers.
#[derive(Clone)]
pub struct ConsensusExecutionBridge {
    inner: Arc<Mutex<BridgeState>>,
    operating_mode: Arc<AtomicU8>,
    repository_publication: crate::radicle_publication::RepositoryPublicationMailbox,
}

#[derive(Default)]
struct BridgeState {
    genesis_validators: Option<GenesisValidators>,
    consensus_status: ConsensusStatus,
    /// Process-local authority fact derived from the active in-memory DKG
    /// material. It is deliberately not persisted or consensus-visible.
    local_threshold_share_present: bool,
    execution_summary_cache: VecDeque<ExecutionSummaryCacheEntry>,
    /// One-time TEE bootstrap payload from the consensus-thread TEE DKG
    /// coordination. It goes to the payload builder so that every
    /// block-1 proposal attempt injects the same bytes (slice 5.1). It must
    /// remain available across rejected or abandoned proposal candidates. The
    /// block-number guard in the payload builder makes it unreachable after
    /// block 1.
    pending_tee_bootstrap: Option<crate::tee_bootstrap_v2::TeeBootstrapV2>,
    /// Channel to the consensus-side drainer that answers `outbe_getFinalization`
    /// RPC requests from the marshal. Set once at marshal-start. `None` on a
    /// node that does not serve finalizations (e.g. before consensus starts).
    finalization_fetcher: Option<FinalizationFetcherTx>,
}

/// The marshal-backed answer to an `outbe_getFinalization` request. It holds
/// the commonware-codec-encoded finalization certificate and the encoded
/// `ConsensusBlock`, ready to hex and ship. A follower's resolver reconstructs
/// `finalization.encode() || block.encode()` from exactly these two fields.
#[derive(Clone, Debug)]
pub struct FinalizedBlockBytes {
    /// `commonware_codec::Encode` bytes of the direct `Finalization` certificate.
    /// Empty for indirectly finalized ancestors; getFinalization must reject
    /// that case while getFinalityProof can supply an authenticated descendant.
    pub finalization: Bytes,
    /// `commonware_codec::Encode` bytes of the finalized `ConsensusBlock`.
    pub block: Bytes,
}

/// Depth of the finalization request queue. An unbounded queue does not
/// protect the drainer. It only changes back-pressure into memory growth and
/// latency. When the queue is full, the bridge sheds a new request, so an
/// overloaded node answers quickly.
pub const FINALIZATION_QUEUE_DEPTH: usize = 256;

/// Maximum time that [`ConsensusExecutionBridge::request_finalization`] waits
/// for the drainer to reply. The `outbe_getFinalityProof` walk also uses this
/// value as its total deadline. It stays below the 10 s request timeout of the
/// default upstream HTTP client.
pub const FINALIZATION_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Sender half of the finalization fetch channel: a height plus a one-shot
/// reply. It lives in the bridge so the `outbe-rpc` handler can request
/// finalized bytes from the consensus thread. That handler cannot see the
/// marshal or `ConsensusBlock`.
type FinalizationFetcherTx = mpsc::Sender<(u64, oneshot::Sender<Option<FinalizedBlockBytes>>)>;

/// Receiver half handed to the consensus-side drainer.
pub type FinalizationFetcherRx =
    mpsc::Receiver<(u64, oneshot::Sender<Option<FinalizedBlockBytes>>)>;

/// The result of [`ConsensusExecutionBridge::request_finalization`].
///
/// "Too busy to look" and "looked and it is not here" are different facts.
/// A caller must be able to tell them apart to retry correctly.
#[derive(Clone, Debug)]
pub enum FinalizationOutcome {
    /// The drainer found the finalized block for the height.
    Found(FinalizedBlockBytes),
    /// The height is not available. Also returned when no fetcher is
    /// installed or the drainer stopped.
    NotFound,
    /// The request queue is full. The request was not enqueued.
    Busy,
    /// The drainer did not reply within [`FINALIZATION_REQUEST_TIMEOUT`].
    TimedOut,
}

#[derive(Clone, Copy)]
struct ExecutionSummaryCacheEntry {
    block_number: u64,
    block_hash: B256,
    summary: ExecutionSummaryArtifact,
    timestamp: u64,
    state_root: Option<B256>,
}

impl ConsensusExecutionBridge {
    /// Creates a new bridge with no pending data.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(BridgeState::default())),
            operating_mode: Arc::new(AtomicU8::new(ConsensusOperatingMode::Follower as u8)),
            repository_publication: Default::default(),
        }
    }

    /// Returns the current ontological consensus operating mode.
    #[must_use]
    pub fn operating_mode(&self) -> ConsensusOperatingMode {
        ConsensusOperatingMode::from_u8(self.operating_mode.load(Ordering::Acquire))
    }

    /// Updates the current ontological consensus operating mode.
    pub fn set_operating_mode(&self, mode: ConsensusOperatingMode) {
        self.operating_mode.store(mode as u8, Ordering::Release);
    }

    pub fn repository_publication_mailbox(
        &self,
    ) -> crate::radicle_publication::RepositoryPublicationMailbox {
        self.repository_publication.clone()
    }

    fn lock_state(&self) -> MutexGuard<'_, BridgeState> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Stores the one-time TEE bootstrap payload for the payload builder to inject.
    /// The consensus thread sets it once the TEE DKG bootstrap coordination completes.
    pub fn set_pending_tee_bootstrap(&self, payload: crate::tee_bootstrap_v2::TeeBootstrapV2) {
        self.lock_state().pending_tee_bootstrap = Some(payload);
    }

    /// Returns a clone of the pending TEE bootstrap payload for a block-1 proposal.
    ///
    /// Proposal construction is retryable. If a candidate build consumed this
    /// value, a rejected first candidate would permanently prevent every later
    /// block-1 proposal. The payload is bounded and immutable after startup.
    /// Thus a clone of it preserves identical bytes across retries.
    pub fn pending_tee_bootstrap(&self) -> Option<crate::tee_bootstrap_v2::TeeBootstrapV2> {
        self.lock_state().pending_tee_bootstrap.clone()
    }

    /// Records a summary from a successfully executed block header.
    ///
    /// This does not make process-local memory authoritative for accounting.
    /// It only allows immediate descendants to settle recently finalized
    /// ancestors while Reth has accepted the block in its tree but the generic
    /// provider has not exposed the canonical header yet.
    pub fn record_execution_summary(
        &self,
        block_number: u64,
        block_hash: B256,
        summary: ExecutionSummaryArtifact,
        timestamp: u64,
    ) {
        self.record_execution_summary_inner(block_number, block_hash, summary, timestamp, None);
    }

    /// Records a summary together with the state root committed by the same
    /// exact block header.
    pub fn record_execution_summary_with_state_root(
        &self,
        block_number: u64,
        block_hash: B256,
        summary: ExecutionSummaryArtifact,
        timestamp: u64,
        state_root: B256,
    ) {
        self.record_execution_summary_inner(
            block_number,
            block_hash,
            summary,
            timestamp,
            Some(state_root),
        );
    }

    fn record_execution_summary_inner(
        &self,
        block_number: u64,
        block_hash: B256,
        summary: ExecutionSummaryArtifact,
        timestamp: u64,
        state_root: Option<B256>,
    ) {
        let mut state = self.lock_state();
        if let Some(entry) = state
            .execution_summary_cache
            .iter_mut()
            .find(|entry| entry.block_number == block_number && entry.block_hash == block_hash)
        {
            entry.summary = summary;
            entry.timestamp = timestamp;
            if state_root.is_some() {
                entry.state_root = state_root;
            }
            return;
        }

        while state.execution_summary_cache.len() >= EXECUTION_SUMMARY_CACHE_LIMIT {
            let _ = state.execution_summary_cache.pop_front();
        }
        state
            .execution_summary_cache
            .push_back(ExecutionSummaryCacheEntry {
                block_number,
                block_hash,
                summary,
                timestamp,
                state_root,
            });
    }

    /// Returns a cached summary for an already executed `(number, hash)` pair.
    pub fn cached_execution_summary(
        &self,
        block_number: u64,
        block_hash: B256,
    ) -> Option<CachedExecutionSummary> {
        let state = self.lock_state();
        state
            .execution_summary_cache
            .iter()
            .rev()
            .find(|entry| entry.block_number == block_number && entry.block_hash == block_hash)
            .map(|entry| CachedExecutionSummary {
                summary: entry.summary,
                timestamp: entry.timestamp,
                state_root: entry.state_root,
            })
    }

    /// Sets the initial validator set expected in genesis.
    ///
    /// Used by the executor during fresh bootstrap to verify that the
    /// on-chain genesis state matches the local consensus bootstrap config.
    pub fn set_genesis_validators(&self, validators: GenesisValidators) {
        let mut state = self.lock_state();
        state.genesis_validators = Some(validators);
    }

    /// Returns and clears the expected genesis validator set.
    pub fn take_genesis_validators(&self) -> Option<GenesisValidators> {
        let mut state = self.lock_state();
        state.genesis_validators.take()
    }

    /// Peeks at expected genesis validators without consuming them.
    pub fn peek_genesis_validators(&self) -> Option<GenesisValidators> {
        let state = self.lock_state();
        state.genesis_validators.clone()
    }

    /// Updates the last finalized block number in the consensus status.
    ///
    /// The application handler calls this after it processes a finalization.
    /// The reporter knows only the Simplex view number, not the actual block
    /// height. The two values can differ when proposals are missed.
    pub fn set_last_finalized_block_number(&self, number: u64) {
        let mut state = self.lock_state();
        state.consensus_status.last_finalized_block = number;
    }

    /// Updates the finalized consensus status snapshot.
    pub fn set_consensus_status(&self, status: ConsensusStatus) {
        let mut state = self.lock_state();
        state.consensus_status = status;
    }

    /// Changes the consensus status in place under one lock. Use this to update
    /// a subset of the fields. A separate read and write through
    /// [`Self::consensus_status`] and [`Self::set_consensus_status`] can write
    /// stale values back over newer values from a different writer.
    pub fn update_consensus_status(&self, update: impl FnOnce(&mut ConsensusStatus)) {
        let mut state = self.lock_state();
        update(&mut state.consensus_status);
    }

    /// Returns a snapshot of the current consensus status.
    pub fn consensus_status(&self) -> ConsensusStatus {
        let state = self.lock_state();
        state.consensus_status.clone()
    }

    /// Publishes whether the active in-memory DKG material contains this
    /// process's private threshold share.
    pub fn set_local_threshold_share_present(&self, present: bool) {
        self.lock_state().local_threshold_share_present = present;
    }

    /// Whether the local process currently has a usable active threshold share.
    ///
    /// Local share possession and network-level VRF safety are independent
    /// facts. A shareless verifier can observe healthy public material. A
    /// signer can retain a private share that is unusable after expiry.
    pub fn has_threshold_shares(&self) -> bool {
        let state = self.lock_state();
        local_threshold_share_is_usable(&state)
    }

    /// Returns the consensus snapshot and its derived local-share report under
    /// one lock so RPC cannot combine values from different transitions.
    pub fn consensus_status_with_threshold_shares(&self) -> (ConsensusStatus, bool) {
        let state = self.lock_state();
        (
            state.consensus_status.clone(),
            local_threshold_share_is_usable(&state),
        )
    }

    /// Installs the finalization fetcher channel and stores the sender. Called
    /// once at marshal-start (validator and follower paths both register one).
    /// The consensus thread drains the matching [`FinalizationFetcherRx`] and
    /// answers from the marshal. Returns the receiver to hand to the drainer.
    /// The channel holds at most [`FINALIZATION_QUEUE_DEPTH`] requests.
    pub fn set_finalization_fetcher(&self) -> FinalizationFetcherRx {
        let (tx, rx) = mpsc::channel(FINALIZATION_QUEUE_DEPTH);
        self.lock_state().finalization_fetcher = Some(tx);
        rx
    }

    /// Requests the finalized certificate + block bytes for `height` from the
    /// consensus thread (used by the `outbe_getFinalization` RPC handler).
    ///
    /// Returns:
    /// - [`FinalizationOutcome::Busy`] when the queue is full. The request is
    ///   not enqueued.
    /// - [`FinalizationOutcome::TimedOut`] when the drainer does not reply
    ///   within [`FINALIZATION_REQUEST_TIMEOUT`].
    /// - [`FinalizationOutcome::NotFound`] when no fetcher is installed, the
    ///   channel is closed, or the drainer has no block for the height.
    ///
    /// The reply travels back over a one-shot so concurrent requests do not
    /// interfere.
    pub async fn request_finalization(&self, height: u64) -> FinalizationOutcome {
        let sender = {
            let state = self.lock_state();
            state.finalization_fetcher.clone()
        };
        let Some(sender) = sender else {
            return FinalizationOutcome::NotFound;
        };
        let (reply_tx, reply_rx) = oneshot::channel();
        match sender.try_send((height, reply_tx)) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => return FinalizationOutcome::Busy,
            Err(mpsc::error::TrySendError::Closed(_)) => return FinalizationOutcome::NotFound,
        }
        match tokio::time::timeout(FINALIZATION_REQUEST_TIMEOUT, reply_rx).await {
            Ok(Ok(Some(bytes))) => FinalizationOutcome::Found(bytes),
            Ok(_) => FinalizationOutcome::NotFound,
            Err(_) => FinalizationOutcome::TimedOut,
        }
    }
}

fn local_threshold_share_is_usable(state: &BridgeState) -> bool {
    state.local_threshold_share_present
        && state
            .consensus_status
            .randomness_status
            .has_threshold_shares()
}

impl Default for ConsensusExecutionBridge {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for ConsensusExecutionBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConsensusExecutionBridge").finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn randomness_status_maps_operator_flags() {
        for (status, expected_active, expected_shares) in [
            (RandomnessStatus::Unknown, false, false),
            (RandomnessStatus::Healthy, true, true),
            (RandomnessStatus::Preparing, true, true),
            (RandomnessStatus::PendingActivation, true, true),
            (RandomnessStatus::Grace, true, true),
            (RandomnessStatus::Degraded, true, false),
            (RandomnessStatus::Expired, false, false),
        ] {
            assert_eq!(status.is_consensus_active(), expected_active, "{status:?}");
            assert_eq!(status.has_threshold_shares(), expected_shares, "{status:?}");
        }
    }

    #[test]
    fn test_bridge_recovers_poisoned_lock_without_panicking() {
        let bridge = ConsensusExecutionBridge::new();
        let poisoned = bridge.clone();

        let _ = std::panic::catch_unwind(move || {
            let _guard = poisoned.inner.lock().unwrap();
            panic!("poison bridge lock");
        });

        bridge.set_genesis_validators(GenesisValidators {
            validators: vec![GenesisValidator {
                address: Address::with_last_byte(0x11),
                consensus_pubkey: [1u8; 48],
            }],
            epoch_length_blocks: 100,
        });

        let validators = bridge.peek_genesis_validators().unwrap();
        assert_eq!(validators.validators.len(), 1);
        assert_eq!(validators.epoch_length_blocks, 100);
    }

    #[test]
    fn test_bridge_last_finalized_block_number() {
        let bridge = ConsensusExecutionBridge::new();
        assert_eq!(bridge.consensus_status().last_finalized_block, 0);

        bridge.set_last_finalized_block_number(42);
        assert_eq!(bridge.consensus_status().last_finalized_block, 42);

        // Reporter sets status (with view as block number placeholder).
        bridge.set_consensus_status(ConsensusStatus {
            current_view: 100,
            last_finalized_block: 0, // placeholder
            ..Default::default()
        });
        // Handler updates the real block number after.
        bridge.set_last_finalized_block_number(99);
        assert_eq!(bridge.consensus_status().last_finalized_block, 99);
    }

    #[test]
    fn bridge_reports_only_usable_active_local_threshold_share() {
        let bridge = ConsensusExecutionBridge::new();
        bridge.set_consensus_status(ConsensusStatus {
            randomness_status: RandomnessStatus::Healthy,
            ..Default::default()
        });
        assert!(
            !bridge.has_threshold_shares(),
            "global Healthy status must not imply a local private share"
        );

        bridge.set_local_threshold_share_present(true);
        assert!(bridge.has_threshold_shares());

        for (status, expected) in [
            (RandomnessStatus::Preparing, true),
            (RandomnessStatus::PendingActivation, true),
            (RandomnessStatus::Grace, true),
            (RandomnessStatus::Degraded, false),
            (RandomnessStatus::Expired, false),
            (RandomnessStatus::Unknown, false),
            (RandomnessStatus::Healthy, true),
        ] {
            bridge.set_consensus_status(ConsensusStatus {
                randomness_status: status,
                ..Default::default()
            });
            assert_eq!(bridge.has_threshold_shares(), expected, "{status:?}");
            let (snapshot, snapshot_has_share) = bridge.consensus_status_with_threshold_shares();
            assert_eq!(snapshot.randomness_status, status);
            assert_eq!(
                snapshot_has_share, expected,
                "atomic snapshot for {status:?}"
            );
        }

        bridge.set_local_threshold_share_present(false);
        assert!(!bridge.has_threshold_shares());
    }

    #[test]
    fn bridge_tracks_consensus_operating_mode_atomically() {
        let bridge = ConsensusExecutionBridge::new();
        assert_eq!(bridge.operating_mode(), ConsensusOperatingMode::Follower);
        assert!(!bridge.operating_mode().is_validator());

        let clone = bridge.clone();
        for (mode, expected_is_validator, expected_json) in [
            (
                ConsensusOperatingMode::UpstreamBootstrap,
                true,
                "\"upstreamBootstrap\"",
            ),
            (ConsensusOperatingMode::Verifier, true, "\"verifier\""),
            (ConsensusOperatingMode::Signer, true, "\"signer\""),
            (ConsensusOperatingMode::Follower, false, "\"follower\""),
        ] {
            bridge.set_operating_mode(mode);
            assert_eq!(clone.operating_mode(), mode);
            assert_eq!(clone.operating_mode().is_validator(), expected_is_validator);
            let serialized = serde_json::to_string(&mode).unwrap();
            assert_eq!(serialized, expected_json);
            let deserialized: ConsensusOperatingMode = serde_json::from_str(&serialized).unwrap();
            assert_eq!(deserialized, mode);
        }
    }

    #[tokio::test]
    async fn finalization_queue_sheds_when_full() {
        let bridge = ConsensusExecutionBridge::new();
        let mut rx = bridge.set_finalization_fetcher();
        let mut pending = Vec::new();
        for height in 0..FINALIZATION_QUEUE_DEPTH as u64 {
            let bridge = bridge.clone();
            pending.push(tokio::spawn(async move {
                bridge.request_finalization(height).await
            }));
        }
        // Let every request task enqueue its request.
        while rx.len() < FINALIZATION_QUEUE_DEPTH {
            tokio::task::yield_now().await;
        }

        assert!(matches!(
            bridge.request_finalization(u64::MAX).await,
            FinalizationOutcome::Busy
        ));
        // The shed request did not enter the queue.
        assert_eq!(rx.len(), FINALIZATION_QUEUE_DEPTH);
        while let Ok((height, _reply)) = rx.try_recv() {
            assert_ne!(height, u64::MAX);
        }
        for task in pending {
            assert!(matches!(task.await.unwrap(), FinalizationOutcome::NotFound));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn finalization_request_times_out() {
        let bridge = ConsensusExecutionBridge::new();
        let mut rx = bridge.set_finalization_fetcher();
        // A drainer that receives each request and never replies.
        let drainer = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Some(request) = rx.recv().await {
                held.push(request);
            }
        });

        let started = tokio::time::Instant::now();
        assert!(matches!(
            bridge.request_finalization(7).await,
            FinalizationOutcome::TimedOut
        ));
        assert_eq!(started.elapsed(), FINALIZATION_REQUEST_TIMEOUT);
        drainer.abort();
    }

    #[tokio::test]
    async fn finalization_request_maps_reply_and_closed_channel() {
        let bridge = ConsensusExecutionBridge::new();
        assert!(matches!(
            bridge.request_finalization(1).await,
            FinalizationOutcome::NotFound
        ));

        let mut rx = bridge.set_finalization_fetcher();
        let drainer = tokio::spawn(async move {
            while let Some((height, reply)) = rx.recv().await {
                let answer = (height == 1).then(|| FinalizedBlockBytes {
                    finalization: Bytes::from_static(&[0x01]),
                    block: Bytes::from_static(&[0x02]),
                });
                let _ = reply.send(answer);
            }
        });
        match bridge.request_finalization(1).await {
            FinalizationOutcome::Found(bytes) => {
                assert_eq!(bytes.finalization.as_ref(), &[0x01]);
                assert_eq!(bytes.block.as_ref(), &[0x02]);
            }
            other => panic!("expected Found, got {other:?}"),
        }
        assert!(matches!(
            bridge.request_finalization(2).await,
            FinalizationOutcome::NotFound
        ));

        drainer.abort();
        let _ = drainer.await;
        assert!(matches!(
            bridge.request_finalization(1).await,
            FinalizationOutcome::NotFound
        ));
    }

    #[test]
    fn update_consensus_status_keeps_fields_it_does_not_change() {
        let bridge = ConsensusExecutionBridge::new();
        bridge.set_last_finalized_block_number(42);
        bridge.update_consensus_status(|status| {
            status.randomness_status = RandomnessStatus::Healthy;
            status.vrf_material_version = 3;
        });
        let status = bridge.consensus_status();
        assert_eq!(status.last_finalized_block, 42);
        assert_eq!(status.randomness_status, RandomnessStatus::Healthy);
        assert_eq!(status.vrf_material_version, 3);
    }
}
