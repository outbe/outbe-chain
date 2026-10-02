//! Scoped collections over an injected transactional storage capability.

use super::super::{PartitionDataSource, PartitionReadSource, PartitionedBatch, StorageScope};
use crate::{
    AtomicWriteBatch, AtomicWriteOperation, Key, Namespace, ScanPage, ScanRequest, StorageError,
    StorageReader, StorageReaderHandle, StorageWriterHandle, StoredValue,
};
use std::sync::Arc;

pub trait ScopeCatalog: Send + Sync {
    fn list_scopes(&self, domain: &str) -> Result<Vec<StorageScope>, StorageError>;
    fn namespaces(&self, scope: &StorageScope) -> Result<Vec<Namespace>, StorageError>;
}

/// Reusable for transactional collection/keyspace stores; no backend enum.
pub struct CollectionDataSource {
    reader: StorageReaderHandle,
    writer: StorageWriterHandle,
    catalog: Arc<dyn ScopeCatalog>,
}
impl CollectionDataSource {
    #[must_use]
    pub fn new(
        reader: StorageReaderHandle,
        writer: StorageWriterHandle,
        catalog: Arc<dyn ScopeCatalog>,
    ) -> Self {
        Self {
            reader,
            writer,
            catalog,
        }
    }
}
impl PartitionReadSource for CollectionDataSource {
    fn open_reader(
        &self,
        scope: &StorageScope,
    ) -> Result<Option<StorageReaderHandle>, StorageError> {
        scope.validate()?;
        Ok(Some(Arc::new(CollectionReader {
            reader: self.reader.clone(),
            scope: scope.clone(),
        })))
    }
    fn list_scopes(&self, domain: &str) -> Result<Vec<StorageScope>, StorageError> {
        self.catalog.list_scopes(domain)
    }
}
impl PartitionDataSource for CollectionDataSource {
    fn verify_write_capability(&self) -> Result<(), StorageError> {
        self.writer.verify_transaction_capability()
    }
    fn commit(&self, batch: &PartitionedBatch) -> Result<(), StorageError> {
        batch.validate()?;
        let mut physical = AtomicWriteBatch::new();
        for op in &batch.operations {
            physical.push(match &op.operation {
                AtomicWriteOperation::Put {
                    namespace,
                    key,
                    record,
                } => AtomicWriteOperation::put_record(
                    super::codec::namespace(&op.scope, namespace)?,
                    key.clone(),
                    record.clone(),
                ),
                AtomicWriteOperation::Delete { namespace, key } => AtomicWriteOperation::delete(
                    super::codec::namespace(&op.scope, namespace)?,
                    key.clone(),
                ),
            });
        }
        let mut clear = vec![];
        for scope in &batch.retired_scopes {
            clear.extend(self.catalog.namespaces(scope)?);
            for op in &batch.operations {
                if &op.scope == scope {
                    let namespace = match &op.operation {
                        AtomicWriteOperation::Put { namespace, .. }
                        | AtomicWriteOperation::Delete { namespace, .. } => namespace,
                    };
                    clear.push(super::codec::namespace(scope, namespace)?);
                }
            }
        }
        clear.sort();
        clear.dedup();
        self.writer.apply_atomic_clearing(&physical, &clear)
    }
}
struct CollectionReader {
    reader: StorageReaderHandle,
    scope: StorageScope,
}
impl StorageReader for CollectionReader {
    fn get_record(
        &self,
        namespace: Namespace,
        key: &Key,
    ) -> Result<Option<StoredValue>, StorageError> {
        self.reader
            .get_record(super::codec::namespace(&self.scope, &namespace)?, key)
    }
    fn get_records(
        &self,
        namespace: Namespace,
        keys: &[Key],
    ) -> Result<Vec<Option<StoredValue>>, StorageError> {
        self.reader
            .get_records(super::codec::namespace(&self.scope, &namespace)?, keys)
    }
    fn scan_prefix(
        &self,
        namespace: Namespace,
        request: ScanRequest<'_>,
    ) -> Result<ScanPage, StorageError> {
        self.reader
            .scan_prefix(super::codec::namespace(&self.scope, &namespace)?, request)
    }
}
