//! Backend-neutral, synchronous storage for node-local off-chain data.

mod memory;
mod mongo;
mod types;

pub use memory::MemoryStorage;
pub use mongo::{MongoStorage, MongoStorageConfig};
pub use types::{
    Key, Namespace, ScanEntry, ScanPage, ScanRequest, StorageError, StorageErrorKind, Value,
    MAX_KEY_BYTES, MAX_NAMESPACE_BYTES, MAX_SCAN_ENTRIES, MAX_SCAN_PAGE_VALUE_BYTES,
    MAX_VALUE_BYTES,
};

use std::sync::Arc;

/// Read authority for an off-chain storage adapter.
pub trait StorageReader: Send + Sync {
    /// Returns the value stored under `namespace` and `key`, if present.
    fn get(&self, namespace: Namespace, key: &Key) -> Result<Option<Value>, StorageError>;

    /// Returns one bounded, ordered page of keys matching a raw-byte prefix.
    fn scan_prefix(
        &self,
        namespace: Namespace,
        request: ScanRequest<'_>,
    ) -> Result<ScanPage, StorageError>;
}

/// Write authority for an off-chain storage adapter.
pub trait StorageWriter: Send + Sync {
    /// Atomically inserts or completely replaces one value.
    fn put(&self, namespace: Namespace, key: &Key, value: &Value) -> Result<(), StorageError>;

    /// Deletes one value. Deleting an absent key succeeds.
    fn delete(&self, namespace: Namespace, key: &Key) -> Result<(), StorageError>;
}

/// Cloneable shared read authority.
pub type StorageReaderHandle = Arc<dyn StorageReader>;

/// Cloneable shared write authority.
pub type StorageWriterHandle = Arc<dyn StorageWriter>;
