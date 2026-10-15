//! Off-chain storage error types.

pub use crate::session::StorageCloseError;
pub use crate::types::{StorageError, StorageErrorKind};

/// Typed corruption causes detected during partitioned prefix scans.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PartitionScanCorruptionError {
    #[error("duplicate entity key across partitions")]
    DuplicateEntityKeyAcrossPartitions,
    #[error("partition record exceeds page bound")]
    PartitionRecordExceedsPageBound,
    #[error("partition scan exceeds entry bound")]
    PartitionScanExceedsEntryBound,
    #[error("partition scan violates prefix or ordering")]
    PartitionScanViolatesPrefixOrOrdering,
    #[error("partition scan exceeds page byte bound")]
    PartitionScanExceedsPageByteBound,
    #[error("partition scan continuation is not its last returned key")]
    PartitionScanContinuationNotLastReturnedKey,
}

impl From<PartitionScanCorruptionError> for StorageError {
    fn from(error: PartitionScanCorruptionError) -> Self {
        Self::Corruption(error.to_string())
    }
}
