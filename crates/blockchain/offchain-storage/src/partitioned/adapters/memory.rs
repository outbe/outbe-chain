//! In-memory datasource composition, kept outside partition orchestration.

use super::super::{PartitionDataSource, PartitionReadSource, PartitionedBatch, StorageScope};
use super::{CollectionDataSource, ScopeCatalog};
use crate::{MemoryStorage, StorageError, StorageReaderHandle};
use std::sync::Arc;

struct MemoryCatalog(Arc<MemoryStorage>);
impl ScopeCatalog for MemoryCatalog {
    fn namespaces(&self, scope: &StorageScope) -> Result<Vec<crate::Namespace>, StorageError> {
        super::codec::namespaces(self.0.namespace_names(), scope)
    }
    fn list_scopes(&self, domain: &str) -> Result<Vec<StorageScope>, StorageError> {
        super::codec::scopes(self.0.namespace_names(), domain)
    }
}

pub struct MemoryPartitionDataSource(CollectionDataSource);
impl Default for MemoryPartitionDataSource {
    fn default() -> Self {
        Self::new()
    }
}
impl MemoryPartitionDataSource {
    #[must_use]
    pub fn new() -> Self {
        let storage = Arc::new(MemoryStorage::new());
        Self(CollectionDataSource::new(
            storage.clone(),
            storage.clone(),
            Arc::new(MemoryCatalog(storage)),
        ))
    }
}
impl PartitionReadSource for MemoryPartitionDataSource {
    fn open_reader(
        &self,
        scope: &StorageScope,
    ) -> Result<Option<StorageReaderHandle>, StorageError> {
        self.0.open_reader(scope)
    }
    fn list_scopes(&self, domain: &str) -> Result<Vec<StorageScope>, StorageError> {
        self.0.list_scopes(domain)
    }
}
impl PartitionDataSource for MemoryPartitionDataSource {
    fn commit(&self, batch: &PartitionedBatch) -> Result<(), StorageError> {
        self.0.commit(batch)
    }
    fn verify_write_capability(&self) -> Result<(), StorageError> {
        self.0.verify_write_capability()
    }
}
