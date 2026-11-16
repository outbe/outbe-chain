//! Claimed signers are scheduling buckets; only individual verification authenticates them.
use super::RawEquivocationEvidence;
use alloy_primitives::B256;
use std::collections::{BTreeSet, VecDeque};

const MAX_QUEUED_EVIDENCE: usize = 64;
const MAX_PER_SIGNER_QUEUED: usize = 4;

pub(super) struct Queued {
    pub raw: RawEquivocationEvidence,
    pub raw_id: B256,
}

#[derive(Default)]
pub(super) struct Admission {
    queue: VecDeque<Queued>,
    ids: BTreeSet<B256>,
}

impl Admission {
    pub(super) fn push(&mut self, queued: Queued) -> bool {
        use crate::metrics::record_equivocation_verify_admission as record;
        if self.ids.contains(&queued.raw_id) {
            record("duplicate");
            return false;
        }
        let key = queued.raw.key();
        if self.queue.iter().filter(|q| q.raw.key() == key).count() >= MAX_PER_SIGNER_QUEUED {
            if let Some(index) = self.queue.iter().position(|q| q.raw.key() == key) {
                if let Some(evicted) = self.queue.remove(index) {
                    self.ids.remove(&evicted.raw_id);
                    record("evicted_per_signer");
                }
            }
        }
        if self.ids.len() >= MAX_QUEUED_EVIDENCE {
            self.pop();
            record("evicted_global");
        }
        self.ids.insert(queued.raw_id);
        self.queue.push_back(queued);
        record("enqueued");
        true
    }

    pub(super) fn pop(&mut self) -> Option<Queued> {
        let queued = self.queue.pop_front()?;
        self.ids.remove(&queued.raw_id);
        Some(queued)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::equivocation_verify::tests::fixture;

    fn queued(template: &RawEquivocationEvidence, signer: u32, epoch: u64, id: u8) -> Queued {
        let mut raw = template.clone();
        raw.signer = commonware_utils::Participant::new(signer);
        raw.epoch = commonware_consensus::types::Epoch::new(epoch);
        Queued {
            raw,
            raw_id: B256::repeat_byte(id),
        }
    }

    #[test]
    fn per_signer_replaces_oldest_and_releases_raw_id() {
        let (raw, _) = fixture();
        let mut admission = Admission::default();
        for id in 0..4 {
            assert!(admission.push(queued(&raw, 0, 1, id)));
        }
        assert!(!admission.push(queued(&raw, 0, 1, 3)));
        assert!(admission.push(queued(&raw, 0, 1, 4)));
        assert!(!admission.ids.contains(&B256::repeat_byte(0)));
        assert_eq!(admission.queue.len(), 4);
        assert_eq!(
            admission.queue.front().unwrap().raw_id,
            B256::repeat_byte(1)
        );
        assert!(admission.push(queued(&raw, 0, 1, 0)));
        assert!(admission.push(queued(&raw, 0, 2, 5)));
        assert_eq!(admission.queue.len(), 5);
    }

    #[test]
    fn global_replaces_oldest_and_pop_releases_raw_id() {
        let (raw, _) = fixture();
        let mut admission = Admission::default();
        for id in 0..64 {
            assert!(admission.push(queued(&raw, u32::from(id), 1, id)));
        }
        assert!(admission.push(queued(&raw, 64, 1, 64)));
        assert!(!admission.ids.contains(&B256::repeat_byte(0)));
        assert_eq!(admission.queue.len(), 64);
        assert_eq!(admission.pop().unwrap().raw_id, B256::repeat_byte(1));
        assert!(!admission.ids.contains(&B256::repeat_byte(1)));
        assert!(admission.push(queued(&raw, 0, 1, 0)));
    }
}
