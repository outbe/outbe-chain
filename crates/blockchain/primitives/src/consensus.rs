//! Consensus-execution bridge types.
//!
//! Shared data structures for passive consensus status and execution-summary
//! cache handoff. Finalized-parent certificate facts travel through Phase 1
//! system transaction input, not through this bridge. This module lives in
//! `outbe-primitives` to avoid circular dependencies between `outbe-consensus`
//! and `outbe-evm`.

mod finalization;

use alloy_primitives::{Address, Bytes, B256};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc, Mutex, MutexGuard,
    },
};

use self::finalization::FinalizationFetcherTx;
pub use self::finalization::{
    FinalizationFetcherRx, FinalizationRequestError, FinalizedBlockBytes, FINALIZATION_QUEUE_DEPTH,
    FINALIZATION_REQUEST_TIMEOUT,
};
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
/// `outbe-execution` consensus policy, and the proposer clamp
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

/// Numeric relation of a child timestamp to its parent timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimestampAdvance {
    /// `timestamp` is not strictly after `parent`.
    NotAfterParent,
    /// Positive drift is above `max_drift`.
    ExceedsMaxDrift { drift: u64 },
    /// Positive drift is within `1..=max_drift`.
    Inside { drift: u64 },
}

/// Classify `timestamp_millis` against `parent_timestamp_millis`.
#[must_use]
pub fn classify_timestamp_advance(
    parent_timestamp_millis: u64,
    timestamp_millis: u64,
    max_drift_millis: u64,
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
    TimestampAdvance::Inside { drift }
}

/// Clamp `now_millis` into `[parent + 1, parent + max_drift]`.
///
/// Addition saturates at `u64::MAX`. A parent at `u64::MAX` has no in-range
/// successor. This function does not apply the absent-parent exception.
#[must_use]
pub fn clamp_timestamp_into_band(
    parent_timestamp_millis: u64,
    now_millis: u64,
    max_drift_millis: u64,
) -> u64 {
    let min_timestamp_millis = parent_timestamp_millis.saturating_add(1);
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
    /// Validator node in Phase 2: running `EpochSupervisor`
    /// with `HybridScheme::verifier` (`share = None`).
    Verifier = 2,
    /// Validator node in Phase 3: running `EpochSupervisor`
    /// with `HybridScheme::signer` (`share = Some`).
    Signer = 3,
}

impl ConsensusOperatingMode {
    /// Stable, bounded label for operating-mode telemetry.
    #[must_use]
    pub const fn as_label(self) -> &'static str {
        match self {
            Self::Follower => "follower",
            Self::UpstreamBootstrap => "upstream_bootstrap",
            Self::Verifier => "verifier",
            Self::Signer => "signer",
        }
    }

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
    equivocation_evidence: crate::equivocation_evidence::EquivocationEvidenceMailbox,
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
    /// block-1 proposal attempt injects the same bytes. It must
    /// remain available across rejected or abandoned proposal candidates. The
    /// block-number guard in the payload builder makes it unreachable after
    /// block 1.
    pending_tee_bootstrap: Option<crate::tee_bootstrap_v2::TeeBootstrapV2>,
    /// Channel to the consensus-side drainer that answers `outbe_getFinalization`
    /// RPC requests from the marshal. Set once at marshal-start. `None` on a
    /// node that does not serve finalizations (e.g. before consensus starts).
    finalization_fetcher: Option<FinalizationFetcherTx>,
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
            equivocation_evidence: crate::equivocation_evidence::EquivocationEvidenceMailbox::new(),
        }
    }

    pub fn equivocation_evidence_mailbox(
        &self,
    ) -> &crate::equivocation_evidence::EquivocationEvidenceMailbox {
        &self.equivocation_evidence
    }

    /// Returns the current ontological consensus operating mode.
    #[must_use]
    pub fn operating_mode(&self) -> ConsensusOperatingMode {
        ConsensusOperatingMode::from_u8(self.operating_mode.load(Ordering::Acquire))
    }

    /// Updates the current ontological consensus operating mode.
    pub fn set_operating_mode(&self, mode: ConsensusOperatingMode) {
        let prev_raw = self.operating_mode.swap(mode as u8, Ordering::AcqRel);
        let prev = ConsensusOperatingMode::from_u8(prev_raw);
        metrics::gauge!("outbe_consensus_operating_mode").set((mode as u8) as f64);
        if prev != mode {
            metrics::counter!("outbe_consensus_mode_transitions_total",
                "from" => prev.as_label(), "to" => mode.as_label())
            .increment(1);
        }
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

    /// Applies `update` to the consensus status under a single lock so
    /// concurrent writers of disjoint fields (such as DKG randomness status)
    /// cannot overwrite `last_finalized_block` with a stale snapshot.
    pub fn update_consensus_status(&self, update: impl FnOnce(&mut ConsensusStatus)) {
        update(&mut self.lock_state().consensus_status);
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
mod tests;
