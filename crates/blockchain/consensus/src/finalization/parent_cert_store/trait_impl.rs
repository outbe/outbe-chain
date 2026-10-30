use super::*;

#[cfg(test)]
use std::sync::atomic::AtomicUsize;

#[cfg(test)]
#[derive(Default)]
pub(crate) struct StoreFaultInjection {
    fail_put_finalization: AtomicUsize,
    fail_prune_height: AtomicUsize,
    fail_prune_cn: AtomicUsize,
}

#[cfg(test)]
fn consume_fault(counter: &AtomicUsize) -> bool {
    counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_sub(1)
        })
        .is_ok()
}

#[cfg(test)]
fn injected_store_failure() -> ParentProofStoreError {
    ParentProofStoreError::Db {
        path: PathBuf::new(),
        source: DatabaseError::Other("injected test store failure".to_owned()),
    }
}

#[cfg(test)]
impl StoreFaultInjection {
    fn check(&self, counter: &AtomicUsize) -> Result<(), ParentProofStoreError> {
        if consume_fault(counter) {
            Err(injected_store_failure())
        } else {
            Ok(())
        }
    }

    pub(super) fn check_put_finalization(&self) -> Result<(), ParentProofStoreError> {
        self.check(&self.fail_put_finalization)
    }

    pub(super) fn check_prune_height(&self) -> Result<(), ParentProofStoreError> {
        self.check(&self.fail_prune_height)
    }

    pub(super) fn check_prune_cn(&self) -> Result<(), ParentProofStoreError> {
        self.check(&self.fail_prune_cn)
    }
}

#[cfg(test)]
impl FinalizedParentCertStore {
    pub(crate) fn fail_next_put_finalization(&self, count: usize) {
        self.faults
            .fail_put_finalization
            .store(count, Ordering::Relaxed);
    }

    pub(crate) fn fail_next_prune_below_height(&self, count: usize) {
        self.faults
            .fail_prune_height
            .store(count, Ordering::Relaxed);
    }

    pub(crate) fn fail_next_prune_cn(&self, count: usize) {
        self.faults.fail_prune_cn.store(count, Ordering::Relaxed);
    }
}

impl CertifiedParentProofStore for FinalizedParentCertStore {
    fn put_finalization(
        &self,
        record: CertifiedParentProofRecord,
    ) -> Result<(), ParentProofStoreError> {
        #[cfg(test)]
        self.faults.check_put_finalization()?;
        debug_assert_eq!(
            record.proof_kind(),
            ParentParticipationProof::Finalization,
            "put_finalization called with non-Finalization record"
        );
        self.put_inner(record, ProofSlot::Finalization)
    }

    fn put_certified_notarization(
        &self,
        record: CertifiedParentProofRecord,
    ) -> Result<(), ParentProofStoreError> {
        debug_assert_eq!(
            record.proof_kind(),
            ParentParticipationProof::CertifiedNotarization,
            "put_certified_notarization called with non-CertifiedNotarization record"
        );
        self.put_inner(record, ProofSlot::CertifiedNotarization)
    }

    fn get_finalization(
        &self,
        key: &CertifiedParentProofKey,
    ) -> Option<CertifiedParentProofRecord> {
        self.lock_read().finalization.get(key).cloned()
    }

    fn get_certified_notarization(
        &self,
        key: &CertifiedParentProofKey,
    ) -> Option<CertifiedParentProofRecord> {
        self.lock_read().certified_notarization.get(key).cloned()
    }

    fn remove(&self, key: &CertifiedParentProofKey) -> Result<bool, ParentProofStoreError> {
        let _writer = self
            .durable_writer
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(backend) = &self.backend {
            backend.remove_record::<tables::OutbeCertifiedParentFinalizationRecords>(key)?;
            backend.remove_record::<tables::OutbeCertifiedParentNotarizationRecords>(key)?;
        }
        let mut state = self.lock_write();
        let removed_fin = state.finalization.remove(key).is_some();
        let removed_cn = state.certified_notarization.remove(key).is_some();
        state.seen_certification_keys.remove(key);
        state.pending_certification_keys.remove(key);
        let removed = removed_fin || removed_cn;
        drop(state);
        if removed {
            self.bump_revision();
        }
        Ok(removed)
    }

    fn prune_below_height(&self, floor: u64) -> Result<usize, ParentProofStoreError> {
        #[cfg(test)]
        self.faults.check_prune_height()?;
        let _writer = self
            .durable_writer
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let fin_drop: Vec<_> = self
            .lock_read()
            .finalization
            .iter()
            .filter_map(|(key, record)| {
                record
                    .finalized_block_number()
                    .is_some_and(|height| height < floor)
                    .then_some(*key)
            })
            .collect();
        if let Some(backend) = &self.backend {
            for key in &fin_drop {
                backend.remove_record::<tables::OutbeCertifiedParentFinalizationRecords>(key)?;
            }
        }
        let mut state = self.lock_write();
        for key in &fin_drop {
            state.finalization.remove(key);
        }
        drop(state);
        if !fin_drop.is_empty() {
            self.bump_revision();
        }
        Ok(fin_drop.len())
    }

    fn len(&self) -> usize {
        let state = self.lock_read();
        state.finalization.len() + state.certified_notarization.len()
    }

    fn oldest_stored_height(&self) -> Option<u64> {
        let state = self.lock_read();
        state
            .finalization
            .values()
            .filter_map(|r| r.finalized_block_number())
            .min()
    }
}

impl CertificationWitnessSink for FinalizedParentCertStore {
    fn mark_local_certification_witness(&self, key: CertifiedParentProofKey) {
        let mut state = self.lock_write();
        if state.seen_certification_keys.contains(&key) {
            return;
        }
        if state.pending_certification_keys.len() >= MAX_PENDING_CERTIFICATION_WITNESSES {
            if let Some(oldest) = state.pending_certification_keys.pop_first() {
                state.seen_certification_keys.remove(&oldest);
            }
        }
        state.pending_certification_keys.insert(key);
        state.seen_certification_keys.insert(key);
    }
}
