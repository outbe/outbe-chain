use super::PendingShared;
use crate::{AtomicWriteBatch, StorageError, StorageWriter};
use std::sync::Arc;

/// Owns the association between one exact batch and its pending mutations.
/// Dropping this handle neither acknowledges nor rolls back pending data.
#[must_use = "retain this handle for ordered durable persistence and acknowledgement"]
pub struct PendingWrite {
    pub(super) shared: Arc<PendingShared>,
    pub(super) generation: Option<u64>,
    pub(super) batch: AtomicWriteBatch,
}

impl PendingWrite {
    pub fn persist(
        &self,
        writer: &dyn StorageWriter,
    ) -> Result<PendingDurableReceipt<'_>, StorageError> {
        let commit = self.shared.commit_gate.lock();
        if let Some(generation) = self.generation {
            if self.shared.state.read().durable_order.front() != Some(&generation) {
                return Err(StorageError::invalid_argument(
                    "pending batch is not the next unacknowledged durable write",
                ));
            }
        }
        writer.apply_atomic(&self.batch)?;
        Ok(PendingDurableReceipt {
            write: self,
            _commit: commit,
        })
    }
}

/// Proof of successful persistence, bound to the handle that supplied the batch.
/// The caller decides whether policy such as its deadline permits acknowledgement.
#[must_use = "acknowledge after policy validation, or drop to retain pending for retry"]
pub struct PendingDurableReceipt<'a> {
    write: &'a PendingWrite,
    _commit: parking_lot::MutexGuard<'a, ()>,
}

impl PendingDurableReceipt<'_> {
    pub fn acknowledge(self) {
        let Some(generation) = self.write.generation else {
            return;
        };
        let mut state = self.write.shared.state.write();
        state.durable_order.pop_front();
        state.retired.retain(|_, retired| *retired != generation);
        state.records.retain(|_, records| {
            records.retain(|_, record| record.generation != generation);
            !records.is_empty()
        });
    }
}
