mod write;
pub use write::{PendingDurableReceipt, PendingWrite};

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use parking_lot::RwLock;

use crate::{
    AtomicWriteBatch, AtomicWriteOperation, Key, Namespace, ScanEntry, ScanPage, ScanRequest,
    StorageError, StorageReader, StorageReaderHandle, StorageWriter, StorageWriterHandle,
    StoredValue, MAX_SCAN_ENTRIES, MAX_SCAN_PAGE_VALUE_BYTES,
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

    fn scan_prefix(
        &self,
        namespace: Namespace,
        request: ScanRequest<'_>,
    ) -> Result<ScanPage, StorageError> {
        request.validate()?;
        let prefix = request.prefix().to_vec();
        let requested_after = request.after().cloned();
        let limit = request.limit();
        let pending = self
            .state
            .read()
            .records
            .get(&namespace.logical())
            .map(|records| {
                records
                    .iter()
                    .filter(|(key, _)| {
                        key.as_bytes().starts_with(&prefix)
                            && requested_after.as_ref().is_none_or(|after| *key > after)
                    })
                    .map(|(key, record)| (key.clone(), record.record.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let mut pending_index = 0;
        let mut base_after = requested_after;
        let mut base_entries = VecDeque::new();
        let mut base_exhausted = false;
        let mut entries = Vec::new();
        let mut value_bytes = 0_usize;
        let mut has_more = false;

        loop {
            if base_entries.is_empty() && !base_exhausted {
                let page = self.base.scan_prefix(
                    namespace.clone(),
                    ScanRequest::new(&prefix, base_after.as_ref(), MAX_SCAN_ENTRIES)?,
                )?;
                if page.entries.is_empty() {
                    if page.next_after.is_some() {
                        return Err(StorageError::Corruption(
                            "base scan returned an empty page with a continuation".to_owned(),
                        ));
                    }
                    base_exhausted = true;
                } else {
                    base_exhausted = page.next_after.is_none();
                    if let Some(next_after) = page.next_after {
                        base_after = Some(next_after);
                    }
                    base_entries.extend(page.entries);
                }
            }

            let base_key = base_entries.front().map(|entry| &entry.key);
            let pending_key = pending.get(pending_index).map(|(key, _)| key);
            let candidate = match (base_key, pending_key) {
                (None, None) => break,
                (Some(_), None) => base_entries.pop_front(),
                (None, Some(_)) => {
                    let (key, record) = &pending[pending_index];
                    pending_index += 1;
                    pending_scan_entry(key, record)
                }
                (Some(base_key), Some(pending_key)) => match base_key.cmp(pending_key) {
                    std::cmp::Ordering::Less => base_entries.pop_front(),
                    std::cmp::Ordering::Equal => {
                        base_entries.pop_front();
                        let (key, record) = &pending[pending_index];
                        pending_index += 1;
                        pending_scan_entry(key, record)
                    }
                    std::cmp::Ordering::Greater => {
                        let (key, record) = &pending[pending_index];
                        pending_index += 1;
                        pending_scan_entry(key, record)
                    }
                },
            };
            let Some(candidate) = candidate else {
                continue;
            };
            if self.is_retired(&namespace, &candidate.key)? {
                continue;
            }
            let candidate_bytes = candidate.value.as_bytes().len()
                + candidate
                    .metadata
                    .as_ref()
                    .map_or(0, |metadata| metadata.encoded_len());
            if entries.len() == limit
                || value_bytes.saturating_add(candidate_bytes) > MAX_SCAN_PAGE_VALUE_BYTES
            {
                has_more = true;
                break;
            }
            value_bytes += candidate_bytes;
            entries.push(candidate);
        }

        let next_after = if has_more {
            Some(
                entries
                    .last()
                    .ok_or_else(|| {
                        StorageError::Corruption(
                            "stored record exceeds the scan page byte bound".to_owned(),
                        )
                    })?
                    .key
                    .clone(),
            )
        } else {
            None
        };
        Ok(ScanPage {
            entries,
            next_after,
        })
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

fn pending_scan_entry(key: &Key, record: &PendingRecord) -> Option<ScanEntry> {
    match record {
        PendingRecord::Put(record) => Some(ScanEntry {
            key: key.clone(),
            value: record.value.clone(),
            metadata: record.metadata.clone(),
        }),
        PendingRecord::Delete => None,
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
