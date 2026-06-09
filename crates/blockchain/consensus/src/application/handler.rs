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
    fmt,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, Instant},
};

/// A-10: Maximum retry attempts for marshal block resolution before structured application failure.
pub(crate) const FINALIZE_MAX_RETRIES: u32 = 5;
/// A-10: Delay between retry attempts for marshal block resolution.
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
type BlockLookupFuture<'a> = Pin<Box<dyn Future<Output = Option<ConsensusBlock>> + Send + 'a>>;

pub(crate) trait AncestryReader: Send + Sync {
    fn get_block_by_height<'a>(&'a self, height: u64) -> BlockLookupFuture<'a>;
    fn get_block_by_hash<'a>(&'a self, hash: B256) -> BlockLookupFuture<'a>;
    fn is_ready(&self) -> bool;
}

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

fn block_boundary_artifact(
    block: &ConsensusBlock,
) -> Result<Option<outbe_primitives::consensus::DkgBoundaryArtifact>, String> {
    match extract_header_artifact_from_block(block)? {
        Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary)) => Ok(Some(boundary)),
        _ => Ok(None),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BoundaryRequirement {
    NoPending,
    AlreadyCommitted,
    MustEmit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BoundaryRequirementError {
    Unavailable(String),
    Conflict(String),
}

impl BoundaryRequirementError {
    fn is_unavailable(&self) -> bool {
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

fn boundary_scan_floor(pending: &outbe_primitives::consensus::DkgBoundaryArtifact) -> u64 {
    if pending.freeze_height <= pending.planned_activation_height {
        pending.freeze_height
    } else {
        pending
            .planned_activation_height
            .saturating_sub(crate::config::DEFAULT_DKG_ACTIVATION_GRACE_BLOCKS)
    }
}

async fn resolve_boundary_requirement<R: AncestryReader>(
    parent: Option<&ConsensusBlock>,
    pending: Option<&outbe_primitives::consensus::DkgBoundaryArtifact>,
    dkg_manager: &crate::dkg_manager::Mailbox,
    ancestry: &R,
) -> Result<BoundaryRequirement, BoundaryRequirementError> {
    let Some(pending) = pending else {
        return Ok(BoundaryRequirement::NoPending);
    };
    let Some(parent) = parent else {
        return Ok(BoundaryRequirement::MustEmit);
    };
    let original_parent_hash = parent.block_hash();
    let pending_hash = crate::dkg_manager::Mailbox::boundary_artifact_hash(pending)
        .map_err(|error| BoundaryRequirementError::Unavailable(error.to_string()))?;

    if let Some(status) = dkg_manager.cached_boundary_status(original_parent_hash, pending_hash) {
        return match status {
            crate::dkg_manager::BoundaryStatus::NoBoundarySeen => Ok(BoundaryRequirement::MustEmit),
            crate::dkg_manager::BoundaryStatus::BoundaryCommitted(committed) => {
                if committed.artifact_hash == pending_hash && committed.artifact == *pending {
                    Ok(BoundaryRequirement::AlreadyCommitted)
                } else {
                    Err(BoundaryRequirementError::Conflict(
                        "cached DKG BoundaryOutcome conflicts with pending boundary".to_string(),
                    ))
                }
            }
            crate::dkg_manager::BoundaryStatus::Conflict => {
                Err(BoundaryRequirementError::Conflict(
                    "cached parent ancestry carries conflicting DKG BoundaryOutcome".to_string(),
                ))
            }
        };
    }

    if !ancestry.is_ready() {
        return Err(BoundaryRequirementError::Unavailable(
            "DKG boundary ancestry unavailable: marshal ancestry reader is not ready".to_string(),
        ));
    }

    let mut current = parent.clone();
    let scan_floor = boundary_scan_floor(pending);
    loop {
        if let Some(boundary) =
            block_boundary_artifact(&current).map_err(BoundaryRequirementError::Unavailable)?
        {
            let boundary_hash = crate::dkg_manager::Mailbox::boundary_artifact_hash(&boundary)
                .map_err(|error| BoundaryRequirementError::Unavailable(error.to_string()))?;
            if boundary_hash == pending_hash && boundary == *pending {
                let committed = crate::dkg_manager::CommittedDkgBoundary {
                    artifact: boundary,
                    artifact_hash: boundary_hash,
                    block_number: current.number(),
                    block_hash: current.block_hash(),
                };
                dkg_manager.record_boundary_status(
                    original_parent_hash,
                    pending_hash,
                    crate::dkg_manager::BoundaryStatus::BoundaryCommitted(committed),
                );
                return Ok(BoundaryRequirement::AlreadyCommitted);
            }
            if boundary.epoch == pending.epoch {
                dkg_manager.record_boundary_status(
                    original_parent_hash,
                    pending_hash,
                    crate::dkg_manager::BoundaryStatus::Conflict,
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
            dkg_manager.record_boundary_status(
                original_parent_hash,
                pending_hash,
                crate::dkg_manager::BoundaryStatus::NoBoundarySeen,
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
                if dkg_manager.evict_boundary_status(stale_hash) {
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

use alloy_consensus::{BlockHeader as _, SignableTransaction as _, Transaction as _};
use alloy_primitives::{Address, Bytes, B256};
use commonware_consensus::types::{Epoch, Height, Round, View};
use commonware_cryptography::{
    bls12381::{primitives::variant::MinSig, PublicKey},
    certificate::{Provider as _, Scheme as _},
};
use commonware_utils::channel::oneshot;
use commonware_utils::ordered::Quorum as _;
use futures::StreamExt;
use outbe_primitives::{
    addresses::REWARDS_ADDRESS,
    reshare_artifact::{
        encode_outbe_block_artifacts, ConsensusHeaderArtifact, FinalizedParentAttestation,
        OutbeBlockArtifacts,
    },
    OutbeExecutionData, OutbePayloadAttributes, OutbePayloadTypes,
};
use reth_ethereum::primitives::SignedTransaction as _;
use reth_node_builder::{BuiltPayload as _, ConsensusEngineHandle};
use reth_payload_builder::PayloadBuilderHandle;
use tracing::{debug, error, info, warn};

use crate::{
    ancestry_readiness::AncestryReadiness,
    block::ConsensusBlock,
    committee_provider::CommitteeProvider,
    digest::Digest,
    executor,
    finalization::{
        actor::BlockCacheHandle,
        parent_cert_store::{CertifiedParentProofKey, CertifiedParentProofRecord},
        state::FinalizationViewHandle,
    },
    hybrid::{HybridElectorConfigProvider, HybridSchemeProvider},
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

fn consensus_leader_evm_address(
    round: Round,
    proposer: &PublicKey,
    certificate_scheme_provider: &HybridSchemeProvider<MinSig>,
    committee_provider: &CommitteeProvider,
) -> Result<Address, String> {
    let epoch = round.epoch();
    let scheme = certificate_scheme_provider
        .scoped(epoch)
        .ok_or_else(|| format!("missing certificate scheme for epoch {epoch}"))?;
    let participant = scheme.participants().index(proposer).ok_or_else(|| {
        format!("consensus leader public key is not in epoch {epoch} participant set")
    })?;
    let index: usize = participant
        .get()
        .try_into()
        .map_err(|_| format!("participant index {} does not fit usize", participant.get()))?;
    let committee = committee_provider
        .ordered_committee(epoch)
        .ok_or_else(|| format!("missing ordered EVM committee for epoch {epoch}"))?;

    committee.get(index).copied().ok_or_else(|| {
        format!("ordered EVM committee for epoch {epoch} is missing participant index {index}")
    })
}

fn validate_rewards_beneficiary(block: &ConsensusBlock) -> Result<(), String> {
    if block.number() > 0 && block.header().beneficiary() != REWARDS_ADDRESS {
        return Err(format!(
            "non-genesis block beneficiary must be REWARDS_ADDRESS {}: got {}",
            REWARDS_ADDRESS,
            block.header().beneficiary()
        ));
    }
    Ok(())
}

fn validate_context_parent_binding(
    block: &ConsensusBlock,
    parent_block: Option<&ConsensusBlock>,
    context_parent_digest: Digest,
    genesis_hash: B256,
) -> Result<(), String> {
    if block.parent_digest() != context_parent_digest {
        return Err(format!(
            "proposed block parent digest {} does not match Simplex context parent {}",
            block.parent_digest().0,
            context_parent_digest.0
        ));
    }

    let expected_number = if context_parent_digest.0 == genesis_hash {
        1
    } else {
        let parent = parent_block.ok_or_else(|| {
            "non-genesis Simplex context parent was not resolved for height validation".to_string()
        })?;
        if parent.digest() != context_parent_digest {
            return Err(format!(
                "resolved parent digest {} does not match Simplex context parent {}",
                parent.digest().0,
                context_parent_digest.0
            ));
        }
        parent.number().checked_add(1).ok_or_else(|| {
            "parent block number overflow while validating proposal height".to_string()
        })?
    };

    if block.number() != expected_number {
        return Err(format!(
            "proposed block number {} does not extend Simplex parent height {}",
            block.number(),
            expected_number.saturating_sub(1)
        ));
    }

    Ok(())
}

fn validate_system_tx_leader_binding(
    block: &ConsensusBlock,
    round: Round,
    proposer: &PublicKey,
    chain_id: u64,
    certificate_scheme_provider: &HybridSchemeProvider<MinSig>,
    committee_provider: &CommitteeProvider,
) -> Result<(), String> {
    let raw_block = block.clone().into_inner().into_block();
    let artifacts = outbe_primitives::reshare_artifact::decode_outbe_block_artifacts(
        raw_block.header.extra_data().as_ref(),
    )
    .map_err(|error| format!("decode Outbe block artifacts for system tx validation: {error}"))?;

    let layout = outbe_primitives::system_tx::split_system_layout(&raw_block.body.transactions)
        .map_err(|error| format!("invalid system tx layout for leader binding: {error}"))?;
    let has_boundary_outcome = matches!(
        &artifacts.consensus_header_artifact,
        Some(outbe_primitives::reshare_artifact::ConsensusHeaderArtifact::BoundaryOutcome(_))
    );
    let has_tee_bootstrap =
        layout.has_begin_kind(outbe_primitives::system_tx::SystemTxKind::TeeBootstrap);
    outbe_primitives::system_tx::validate_active_system_tx_set(
        &layout,
        raw_block.header.number(),
        has_boundary_outcome,
        has_tee_bootstrap,
    )
    .map_err(|error| format!("invalid system tx set: {error}"))?;

    if layout.system_tx_count() == 0 {
        return Ok(());
    }

    if raw_block.header.number() >= 2 {
        let finalization_tx = *layout
            .begin
            .first()
            .ok_or_else(|| "missing CertifiedParentAccounting system tx".to_string())?;
        let input =
            outbe_primitives::system_tx::SystemTxInputV2::decode(finalization_tx.input().as_ref())
                .map_err(|error| {
                    format!("decode CertifiedParentAccounting system tx input: {error}")
                })?;
        let outbe_primitives::system_tx::SystemTxInputV2::CertifiedParentAccounting { metadata } =
            input
        else {
            return Err("expected CertifiedParentAccounting system tx at begin ordinal 0".into());
        };
        if metadata.finalized_block_hash != raw_block.header.parent_hash() {
            return Err(format!(
                "CertifiedParentAccounting metadata hash must match block parent: expected {}, got {}",
                raw_block.header.parent_hash(),
                metadata.finalized_block_hash
            ));
        }
    }

    if let Some(outbe_primitives::reshare_artifact::ConsensusHeaderArtifact::BoundaryOutcome(
        header_artifact,
    )) = artifacts.consensus_header_artifact.as_ref()
    {
        let mut found = false;
        for tx in layout.begin.iter().chain(layout.end.iter()) {
            let tx = *tx;
            let input = outbe_primitives::system_tx::SystemTxInputV2::decode(tx.input().as_ref())
                .map_err(|error| format!("decode system transaction input: {error}"))?;
            if let outbe_primitives::system_tx::SystemTxInputV2::BoundaryOutcome { artifact } =
                input
            {
                if &artifact != header_artifact {
                    return Err("BoundaryOutcome system tx artifact mismatch".into());
                }
                found = true;
            }
        }
        if !found {
            return Err("missing BoundaryOutcome system tx for header artifact".into());
        }
    }

    for (ordinal, tx) in layout.begin.iter().chain(layout.end.iter()).enumerate() {
        let tx = *tx;
        let input = outbe_primitives::system_tx::SystemTxInputV2::decode(tx.input().as_ref())
            .map_err(|error| format!("decode system transaction input: {error}"))?;
        let ordinal: u8 = ordinal
            .try_into()
            .map_err(|_| format!("system tx ordinal {ordinal} exceeds u8 range"))?;
        let unsigned = outbe_primitives::system_tx::build_unsigned_system_tx(
            input.kind(),
            ordinal,
            raw_block.header.number(),
            chain_id,
            input.encode().map_err(|error| error.to_string())?,
        )
        .map_err(|error| format!("build unsigned system transaction: {error}"))?;
        if tx.signature_hash() != unsigned.signature_hash() {
            return Err(format!(
                "system tx signature_hash mismatch for {:?} at ordinal {}",
                input.kind(),
                ordinal
            ));
        }
    }

    let expected = consensus_leader_evm_address(
        round,
        proposer,
        certificate_scheme_provider,
        committee_provider,
    )?;
    for tx in layout.begin.iter().chain(layout.end.iter()) {
        let signer = tx
            .try_recover()
            .map_err(|error| format!("recover system tx signer for leader binding: {error}"))?;
        if signer != expected {
            return Err(format!(
                "system tx signer {signer} does not match consensus leader EVM address {expected}"
            ));
        }
    }

    Ok(())
}

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

pub struct ApplicationHandler {
    /// Receiver for messages from the Automaton/Relay side.
    rx: futures::channel::mpsc::Receiver<Message>,

    pub(crate) shared: ApplicationShared,

    /// Signer bitmap from the last finalization certificate.
    #[allow(dead_code)]
    last_signers: Option<Vec<bool>>,
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
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        rx: futures::channel::mpsc::Receiver<Message>,
        engine: EngineHandle,
        payload_builder: PayloadBuilder,
        executor_mailbox: executor::Mailbox,
        genesis_hash: B256,
        validators: ValidatorSet,
        chain_id: u64,
        _broadcast_mailbox: crate::marshal_types::BroadcastMailbox,
        marshal_mailbox: crate::marshal_types::MarshalMailbox,
        certificate_scheme_provider: HybridSchemeProvider<MinSig>,
        elector_config_provider: HybridElectorConfigProvider<MinSig>,
        committee_provider: CommitteeProvider,
        dkg_manager: crate::dkg_manager::Mailbox,
        vrf_safety: VrfSafetyGate,
        epoch_fence: ApplicationEpochFence,
        ancestry_readiness: AncestryReadiness,
        finalization_view: FinalizationViewHandle,
        block_cache: BlockCacheHandle,
        finalization_selector: crate::finalization::selection::ParentProofSelector,
        payload_resolve_time: std::time::Duration,
        payload_return_time: std::time::Duration,
        min_block_time: std::time::Duration,
        proposer_evm_address: Option<Address>,
        trust_el_head: bool,
        late_sig_store: crate::finalization::late_sig_store::SharedLateFinalizeStore,
    ) -> Self {
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
            last_signers: None,
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
                let view = self
                    .finalization_view
                    .read()
                    .expect("FinalizationView lock poisoned in handle_genesis");
                if view.last_finalized_number > 0
                    && view.forkchoice.finalized_block_hash != B256::ZERO
                {
                    debug!(
                        finalized_number = view.last_finalized_number,
                        finalized_hash = %view.forkchoice.finalized_block_hash,
                        "handle_genesis(epoch=0): using execution head as anchor (--testnet.trust-el-head)"
                    );
                    let _ = genesis
                        .response
                        .send(Digest(view.forkchoice.finalized_block_hash));
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
            let (height, hash) = {
                let view = self.finalization_view.read().expect(
                    "FinalizationView lock poisoned in handle_genesis; \
                     this indicates a panic in the FinalizationActor write path",
                );
                (
                    view.last_finalized_number,
                    view.forkchoice.finalized_block_hash,
                )
            };
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

        let (expected_height, expected_hash, finalized_round) = {
            let view = self
                .finalization_view
                .read()
                .expect("FinalizationView lock poisoned in resolve_epoch_boundary_parent");
            (
                view.last_finalized_number,
                view.forkchoice.finalized_block_hash,
                view.last_finalized_round,
            )
        };
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
                    .expect("block_cache poisoned")
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

            {
                let mut view = self
                    .finalization_view
                    .write()
                    .expect("FinalizationView poisoned");
                view.last_timestamp_millis =
                    std::cmp::max(view.last_timestamp_millis, parent_block.timestamp_millis());
            }
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

        let min_timestamp_millis = self
            .finalization_view
            .read()
            .expect("FinalizationView poisoned")
            .last_timestamp_millis
            .saturating_add(1);
        let now_millis = unix_now_millis()?;
        let timestamp_millis = std::cmp::max(now_millis, min_timestamp_millis);
        let prev_randao = {
            let mut view = self
                .finalization_view
                .write()
                .expect("FinalizationView poisoned");
            view.last_timestamp_millis =
                std::cmp::max(view.last_timestamp_millis, timestamp_millis);
            view.prev_randao
        };

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
        let consensus_header_artifact = match resolve_boundary_requirement(
            parent_block.as_ref(),
            pending_boundary.as_ref(),
            &self.dkg_manager,
            &ancestry,
        )
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
        // (finalization first → certified-notarization → forfeit). The
        // `payload_return_time` budget no longer gates the lookup — the
        // selector returns synchronously. Remote-fetch fallback for the
        // bounded path is added Batch 3 wiring; until then a
        // missing record forfeits the slot deterministically with the
        // parent-proof-unavailable metric.
        let parent_proof_record = if parent_height.get() == 0 {
            None
        } else {
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
                return Ok(BuildBlockOutcome::ParentProofUnavailable);
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
                Some(record) => Some(record),
                None => {
                    crate::metrics::record_parent_cert_missing();
                    crate::metrics::record_parent_proof_unavailable_forfeit();
                    crate::metrics::record_phase1_parent_proof_unavailable();
                    return Ok(BuildBlockOutcome::ParentProofUnavailable);
                }
            }
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
            let mut guard = self.block_cache.lock().expect("block_cache poisoned");
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

                let mut view = self
                    .finalization_view
                    .write()
                    .expect("FinalizationView poisoned");
                view.last_timestamp_millis =
                    std::cmp::max(view.last_timestamp_millis, parent_block.timestamp_millis());
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
        {
            let mut view = self
                .finalization_view
                .write()
                .expect("FinalizationView poisoned");
            view.last_timestamp_millis =
                std::cmp::max(view.last_timestamp_millis, block.timestamp_millis());
        }

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
            .expect("block_cache poisoned")
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

impl ApplicationHandler {
    /// Store the last certificate's signer bitmap for participation encoding.
    pub fn set_last_signers(&mut self, signers: Vec<bool>) {
        self.last_signers = Some(signers);
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
    certificate_scheme_provider: &HybridSchemeProvider<MinSig>,
    committee_provider: &CommitteeProvider,
    dkg_manager: &crate::dkg_manager::Mailbox,
    ancestry: &impl AncestryReader,
) -> Result<(), String> {
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

    match resolve_boundary_requirement(
        parent_block,
        expected_boundary.as_ref(),
        dkg_manager,
        ancestry,
    )
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
mod tests {
    use std::{
        collections::BTreeMap,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    };

    use alloy_primitives::{address, Address, Bytes, B256};
    use commonware_codec::Encode as _;
    use commonware_consensus::{
        simplex::types::{Finalization, Proposal, Subject},
        types::{Epoch, Round, View},
    };
    use commonware_cryptography::{
        bls12381::{
            self,
            dkg::feldman_desmedt::{Dealer, Info, Output, Player},
            primitives::{sharing::Mode, variant::MinSig},
        },
        certificate::Scheme as _,
        Hasher, Sha256, Signer as _,
    };
    use commonware_math::algebra::Random;
    use commonware_parallel::Sequential;
    use commonware_utils::{
        ordered::{Quorum as _, Set},
        N3f1, TryCollect as _,
    };
    use outbe_primitives::consensus_metadata::CertifiedParentAccountingMetadata;
    use outbe_primitives::reshare_artifact::{
        encode_consensus_header_artifact, ConsensusHeaderArtifact,
    };
    use outbe_primitives::signer::OutbeEvmSigner;
    use outbe_primitives::system_tx::{build_unsigned_system_tx, SystemTxInputV2};
    use outbe_primitives::OutbeHeader;
    use reth_ethereum::{primitives::SealedBlock, Block, TransactionSigned};

    use crate::dkg_manager::{self, Mailbox as DkgManagerMailbox};
    use crate::finalization::util::{
        build_signer_bitmap, validate_consensus_metadata, AttestationVerdict,
    };
    use crate::hybrid::{HybridScheme, HybridSchemeProvider};

    use super::{
        resolve_boundary_requirement, validate_context_parent_binding,
        validate_header_consensus_artifacts, validate_rewards_beneficiary,
        validate_system_tx_leader_binding, AncestryReader, ApplicationEpochFence,
        BlockLookupFuture, BoundaryRequirement, CommitteeProvider, ConsensusBlock, Digest,
        EpochFenceRejection,
    };

    const V1: Address = address!("0x1111111111111111111111111111111111111111");
    const V2: Address = address!("0x2222222222222222222222222222222222222222");
    const V3: Address = address!("0x3333333333333333333333333333333333333333");
    const V4: Address = address!("0x4444444444444444444444444444444444444444");
    const OUTSIDER: Address = address!("0xdeaddeaddeaddeaddeaddeaddeaddeaddeaddead");

    #[derive(Clone, Default)]
    struct TestAncestryReader {
        blocks_by_height: BTreeMap<u64, ConsensusBlock>,
        blocks_by_hash: BTreeMap<B256, ConsensusBlock>,
        ready: bool,
        height_lookups: Arc<AtomicUsize>,
        hash_lookups: Arc<AtomicUsize>,
    }

    impl TestAncestryReader {
        fn ready() -> Self {
            Self {
                blocks_by_height: BTreeMap::new(),
                blocks_by_hash: BTreeMap::new(),
                ready: true,
                height_lookups: Arc::new(AtomicUsize::new(0)),
                hash_lookups: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn not_ready() -> Self {
            Self {
                blocks_by_height: BTreeMap::new(),
                blocks_by_hash: BTreeMap::new(),
                ready: false,
                height_lookups: Arc::new(AtomicUsize::new(0)),
                hash_lookups: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn with_block(mut self, block: ConsensusBlock) -> Self {
            self.blocks_by_height.insert(block.number(), block);
            self
        }

        fn with_hash_block(mut self, block: ConsensusBlock) -> Self {
            self.blocks_by_hash.insert(block.block_hash(), block);
            self
        }

        fn lookup_count(&self) -> usize {
            self.height_lookups.load(Ordering::SeqCst) + self.hash_lookups.load(Ordering::SeqCst)
        }
    }

    impl AncestryReader for TestAncestryReader {
        fn get_block_by_height<'a>(&'a self, height: u64) -> BlockLookupFuture<'a> {
            self.height_lookups.fetch_add(1, Ordering::SeqCst);
            let block = self.blocks_by_height.get(&height).cloned();
            Box::pin(async move { block })
        }

        fn get_block_by_hash<'a>(&'a self, hash: B256) -> BlockLookupFuture<'a> {
            self.hash_lookups.fetch_add(1, Ordering::SeqCst);
            let block = self.blocks_by_hash.get(&hash).cloned().or_else(|| {
                self.blocks_by_height
                    .values()
                    .find(|block| block.block_hash() == hash)
                    .cloned()
            });
            Box::pin(async move { block })
        }

        fn is_ready(&self) -> bool {
            self.ready
        }
    }

    fn validator_set_from_keys(keys: &[bls12381::PrivateKey]) -> crate::validators::ValidatorSet {
        let addresses = [V1, V2, V3, V4];
        crate::validators::ValidatorSet {
            public_keys: keys.iter().map(|key| key.public_key()).collect(),
            addresses: addresses[..keys.len()].to_vec(),
            p2p_addresses: vec![crate::validators::ValidatorP2pAddress::Missing; keys.len()],
        }
    }

    fn leader_binding_providers(
        epoch: Epoch,
        validator_set: &crate::validators::ValidatorSet,
    ) -> (HybridSchemeProvider<MinSig>, CommitteeProvider) {
        let participants: Set<bls12381::PublicKey> = validator_set
            .public_keys
            .iter()
            .cloned()
            .try_collect()
            .expect("participants should build");
        let dkg = crate::bls::bootstrap_dkg(
            validator_set
                .public_keys
                .len()
                .try_into()
                .expect("validator count fits u32"),
        )
        .expect("bootstrap dkg should succeed");
        let verifier = HybridScheme::<MinSig>::verifier(
            crate::config::NAMESPACE,
            participants.clone(),
            dkg.polynomial,
        )
        .expect("verifier scheme should build");
        let ordered_committee = participants
            .iter()
            .map(|public_key| {
                let index = validator_set
                    .public_keys
                    .iter()
                    .position(|candidate| candidate == public_key)
                    .expect("participant exists in validator set");
                validator_set.addresses[index]
            })
            .collect();

        let scheme_provider = HybridSchemeProvider::new();
        let committee_provider = CommitteeProvider::new();
        assert!(scheme_provider.register(epoch, verifier));
        assert!(committee_provider.register(epoch, ordered_committee));
        (scheme_provider, committee_provider)
    }

    fn participants_with_count(n: u64) -> (Vec<bls12381::PrivateKey>, Set<bls12381::PublicKey>) {
        let keys: Vec<bls12381::PrivateKey> = (0..n)
            .map(|i| bls12381::PrivateKey::from_seed(i + 1))
            .collect();
        let participants: Set<bls12381::PublicKey> = keys
            .iter()
            .map(|sk| bls12381::PublicKey::from(sk.clone()))
            .try_collect()
            .expect("participants should build");
        (keys, participants)
    }

    fn participants() -> (Vec<bls12381::PrivateKey>, Set<bls12381::PublicKey>) {
        participants_with_count(3)
    }

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

    fn block_with_header_artifact(artifact: &ConsensusHeaderArtifact) -> ConsensusBlock {
        let mut block = Block::default();
        block.header.extra_data = encode_consensus_header_artifact(artifact).unwrap();
        let block = block.map_header(OutbeHeader::new);
        ConsensusBlock::from_sealed(SealedBlock::seal_slow(block))
    }

    fn block_with_number_parent_and_header_artifact(
        number: u64,
        parent_hash: B256,
        artifact: &ConsensusHeaderArtifact,
    ) -> ConsensusBlock {
        let mut block = Block::default();
        block.header.number = number;
        block.header.parent_hash = parent_hash;
        block.header.extra_data = encode_consensus_header_artifact(artifact).unwrap();
        let block = block.map_header(OutbeHeader::new);
        ConsensusBlock::from_sealed(SealedBlock::seal_slow(block))
    }

    fn block_with_number(number: u64) -> ConsensusBlock {
        block_with_number_and_parent(number, B256::ZERO)
    }

    fn block_with_number_and_parent(number: u64, parent_hash: B256) -> ConsensusBlock {
        let mut block = Block::default();
        block.header.number = number;
        block.header.parent_hash = parent_hash;
        let block = block.map_header(OutbeHeader::new);
        ConsensusBlock::from_sealed(SealedBlock::seal_slow(block))
    }

    fn sign_system_input(
        signer: &OutbeEvmSigner,
        input: SystemTxInputV2,
        ordinal: u8,
        block_number: u64,
        chain_id: u64,
    ) -> TransactionSigned {
        let unsigned = build_unsigned_system_tx(
            input.kind(),
            ordinal,
            block_number,
            chain_id,
            input.encode().expect("input encodes"),
        )
        .expect("system tx builds");
        signer.sign_unsigned(unsigned).expect("system tx signs")
    }

    fn finalized_metadata(finalized_block_hash: B256) -> CertifiedParentAccountingMetadata {
        CertifiedParentAccountingMetadata {
            finalized_block_number: 1,
            finalized_block_hash,
            finalized_view: 1,
            ..Default::default()
        }
    }

    fn block_with_system_inputs(
        signer: &OutbeEvmSigner,
        block_number: u64,
        parent_hash: B256,
        extra_data: Bytes,
        inputs: Vec<SystemTxInputV2>,
        chain_id: u64,
    ) -> ConsensusBlock {
        let mut block = Block::default();
        block.header.number = block_number;
        block.header.parent_hash = parent_hash;
        block.header.extra_data = extra_data;
        for (ordinal, input) in inputs.into_iter().enumerate() {
            block.body.transactions.push(sign_system_input(
                signer,
                input,
                ordinal.try_into().expect("test ordinal fits"),
                block_number,
                chain_id,
            ));
        }
        let block = block.map_header(OutbeHeader::new);
        ConsensusBlock::from_sealed(SealedBlock::seal_slow(block))
    }

    fn block_with_system_tx(signer: &OutbeEvmSigner) -> ConsensusBlock {
        // block 1 mandatorily carries a BoundaryOutcome under V2,
        // so the minimum-shape "block with system txs" test fixture moved to
        // block 2 where the canonical layout is
        // `[CertifiedParentAccounting, CycleTick, OracleSlashWindow]`.
        let parent_hash = B256::ZERO;
        block_with_system_inputs(
            signer,
            2,
            parent_hash,
            Bytes::new(),
            vec![
                SystemTxInputV2::CertifiedParentAccounting {
                    metadata: finalized_metadata(parent_hash),
                },
                SystemTxInputV2::LateFinalizeCredits {
                    artifact: Default::default(),
                },
                SystemTxInputV2::CycleTick,
                SystemTxInputV2::OracleSlashWindow,
            ],
            outbe_primitives::chain::CHAIN_ID,
        )
    }

    #[allow(clippy::type_complexity)]
    fn dkg_runtime_artifacts() -> (
        Vec<bls12381::PrivateKey>,
        Set<bls12381::PublicKey>,
        Output<MinSig, bls12381::PublicKey>,
        commonware_cryptography::bls12381::primitives::sharing::Sharing<MinSig>,
        Bytes,
    ) {
        let mut keys: Vec<bls12381::PrivateKey> = (0..3)
            .map(|_| bls12381::PrivateKey::random(rand_core::OsRng))
            .collect();
        keys.sort_by_key(|a| a.public_key().encode());

        let participants: Set<bls12381::PublicKey> =
            keys.iter().map(|k| k.public_key()).try_collect().unwrap();

        let info = Info::<MinSig, bls12381::PublicKey>::new::<N3f1>(
            crate::config::NAMESPACE,
            7,
            None,
            Mode::NonZeroCounter,
            participants.clone(),
            participants.clone(),
        )
        .unwrap();

        let mut dealers = Vec::new();
        let mut pub_msgs = Vec::new();
        let mut all_priv_msgs = Vec::new();

        for key in &keys {
            let (dealer, pub_msg, priv_msgs) =
                Dealer::<MinSig, bls12381::PrivateKey>::start::<N3f1>(
                    rand_core::OsRng,
                    info.clone(),
                    key.clone(),
                    None,
                )
                .unwrap();
            dealers.push(dealer);
            pub_msgs.push(pub_msg);
            all_priv_msgs.push(priv_msgs);
        }

        let mut players: Vec<Player<MinSig, bls12381::PrivateKey>> = keys
            .iter()
            .map(|k| Player::new(info.clone(), k.clone()).unwrap())
            .collect();

        for (dealer_idx, (pub_msg, priv_msgs)) in
            pub_msgs.iter().zip(all_priv_msgs.iter()).enumerate()
        {
            let dealer_pk = keys[dealer_idx].public_key();
            for (player_pk, priv_msg) in priv_msgs {
                let player_idx = keys
                    .iter()
                    .position(|k| &k.public_key() == player_pk)
                    .unwrap();
                if let Some(ack) = players[player_idx].dealer_message::<N3f1>(
                    dealer_pk.clone(),
                    pub_msg.clone(),
                    priv_msg.clone(),
                ) {
                    dealers[dealer_idx]
                        .receive_player_ack(player_pk.clone(), ack)
                        .unwrap();
                }
            }
        }

        let mut logs = std::collections::BTreeMap::new();
        let mut first_log = None;
        for dealer in dealers {
            let signed_log = dealer.finalize::<N3f1>();
            if first_log.is_none() {
                first_log = Some(Bytes::from(signed_log.encode()));
            }
            if let Some((pk, log)) = signed_log.check(&info) {
                logs.insert(pk, log);
            }
        }

        let mut dkg_logs = commonware_cryptography::bls12381::dkg::feldman_desmedt::Logs::<
            MinSig,
            bls12381::PublicKey,
            N3f1,
        >::new(info.clone());
        for (dealer_pk, log) in logs {
            dkg_logs.record(dealer_pk, log);
        }
        let (output, _share) = players
            .remove(0)
            .finalize::<N3f1, commonware_cryptography::bls12381::Batch>(
                &mut rand_core::OsRng,
                dkg_logs,
                &Sequential,
            )
            .unwrap();
        let polynomial = output.public().clone();

        (keys, participants, output, polynomial, first_log.unwrap())
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
                    crate::config::NAMESPACE,
                    participants.clone(),
                    key.clone(),
                    dkg.polynomial.clone(),
                    dkg.shares[idx.get() as usize].clone(),
                )
                .expect("signer scheme should build")
            })
            .collect();
        let verifier = HybridScheme::<MinSig>::verifier(
            crate::config::NAMESPACE,
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

    #[test]
    fn rewards_beneficiary_rejects_non_genesis_mismatch() {
        let block = block_with_number_and_parent(1, B256::ZERO);
        let error = validate_rewards_beneficiary(&block)
            .expect_err("non-genesis beneficiary must be rewards escrow");
        assert!(error.contains("beneficiary must be REWARDS_ADDRESS"));
    }

    #[test]
    fn context_parent_binding_accepts_direct_child() {
        let parent = block_with_number(7);
        let child = block_with_number_and_parent(8, parent.block_hash());

        validate_context_parent_binding(&child, Some(&parent), parent.digest(), B256::ZERO)
            .expect("direct child extends Simplex context parent");
    }

    #[test]
    fn context_parent_binding_rejects_wrong_parent_digest() {
        let parent = block_with_number(7);
        let child = block_with_number_and_parent(8, B256::from([0x44; 32]));

        let error =
            validate_context_parent_binding(&child, Some(&parent), parent.digest(), B256::ZERO)
                .expect_err("child must bind header parent to Simplex context parent");
        assert!(error.contains("does not match Simplex context parent"));
    }

    #[test]
    fn context_parent_binding_rejects_height_gap() {
        let parent = block_with_number(7);
        let child = block_with_number_and_parent(9, parent.block_hash());

        let error =
            validate_context_parent_binding(&child, Some(&parent), parent.digest(), B256::ZERO)
                .expect_err("child height must be parent height plus one");
        assert!(error.contains("does not extend Simplex parent height"));
    }

    #[test]
    fn context_parent_binding_accepts_genesis_parent_for_block_one() {
        let genesis_hash = B256::from([0x55; 32]);
        let child = block_with_number_and_parent(1, genesis_hash);

        validate_context_parent_binding(&child, None, Digest(genesis_hash), genesis_hash)
            .expect("block 1 extends genesis parent");
    }

    #[test]
    fn system_tx_validation_rejects_missing_mandatory_kind_before_engine_status() {
        let (keys, _) = participants();
        let validator_set = validator_set_from_keys(&keys);
        let (scheme_provider, committee_provider) =
            leader_binding_providers(Epoch::new(0), &validator_set);
        let block = block_with_number(1);

        let error = validate_system_tx_leader_binding(
            &block,
            Round::new(Epoch::new(0), View::new(1)),
            &keys[0].public_key(),
            outbe_primitives::chain::CHAIN_ID,
            &scheme_provider,
            &committee_provider,
        )
        .expect_err("block 1 must carry mandatory CycleTick system tx");

        assert!(error.contains("invalid system tx set"));
    }

    #[test]
    fn system_tx_leader_binding_accepts_consensus_leader_address() {
        let (keys, _) = participants();
        let signer = OutbeEvmSigner::from_secret_bytes([7u8; 32]).unwrap();
        let mut validator_set = validator_set_from_keys(&keys);
        validator_set.addresses[0] = signer.address();
        let (scheme_provider, committee_provider) =
            leader_binding_providers(Epoch::new(0), &validator_set);
        let block = block_with_system_tx(&signer);

        validate_system_tx_leader_binding(
            &block,
            Round::new(Epoch::new(0), View::new(1)),
            &keys[0].public_key(),
            outbe_primitives::chain::CHAIN_ID,
            &scheme_provider,
            &committee_provider,
        )
        .expect("system tx signer matches consensus leader EVM address");
    }

    #[test]
    fn system_tx_leader_binding_uses_epoch_registered_committee() {
        let (keys, _) = participants();
        let signer = OutbeEvmSigner::from_secret_bytes([9u8; 32]).unwrap();
        let mut validator_set = validator_set_from_keys(&keys);
        validator_set.addresses[1] = signer.address();
        let (scheme_provider, committee_provider) =
            leader_binding_providers(Epoch::new(1), &validator_set);
        let block = block_with_system_tx(&signer);

        validate_system_tx_leader_binding(
            &block,
            Round::new(Epoch::new(1), View::new(1)),
            &keys[1].public_key(),
            outbe_primitives::chain::CHAIN_ID,
            &scheme_provider,
            &committee_provider,
        )
        .expect("epoch-scoped committee maps current leader to EVM signer");
    }

    #[test]
    fn system_tx_leader_binding_rejects_non_leader_signer() {
        let (keys, _) = participants();
        let leader_signer = OutbeEvmSigner::from_secret_bytes([7u8; 32]).unwrap();
        let non_leader_signer = OutbeEvmSigner::from_secret_bytes([8u8; 32]).unwrap();
        let mut validator_set = validator_set_from_keys(&keys);
        validator_set.addresses[0] = leader_signer.address();
        validator_set.addresses[1] = non_leader_signer.address();
        let (scheme_provider, committee_provider) =
            leader_binding_providers(Epoch::new(0), &validator_set);
        let block = block_with_system_tx(&non_leader_signer);

        let error = validate_system_tx_leader_binding(
            &block,
            Round::new(Epoch::new(0), View::new(1)),
            &keys[0].public_key(),
            outbe_primitives::chain::CHAIN_ID,
            &scheme_provider,
            &committee_provider,
        )
        .expect_err("non-leader system tx signer must be rejected");
        assert!(error.contains("does not match consensus leader EVM address"));
    }

    #[test]
    fn system_tx_validation_rejects_wrong_chain_id_before_engine_status() {
        let (keys, _) = participants();
        let signer = OutbeEvmSigner::from_secret_bytes([7u8; 32]).unwrap();
        let mut validator_set = validator_set_from_keys(&keys);
        validator_set.addresses[0] = signer.address();
        let (scheme_provider, committee_provider) =
            leader_binding_providers(Epoch::new(0), &validator_set);
        let block = block_with_system_tx(&signer);

        let error = validate_system_tx_leader_binding(
            &block,
            Round::new(Epoch::new(0), View::new(1)),
            &keys[0].public_key(),
            outbe_primitives::chain::CHAIN_ID + 1,
            &scheme_provider,
            &committee_provider,
        )
        .expect_err("wrong active chain id must be rejected before Engine status");
        assert!(error.contains("system tx signature_hash mismatch"));
    }

    #[test]
    fn system_tx_validation_rejects_finalization_parent_hash_mismatch_before_engine_status() {
        let (keys, _) = participants();
        let signer = OutbeEvmSigner::from_secret_bytes([7u8; 32]).unwrap();
        let mut validator_set = validator_set_from_keys(&keys);
        validator_set.addresses[0] = signer.address();
        let (scheme_provider, committee_provider) =
            leader_binding_providers(Epoch::new(0), &validator_set);
        let parent_hash = B256::from([0x11; 32]);
        let wrong_hash = B256::from([0x22; 32]);
        let block = block_with_system_inputs(
            &signer,
            2,
            parent_hash,
            Bytes::new(),
            vec![
                SystemTxInputV2::CertifiedParentAccounting {
                    metadata: finalized_metadata(wrong_hash),
                },
                SystemTxInputV2::LateFinalizeCredits {
                    artifact: Default::default(),
                },
                SystemTxInputV2::CycleTick,
                SystemTxInputV2::OracleSlashWindow,
            ],
            outbe_primitives::chain::CHAIN_ID,
        );

        let error = validate_system_tx_leader_binding(
            &block,
            Round::new(Epoch::new(0), View::new(1)),
            &keys[0].public_key(),
            outbe_primitives::chain::CHAIN_ID,
            &scheme_provider,
            &committee_provider,
        )
        .expect_err(
            "CertifiedParentAccounting metadata must bind to header parent hash before Engine status",
        );
        assert!(error.contains("CertifiedParentAccounting metadata hash must match block parent"));
    }

    #[test]
    fn system_tx_validation_rejects_boundary_calldata_mismatch_before_engine_status() {
        let (keys, _participants, output, _polynomial, _dealer_log) = dkg_runtime_artifacts();
        let signer = OutbeEvmSigner::from_secret_bytes([7u8; 32]).unwrap();
        let mut validator_set = validator_set_from_keys(&keys);
        validator_set.addresses[0] = signer.address();
        let header_artifact =
            dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
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
        let mut tx_artifact = header_artifact.clone();
        tx_artifact.planned_activation_height =
            tx_artifact.planned_activation_height.saturating_add(1);

        let (scheme_provider, committee_provider) =
            leader_binding_providers(Epoch::new(0), &validator_set);
        let parent_hash = B256::from([0x33; 32]);
        let block = block_with_system_inputs(
            &signer,
            2,
            parent_hash,
            encode_consensus_header_artifact(&ConsensusHeaderArtifact::BoundaryOutcome(
                header_artifact,
            ))
            .expect("header artifact encodes"),
            vec![
                SystemTxInputV2::CertifiedParentAccounting {
                    metadata: finalized_metadata(parent_hash),
                },
                SystemTxInputV2::LateFinalizeCredits {
                    artifact: Default::default(),
                },
                SystemTxInputV2::CycleTick,
                SystemTxInputV2::BoundaryOutcome {
                    artifact: tx_artifact,
                },
                SystemTxInputV2::OracleSlashWindow,
            ],
            outbe_primitives::chain::CHAIN_ID,
        );

        let error = validate_system_tx_leader_binding(
            &block,
            Round::new(Epoch::new(0), View::new(1)),
            &keys[0].public_key(),
            outbe_primitives::chain::CHAIN_ID,
            &scheme_provider,
            &committee_provider,
        )
        .expect_err("BoundaryOutcome calldata must bind to header artifact before Engine status");
        assert!(error.contains("BoundaryOutcome system tx artifact mismatch"));
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
    async fn boundary_requirement_is_derived_from_parent_snapshot_not_local_served_flag() {
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
        let parent =
            block_with_header_artifact(&ConsensusHeaderArtifact::BoundaryOutcome(artifact.clone()));
        let manager = DkgManagerMailbox::new();
        let ancestry = TestAncestryReader::ready();

        assert_eq!(
            resolve_boundary_requirement(Some(&parent), Some(&artifact), &manager, &ancestry)
                .await
                .expect("parent ancestry should decode"),
            BoundaryRequirement::AlreadyCommitted
        );
    }

    #[tokio::test]
    async fn boundary_requirement_no_pending_does_not_read_ancestry() {
        let parent = block_with_number_and_parent(120, B256::from([0x44; 32]));
        let manager = DkgManagerMailbox::new();
        let ancestry = TestAncestryReader::ready();

        assert_eq!(
            resolve_boundary_requirement(Some(&parent), None, &manager, &ancestry)
                .await
                .expect("no pending boundary is a normal requirement state"),
            BoundaryRequirement::NoPending
        );
        assert_eq!(ancestry.lookup_count(), 0);
    }

    #[tokio::test]
    async fn boundary_requirement_uses_marshal_ancestry_after_block_cache_eviction() {
        let (keys, _participants, output, _polynomial, _dealer_log) = dkg_runtime_artifacts();
        let validator_set = validator_set_from_keys(&keys);
        let artifact = dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
            epoch: Epoch::new(1),
            validator_set: &validator_set,
            output: &output,
            is_full_dkg: false,
            dkg_cycle: 1,
            freeze_height: 90,
            planned_activation_height: 120,
            vrf_material_version: 1,
            is_validator_set_change: false,
            tee_reshare_registrations: Vec::new(),
        })
        .unwrap();
        let mut parent_hash = B256::ZERO;
        let mut ancestry = TestAncestryReader::ready();
        let mut parent = None;
        for number in 90..=120 {
            let block = block_with_number_and_parent(number, parent_hash);
            parent_hash = block.block_hash();
            if number == 120 {
                parent = Some(block.clone());
            }
            ancestry = ancestry.with_block(block);
        }
        let parent = parent.expect("parent block exists");
        let manager = DkgManagerMailbox::new();

        assert_eq!(
            resolve_boundary_requirement(Some(&parent), Some(&artifact), &manager, &ancestry)
                .await
                .expect("marshal ancestry should resolve"),
            BoundaryRequirement::MustEmit
        );
    }

    #[tokio::test]
    async fn boundary_requirement_finds_deep_committed_boundary() {
        let (keys, _participants, output, _polynomial, _dealer_log) = dkg_runtime_artifacts();
        let validator_set = validator_set_from_keys(&keys);
        let artifact = dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
            epoch: Epoch::new(1),
            validator_set: &validator_set,
            output: &output,
            is_full_dkg: false,
            dkg_cycle: 1,
            freeze_height: 90,
            planned_activation_height: 120,
            vrf_material_version: 1,
            is_validator_set_change: false,
            tee_reshare_registrations: Vec::new(),
        })
        .unwrap();
        let mut parent_hash = B256::ZERO;
        let mut ancestry = TestAncestryReader::ready();
        let mut parent = None;
        for number in 90..=120 {
            let block = if number == 90 {
                block_with_number_parent_and_header_artifact(
                    number,
                    parent_hash,
                    &ConsensusHeaderArtifact::BoundaryOutcome(artifact.clone()),
                )
            } else {
                block_with_number_and_parent(number, parent_hash)
            };
            parent_hash = block.block_hash();
            if number == 120 {
                parent = Some(block.clone());
            }
            ancestry = ancestry.with_block(block);
        }
        let parent = parent.expect("parent block exists");
        let manager = DkgManagerMailbox::new();

        assert_eq!(
            resolve_boundary_requirement(Some(&parent), Some(&artifact), &manager, &ancestry)
                .await
                .expect("marshal ancestry should resolve"),
            BoundaryRequirement::AlreadyCommitted
        );

        let not_ready = TestAncestryReader::not_ready();
        assert_eq!(
            resolve_boundary_requirement(Some(&parent), Some(&artifact), &manager, &not_ready)
                .await
                .expect("cached boundary status should avoid cold ancestry reads"),
            BoundaryRequirement::AlreadyCommitted
        );
    }

    #[tokio::test]
    async fn boundary_requirement_finds_boundary_committed_at_late_activation_height() {
        let (keys, _participants, output, _polynomial, _dealer_log) = dkg_runtime_artifacts();
        let validator_set = validator_set_from_keys(&keys);
        let planned_activation_height: u64 = 120;
        let late_activation_height = planned_activation_height
            .saturating_add(crate::config::DEFAULT_DKG_ACTIVATION_GRACE_BLOCKS);
        let artifact = dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
            epoch: Epoch::new(1),
            validator_set: &validator_set,
            output: &output,
            is_full_dkg: false,
            dkg_cycle: 1,
            freeze_height: 90,
            planned_activation_height,
            vrf_material_version: 1,
            is_validator_set_change: false,
            tee_reshare_registrations: Vec::new(),
        })
        .unwrap();
        let mut parent_hash = B256::ZERO;
        let mut ancestry = TestAncestryReader::ready();
        let mut parent = None;
        for number in 90..=late_activation_height {
            let block = if number == late_activation_height {
                block_with_number_parent_and_header_artifact(
                    number,
                    parent_hash,
                    &ConsensusHeaderArtifact::BoundaryOutcome(artifact.clone()),
                )
            } else {
                block_with_number_and_parent(number, parent_hash)
            };
            parent_hash = block.block_hash();
            if number == late_activation_height {
                parent = Some(block.clone());
            }
            ancestry = ancestry.with_block(block);
        }
        let parent = parent.expect("late activation parent exists");
        let manager = DkgManagerMailbox::new();

        assert_eq!(
            resolve_boundary_requirement(Some(&parent), Some(&artifact), &manager, &ancestry)
                .await
                .expect("late activation boundary should resolve"),
            BoundaryRequirement::AlreadyCommitted
        );
    }

    #[tokio::test]
    async fn boundary_requirement_uses_hash_lookup_when_height_lookup_is_stale() {
        let (keys, _participants, output, _polynomial, _dealer_log) = dkg_runtime_artifacts();
        let validator_set = validator_set_from_keys(&keys);
        let artifact = dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
            epoch: Epoch::new(1),
            validator_set: &validator_set,
            output: &output,
            is_full_dkg: false,
            dkg_cycle: 1,
            freeze_height: 119,
            planned_activation_height: 120,
            vrf_material_version: 1,
            is_validator_set_change: false,
            tee_reshare_registrations: Vec::new(),
        })
        .unwrap();
        let stale_parent = block_with_number_and_parent(119, B256::from([0x11; 32]));
        let canonical_parent = block_with_number_and_parent(119, B256::from([0x22; 32]));
        let parent = block_with_number_and_parent(120, canonical_parent.block_hash());
        let ancestry = TestAncestryReader::ready()
            .with_block(stale_parent)
            .with_hash_block(canonical_parent);
        let manager = DkgManagerMailbox::new();
        let pending_hash = DkgManagerMailbox::boundary_artifact_hash(&artifact).unwrap();
        let stale_hash = ancestry
            .blocks_by_height
            .get(&119)
            .expect("stale height hit exists")
            .block_hash();
        manager.record_boundary_status(
            stale_hash,
            pending_hash,
            crate::dkg_manager::BoundaryStatus::NoBoundarySeen,
        );
        assert!(manager
            .cached_boundary_status(stale_hash, pending_hash)
            .is_some());

        assert_eq!(
            resolve_boundary_requirement(Some(&parent), Some(&artifact), &manager, &ancestry)
                .await
                .expect("hash lookup should recover canonical ancestry after stale height hit"),
            BoundaryRequirement::MustEmit
        );
        assert!(
            manager
                .cached_boundary_status(stale_hash, pending_hash)
                .is_none(),
            "stale parent status must be explicitly evicted when height lookup returns a non-canonical block"
        );
    }

    #[tokio::test]
    async fn boundary_requirement_rejects_missing_canonical_parent_after_stale_height_hit() {
        let (keys, _participants, output, _polynomial, _dealer_log) = dkg_runtime_artifacts();
        let validator_set = validator_set_from_keys(&keys);
        let artifact = dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
            epoch: Epoch::new(1),
            validator_set: &validator_set,
            output: &output,
            is_full_dkg: false,
            dkg_cycle: 1,
            freeze_height: 119,
            planned_activation_height: 120,
            vrf_material_version: 1,
            is_validator_set_change: false,
            tee_reshare_registrations: Vec::new(),
        })
        .unwrap();
        let stale_parent = block_with_number_and_parent(119, B256::from([0x11; 32]));
        let canonical_parent = block_with_number_and_parent(119, B256::from([0x22; 32]));
        let parent = block_with_number_and_parent(120, canonical_parent.block_hash());
        let ancestry = TestAncestryReader::ready().with_block(stale_parent);
        let manager = DkgManagerMailbox::new();

        let error =
            resolve_boundary_requirement(Some(&parent), Some(&artifact), &manager, &ancestry)
                .await
                .unwrap_err();
        assert!(error.to_string().contains("missing parent"));
    }

    #[tokio::test]
    async fn boundary_requirement_reports_backfill_not_ready() {
        let (keys, _participants, output, _polynomial, _dealer_log) = dkg_runtime_artifacts();
        let validator_set = validator_set_from_keys(&keys);
        let artifact = dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
            epoch: Epoch::new(1),
            validator_set: &validator_set,
            output: &output,
            is_full_dkg: false,
            dkg_cycle: 1,
            freeze_height: 90,
            planned_activation_height: 120,
            vrf_material_version: 1,
            is_validator_set_change: false,
            tee_reshare_registrations: Vec::new(),
        })
        .unwrap();
        let parent = block_with_number_and_parent(120, B256::from([0x33; 32]));
        let manager = DkgManagerMailbox::new();
        let ancestry = TestAncestryReader::not_ready();

        let error =
            resolve_boundary_requirement(Some(&parent), Some(&artifact), &manager, &ancestry)
                .await
                .unwrap_err();
        assert!(error.to_string().contains("not ready"));
        assert_eq!(ancestry.lookup_count(), 0);
    }

    #[tokio::test]
    async fn boundary_requirement_rejects_same_epoch_conflict() {
        let (keys, _participants, output, _polynomial, _dealer_log) = dkg_runtime_artifacts();
        let validator_set = validator_set_from_keys(&keys);
        let artifact = dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
            epoch: Epoch::new(1),
            validator_set: &validator_set,
            output: &output,
            is_full_dkg: false,
            dkg_cycle: 1,
            freeze_height: 90,
            planned_activation_height: 120,
            vrf_material_version: 1,
            is_validator_set_change: false,
            tee_reshare_registrations: Vec::new(),
        })
        .unwrap();
        let mut conflicting = artifact.clone();
        conflicting.vrf_material_version = conflicting.vrf_material_version.saturating_add(1);
        let parent = block_with_number_parent_and_header_artifact(
            120,
            B256::ZERO,
            &ConsensusHeaderArtifact::BoundaryOutcome(conflicting),
        );
        let manager = DkgManagerMailbox::new();
        let ancestry = TestAncestryReader::ready();

        let error =
            resolve_boundary_requirement(Some(&parent), Some(&artifact), &manager, &ancestry)
                .await
                .unwrap_err();
        assert!(error
            .to_string()
            .contains("conflicting DKG BoundaryOutcome"));
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
