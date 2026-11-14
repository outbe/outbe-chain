//! Block verification telemetry.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Closed reasons why a local verification task cannot emit a vote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoteWithheldReason {
    Unavailable,
    VerifyError,
    EpochFence,
    PublishEpochFence,
    ViewCancelled,
    ResolveTimeoutOrMissing,
}

impl VoteWithheldReason {
    pub const fn as_label(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::VerifyError => "verify_error",
            Self::EpochFence => "epoch_fence",
            Self::PublishEpochFence => "publish_epoch_fence",
            Self::ViewCancelled => "view_cancelled",
            Self::ResolveTimeoutOrMissing => "resolve_timeout_or_missing",
        }
    }
}

/// First reason wins, including when cancellation follows an already withheld vote.
#[derive(Default)]
pub struct WithheldVoteRecord {
    recorded: AtomicBool,
}

impl WithheldVoteRecord {
    pub fn record(&self, reason: VoteWithheldReason) {
        if !self.recorded.swap(true, Ordering::Relaxed) {
            metrics::counter!("outbe_consensus_vote_withheld_total", "reason" => reason.as_label())
                .increment(1);
        }
    }
}

/// Retains the gauge handle so cancellation decrements the same recorder.
pub struct VerifyInFlightGuard {
    gauge: metrics::Gauge,
}

impl VerifyInFlightGuard {
    pub fn new() -> Self {
        let gauge = metrics::gauge!("outbe_consensus_verify_in_flight_blocks");
        gauge.increment(1.0);
        Self { gauge }
    }
}

impl Default for VerifyInFlightGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for VerifyInFlightGuard {
    fn drop(&mut self) {
        self.gauge.decrement(1.0);
    }
}

pub fn record_block_verify_decision_ms(duration: Duration) {
    metrics::histogram!("outbe_consensus_block_verify_decision_ms")
        .record(duration.as_secs_f64() * 1000.0);
}

pub fn record_candidate_timestamp_ahead_ms(ahead_ms: u64) {
    metrics::histogram!("outbe_consensus_candidate_timestamp_ahead_ms").record(ahead_ms as f64);
}

#[cfg(test)]
mod tests {
    use super::*;
    use metrics_util::debugging::{DebugValue, DebuggingRecorder};

    #[test]
    fn withheld_vote_records_only_the_first_reason() {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        metrics::with_local_recorder(&recorder, || {
            let record = WithheldVoteRecord::default();
            record.record(VoteWithheldReason::ResolveTimeoutOrMissing);
            record.record(VoteWithheldReason::VerifyError);
            record.record(VoteWithheldReason::ViewCancelled);
        });
        let snapshot = snapshotter.snapshot().into_vec();
        assert_eq!(snapshot.len(), 1);
        let (key, _, _, value) = &snapshot[0];
        assert_eq!(key.key().name(), "outbe_consensus_vote_withheld_total");
        let labels: Vec<_> = key
            .key()
            .labels()
            .map(|label| (label.key(), label.value()))
            .collect();
        assert_eq!(labels, [("reason", "resolve_timeout_or_missing")]);
        assert_eq!(*value, DebugValue::Counter(1));
    }

    #[test]
    fn withheld_reason_labels_are_closed_and_stable() {
        for (reason, label) in [
            (VoteWithheldReason::Unavailable, "unavailable"),
            (VoteWithheldReason::VerifyError, "verify_error"),
            (VoteWithheldReason::EpochFence, "epoch_fence"),
            (VoteWithheldReason::PublishEpochFence, "publish_epoch_fence"),
            (VoteWithheldReason::ViewCancelled, "view_cancelled"),
            (
                VoteWithheldReason::ResolveTimeoutOrMissing,
                "resolve_timeout_or_missing",
            ),
        ] {
            assert_eq!(reason.as_label(), label);
            let recorder = DebuggingRecorder::new();
            metrics::with_local_recorder(&recorder, || {
                WithheldVoteRecord::default().record(reason)
            });
            let snapshot = recorder.snapshotter().snapshot().into_vec();
            assert_eq!(snapshot.len(), 1);
            assert_eq!(snapshot[0].3, DebugValue::Counter(1));
            assert_eq!(snapshot[0].0.key().labels().next().unwrap().value(), label);
        }
    }

    #[test]
    fn in_flight_guard_balances_on_drop() {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        let guard = metrics::with_local_recorder(&recorder, VerifyInFlightGuard::new);
        assert_eq!(
            snapshotter.snapshot().into_vec()[0].3,
            DebugValue::Gauge(1.0.into())
        );
        drop(guard);
        // DebuggingRecorder snapshots consume values: +1 followed by -1 sums to zero.
        assert_eq!(
            snapshotter.snapshot().into_vec()[0].3,
            DebugValue::Gauge((-1.0).into())
        );
    }

    #[test]
    fn histograms_record_milliseconds() {
        let recorder = DebuggingRecorder::new();
        metrics::with_local_recorder(&recorder, || {
            record_block_verify_decision_ms(std::time::Duration::from_micros(1500));
            record_candidate_timestamp_ahead_ms(17);
        });
        let snapshot = recorder.snapshotter().snapshot().into_vec();
        assert_eq!(snapshot.len(), 2);
        for (key, _, _, value) in snapshot {
            let expected = match key.key().name() {
                "outbe_consensus_block_verify_decision_ms" => 1.5,
                "outbe_consensus_candidate_timestamp_ahead_ms" => 17.0,
                name => panic!("unexpected metric: {name}"),
            };
            assert_eq!(value, DebugValue::Histogram(vec![expected.into()]));
        }
    }
}
