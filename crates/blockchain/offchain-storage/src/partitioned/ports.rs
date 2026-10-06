//! Ports implemented by physical adapters and domain-owned routing.

use super::{PartitionedBatch, StorageScope};
use crate::{Key, Namespace, ScanRequest, StorageError, StorageReaderHandle};

pub trait PartitionReadSource: Send + Sync {
    /// Missing partitions stay missing. This call must never initialize a database.
    fn open_reader(
        &self,
        scope: &StorageScope,
    ) -> Result<Option<StorageReaderHandle>, StorageError>;
    /// Enumerate physical partitions, including records absent from derived indexes.
    fn list_scopes(&self, domain: &str) -> Result<Vec<StorageScope>, StorageError>;
}

pub trait PartitionDataSource: PartitionReadSource {
    /// Acknowledges only after every partition and the commit marker are durable.
    /// Recovery and transaction implementation belong to the adapter.
    fn commit(&self, batch: &PartitionedBatch) -> Result<(), StorageError>;
    fn verify_write_capability(&self) -> Result<(), StorageError>;
}

#[derive(Clone)]
pub struct ReadLocation {
    pub scope: StorageScope,
    /// Index-selected records must exist. Absence is corruption, not a miss.
    pub require_present: bool,
}

/// Domain-owned routing. Storage has no knowledge of entity names or ID formats.
pub trait PartitionRouting: Send + Sync {
    /// Bulk lookup lets locator-backed rules use one indexed datasource read.
    fn points(
        &self,
        namespace: &Namespace,
        keys: &[Key],
        source: &dyn PartitionReadSource,
    ) -> Result<Vec<Option<ReadLocation>>, StorageError> {
        keys.iter()
            .map(|key| self.point(namespace, key, source))
            .collect()
    }

    fn point(
        &self,
        namespace: &Namespace,
        key: &Key,
        source: &dyn PartitionReadSource,
    ) -> Result<Option<ReadLocation>, StorageError>;
    fn scan(
        &self,
        namespace: &Namespace,
        request: ScanRequest<'_>,
        source: &dyn PartitionReadSource,
    ) -> Result<Vec<StorageScope>, StorageError>;
}
