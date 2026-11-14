//! Observational inclusion-stall detection; never changes pool admission or eviction.

use std::time::Duration;

use outbe_primitives::block::BlockHeightRegime;
use reth_transaction_pool::PoolSize;

/// Why a nonempty pool has not contributed user transactions to committed blocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InclusionAlarmKind {
    PendingNotIncluded,
    QueuedOnly,
}

impl InclusionAlarmKind {
    pub const fn as_label(&self) -> &'static str {
        match self {
            Self::PendingNotIncluded => "pending_not_included",
            Self::QueuedOnly => "queued_only",
        }
    }
}

/// Counts consecutive eligible blocks without user transactions.
#[derive(Debug)]
pub struct InclusionStallTracker {
    pending_warn_after: u64,
    queued_warn_after: u64,
    empty_blocks: u64,
    kind: Option<InclusionAlarmKind>,
}

impl InclusionStallTracker {
    pub fn new(
        min_block_time: Duration,
        queued_lifetime: Duration,
        staleness_interval_secs: u64,
    ) -> Self {
        let effective_min_block_time = min_block_time.max(Duration::from_millis(1));
        // Warn at one quarter of the relevant expiry budget, then repeat at
        // that cadence. Defaults: pending 150s / 75 blocks, queued 30s / 15 blocks.
        let blocks = |lifetime: Duration| {
            (lifetime.as_nanos() / 4 / effective_min_block_time.as_nanos())
                .clamp(1, u64::MAX as u128) as u64
        };
        Self {
            pending_warn_after: blocks(Duration::from_secs(staleness_interval_secs)),
            queued_warn_after: blocks(queued_lifetime),
            empty_blocks: 0,
            kind: None,
        }
    }

    pub fn reset(&mut self) {
        self.empty_blocks = 0;
        self.kind = None;
    }

    /// Observe each committed block in order, using current O(1) pool sizes.
    pub fn observe_block(
        &mut self,
        number: u64,
        user_txs: usize,
        size: PoolSize,
    ) -> Option<InclusionAlarmKind> {
        let kind = match (
            BlockHeightRegime::for_block(number).allows_user_transactions(),
            user_txs,
            size.pending,
            size.queued,
        ) {
            (true, 0, 1.., _) => InclusionAlarmKind::PendingNotIncluded,
            (true, 0, 0, 1..) => InclusionAlarmKind::QueuedOnly,
            _ => {
                self.reset();
                return None;
            }
        };
        if self.kind != Some(kind) {
            self.reset();
            self.kind = Some(kind);
        }
        self.empty_blocks = self.empty_blocks.saturating_add(1);
        let warn_after = match kind {
            InclusionAlarmKind::PendingNotIncluded => self.pending_warn_after,
            InclusionAlarmKind::QueuedOnly => self.queued_warn_after,
        };
        let rewarn_every = warn_after;
        (self.empty_blocks >= warn_after
            && (self.empty_blocks - warn_after).is_multiple_of(rewarn_every))
        .then_some(kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracker() -> InclusionStallTracker {
        InclusionStallTracker::new(Duration::from_secs(2), Duration::from_secs(120), 600)
    }

    fn size(pending: usize, queued: usize) -> PoolSize {
        PoolSize {
            pending,
            queued,
            ..Default::default()
        }
    }

    #[test]
    fn pending_warns_and_rewarns_before_staleness_eviction() {
        let mut tracker = tracker();
        let mut alarms = Vec::new();
        for block in 2..=151 {
            if let Some(kind) = tracker.observe_block(block, 0, size(1, 0)) {
                alarms.push((block, kind));
            }
        }
        assert_eq!(
            alarms,
            vec![
                (76, InclusionAlarmKind::PendingNotIncluded),
                (151, InclusionAlarmKind::PendingNotIncluded)
            ]
        );
        assert!(tracker.pending_warn_after * 2 < 600);
    }

    #[test]
    fn queued_warns_before_lifetime_and_bootstrap_never_accumulates() {
        let mut tracker = tracker();
        for number in [0, 1].into_iter().cycle().take(100) {
            assert_eq!(tracker.observe_block(number, 0, size(0, 1)), None);
        }
        for block in 2..16 {
            assert_eq!(tracker.observe_block(block, 0, size(0, 1)), None);
        }
        assert_eq!(
            tracker.observe_block(16, 0, size(0, 1)),
            Some(InclusionAlarmKind::QueuedOnly)
        );
        assert!(tracker.queued_warn_after * 2 < 120);
    }

    #[test]
    fn user_transaction_reset_and_backlog_kind_change_start_fresh_series() {
        let mut tracker = tracker();
        for block in 2..16 {
            tracker.observe_block(block, 0, size(0, 1));
        }
        assert_eq!(tracker.observe_block(16, 1, size(0, 1)), None);
        assert_eq!(tracker.observe_block(17, 0, size(0, 1)), None);
        assert_eq!(tracker.observe_block(18, 0, size(1, 1)), None);
        assert_eq!(tracker.empty_blocks, 1);
        tracker.reset();
        assert_eq!(tracker.empty_blocks, 0);
        assert_eq!(tracker.kind, None);
    }

    #[test]
    fn zero_minimum_block_time_does_not_divide_by_zero() {
        let mut tracker = InclusionStallTracker::new(Duration::ZERO, Duration::from_secs(120), 600);
        assert_eq!(tracker.observe_block(2, 0, size(1, 0)), None);
        assert_eq!(tracker.pending_warn_after, 150_000);
        assert_eq!(
            InclusionAlarmKind::PendingNotIncluded.as_label(),
            "pending_not_included"
        );
        assert_eq!(InclusionAlarmKind::QueuedOnly.as_label(), "queued_only");
    }
}
