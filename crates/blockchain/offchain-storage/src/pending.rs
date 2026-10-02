mod scan;
mod write;
pub use write::{PendingDurableReceipt, PendingWrite};

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use parking_lot::RwLock;

use crate::{
    AtomicWriteBatch, AtomicWriteOperation, Key, Namespace, ScanPage, ScanRequest, StorageError,
    StorageReader, StorageReaderHandle, StorageWriter, StorageWriterHandle, StoredValue,
};

#[derive(Clone, Debug)]
enum PendingRecord {
    Put(StoredValue),
    Delete,
}

#[derive(Clone, Debug)]
struct VersionedPendingRecord {
    generation: u64,
    record: PendingRecord,
}

#[derive(Default)]
struct PendingState {
    generation: u64,
    durable_order: VecDeque<u64>,
    records: BTreeMap<Namespace, BTreeMap<Key, VersionedPendingRecord>>,
    retired: BTreeMap<crate::StorageScope, u64>,
}

struct PendingCore {
    base: StorageReaderHandle,
    state: RwLock<PendingState>,
    commit_gate: parking_lot::Mutex<()>,
}

/// Process-local finalized mutations layered over one captured durable writer.
pub struct PendingOverlayStorage {
    core: Arc<PendingCore>,
    writer: StorageWriterHandle,
}

/// Reader-only pending view. Prepared journals use it. It has no durable persist.
pub(crate) struct PendingLogicalView {
    core: Arc<PendingCore>,
}

impl PendingOverlayStorage {
    #[must_use]
    pub fn new(base: StorageReaderHandle, writer: StorageWriterHandle) -> Self {
        Self {
            core: Arc::new(PendingCore {
                base,
                state: RwLock::new(PendingState::default()),
                commit_gate: parking_lot::Mutex::new(()),
            }),
            writer,
        }
    }

    /// Applies the exact batch and captures its pending ownership in one operation.
    pub fn stage(&self, batch: AtomicWriteBatch) -> Result<PendingWrite, StorageError> {
        let generation = self.core.apply_batch(&batch)?;
        Ok(PendingWrite {
            core: self.core.clone(),
            writer: self.writer.clone(),
            generation,
            batch,
        })
    }
}

impl PendingLogicalView {
    #[must_use]
    pub(crate) fn new(base: StorageReaderHandle) -> Self {
        Self {
            core: Arc::new(PendingCore {
                base,
                state: RwLock::new(PendingState::default()),
                commit_gate: parking_lot::Mutex::new(()),
            }),
        }
    }
}

impl PendingCore {
    fn is_retired(&self, namespace: &Namespace, key: &Key) -> Result<bool, StorageError> {
        if self.state.read().retired.is_empty() {
            return Ok(false);
        }
        let scope = self.base.storage_scope(namespace, key)?;
        Ok(scope.is_some_and(|scope| self.state.read().retired.contains_key(&scope)))
    }

    fn storage_scope(
        &self,
        namespace: &Namespace,
        key: &Key,
    ) -> Result<Option<crate::StorageScope>, StorageError> {
        self.base.storage_scope(namespace, key)
    }

    fn get_record(
        &self,
        namespace: Namespace,
        key: &Key,
    ) -> Result<Option<StoredValue>, StorageError> {
        if self.is_retired(&namespace, key)? {
            return Ok(None);
        }
        match self
            .state
            .read()
            .records
            .get(&namespace.logical())
            .and_then(|records| records.get(key))
            .cloned()
        {
            Some(VersionedPendingRecord {
                record: PendingRecord::Put(record),
                ..
            }) => Ok(Some(record)),
            Some(VersionedPendingRecord {
                record: PendingRecord::Delete,
                ..
            }) => Ok(None),
            None => self.base.get_record(namespace, key),
        }
    }

    fn get_records(
        &self,
        namespace: Namespace,
        keys: &[Key],
    ) -> Result<Vec<Option<StoredValue>>, StorageError> {
        keys.iter()
            .map(|key| self.get_record(namespace.clone(), key))
            .collect()
    }

    fn apply_batch(&self, batch: &AtomicWriteBatch) -> Result<Option<u64>, StorageError> {
        batch.validate()?;
        if batch.is_empty() {
            return Ok(None);
        }
        let mut state = self.state.write();
        let generation = state
            .generation
            .checked_add(1)
            .ok_or_else(|| StorageError::Corruption("pending generation overflow".to_owned()))?;
        state.generation = generation;
        state.durable_order.push_back(generation);
        for scope in batch.retired_scopes() {
            state.retired.insert(scope.clone(), generation);
        }
        for operation in batch.operations() {
            match operation {
                AtomicWriteOperation::Put {
                    namespace,
                    key,
                    record,
                } => {
                    state
                        .records
                        .entry(namespace.logical())
                        .or_default()
                        .insert(
                            key.clone(),
                            VersionedPendingRecord {
                                generation,
                                record: PendingRecord::Put(record.clone()),
                            },
                        );
                }
                AtomicWriteOperation::Delete { namespace, key } => {
                    state
                        .records
                        .entry(namespace.logical())
                        .or_default()
                        .insert(
                            key.clone(),
                            VersionedPendingRecord {
                                generation,
                                record: PendingRecord::Delete,
                            },
                        );
                }
            }
        }
        Ok(Some(generation))
    }
}

macro_rules! delegate_pending_reader {
    ($ty:ty) => {
        impl StorageReader for $ty {
            fn storage_scope(
                &self,
                namespace: &Namespace,
                key: &Key,
            ) -> Result<Option<crate::StorageScope>, StorageError> {
                self.core.storage_scope(namespace, key)
            }

            fn get_record(
                &self,
                namespace: Namespace,
                key: &Key,
            ) -> Result<Option<StoredValue>, StorageError> {
                self.core.get_record(namespace, key)
            }

            fn get_records(
                &self,
                namespace: Namespace,
                keys: &[Key],
            ) -> Result<Vec<Option<StoredValue>>, StorageError> {
                self.core.get_records(namespace, keys)
            }

            fn scan_prefix(
                &self,
                namespace: Namespace,
                request: ScanRequest<'_>,
            ) -> Result<ScanPage, StorageError> {
                self.core.scan_prefix(namespace, request)
            }
        }
    };
}

delegate_pending_reader!(PendingOverlayStorage);
delegate_pending_reader!(PendingLogicalView);

impl StorageWriter for PendingOverlayStorage {
    fn apply_atomic(&self, _batch: &AtomicWriteBatch) -> Result<(), StorageError> {
        Err(StorageError::invalid_argument(
            "durable pending overlay accepts writes only through stage",
        ))
    }
}

impl StorageWriter for PendingLogicalView {
    fn apply_atomic(&self, batch: &AtomicWriteBatch) -> Result<(), StorageError> {
        self.core.apply_batch(batch).map(|_| ())
    }
}
