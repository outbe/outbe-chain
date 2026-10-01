use super::PendingCore;
use crate::{AtomicWriteBatch, StorageError, StorageWriterHandle};
use std::sync::Arc;

/// Owns the association between one exact batch and its pending mutations.
/// Dropping this handle neither acknowledges nor rolls back pending data.
#[must_use = "retain this handle for ordered durable persistence and acknowledgement"]
pub struct PendingWrite {
    pub(super) core: Arc<PendingCore>,
    pub(super) writer: StorageWriterHandle,
    pub(super) generation: Option<u64>,
    pub(super) batch: AtomicWriteBatch,
}

impl PendingWrite {
    pub fn persist(&self) -> Result<PendingDurableReceipt<'_>, StorageError> {
        let commit = self.core.commit_gate.lock();
        if let Some(generation) = self.generation {
            if self.core.state.read().durable_order.front() != Some(&generation) {
                return Err(StorageError::invalid_argument(
                    "pending batch is not the next unacknowledged durable write",
                ));
            }
        }
        self.writer.apply_atomic(&self.batch)?;
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
        let mut state = self.write.core.state.write();
        state.durable_order.pop_front();
        state.retired.retain(|_, retired| *retired != generation);
        state.records.retain(|_, records| {
            records.retain(|_, record| record.generation != generation);
            !records.is_empty()
        });
    }
}
