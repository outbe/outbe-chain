//! DKG boundary resolution: the parent-ancestry scan that decides whether a
//! block must emit, already committed, or has no pending DKG `BoundaryOutcome`,
//! plus its process-local boundary-status cache.

use std::{fmt, future::Future, pin::Pin};

use alloy_primitives::{keccak256, B256};
use eyre::Result;
use outbe_primitives::{
    consensus::DkgBoundaryArtifact,
    reshare_artifact::{encode_boundary_artifact, ConsensusHeaderArtifact},
};
use tracing::debug;

use super::{Mailbox, State};
use crate::{
    block::ConsensusBlock, config, finalization::util::extract_header_artifact_from_block,
};

/// Boxed, `Send` future returned by [`AncestryReader`] lookups. Mirrors the
/// marshal-backed block lookup that the application handler provides; the trait
/// methods carry no async context, so each returns an owned future.
pub type BlockLookupFuture<'a> = Pin<Box<dyn Future<Output = Option<ConsensusBlock>> + Send + 'a>>;

/// Read-only ancestry access used by [`Mailbox::resolve_boundary`] to walk a
/// proposal/verification parent chain looking for an already-committed DKG
/// boundary. The production implementation (`MarshalAncestryReader`) lives in
/// the application handler - `dkg_manager` is the sole consumer and defines the
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
// enum that is held briefly per epoch - not worth it for the stack-size delta.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BoundaryStatus {
    NoBoundarySeen,
    BoundaryCommitted(CommittedDkgBoundary),
    Conflict,
}

#[derive(Clone, Debug)]
pub(super) struct BoundaryStatusCacheEntry {
    pub(super) query_artifact_hash: B256,
    pub(super) status: BoundaryStatus,
}

impl Mailbox {
    pub fn boundary_artifact_hash(artifact: &DkgBoundaryArtifact) -> Result<B256> {
        let bytes = encode_boundary_artifact(artifact)
            .map_err(|error| eyre::eyre!("failed to encode DKG boundary artifact: {error}"))?;
        Ok(keccak256(bytes.as_ref()))
    }

    pub(super) fn cache_boundary_status(
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

    pub(super) fn clear_boundary_status_cache_inner(state: &mut State) {
        state.boundary_status_cache.clear();
        state.boundary_status_lru.clear();
    }

    pub(super) fn cached_boundary_status(
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

    pub(super) fn record_boundary_status(
        &self,
        parent_hash: B256,
        query_artifact_hash: B256,
        status: BoundaryStatus,
    ) {
        self.with_state(|state| {
            Self::cache_boundary_status(state, parent_hash, query_artifact_hash, status);
        });
    }

    pub(super) fn evict_boundary_status(&self, parent_hash: B256) -> bool {
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
    /// `with_state` call - no lock guard is ever held across an `.await`.
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
}
