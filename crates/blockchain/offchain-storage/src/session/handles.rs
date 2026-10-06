use super::Session;
use crate::{
    AtomicWriteBatch, Key, Namespace, ScanPage, ScanRequest, StorageError, StorageReader,
    StorageScope, StorageWriter, StoredValue,
};
use std::sync::Arc;

pub(super) struct SessionReader(pub(super) Arc<Session>);
impl StorageReader for SessionReader {
    fn get_record(
        &self,
        namespace: Namespace,
        key: &Key,
    ) -> Result<Option<StoredValue>, StorageError> {
        self.0
            .reader
            .as_ref()
            .expect("live session")
            .get_record(namespace, key)
    }
    fn get_records(
        &self,
        namespace: Namespace,
        keys: &[Key],
    ) -> Result<Vec<Option<StoredValue>>, StorageError> {
        self.0
            .reader
            .as_ref()
            .expect("live session")
            .get_records(namespace, keys)
    }
    fn storage_scope(
        &self,
        namespace: &Namespace,
        key: &Key,
    ) -> Result<Option<StorageScope>, StorageError> {
        self.0
            .reader
            .as_ref()
            .expect("live session")
            .storage_scope(namespace, key)
    }
    fn scan_prefix(
        &self,
        namespace: Namespace,
        request: ScanRequest<'_>,
    ) -> Result<ScanPage, StorageError> {
        self.0
            .reader
            .as_ref()
            .expect("live session")
            .scan_prefix(namespace, request)
    }
}

pub(super) struct BootstrapPermit(pub(super) Arc<parking_lot::RwLock<bool>>);
impl BootstrapPermit {
    pub(super) fn new() -> Self {
        Self(Arc::new(parking_lot::RwLock::new(true)))
    }
}
impl Drop for BootstrapPermit {
    fn drop(&mut self) {
        *self.0.write() = false;
    }
}

pub(super) struct SessionWriter(
    pub(super) Arc<Session>,
    pub(super) Option<Arc<parking_lot::RwLock<bool>>>,
);
impl SessionWriter {
    fn write<T>(
        &self,
        operation: impl FnOnce(&dyn StorageWriter) -> Result<T, StorageError>,
    ) -> Result<T, StorageError> {
        let permit = self.1.as_ref().map(|permit| permit.read());
        let authorized = permit.as_ref().map_or_else(
            || self.0.activated.load(std::sync::atomic::Ordering::Acquire),
            |permit| **permit,
        );
        if !authorized {
            return Err(StorageError::InvalidArgument(
                "storage write capability is not activated or has expired".into(),
            ));
        }
        operation(self.0.writer.as_ref().expect("live session").as_ref())
    }
}
impl StorageWriter for SessionWriter {
    fn verify_transaction_capability(&self) -> Result<(), StorageError> {
        self.write(|writer| writer.verify_transaction_capability())
    }
    fn apply_atomic(&self, batch: &AtomicWriteBatch) -> Result<(), StorageError> {
        self.write(|writer| writer.apply_atomic(batch))
    }
    fn apply_atomic_clearing(
        &self,
        batch: &AtomicWriteBatch,
        namespaces: &[Namespace],
    ) -> Result<(), StorageError> {
        self.write(|writer| writer.apply_atomic_clearing(batch, namespaces))
    }
}
