//! Production ownership and finalization boundary for the compressed-entity tree.
//!
//! The manager deliberately keeps speculative candidates in memory while the
//! sole finalized tree lives in the CE-owned MDBX environment. Opening a
//! parent view takes one immutable MDBX read transaction; finalization applies
//! one exact candidate atomically before advancing retention and removing
//! cache entries.

use std::{
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};

use alloy_primitives::B256;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

use crate::{
    api::{AuthenticatedParentTree, AuthenticatedParentTreeFactory},
    persistence::{
        ApplyOutcome, CeMdbx, CeRetentionCursor, ExactParentIdentity, FinalizedMarker,
        PersistenceError,
    },
    staging::{
        AuthenticatedCatalogView, CandidateCache, CandidateCacheLimits, ProvisionalTreeBatch,
        PublicationOutcome, StagedTreeBatch, StagingError,
    },
    MdbxAuthenticatedTree,
};

/// PoC default for the small gate before the next finalized CE apply.
///
/// Production wiring may set a stricter value, but may not leave the gate
/// unbounded.
pub const DEFAULT_EXPORT_LEASE_OPEN_TIMEOUT: Duration = Duration::from_secs(2);
const EXPORT_LEASE_OFFER_DOMAIN: [u8; 4] = *b"OCEO";
const EXPORT_LEASE_OPEN_ACK_DOMAIN: [u8; 4] = *b"OCEA";
const EXPORT_LEASE_WIRE_LEN: usize = 4 + 8 + 32 + 4 + 8 + 32 + 32;

/// Observable state of one opaque CE export lease generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExportLeaseStatus {
    Pending,
    Opened,
    TimedOut,
}

/// Fixed-size offer sent to the exporter. Its fields are private so only the
/// tree owner can mint a generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExportLeaseOffer {
    generation: u64,
    challenge: B256,
    identity: ExactParentIdentity,
}

impl ExportLeaseOffer {
    #[must_use]
    pub const fn generation(self) -> u64 {
        self.generation
    }

    #[must_use]
    pub const fn identity(self) -> ExactParentIdentity {
        self.identity
    }

    #[must_use]
    pub fn encode_fixed(self) -> [u8; EXPORT_LEASE_WIRE_LEN] {
        encode_export_lease(
            EXPORT_LEASE_OFFER_DOMAIN,
            self.generation,
            self.challenge,
            self.identity,
        )
    }

    pub fn decode_fixed(bytes: &[u8]) -> Result<Self, TreeServiceError> {
        let (generation, challenge, identity) =
            decode_export_lease(bytes, EXPORT_LEASE_OFFER_DOMAIN)?;
        Ok(Self {
            generation,
            challenge,
            identity,
        })
    }

    /// Constructs an acknowledgement only from a view that actually opened the
    /// exact offered marker/root.
    pub fn confirm_open(
        self,
        view: &AuthenticatedCatalogView,
    ) -> Result<ExportLeaseOpenAck, TreeServiceError> {
        let actual = view.identity();
        if actual != self.identity {
            return Err(TreeServiceError::ExportLeaseIdentityMismatch {
                expected: self.identity,
                actual,
            });
        }
        Ok(ExportLeaseOpenAck {
            generation: self.generation,
            challenge: self.challenge,
            identity: self.identity,
        })
    }
}

/// Opaque acknowledgement minted only after an exact read-only view opens.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExportLeaseOpenAck {
    generation: u64,
    challenge: B256,
    identity: ExactParentIdentity,
}

impl ExportLeaseOpenAck {
    #[must_use]
    pub fn encode_fixed(self) -> [u8; EXPORT_LEASE_WIRE_LEN] {
        encode_export_lease(
            EXPORT_LEASE_OPEN_ACK_DOMAIN,
            self.generation,
            self.challenge,
            self.identity,
        )
    }

    fn decode_fixed(bytes: &[u8]) -> Result<Self, TreeServiceError> {
        let (generation, challenge, identity) =
            decode_export_lease(bytes, EXPORT_LEASE_OPEN_ACK_DOMAIN)?;
        Ok(Self {
            generation,
            challenge,
            identity,
        })
    }
}

fn encode_export_lease(
    domain: [u8; 4],
    generation: u64,
    challenge: B256,
    identity: ExactParentIdentity,
) -> [u8; EXPORT_LEASE_WIRE_LEN] {
    let mut bytes = [0_u8; EXPORT_LEASE_WIRE_LEN];
    bytes[..4].copy_from_slice(&domain);
    bytes[4..12].copy_from_slice(&generation.to_be_bytes());
    bytes[12..44].copy_from_slice(challenge.as_slice());
    bytes[44..48].copy_from_slice(&identity.commitment_scheme_version.to_be_bytes());
    bytes[48..56].copy_from_slice(&identity.block_number.to_be_bytes());
    bytes[56..88].copy_from_slice(identity.block_hash.as_slice());
    bytes[88..120].copy_from_slice(identity.root.as_slice());
    bytes
}

fn decode_export_lease(
    bytes: &[u8],
    expected_domain: [u8; 4],
) -> Result<(u64, B256, ExactParentIdentity), TreeServiceError> {
    if bytes.len() != EXPORT_LEASE_WIRE_LEN || bytes[..4] != expected_domain {
        return Err(TreeServiceError::MalformedExportLease);
    }
    let generation = u64::from_be_bytes(
        bytes[4..12]
            .try_into()
            .map_err(|_| TreeServiceError::MalformedExportLease)?,
    );
    if generation == 0 {
        return Err(TreeServiceError::MalformedExportLease);
    }
    let challenge = B256::from_slice(&bytes[12..44]);
    if challenge.is_zero() {
        return Err(TreeServiceError::MalformedExportLease);
    }
    let commitment_scheme_version = u32::from_be_bytes(
        bytes[44..48]
            .try_into()
            .map_err(|_| TreeServiceError::MalformedExportLease)?,
    );
    let block_number = u64::from_be_bytes(
        bytes[48..56]
            .try_into()
            .map_err(|_| TreeServiceError::MalformedExportLease)?,
    );
    Ok((
        generation,
        challenge,
        ExactParentIdentity {
            commitment_scheme_version,
            block_number,
            block_hash: B256::from_slice(&bytes[56..88]),
            root: B256::from_slice(&bytes[88..120]),
        },
    ))
}

#[derive(Clone, Copy, Debug)]
struct ActiveExportLease {
    offer: ExportLeaseOffer,
    deadline: Instant,
}

#[derive(Debug)]
struct ExportLeaseState {
    next_generation: u64,
    active: Option<ActiveExportLease>,
    last: Option<(ExportLeaseOffer, ExportLeaseStatus)>,
}

#[derive(Debug)]
struct ExportLeaseGate {
    timeout: Duration,
    state: Mutex<ExportLeaseState>,
    changed: Condvar,
}

impl ExportLeaseGate {
    fn new(timeout: Duration) -> Result<Self, TreeServiceError> {
        if timeout.is_zero() || timeout > DEFAULT_EXPORT_LEASE_OPEN_TIMEOUT {
            return Err(TreeServiceError::InvalidExportLeaseTimeout);
        }
        Ok(Self {
            timeout,
            state: Mutex::new(ExportLeaseState {
                next_generation: 1,
                active: None,
                last: None,
            }),
            changed: Condvar::new(),
        })
    }

    fn arm(
        &self,
        identity: ExactParentIdentity,
        challenge: B256,
    ) -> Result<ExportLeaseOffer, TreeServiceError> {
        if challenge.is_zero() {
            return Err(TreeServiceError::ZeroExportLeaseChallenge);
        }
        let mut state = self.lock()?;
        expire_if_due(&mut state, Instant::now());
        if let Some(active) = state.active {
            if active.offer.identity == identity && active.offer.challenge == challenge {
                return Ok(active.offer);
            }
            return Err(TreeServiceError::ConflictingExportLease {
                active: active.offer.identity,
                requested: identity,
            });
        }
        let generation = state.next_generation;
        state.next_generation = generation
            .checked_add(1)
            .ok_or(TreeServiceError::ExportLeaseGenerationOverflow)?;
        let deadline = Instant::now()
            .checked_add(self.timeout)
            .ok_or(TreeServiceError::InvalidExportLeaseTimeout)?;
        let offer = ExportLeaseOffer {
            generation,
            challenge,
            identity,
        };
        state.active = Some(ActiveExportLease { offer, deadline });
        state.last = Some((offer, ExportLeaseStatus::Pending));
        Ok(offer)
    }

    fn acknowledge(&self, ack: ExportLeaseOpenAck) -> Result<(), TreeServiceError> {
        let mut state = self.lock()?;
        expire_if_due(&mut state, Instant::now());
        let Some(active) = state.active else {
            if let Some((last, ExportLeaseStatus::Opened)) = state.last {
                if last.generation == ack.generation
                    && last.challenge == ack.challenge
                    && last.identity == ack.identity
                {
                    return Ok(());
                }
            }
            return Err(stale_export_lease(&state, ack.generation));
        };
        if active.offer.generation != ack.generation {
            return Err(stale_export_lease(&state, ack.generation));
        }
        if active.offer.identity != ack.identity {
            return Err(TreeServiceError::ExportLeaseIdentityMismatch {
                expected: active.offer.identity,
                actual: ack.identity,
            });
        }
        if active.offer.challenge != ack.challenge {
            return Err(TreeServiceError::ExportLeaseChallengeMismatch);
        }
        state.active = None;
        state.last = Some((active.offer, ExportLeaseStatus::Opened));
        self.changed.notify_all();
        Ok(())
    }

    fn status(&self, generation: u64) -> Result<ExportLeaseStatus, TreeServiceError> {
        let mut state = self.lock()?;
        expire_if_due(&mut state, Instant::now());
        if state
            .active
            .is_some_and(|active| active.offer.generation == generation)
        {
            return Ok(ExportLeaseStatus::Pending);
        }
        match state.last {
            Some((actual, status)) if actual.generation == generation => Ok(status),
            _ => Err(stale_export_lease(&state, generation)),
        }
    }

    fn wait_before_next_apply(&self, current: FinalizedMarker) -> Result<(), TreeServiceError> {
        let expected = exact_identity(current);
        let mut state = self.lock()?;
        loop {
            expire_if_due(&mut state, Instant::now());
            let Some(active) = state.active else {
                return Ok(());
            };
            if active.offer.identity != expected {
                return Err(TreeServiceError::ConflictingExportLease {
                    active: active.offer.identity,
                    requested: expected,
                });
            }
            let remaining = active.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                expire_if_due(&mut state, Instant::now());
                return Ok(());
            }
            let (next, _) = self
                .changed
                .wait_timeout(state, remaining)
                .map_err(|_| TreeServiceError::LockPoisoned("export lease gate"))?;
            state = next;
        }
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, ExportLeaseState>, TreeServiceError> {
        self.state
            .lock()
            .map_err(|_| TreeServiceError::LockPoisoned("export lease gate"))
    }
}

fn expire_if_due(state: &mut ExportLeaseState, now: Instant) {
    if let Some(active) = state.active {
        if now >= active.deadline {
            state.active = None;
            state.last = Some((active.offer, ExportLeaseStatus::TimedOut));
        }
    }
}

fn stale_export_lease(state: &ExportLeaseState, requested: u64) -> TreeServiceError {
    let actual = state
        .active
        .map(|active| active.offer.generation)
        .or_else(|| state.last.map(|(offer, _)| offer.generation));
    TreeServiceError::StaleExportLease { requested, actual }
}

fn exact_identity(marker: FinalizedMarker) -> ExactParentIdentity {
    ExactParentIdentity {
        commitment_scheme_version: marker.commitment_scheme_version,
        block_number: marker.height,
        block_hash: marker.block_hash,
        root: marker.new_root,
    }
}

/// Result of applying an exact finalized block to the CE materialization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FinalizedCandidateOutcome {
    Applied(FinalizedMarker),
    AlreadyApplied(FinalizedMarker),
}

impl FinalizedCandidateOutcome {
    #[must_use]
    pub const fn marker(self) -> FinalizedMarker {
        match self {
            Self::Applied(marker) | Self::AlreadyApplied(marker) => marker,
        }
    }
}

/// Explicitly-owned production tree service. There are no process globals and
/// no implicit cache limits: callers must supply benchmark-derived bounds.
#[derive(Debug)]
pub struct CompressedTreeService {
    db: Arc<CeMdbx>,
    candidates: Mutex<CandidateCache>,
    retention: CeRetentionCursor,
    finalization: Mutex<()>,
    export_lease: ExportLeaseGate,
}

impl CompressedTreeService {
    pub(crate) fn open_finalized_snapshot(
        &self,
    ) -> Result<Box<dyn crate::staging::FinalizedTreeSnapshot>, TreeServiceError> {
        self.db.open_snapshot().map_err(Into::into)
    }

    /// Takes ownership of the CE MDBX environment and seeds retention from its
    /// already-verified finalized marker.
    pub fn new(db: CeMdbx, limits: CandidateCacheLimits) -> Result<Self, TreeServiceError> {
        Self::new_with_export_lease_timeout(db, limits, DEFAULT_EXPORT_LEASE_OPEN_TIMEOUT)
    }

    pub fn new_with_export_lease_timeout(
        db: CeMdbx,
        limits: CandidateCacheLimits,
        export_lease_timeout: Duration,
    ) -> Result<Self, TreeServiceError> {
        let marker = db.marker()?;
        Ok(Self {
            db: Arc::new(db),
            candidates: Mutex::new(CandidateCache::new(limits)),
            retention: CeRetentionCursor::from_verified_marker(marker),
            finalization: Mutex::new(()),
            export_lease: ExportLeaseGate::new(export_lease_timeout)?,
        })
    }

    /// Arms one exact marker while holding the same serialization boundary as
    /// finalized apply. A missed marker is a local availability failure.
    pub fn arm_finalized_export(
        &self,
        required: FinalizedMarker,
        challenge: B256,
    ) -> Result<ExportLeaseOffer, TreeServiceError> {
        let _guard = self
            .finalization
            .lock()
            .map_err(|_| TreeServiceError::LockPoisoned("finalization boundary"))?;
        let current = self.db.marker()?;
        if current != required {
            return Err(TreeServiceError::ExportLeaseMarkerAdvanced {
                requested: required,
                current,
            });
        }
        self.export_lease.arm(exact_identity(required), challenge)
    }

    /// Returns the fixed identity of the CE environment owned by this service.
    ///
    /// Snapshot handoff exposes only the schema number from this identity; it
    /// never exposes the database path or a writer-capable handle.
    #[must_use]
    pub fn environment_identity(&self) -> &crate::persistence::EnvironmentIdentity {
        self.db.identity()
    }

    pub(crate) fn acknowledge_export_open(
        &self,
        ack: ExportLeaseOpenAck,
    ) -> Result<(), TreeServiceError> {
        self.export_lease.acknowledge(ack)
    }

    /// Accepts only the acknowledgement wire domain emitted after an exporter
    /// opened and verified the exact read-only snapshot.
    pub fn acknowledge_export_open_bytes(&self, bytes: &[u8]) -> Result<(), TreeServiceError> {
        self.acknowledge_export_open(ExportLeaseOpenAck::decode_fixed(bytes)?)
    }

    pub fn export_lease_status(
        &self,
        generation: u64,
    ) -> Result<ExportLeaseStatus, TreeServiceError> {
        self.export_lease.status(generation)
    }

    /// Opens one exact-parent tree session over one immutable MDBX snapshot.
    /// It checks every marker field against `identity` before it returns the
    /// session.
    pub fn open_parent(
        &self,
        identity: ExactParentIdentity,
    ) -> Result<Arc<dyn AuthenticatedParentTree>, TreeServiceError> {
        let tree = MdbxAuthenticatedTree::open(Arc::clone(&self.db), identity)
            .map_err(TreeServiceError::ParentView)?;
        Ok(Arc::new(tree))
    }

    /// Freezes and publishes a provisional batch under the executor-assigned
    /// block hash. Repeating the identical publication is a successful no-op.
    pub fn publish_candidate(
        &self,
        block_hash: B256,
        provisional: ProvisionalTreeBatch,
    ) -> Result<PublicationOutcome, TreeServiceError> {
        let _guard = self
            .finalization
            .lock()
            .map_err(|_| TreeServiceError::LockPoisoned("finalization boundary"))?;
        let current = self.db.marker()?;
        let batch = provisional.freeze(block_hash);
        if current == batch.marker(current.commitment_scheme_version) {
            return Ok(PublicationOutcome::AlreadyPublished);
        }
        if batch.block_number != current.height.saturating_add(1)
            || batch.parent_block_hash != current.block_hash
            || batch.parent_root() != current.new_root
        {
            return Err(TreeServiceError::NonContiguousPublication {
                current_marker: current,
                candidate_height: batch.block_number,
                block_hash,
                parent_block_hash: batch.parent_block_hash,
                parent_root: batch.parent_root(),
            });
        }
        self.candidates
            .lock()
            .map_err(|_| TreeServiceError::LockPoisoned("candidate cache"))?
            .publish(batch)
            .map_err(Into::into)
    }

    /// Fetches an immutable exact candidate. It rejects a matching hash at
    /// another height and does not silently reinterpret it.
    pub fn candidate(
        &self,
        block_number: u64,
        block_hash: B256,
    ) -> Result<Option<Arc<StagedTreeBatch>>, TreeServiceError> {
        let candidate = self
            .candidates
            .lock()
            .map_err(|_| TreeServiceError::LockPoisoned("candidate cache"))?
            .get(block_hash);
        match candidate {
            Some(candidate) if candidate.block_number != block_number => {
                Err(TreeServiceError::CandidateIdentityMismatch {
                    requested_height: block_number,
                    block_hash,
                    candidate_height: candidate.block_number,
                })
            }
            candidate => Ok(candidate),
        }
    }

    /// Drops one proposer candidate after a later payload guard rejects the
    /// assembled block. Finalization and removal share one serialization lock.
    pub fn discard_candidate(
        &self,
        block_number: u64,
        block_hash: B256,
    ) -> Result<bool, TreeServiceError> {
        let _guard = self
            .finalization
            .lock()
            .map_err(|_| TreeServiceError::LockPoisoned("finalization boundary"))?;
        let mut candidates = self
            .candidates
            .lock()
            .map_err(|_| TreeServiceError::LockPoisoned("candidate cache"))?;
        if let Some(candidate) = candidates.get(block_hash) {
            if candidate.block_number != block_number {
                return Err(TreeServiceError::CandidateIdentityMismatch {
                    requested_height: block_number,
                    block_hash,
                    candidate_height: candidate.block_number,
                });
            }
        }
        Ok(candidates.remove(block_hash).is_some())
    }

    /// Applies the exact candidate and only then advances retention and removes
    /// the winning/losing candidates at or below its height. Repeating a known
    /// completed finalization is idempotent even after cache removal.
    pub fn apply_finalized(
        &self,
        block_number: u64,
        block_hash: B256,
        authoritative_root: B256,
    ) -> Result<FinalizedCandidateOutcome, TreeServiceError> {
        let _guard = self
            .finalization
            .lock()
            .map_err(|_| TreeServiceError::LockPoisoned("finalization boundary"))?;
        let marker_before = self.db.marker()?;
        if block_number > marker_before.height {
            self.export_lease.wait_before_next_apply(marker_before)?;
        }

        let Some(candidate) = self.candidate(block_number, block_hash)? else {
            let marker = marker_before;
            if marker.height == block_number
                && marker.block_hash == block_hash
                && marker.new_root == authoritative_root
            {
                self.retention
                    .advance_or_confirm_after_known_commit(marker)?;
                self.remove_finalized_candidates(block_number)?;
                return Ok(FinalizedCandidateOutcome::AlreadyApplied(marker));
            }
            return Err(TreeServiceError::CandidateMissing {
                block_number,
                block_hash,
                current_marker: marker,
            });
        };

        if candidate.new_root() != authoritative_root {
            return Err(TreeServiceError::AuthoritativeRootMismatch {
                block_number,
                block_hash,
                candidate_root: candidate.new_root(),
                authoritative_root,
            });
        }

        let outcome = self.db.apply_finalized(&candidate)?;
        let finalized = match outcome {
            ApplyOutcome::Applied(marker) => FinalizedCandidateOutcome::Applied(marker),
            ApplyOutcome::AlreadyApplied(marker) => {
                FinalizedCandidateOutcome::AlreadyApplied(marker)
            }
        };

        // The cursor cannot move until MDBX has returned a known-successful
        // outcome. Cache removal is deliberately last.
        self.retention
            .advance_or_confirm_after_known_commit(finalized.marker())?;
        self.remove_finalized_candidates(block_number)?;
        Ok(finalized)
    }

    /// Explicit, idempotent cache cleanup policy used after a successful
    /// finalized apply.
    pub fn remove_finalized_candidates(&self, height: u64) -> Result<(), TreeServiceError> {
        self.candidates
            .lock()
            .map_err(|_| TreeServiceError::LockPoisoned("candidate cache"))?
            .remove_finalized(height);
        Ok(())
    }

    /// Restart never attempts to resurrect speculative state.
    pub fn discard_speculative_candidates(&self) -> Result<(), TreeServiceError> {
        self.candidates
            .lock()
            .map_err(|_| TreeServiceError::LockPoisoned("candidate cache"))?
            .discard_all_on_restart();
        Ok(())
    }

    pub fn finalized_marker(&self) -> Result<FinalizedMarker, TreeServiceError> {
        self.db.marker().map_err(Into::into)
    }

    #[must_use]
    pub fn retention_height(&self) -> u64 {
        self.retention.height()
    }
}

impl AuthenticatedParentTreeFactory for CompressedTreeService {
    fn open_parent(
        &self,
        identity: ExactParentIdentity,
    ) -> outbe_primitives::error::Result<Arc<dyn AuthenticatedParentTree>> {
        CompressedTreeService::open_parent(self, identity).map_err(|error| match error {
            TreeServiceError::ParentView(error) => error,
            other => crate::errors::CompressedEntitiesTreeUnavailableError::Service(other).into(),
        })
    }
}

#[derive(Debug, Error)]
pub enum TreeServiceError {
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
    #[error(transparent)]
    Staging(#[from] StagingError),
    #[error("candidate shard count mismatch: expected {expected}, got {actual}")]
    ShardCountMismatch { expected: u32, actual: u32 },
    #[error("unable to open exact compressed-tree parent: {0}")]
    ParentView(PrecompileError),
    #[error("compressed-tree {0} lock is poisoned")]
    LockPoisoned(&'static str),
    #[error("CE export lease timeout must be non-zero and no greater than the PoC bound")]
    InvalidExportLeaseTimeout,
    #[error("CE export lease generation overflow")]
    ExportLeaseGenerationOverflow,
    #[error("CE export lease challenge cannot be zero")]
    ZeroExportLeaseChallenge,
    #[error("CE export lease acknowledgement challenge does not match the active offer")]
    ExportLeaseChallengeMismatch,
    #[error("CE export lease fixed-size encoding is malformed")]
    MalformedExportLease,
    #[error(
        "CE export marker was missed: requested {requested:?}, current finalized marker {current:?}"
    )]
    ExportLeaseMarkerAdvanced {
        requested: FinalizedMarker,
        current: FinalizedMarker,
    },
    #[error("conflicting CE export lease: active {active:?}, requested {requested:?}")]
    ConflictingExportLease {
        active: ExactParentIdentity,
        requested: ExactParentIdentity,
    },
    #[error("stale CE export lease generation {requested}; current generation is {actual:?}")]
    StaleExportLease { requested: u64, actual: Option<u64> },
    #[error("CE export lease identity mismatch: expected {expected:?}, got {actual:?}")]
    ExportLeaseIdentityMismatch {
        expected: ExactParentIdentity,
        actual: ExactParentIdentity,
    },
    #[error(
        "candidate {block_hash} requested at height {requested_height}, stored at {candidate_height}"
    )]
    CandidateIdentityMismatch {
        requested_height: u64,
        block_hash: B256,
        candidate_height: u64,
    },
    #[error(
        "candidate for finalized block {block_number}/{block_hash} is missing; current marker is {current_marker:?}"
    )]
    CandidateMissing {
        block_number: u64,
        block_hash: B256,
        current_marker: FinalizedMarker,
    },
    #[error(
        "candidate {candidate_height}/{block_hash} does not extend current marker {current_marker:?}: parent {parent_block_hash}/{parent_root}"
    )]
    NonContiguousPublication {
        current_marker: FinalizedMarker,
        candidate_height: u64,
        block_hash: B256,
        parent_block_hash: B256,
        parent_root: B256,
    },
    #[error(
        "candidate {block_number}/{block_hash} root {candidate_root} differs from authoritative EVM root {authoritative_root}"
    )]
    AuthoritativeRootMismatch {
        block_number: u64,
        block_hash: B256,
        candidate_root: B256,
        authoritative_root: B256,
    },
}

// ADR-009's flat-namespace fixtures are replaced by ADR-010 catalog fixtures below.
#[cfg(test)]
mod tests;
