//! RocksDB datasource. Database ownership and cross-partition recovery live here.

mod journal;
mod layout;
mod read_view;

use super::super::{
    PartitionDataSource, PartitionId, PartitionReadSource, PartitionedBatch, StorageScope,
};
use crate::{
    AtomicWriteBatch, Namespace, RocksDbCloseWaiter, RocksDbStorage, StorageError,
    StorageReaderHandle, StorageWriter,
};
use parking_lot::Mutex;
pub use read_view::RocksPartitionReadView;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

pub struct RocksPartitionDataSource {
    root: PathBuf,
    handles: Mutex<BTreeMap<StorageScope, Arc<RocksDbStorage>>>,
    commit_gate: Mutex<()>,
    pending: Mutex<Option<PartitionedBatch>>,
    validation_pending: AtomicBool,
    // Drop system ownership last, after all partition primaries.
    system: Arc<RocksDbStorage>,
}
impl RocksPartitionDataSource {
    pub fn open(root: &Path) -> Result<Self, StorageError> {
        layout::reject_legacy(root)?;
        let path = root.join("system/shared");
        std::fs::create_dir_all(&path).map_err(StorageError::unavailable)?;
        let source = Self {
            root: root.to_owned(),
            handles: Mutex::new(BTreeMap::new()),
            commit_gate: Mutex::new(()),
            pending: Mutex::new(None),
            validation_pending: AtomicBool::new(false),
            system: Arc::new(RocksDbStorage::open(path)?),
        };
        let pending = source.load_journal()?;
        source
            .validation_pending
            .store(pending.is_some(), Ordering::Release);
        *source.pending.lock() = pending;
        Ok(source)
    }
    /// Complete replay only after the owner has validated the prepared checkpoint.
    pub fn complete_recovery(&self) -> Result<(), StorageError> {
        let _guard = self.commit_gate.lock();
        let batch = self.pending.lock().clone();
        if let Some(batch) = batch {
            self.apply_prepared(&batch)?;
            self.clear_journal()?;
            *self.pending.lock() = None;
        }
        self.validation_pending.store(false, Ordering::Release);
        Ok(())
    }
    pub fn close_waiter(&self) -> RocksDbCloseWaiter {
        self.system.close_waiter()
    }
    fn writer(&self, scope: &StorageScope) -> Result<Arc<RocksDbStorage>, StorageError> {
        if scope == &StorageScope::shared("system")? {
            return Ok(self.system.clone());
        }
        let mut handles = self.handles.lock();
        if let Some(storage) = handles.get(scope) {
            return Ok(storage.clone());
        }
        let path = self.root.join(layout::relative_path(scope));
        std::fs::create_dir_all(&path).map_err(StorageError::unavailable)?;
        let storage = Arc::new(RocksDbStorage::open(path)?);
        handles.insert(scope.clone(), storage.clone());
        Ok(storage)
    }
    fn apply_prepared(&self, batch: &PartitionedBatch) -> Result<(), StorageError> {
        // Commit the system scope last. It contains the authoritative checkpoint.
        let mut groups: BTreeMap<StorageScope, AtomicWriteBatch> = BTreeMap::new();
        for op in &batch.operations {
            groups
                .entry(op.scope.clone())
                .or_default()
                .push(op.operation.clone());
        }
        for scope in &batch.retired_scopes {
            groups.remove(scope);
        }
        let system = groups.remove(&StorageScope::shared("system")?);
        for (scope, operations) in groups {
            self.writer(&scope)?.apply_atomic(&operations)?;
        }
        for scope in &batch.retired_scopes {
            self.handles.lock().remove(scope);
            let path = self.root.join(layout::relative_path(scope));
            match std::fs::remove_dir_all(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(StorageError::unavailable(e)),
            }
            if let Some(parent) = path.parent().filter(|parent| parent.exists()) {
                std::fs::File::open(parent)
                    .and_then(|file| file.sync_all())
                    .map_err(StorageError::unavailable)?;
            }
        }
        if let Some(batch) = system {
            self.system.apply_atomic(&batch)?;
        }
        Ok(())
    }
}
impl PartitionReadSource for RocksPartitionDataSource {
    fn open_reader(
        &self,
        scope: &StorageScope,
    ) -> Result<Option<StorageReaderHandle>, StorageError> {
        scope.validate()?;
        let pending = self.pending.lock().clone();
        if pending
            .as_ref()
            .is_some_and(|batch| batch.retired_scopes.contains(scope))
        {
            return Ok(None);
        }
        let mut operations = AtomicWriteBatch::new();
        if let Some(batch) = &pending {
            operations.extend(
                batch
                    .operations
                    .iter()
                    .filter(|op| &op.scope == scope)
                    .map(|op| op.operation.clone()),
            );
        }
        let base: StorageReaderHandle = if self
            .root
            .join(layout::relative_path(scope))
            .join("CURRENT")
            .is_file()
        {
            self.writer(scope)?
        } else if operations.is_empty() {
            return Ok(None);
        } else {
            Arc::new(crate::MemoryStorage::new())
        };
        if operations.is_empty() {
            return Ok(Some(base));
        }
        let view = Arc::new(crate::PendingOverlayStorage::new(base));
        view.apply_atomic(&operations)?;
        Ok(Some(view))
    }
    fn list_scopes(&self, domain: &str) -> Result<Vec<StorageScope>, StorageError> {
        Namespace::new(domain)?;
        let mut scopes = layout::enumerate(&self.root)?;
        if let Some(batch) = self.pending.lock().as_ref() {
            scopes.extend(batch.operations.iter().map(|op| op.scope.clone()));
            scopes.retain(|scope| !batch.retired_scopes.contains(scope));
        }
        scopes.retain(|scope| scope.domain == domain);
        scopes.sort();
        scopes.dedup();
        Ok(scopes)
    }
}
impl PartitionDataSource for RocksPartitionDataSource {
    fn verify_write_capability(&self) -> Result<(), StorageError> {
        self.system.verify_transaction_capability()
    }
    fn commit(&self, batch: &PartitionedBatch) -> Result<(), StorageError> {
        batch.validate()?;
        let _guard = self.commit_gate.lock();
        if self.validation_pending.load(Ordering::Acquire) {
            return Err(StorageError::InvalidArgument(
                "validate prepared checkpoint and complete recovery before writing".into(),
            ));
        }
        let pending = self.pending.lock().clone();
        if let Some(prepared) = pending {
            self.apply_prepared(&prepared)?;
            self.clear_journal()?;
            *self.pending.lock() = None;
        }
        self.save_journal(batch)?;
        *self.pending.lock() = Some(batch.clone());
        self.apply_prepared(batch)?;
        self.clear_journal()?;
        *self.pending.lock() = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
