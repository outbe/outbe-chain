//! Backend-neutral, synchronous storage for node-local off-chain data.

mod config;
mod day_databases;
mod day_directory;
mod memory;
mod mongo;
pub mod partitioned;
mod pending;
mod provider;
mod rocks;
mod rocks_codec;
mod session;
mod types;

pub use config::{RocksDbConfig, StorageBackend, StorageConfig};
pub use day_databases::DayDatabases;
pub use day_directory::DayDirectory;
pub use memory::MemoryStorage;
pub use mongo::{MongoStorage, MongoStorageConfig, MongoWriterLease};
pub use partitioned::{
    OwnerModuloPartition, PartitionContext, PartitionDataSource, PartitionId, PartitionReadSource,
    PartitionRouting, PartitionStrategy, PartitionedBatch, PartitionedStorage, ReadLocation,
    SharedPartition, StorageScope, WorldwideDayPartition,
};
pub use pending::{PendingDurableReceipt, PendingOverlayStorage, PendingWrite};
pub use provider::{StorageProvider, StorageReadSource};
pub use rocks::{RocksDbCloseWaiter, RocksDbReader, RocksDbStorage};
pub use session::{
    OpenedStorage, StorageCloseError, StorageCompletion, StorageLifecycle, StorageOwnershipGuard,
};
pub use types::{
    AtomicWriteBatch, AtomicWriteOperation, Key, Namespace, ScanEntry, ScanPage, ScanRequest,
    StorageError, StorageErrorKind, StorageMetadata, StoredValue, Value, MAX_ATOMIC_BATCH_BYTES,
    MAX_ATOMIC_BATCH_OPERATIONS, MAX_KEY_BYTES, MAX_METADATA_ENTRIES, MAX_METADATA_KEY_BYTES,
    MAX_METADATA_VALUE_BYTES, MAX_NAMESPACE_BYTES, MAX_SCAN_ENTRIES, MAX_SCAN_PAGE_VALUE_BYTES,
    MAX_VALUE_BYTES,
};

use std::sync::Arc;

/// Read authority for an off-chain storage adapter.
pub trait StorageReader: Send + Sync {
    /// Returns the value and optional metadata stored under one key.
    fn get_record(
        &self,
        namespace: Namespace,
        key: &Key,
    ) -> Result<Option<StoredValue>, StorageError>;

    /// Logical routing introspection for overlays; raw adapters may have no scope.
    fn storage_scope(
        &self,
        namespace: &Namespace,
        _key: &Key,
    ) -> Result<Option<StorageScope>, StorageError> {
        Ok(namespace.scope().cloned())
    }

    /// Returns only the stored value, intentionally discarding metadata.
    fn get(&self, namespace: Namespace, key: &Key) -> Result<Option<Value>, StorageError> {
        self.get_record(namespace, key)
            .map(|record| record.map(|record| record.value))
    }

    /// Returns records in the same order as the supplied keys.
    fn get_records(
        &self,
        namespace: Namespace,
        keys: &[Key],
    ) -> Result<Vec<Option<StoredValue>>, StorageError> {
        keys.iter()
            .map(|key| self.get_record(namespace.clone(), key))
            .collect()
    }

    /// Returns one bounded, ordered page of keys matching a raw-byte prefix.
    fn scan_prefix(
        &self,
        namespace: Namespace,
        request: ScanRequest<'_>,
    ) -> Result<ScanPage, StorageError>;
}

/// Write authority for an off-chain storage adapter.
pub trait StorageWriter: Send + Sync {
    /// Performs one acknowledged transaction-capability operation.
    ///
    /// Durable adapters override this to prove that a recovered connection can
    /// actually start transactions. In-memory/test adapters are transaction-capable
    /// by construction.
    fn verify_transaction_capability(&self) -> Result<(), StorageError> {
        Ok(())
    }

    /// Atomically inserts or completely replaces one value.
    fn put(&self, namespace: Namespace, key: &Key, value: &Value) -> Result<(), StorageError> {
        self.apply_atomic(&AtomicWriteBatch::from_operations(vec![
            AtomicWriteOperation::put(namespace, key.clone(), value.clone()),
        ]))
    }

    /// Deletes one value. Deleting an absent key succeeds.
    fn delete(&self, namespace: Namespace, key: &Key) -> Result<(), StorageError> {
        self.apply_atomic(&AtomicWriteBatch::from_operations(vec![
            AtomicWriteOperation::delete(namespace, key.clone()),
        ]))
    }

    /// Transactional collection adapters can remove whole keyspaces without row scans.
    fn apply_atomic_clearing(
        &self,
        batch: &AtomicWriteBatch,
        namespaces: &[Namespace],
    ) -> Result<(), StorageError> {
        if !namespaces.is_empty() {
            return Err(StorageError::InvalidArgument(
                "adapter cannot clear namespaces transactionally".into(),
            ));
        }
        self.apply_atomic(batch)
    }

    /// Applies every ordered mutation atomically or leaves the adapter unchanged.
    fn apply_atomic(&self, batch: &AtomicWriteBatch) -> Result<(), StorageError>;
}

/// Cloneable shared read authority.
pub type StorageReaderHandle = Arc<dyn StorageReader>;

/// Cloneable shared write authority.
pub type StorageWriterHandle = Arc<dyn StorageWriter>;
