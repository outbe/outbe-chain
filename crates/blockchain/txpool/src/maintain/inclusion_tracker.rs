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

/// Tracks consecutive eligible blocks and their elapsed canonical time without user transactions.
#[derive(Debug)]
pub struct InclusionStallTracker {
    pending_warn_after: u64,
    queued_warn_after: u64,
    pending_warn_after_secs: u64,
    queued_warn_after_secs: u64,
    empty_blocks: u64,
    blocks_since_last_alarm: u64,
    first_empty_timestamp_secs: Option<u64>,
    last_alarm_timestamp_secs: Option<u64>,
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
            pending_warn_after_secs: staleness_interval_secs / 4,
            queued_warn_after_secs: (queued_lifetime / 4).as_secs(),
            empty_blocks: 0,
            blocks_since_last_alarm: 0,
            first_empty_timestamp_secs: None,
            last_alarm_timestamp_secs: None,
            kind: None,
        }
    }

    pub fn reset(&mut self) {
        self.empty_blocks = 0;
        self.blocks_since_last_alarm = 0;
        self.first_empty_timestamp_secs = None;
        self.last_alarm_timestamp_secs = None;
        self.kind = None;
    }

    /// Observe each committed block in order, using current O(1) pool sizes.
    pub fn observe_block(
        &mut self,
        number: u64,
        block_timestamp_secs: u64,
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
        self.blocks_since_last_alarm = self.blocks_since_last_alarm.saturating_add(1);
        let first = *self
            .first_empty_timestamp_secs
            .get_or_insert(block_timestamp_secs);
        let (warn_after_blocks, warn_after_secs) = match kind {
            InclusionAlarmKind::PendingNotIncluded => {
                (self.pending_warn_after, self.pending_warn_after_secs)
            }
            InclusionAlarmKind::QueuedOnly => (self.queued_warn_after, self.queued_warn_after_secs),
        };
        let alarm_due = match self.last_alarm_timestamp_secs {
            None => {
                self.empty_blocks >= warn_after_blocks
                    || (self.empty_blocks >= 2
                        && block_timestamp_secs.saturating_sub(first) >= warn_after_secs)
            }
            Some(last) => {
                self.blocks_since_last_alarm >= warn_after_blocks
                    || block_timestamp_secs.saturating_sub(last) >= warn_after_secs
            }
        };
        if !alarm_due {
            return None;
        }
        self.last_alarm_timestamp_secs = Some(block_timestamp_secs);
        self.blocks_since_last_alarm = 0;
        Some(kind)
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
            if let Some(kind) = tracker.observe_block(block, block * 2, 0, size(1, 0)) {
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
            assert_eq!(
                tracker.observe_block(number, number * 2, 0, size(0, 1)),
                None
            );
        }
        for block in 2..16 {
            assert_eq!(tracker.observe_block(block, block * 2, 0, size(0, 1)), None);
        }
        assert_eq!(
            tracker.observe_block(16, 16 * 2, 0, size(0, 1)),
            Some(InclusionAlarmKind::QueuedOnly)
        );
        assert!(tracker.queued_warn_after * 2 < 120);
    }

    #[test]
    fn user_transaction_reset_and_backlog_kind_change_start_fresh_series() {
        let mut tracker = tracker();
        for block in 2..16 {
            tracker.observe_block(block, block * 2, 0, size(0, 1));
        }
        assert_eq!(tracker.observe_block(16, 16 * 2, 1, size(0, 1)), None);
        assert_eq!(tracker.observe_block(17, 17 * 2, 0, size(0, 1)), None);
        assert_eq!(tracker.observe_block(18, 18 * 2, 0, size(1, 1)), None);
        assert_eq!(tracker.empty_blocks, 1);
        tracker.reset();
        assert_eq!(tracker.empty_blocks, 0);
        assert_eq!(tracker.kind, None);
        assert_eq!(tracker.first_empty_timestamp_secs, None);
        assert_eq!(tracker.last_alarm_timestamp_secs, None);
        assert_eq!(tracker.blocks_since_last_alarm, 0);
    }

    #[test]
    fn zero_minimum_block_time_does_not_divide_by_zero() {
        let mut tracker = InclusionStallTracker::new(Duration::ZERO, Duration::from_secs(120), 600);
        assert_eq!(tracker.observe_block(2, 2 * 2, 0, size(1, 0)), None);
        assert_eq!(tracker.pending_warn_after, 150_000);
        assert_eq!(
            InclusionAlarmKind::PendingNotIncluded.as_label(),
            "pending_not_included"
        );
        assert_eq!(InclusionAlarmKind::QueuedOnly.as_label(), "queued_only");
    }

    #[test]
    fn slow_blocks_warn_and_rewarn_within_expiry_windows() {
        for (cadence, pool_size, kind, first_alarm, expiry) in [
            (15, size(0, 1), InclusionAlarmKind::QueuedOnly, 30, 120),
            (36, size(0, 1), InclusionAlarmKind::QueuedOnly, 36, 120),
            (
                15,
                size(1, 0),
                InclusionAlarmKind::PendingNotIncluded,
                150,
                600,
            ),
            (
                36,
                size(1, 0),
                InclusionAlarmKind::PendingNotIncluded,
                180,
                600,
            ),
        ] {
            let mut tracker = tracker();
            let mut alarms = Vec::new();
            for index in 0..=2 * first_alarm / cadence {
                let elapsed = index * cadence;
                if let Some(observed) =
                    tracker.observe_block(2 + index, 10_000 + elapsed, 0, pool_size)
                {
                    alarms.push((elapsed, observed));
                }
            }
            assert_eq!(alarms, vec![(first_alarm, kind), (2 * first_alarm, kind)]);
            assert!(2 * first_alarm < expiry);
        }
    }

    #[test]
    fn elapsed_time_resets_on_kind_change_inclusion_and_explicit_reset() {
        let mut tracker = tracker();
        assert_eq!(tracker.observe_block(2, 1_000, 0, size(0, 1)), None);
        assert_eq!(
            tracker.observe_block(3, 1_036, 0, size(0, 1)),
            Some(InclusionAlarmKind::QueuedOnly)
        );
        assert_eq!(tracker.observe_block(4, 2_000, 0, size(1, 0)), None);
        assert_eq!(tracker.first_empty_timestamp_secs, Some(2_000));
        assert_eq!(tracker.last_alarm_timestamp_secs, None);
        assert_eq!(tracker.observe_block(5, 2_015, 0, size(1, 0)), None);
        assert_eq!(
            tracker.observe_block(6, 2_150, 0, size(1, 0)),
            Some(InclusionAlarmKind::PendingNotIncluded)
        );
        assert_eq!(tracker.observe_block(7, 2_200, 1, size(1, 0)), None);
        assert_eq!(tracker.first_empty_timestamp_secs, None);
        assert_eq!(tracker.last_alarm_timestamp_secs, None);
        assert_eq!(tracker.observe_block(8, 3_000, 0, size(0, 1)), None);
        assert_eq!(tracker.observe_block(9, 2_999, 0, size(0, 1)), None);
        tracker.reset();
        assert_eq!(tracker.observe_block(10, 4_000, 0, size(0, 1)), None);
        assert_eq!(tracker.observe_block(11, 4_015, 0, size(0, 1)), None);
        assert_eq!(tracker.last_alarm_timestamp_secs, None);
    }
}
