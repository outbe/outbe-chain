//! Mutable per-view state owned by one reporter.

use std::sync::{Arc, Mutex};

use alloy_primitives::Address;
use commonware_cryptography::bls12381::primitives::variant::MinSig;

use crate::hybrid::HybridCertificate;

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
    pub(super) pending_byzantine: Vec<Address>,
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
        self.pending_byzantine.push(addr);
    }

    /// Drain the buffered byzantine addresses, sorted and deduplicated.
    pub(super) fn drain_byzantine_sorted(&mut self) -> Vec<Address> {
        let mut drained = std::mem::take(&mut self.pending_byzantine);
        drained.sort_unstable();
        drained.dedup();
        drained
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

#[derive(Clone, Default)]
pub struct ReporterContinuity {
    inner: Arc<Mutex<ReporterContinuityState>>,
}

#[derive(Clone, Default)]
pub struct ReporterContinuityState {
    pub last_finalized_view: u64,
    pub last_certificate: Option<HybridCertificate<MinSig>>,
    pub last_vrf_seed: Option<Vec<u8>>,
}

impl ReporterContinuity {
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

    use super::{skipped_views, ReporterViewState};

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
