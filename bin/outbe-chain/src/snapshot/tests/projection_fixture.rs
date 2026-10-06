//! Scoped projection fixture shared by snapshot scenarios.
use outbe_offchain_storage::partitioned::adapters::{
    RocksPartitionDataSource, RocksPartitionReadView,
};
use outbe_offchain_storage::{
    AtomicWriteBatch, Key, Namespace, PartitionedStorage, ScanPage, ScanRequest, StorageError,
    StorageReader, StorageReaderHandle, StorageWriter, StoredValue,
};
use std::{path::Path, sync::Arc};
pub(super) struct PartitionFixtureStore(PartitionedStorage);
impl PartitionFixtureStore {
    pub(super) fn open(root: &Path) -> Result<Self, StorageError> {
        let source = Arc::new(RocksPartitionDataSource::open(root)?);
        source.complete_recovery()?;
        Ok(Self(PartitionedStorage::new(
            source,
            outbe_offchain_data::entity_partition_routing()?,
        )))
    }
}
impl StorageReader for PartitionFixtureStore {
    fn storage_scope(
        &self,
        namespace: &Namespace,
        key: &Key,
    ) -> Result<Option<outbe_offchain_storage::StorageScope>, StorageError> {
        self.0.storage_scope(namespace, key)
    }
    fn get_record(
        &self,
        namespace: Namespace,
        key: &Key,
    ) -> Result<Option<StoredValue>, StorageError> {
        self.0.get_record(namespace, key)
    }
    fn scan_prefix(
        &self,
        namespace: Namespace,
        request: ScanRequest<'_>,
    ) -> Result<ScanPage, StorageError> {
        self.0.scan_prefix(namespace, request)
    }
}
impl StorageWriter for PartitionFixtureStore {
    fn apply_atomic(&self, batch: &AtomicWriteBatch) -> Result<(), StorageError> {
        self.0.apply_atomic(batch)
    }
}
pub(super) struct PartitionFixtureReader(StorageReaderHandle);
impl PartitionFixtureReader {
    pub(super) fn open(root: &Path, scratch: &Path) -> Result<Self, StorageError> {
        Ok(Self(Arc::new(PartitionedStorage::read_only(
            Arc::new(RocksPartitionReadView::open(root, scratch)?),
            outbe_offchain_data::entity_partition_routing()?,
        ))))
    }
}
impl StorageReader for PartitionFixtureReader {
    fn storage_scope(
        &self,
        namespace: &Namespace,
        key: &Key,
    ) -> Result<Option<outbe_offchain_storage::StorageScope>, StorageError> {
        self.0.storage_scope(namespace, key)
    }
    fn get_record(
        &self,
        namespace: Namespace,
        key: &Key,
    ) -> Result<Option<StoredValue>, StorageError> {
        self.0.get_record(namespace, key)
    }
    fn scan_prefix(
        &self,
        namespace: Namespace,
        request: ScanRequest<'_>,
    ) -> Result<ScanPage, StorageError> {
        self.0.scan_prefix(namespace, request)
    }
}
