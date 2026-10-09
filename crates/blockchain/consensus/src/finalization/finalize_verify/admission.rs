//! Bounded, best-effort admission. Claimed signers are scheduling buckets, not
//! authenticated identities; only the verifier can authenticate a vote.
use super::{Job, MAX_QUEUED_BYTES, MAX_QUEUED_VOTES, MAX_VOTES_PER_SIGNER};
use alloy_primitives::B256;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

type SignerKey = (u64, u32);

pub(super) struct Queued {
    pub job: Job,
    pub id: B256,
    pub bytes: usize,
}

#[derive(Default)]
pub(super) struct Admission {
    queues: BTreeMap<SignerKey, VecDeque<Queued>>,
    ready: VecDeque<SignerKey>,
    ids: BTreeSet<B256>,
    bytes: usize,
}

impl Admission {
    pub fn push(&mut self, key: SignerKey, queued: Queued) -> bool {
        if self.ids.contains(&queued.id) {
            metrics::counter!("outbe_finalize_verify_admission_total", "result" => "duplicate")
                .increment(1);
            return false;
        }
        // New work can replace the oldest item in its own full bucket. In
        // particular, a forged first vote never permanently reserves a target.
        if self
            .queues
            .get(&key)
            .is_some_and(|q| q.len() >= MAX_VOTES_PER_SIGNER)
        {
            self.discard_front(key);
        }
        while self.ids.len() >= MAX_QUEUED_VOTES
            || self.bytes.saturating_add(queued.bytes) > MAX_QUEUED_BYTES
        {
            // At capacity, reclaim from the largest backlog; ties reclaim a
            // bucket at the back of the current scheduling order. This gives a newly arriving signer a turn
            // instead of allowing one claimed signer to monopolize ingress.
            let largest = self
                .ready
                .iter()
                .copied()
                .max_by_key(|key| self.queues.get(key).map_or(0, VecDeque::len));
            // Nothing left to reclaim: the vote alone exceeds the byte budget.
            let Some(largest) = largest else {
                return false;
            };
            if !self.discard_front(largest) {
                return false;
            }
        }
        self.bytes = self.bytes.saturating_add(queued.bytes);
        self.ids.insert(queued.id);
        let bucket = self.queues.entry(key).or_default();
        if bucket.is_empty() {
            self.ready.push_back(key);
        }
        bucket.push_back(queued);
        self.record_depth();
        true
    }

    /// Evicts the oldest vote of `key`. `false` when `key` held nothing, in
    /// which case its stale scheduling entry is dropped.
    fn discard_front(&mut self, key: SignerKey) -> bool {
        let Some(queued) = self.queues.get_mut(&key).and_then(VecDeque::pop_front) else {
            self.queues.remove(&key);
            self.ready.retain(|scheduled| *scheduled != key);
            return false;
        };
        self.bytes = self.bytes.saturating_sub(queued.bytes);
        self.ids.remove(&queued.id);
        if self.queues.get(&key).is_none_or(VecDeque::is_empty) {
            self.queues.remove(&key);
            self.ready.retain(|scheduled| *scheduled != key);
        }
        metrics::counter!("outbe_finalize_verify_admission_total", "result" => "evicted")
            .increment(1);
        true
    }

    pub fn pop(&mut self) -> Option<Queued> {
        while let Some(key) = self.ready.pop_front() {
            let Some(queued) = self.queues.get_mut(&key).and_then(VecDeque::pop_front) else {
                self.queues.remove(&key);
                continue;
            };
            if self.queues.get(&key).is_none_or(VecDeque::is_empty) {
                self.queues.remove(&key);
            } else {
                self.ready.push_back(key);
            }
            self.bytes = self.bytes.saturating_sub(queued.bytes);
            self.ids.remove(&queued.id);
            self.record_depth();
            return Some(queued);
        }
        None
    }

    fn record_depth(&self) {
        metrics::gauge!("outbe_finalize_verify_queued_votes").set(self.ids.len() as f64);
        metrics::gauge!("outbe_finalize_verify_queued_bytes").set(self.bytes as f64);
    }

    #[cfg(test)]
    pub fn depth(&self) -> (usize, usize) {
        (self.ids.len(), self.bytes)
    }
}
