use super::{
    tables, validate_root, Decoder, PersistenceError, FINALIZED_MARKER_ENCODED_LEN,
    LAST_APPLIED_KEY,
};
use alloy_primitives::B256;
use reth_db::transaction::DbTx;
use std::path::Path;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

/// Atomic finalized progress marker, encoded exactly as specified by ADR-008.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FinalizedMarker {
    pub commitment_scheme_version: u32,
    pub height: u64,
    pub block_hash: B256,
    pub parent_block_hash: B256,
    pub parent_root: B256,
    pub new_root: B256,
}

impl FinalizedMarker {
    #[must_use]
    pub fn encode(self) -> [u8; FINALIZED_MARKER_ENCODED_LEN] {
        let mut bytes = [0_u8; FINALIZED_MARKER_ENCODED_LEN];
        bytes[0..4].copy_from_slice(&self.commitment_scheme_version.to_be_bytes());
        bytes[4..12].copy_from_slice(&self.height.to_be_bytes());
        bytes[12..44].copy_from_slice(self.block_hash.as_slice());
        bytes[44..76].copy_from_slice(self.parent_block_hash.as_slice());
        bytes[76..108].copy_from_slice(self.parent_root.as_slice());
        bytes[108..140].copy_from_slice(self.new_root.as_slice());
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, PersistenceError> {
        if bytes.len() != FINALIZED_MARKER_ENCODED_LEN {
            return Err(PersistenceError::MalformedCodec {
                record: "last_applied",
                expected: "140 bytes",
                actual: bytes.len(),
            });
        }
        let mut decoder = Decoder::new(bytes, "last_applied");
        let marker = Self {
            commitment_scheme_version: decoder.u32()?,
            height: decoder.u64()?,
            block_hash: decoder.b256()?,
            parent_block_hash: decoder.b256()?,
            parent_root: decoder.b256()?,
            new_root: decoder.b256()?,
        };
        decoder.finish()?;
        validate_root(marker.parent_root)?;
        validate_root(marker.new_root)?;
        Ok(marker)
    }

    pub fn verify_exact_parent(
        self,
        required: ExactParentIdentity,
    ) -> Result<(), PersistenceError> {
        if self.commitment_scheme_version != required.commitment_scheme_version
            || self.height != required.block_number
            || self.block_hash != required.block_hash
            || self.new_root != required.root
        {
            return Err(PersistenceError::ExactParentMismatch {
                required,
                actual: self,
            });
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExactParentIdentity {
    pub commitment_scheme_version: u32,
    pub block_number: u64,
    pub block_hash: B256,
    /// The root read from the exact parent's authoritative EVM slot.
    pub root: B256,
}

/// The durable finalized EVM/finality checkpoint used during restart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DurableFinalizedCheckpoint {
    pub commitment_scheme_version: u32,
    pub height: u64,
    pub block_hash: B256,
    pub root: B256,
    pub parent_block_hash: B256,
    pub parent_root: B256,
    pub consensus_finalized_height: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestartClassification {
    Equal,
    Behind { first_missing: u64, target: u64 },
    Ahead,
    Conflict,
}

pub fn classify_restart(
    marker: FinalizedMarker,
    durable: DurableFinalizedCheckpoint,
) -> RestartClassification {
    if marker.commitment_scheme_version != durable.commitment_scheme_version {
        return RestartClassification::Conflict;
    }
    if marker.height > durable.height || marker.height > durable.consensus_finalized_height {
        return RestartClassification::Ahead;
    }
    if marker.height < durable.height {
        return RestartClassification::Behind {
            first_missing: marker.height.saturating_add(1),
            target: durable.height.min(durable.consensus_finalized_height),
        };
    }
    if marker.block_hash == durable.block_hash
        && marker.new_root == durable.root
        && marker.parent_block_hash == durable.parent_block_hash
        && marker.parent_root == durable.parent_root
    {
        RestartClassification::Equal
    } else {
        RestartClassification::Conflict
    }
}

/// Local pruning fence. It is seeded only from a root-verified marker and moves
/// only after a known-successful CE transaction.
#[derive(Debug)]
pub struct CeRetentionCursor(AtomicU64);

impl CeRetentionCursor {
    #[must_use]
    pub const fn from_verified_marker(marker: FinalizedMarker) -> Self {
        Self(AtomicU64::new(marker.height))
    }

    #[must_use]
    pub fn height(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }

    pub fn advance_after_known_commit(
        &self,
        previous: FinalizedMarker,
        committed: FinalizedMarker,
    ) -> Result<(), PersistenceError> {
        if committed.height != previous.height.saturating_add(1) || self.height() != previous.height
        {
            return Err(PersistenceError::RetentionAdvanceOutOfOrder {
                cursor: self.height(),
                previous: previous.height,
                committed: committed.height,
            });
        }
        self.0.store(committed.height, Ordering::Release);
        Ok(())
    }

    /// Advances by one after a known-successful CE apply, or confirms an
    /// already-observed commit. This covers retry after an MDBX commit whose
    /// first return path was ambiguous without weakening contiguous progress.
    pub fn advance_or_confirm_after_known_commit(
        &self,
        committed: FinalizedMarker,
    ) -> Result<(), PersistenceError> {
        let current = self.height();
        if current == committed.height {
            return Ok(());
        }
        if committed.height != current.saturating_add(1) {
            return Err(PersistenceError::RetentionAdvanceOutOfOrder {
                cursor: current,
                previous: current,
                committed: committed.height,
            });
        }
        self.0
            .compare_exchange(
                current,
                committed.height,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|actual| PersistenceError::RetentionAdvanceOutOfOrder {
                cursor: actual,
                previous: current,
                committed: committed.height,
            })?;
        Ok(())
    }
}

/// Crash-injection stages for the finalized persistence/ACK boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FinalizationStage {
    Delivered,
    MarshalDurable,
    RethFinalized,
    RethPersisted,
    ProviderVerified,
    CeCommitUnknown,
    CeCommitted,
    RetentionAdvanced,
    CacheRemoved,
    MarshalAcknowledged,
}

impl FinalizationStage {
    #[must_use]
    pub fn marshal_ack_allowed(self) -> bool {
        matches!(self, Self::CacheRemoved | Self::MarshalAcknowledged)
    }

    #[must_use]
    pub fn restart_requires_marker_classification(self) -> bool {
        matches!(self, Self::CeCommitUnknown)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyOutcome {
    Applied(FinalizedMarker),
    AlreadyApplied(FinalizedMarker),
}

pub(super) fn read_marker<T: DbTx>(
    tx: &T,
    path: &Path,
) -> Result<FinalizedMarker, PersistenceError> {
    let bytes = tx
        .get::<tables::CeMetadata>(LAST_APPLIED_KEY.to_vec())
        .map_err(|error| PersistenceError::Database {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?
        .ok_or(PersistenceError::MissingFinalizedMarker)?;
    FinalizedMarker::decode(&bytes)
}
