//! Mutable per-view state owned by one reporter.

use std::{
    collections::{BTreeSet, VecDeque},
    sync::{Arc, Mutex},
};

use alloy_primitives::Address;
use commonware_cryptography::bls12381::primitives::variant::MinSig;

use crate::{digest::Digest, hybrid::HybridCertificate};

/// Mutable per-view state owned by a single `OutbeReporter` instance, separated
/// from the immutable epoch wiring. Holds the finalization cursor (the lower
/// bound for skipped-view metrics) and the byzantine-evidence
/// buffer that each finalization drains. Its operations are unit-tested in
/// isolation. Thus byzantine buffering and the finalization cursor are no
/// longer loose fields threaded through the handlers.
#[derive(Clone, Default)]
pub(super) struct ReporterViewState {
    pub(super) last_finalized_view: u64,
    pub(super) last_certificate: Option<HybridCertificate<MinSig>>,
    pub(super) pending_byzantine: BTreeSet<Address>,
}

impl ReporterViewState {
    pub(super) fn last_finalized_view(&self) -> u64 {
        self.last_finalized_view
    }

    pub(super) fn last_certificate(&self) -> Option<&HybridCertificate<MinSig>> {
        self.last_certificate.as_ref()
    }

    /// Buffer a byzantine validator address until the next finalization drains it.
    pub(super) fn buffer_byzantine(&mut self, addr: Address) {
        self.pending_byzantine.insert(addr);
    }

    /// Drain the buffered byzantine addresses in sorted order.
    ///
    /// Insert already rejects a repeated address, so the drain does not sort again.
    pub(super) fn drain_byzantine_sorted(&mut self) -> Vec<Address> {
        std::mem::take(&mut self.pending_byzantine)
            .into_iter()
            .collect()
    }

    /// Advance the finalization cursor used by skipped-view metrics.
    pub(super) fn record_finalization(
        &mut self,
        view: u64,
        certificate: HybridCertificate<MinSig>,
    ) {
        self.last_finalized_view = view;
        self.last_certificate = Some(certificate);
    }
}

const SELF_PROPOSED_DIGESTS_CAP: usize = 64;

#[derive(Clone, Default, Debug)]
pub struct SelfProposedDigests(Arc<Mutex<VecDeque<Digest>>>);

impl SelfProposedDigests {
    pub fn record(&self, digest: Digest) {
        let mut digests = match self.0.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if digests.contains(&digest) {
            return;
        }
        if digests.len() == SELF_PROPOSED_DIGESTS_CAP {
            digests.pop_front();
        }
        digests.push_back(digest);
    }

    pub fn take(&self, digest: &Digest) -> bool {
        let mut digests = match self.0.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(idx) = digests.iter().position(|candidate| candidate == digest) {
            digests.remove(idx);
            true
        } else {
            false
        }
    }
}

#[derive(Clone, Default)]
pub struct ReporterContinuity {
    inner: Arc<Mutex<ReporterContinuityState>>,
    self_proposed: SelfProposedDigests,
}

#[derive(Clone, Default)]
pub struct ReporterContinuityState {
    pub last_finalized_view: u64,
    pub last_certificate: Option<HybridCertificate<MinSig>>,
    pub last_vrf_seed: Option<Vec<u8>>,
}

impl ReporterContinuity {
    pub fn self_proposed_digests(&self) -> SelfProposedDigests {
        self.self_proposed.clone()
    }

    pub fn take_self_proposed(&self, digest: &Digest) -> bool {
        self.self_proposed.take(digest)
    }

    pub fn count_if_self_proposed(&self, digest: &Digest) {
        if self.take_self_proposed(digest) {
            crate::metrics::propose::record_self_finalized_block();
        }
    }

    pub fn snapshot(&self) -> ReporterContinuityState {
        match self.inner.lock() {
            Ok(state) => state.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    pub fn update(
        &self,
        last_finalized_view: u64,
        last_certificate: Option<HybridCertificate<MinSig>>,
        last_vrf_seed: Option<Vec<u8>>,
    ) {
        let mut state = match self.inner.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        state.last_finalized_view = last_finalized_view;
        state.last_certificate = last_certificate;
        state.last_vrf_seed = last_vrf_seed;
    }
}

/// Views skipped between two finalizations of one engine run. `None` when the
/// previous finalized view is unknown (0: first finalization after start or an
/// epoch restart) or the counter did not advance, so no false gap is recorded.
pub(super) fn skipped_views(last_finalized_view: u64, view: u64) -> Option<u64> {
    (last_finalized_view != 0 && view > last_finalized_view).then(|| view - last_finalized_view - 1)
}

#[cfg(test)]
mod tests {
    use alloy_primitives::address;

    use super::{skipped_views, ReporterContinuity, ReporterViewState, SelfProposedDigests};
    use crate::digest::Digest;
    use metrics_util::debugging::{DebugValue, DebuggingRecorder};

    fn digest(value: u8) -> Digest {
        Digest::from(alloy_primitives::B256::repeat_byte(value))
    }

    #[test]
    fn self_proposed_digests_are_consumed_once_and_ignore_foreign_digests() {
        let digests = SelfProposedDigests::default();
        digests.record(digest(1));
        assert!(!digests.take(&digest(2)));
        assert!(digests.take(&digest(1)));
        assert!(!digests.take(&digest(1)));
    }

    #[test]
    fn duplicate_self_proposals_do_not_evict_the_oldest_digest() {
        let digests = SelfProposedDigests::default();
        for value in 0..64 {
            digests.record(digest(value));
        }
        digests.record(digest(63));
        assert!(digests.take(&digest(0)));
        assert!(digests.take(&digest(63)));
        assert!(!digests.take(&digest(63)));
    }

    #[test]
    fn self_proposals_evict_at_capacity_and_share_epoch_continuity() {
        let continuity = ReporterContinuity::default();
        let next_epoch = continuity.clone();
        let digests = continuity.self_proposed_digests();
        for value in 0..=64 {
            digests.record(digest(value));
        }
        assert!(!next_epoch.take_self_proposed(&digest(0)));
        for value in 1..=64 {
            assert!(next_epoch.take_self_proposed(&digest(value)));
            assert!(!continuity.take_self_proposed(&digest(value)));
        }
        next_epoch.self_proposed_digests().record(digest(65));
        assert!(digests.take(&digest(65)));
    }

    #[test]
    fn self_finalized_counter_counts_only_the_first_recorded_match() {
        let recorder = DebuggingRecorder::new();
        let continuity = ReporterContinuity::default();
        continuity.self_proposed_digests().record(digest(1));
        metrics::with_local_recorder(&recorder, || {
            continuity.count_if_self_proposed(&digest(1));
            continuity.count_if_self_proposed(&digest(1));
            continuity.count_if_self_proposed(&digest(2));
        });
        let snapshot = recorder.snapshotter().snapshot().into_vec();
        assert_eq!(snapshot.len(), 1);
        let (key, _, _, value) = &snapshot[0];
        assert_eq!(
            key.key().name(),
            "outbe_consensus_self_finalized_blocks_total"
        );
        assert_eq!(key.key().labels().count(), 0);
        assert_eq!(*value, DebugValue::Counter(1));
    }

    #[test]
    fn view_state_buffers_drains_dedup_and_records_cursor() {
        let mut state = ReporterViewState::default();
        assert_eq!(state.last_finalized_view(), 0);
        assert!(state.last_certificate().is_none());

        let a = address!("0x0000000000000000000000000000000000000011");
        let b = address!("0x0000000000000000000000000000000000000022");
        state.buffer_byzantine(b);
        state.buffer_byzantine(a);
        state.buffer_byzantine(a);
        assert_eq!(
            state.pending_byzantine.len(),
            2,
            "duplicate evidence is stored once"
        );

        let drained = state.drain_byzantine_sorted();
        assert_eq!(
            drained,
            vec![a, b],
            "drained evidence is sorted and deduplicated"
        );
        assert!(
            state.drain_byzantine_sorted().is_empty(),
            "the buffer is emptied by drain"
        );

        let cert = super::super::tests::sample_certificate();
        state.record_finalization(17, cert.clone());
        assert_eq!(state.last_finalized_view(), 17);
        assert_eq!(state.last_certificate(), Some(&cert));
    }

    #[test]
    fn skipped_views_ignore_an_unknown_previous_view() {
        assert_eq!(skipped_views(0, 1_200), None);
        assert_eq!(skipped_views(40, 40), None);
        assert_eq!(skipped_views(40, 3), None);
        assert_eq!(skipped_views(40, 41), Some(0));
        assert_eq!(skipped_views(40, 300), Some(259));
    }
}
