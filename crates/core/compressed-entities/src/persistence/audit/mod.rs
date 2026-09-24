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
/// on drop; installed native files are never cleanup targets.
pub struct CeAuditWork {
    root: PathBuf,
    limits: CeAuditLimits,
}

impl CeAuditWork {
    pub fn create(path: impl AsRef<Path>, limits: CeAuditLimits) -> Result<Self, CeAuditError> {
        if limits.records_per_run == 0 || limits.merge_fan_in < 2 {
            return Err(CeAuditError::Invalid("invalid audit work limits".into()));
        }
        let tree_record_size = std::mem::size_of::<(crate::smt::TreeKey, crate::smt::TreeLeaf)>();
        if limits
            .records_per_run
            .checked_mul(tree_record_size)
            .is_none_or(|bytes| bytes > isize::MAX as usize)
        {
            return Err(CeAuditError::Invalid("tree buffer size overflow".into()));
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
                _ => return Err(CeAuditError::Invalid("record populations differ".into())),
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
