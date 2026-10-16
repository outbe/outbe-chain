//! Exhaustive, read-only CE validation with disposable external scratch.

mod bodies;
mod store;
mod trees;

pub use bodies::{CeBodyAudit, CeBodyAuditReport};

use super::{LeafValue, PersistenceError, TreeKey, TreeNamespace};
use alloy_primitives::B256;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug)]
pub struct CeAuditLimits {
    pub records_per_run: usize,
    pub merge_fan_in: usize,
}

impl Default for CeAuditLimits {
    fn default() -> Self {
        Self {
            records_per_run: 4096,
            merge_fan_in: 16,
        }
    }
}

/// An exclusively created operation directory. Only this directory is removed
/// on drop. Installed native files are never cleanup targets.
pub struct CeAuditWork {
    root: PathBuf,
    limits: CeAuditLimits,
}

impl CeAuditWork {
    pub fn create(path: impl AsRef<Path>, limits: CeAuditLimits) -> Result<Self, CeAuditError> {
        if limits.records_per_run == 0 || limits.merge_fan_in < 2 {
            return Err(CeAuditError::InvalidAuditWorkLimits);
        }
        let tree_record_size = std::mem::size_of::<(crate::smt::TreeKey, crate::smt::TreeLeaf)>();
        if limits
            .records_per_run
            .checked_mul(tree_record_size)
            .is_none_or(|bytes| bytes > isize::MAX as usize)
        {
            return Err(CeAuditError::TreeBufferSizeOverflow);
        }
        std::fs::create_dir(path.as_ref())?;
        Ok(Self {
            root: path.as_ref().to_path_buf(),
            limits,
        })
    }

    /// Compare complete fixed-width populations without retaining them in memory.
    /// `key_len` identifies the unique prefix; the remaining bytes are compared values.
    pub fn compare_records<const N: usize>(
        &self,
        key_len: usize,
        expected: impl IntoIterator<Item = Result<[u8; N], CeAuditError>>,
        actual: impl IntoIterator<Item = Result<[u8; N], CeAuditError>>,
    ) -> Result<(), CeAuditError> {
        let mut left = store::RecordSorter::new(self, key_len)?;
        let mut right = store::RecordSorter::new(self, key_len)?;
        for record in expected {
            left.push(record?)?;
        }
        for record in actual {
            right.push(record?)?;
        }
        let mut left = left.finish()?;
        let mut right = right.finish()?;
        loop {
            match (left.next().transpose()?, right.next().transpose()?) {
                (None, None) => return Ok(()),
                (Some(a), Some(b)) if a == b => {}
                _ => return Err(CeAuditError::RecordPopulationsDiffer),
            }
        }
    }
}

impl Drop for CeAuditWork {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CeAuditError {
    #[error("invalid CE audit data: {0}")]
    Invalid(String),
    #[error("invalid CE audit data: invalid audit work limits")]
    InvalidAuditWorkLimits,
    #[error("invalid CE audit data: tree buffer size overflow")]
    TreeBufferSizeOverflow,
    #[error("invalid CE audit data: record populations differ")]
    RecordPopulationsDiffer,
    #[error("invalid CE audit data: {0}")]
    Commitment(#[from] crate::CommitmentError),
    #[error("invalid CE audit data: {0}")]
    Collection(#[from] crate::collection::CollectionError),
    #[error("invalid CE audit data: {0}")]
    Smt(#[from] crate::smt::TreeError),
    #[error("invalid CE audit data: {0}")]
    Sharding(#[from] crate::sharding::ShardingError),
    #[error("invalid CE audit data: {0}")]
    PointReadService(#[from] crate::proof::PointReadServiceError),
    #[error("invalid CE audit data: body collection is missing from catalog")]
    MissingBodyCollectionInCatalog,
    #[error("invalid CE audit data: body count overflow")]
    BodyCountOverflow,
    #[error("invalid CE audit data: live CE leaves and primary bodies differ")]
    LiveLeavesAndPrimaryBodiesDiffer,
    #[error("invalid CE audit data: invalid sort dimensions or limits")]
    InvalidSortDimensionsOrLimits,
    #[error("invalid CE audit data: sort merge layout overflow")]
    SortMergeLayoutOverflow,
    #[error("invalid CE audit data: sort run count overflow")]
    SortRunCountOverflow,
    #[error("invalid CE audit data: cannot reserve sort buffer")]
    CannotReserveSortBuffer,
    #[error("invalid CE audit data: sort directory counter overflow")]
    SortDirectoryCounterOverflow,
    #[error("invalid CE audit data: sorter already failed")]
    SorterAlreadyFailed,
    #[error("invalid CE audit data: sort run counter overflow")]
    SortRunCounterOverflow,
    #[error("invalid CE audit data: sort count overflow")]
    SortCountOverflow,
    #[error("invalid CE audit data: sort fan-in overflow")]
    SortFanInOverflow,
    #[error("invalid CE audit data: sort pass overflow")]
    SortPassOverflow,
    #[error("invalid CE audit data: sort buffer size overflow")]
    SortBufferSizeOverflow,
    #[error("invalid CE audit data: sort buffer exceeds addressable memory")]
    SortBufferExceedsAddressableMemory,
    #[error("invalid CE audit data: sort run length overflow")]
    SortRunLengthOverflow,
    #[error("invalid CE audit data: sort width overflow")]
    SortWidthOverflow,
    #[error("invalid CE audit data: sort key width overflow")]
    SortKeyWidthOverflow,
    #[error("invalid CE audit data: too many sort records")]
    TooManySortRecords,
    #[error("invalid CE audit data: missing sort records")]
    MissingSortRecords,
    #[error("invalid CE audit data: sort run is not a regular file")]
    SortRunNotRegularFile,
    #[error("invalid CE audit data: invalid sort run header or length")]
    InvalidSortRunHeaderOrLength,
    #[error("invalid CE audit data: trailing sort run bytes")]
    TrailingSortRunBytes,
    #[error("invalid CE audit data: duplicate or unordered sort identity")]
    DuplicateOrUnorderedSortIdentity,
    #[error("invalid CE audit data: sort merge count overflow")]
    SortMergeCountOverflow,
    #[error("invalid CE audit data: cannot reserve sort readers")]
    CannotReserveSortReaders,
    #[error("invalid CE audit data: cannot reserve sort heads")]
    CannotReserveSortHeads,
    #[error("invalid CE audit data: sort record count overflow")]
    SortRecordCountOverflow,
    #[error("invalid CE audit data: audit scratch overlaps the source environment")]
    ScratchOverlapsSourceEnvironment,
    #[error("invalid CE audit data: create audit scratch database: {reason}")]
    CreateScratchDatabase { reason: String },
    #[error("invalid CE audit data: shard is outside its collection topology")]
    ShardOutsideCollectionTopology,
    #[error("invalid CE audit data: catalog entry has no materialized collection roots")]
    MissingMaterializedCollectionRoots,
    #[error("invalid CE audit data: collection roots lack a catalog leaf")]
    MissingCatalogLeafForCollectionRoots,
    #[error("invalid CE audit data: collection root differs from catalog leaf")]
    CollectionRootDiffersFromCatalogLeaf,
    #[error("invalid CE audit data: persisted {table} population differs from reconstruction")]
    PersistedTablePopulationDiffers { table: &'static str },
    #[error("invalid CE audit data: CE marker differs from required block identity")]
    MarkerDiffersFromRequiredIdentity,
    #[error("invalid CE audit data: genesis must use its native empty catalog and configured block identity")]
    InvalidGenesisCatalogOrIdentity,
    #[error("invalid CE audit data: sealed catalog root differs from required root")]
    SealedCatalogRootMismatch,
    #[error("invalid CE audit data: CE environment identity changed")]
    EnvironmentIdentityChanged,
    #[error("invalid CE audit data: unknown CE metadata record")]
    UnknownMetadataRecord,
    #[error("invalid CE audit data: cannot reserve tree leaf buffer")]
    CannotReserveTreeLeafBuffer,
    #[error("invalid CE audit data: leaf count overflow")]
    LeafCountOverflow,
    #[error("invalid CE audit data: rebuilt root differs for {namespace:?}")]
    RebuiltRootDiffers { namespace: TreeNamespace },
    #[error("invalid CE audit data: tree count overflow")]
    TreeCountOverflow,
    #[error("invalid CE audit data: leaf is stored in the wrong native shard")]
    LeafInWrongNativeShard,
    #[error(transparent)]
    Native(Box<PersistenceError>),
    #[error(transparent)]
    Database(#[from] reth_db::DatabaseError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl From<PersistenceError> for CeAuditError {
    fn from(error: PersistenceError) -> Self {
        Self::Native(Box::new(error))
    }
}

pub trait CeAuditVisitor {
    fn visit_leaf(
        &mut self,
        namespace: TreeNamespace,
        key: TreeKey,
        value: LeafValue,
    ) -> Result<(), CeAuditError>;
}

#[derive(Debug)]
pub struct CeAuditReport {
    pub sealed_root: B256,
    pub trees: u64,
    pub leaves: u64,
    pub peak_buffered_leaves: usize,
}

pub(super) use trees::audit;

#[cfg(all(test, feature = "snapshot-integration"))]
mod tests;
