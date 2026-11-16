//! Consensus executor telemetry.

use metrics::{gauge, histogram};
use std::time::Duration;

/// Record the number of messages waiting for the executor.
pub fn record_executor_queue_entries(depth: usize) {
    gauge!("outbe_consensus_executor_queue_entries").set(depth as f64);
}

/// Record time spent waiting in the executor mailbox.
pub fn record_executor_queue_wait_ms(duration: Duration) {
    histogram!("outbe_consensus_executor_queue_wait_ms").record(duration.as_secs_f64() * 1000.0);
}

/// Record only the engine payload call, excluding ancestry fetching.
pub fn record_block_verify_execution_ms(duration: Duration) {
    histogram!("outbe_consensus_block_verify_execution_ms").record(duration.as_secs_f64() * 1000.0);
}

/// Record how far local execution finalization trails known consensus finality.
pub fn record_executor_finalization_lag_blocks(lag: u64) {
    gauge!("outbe_consensus_executor_finalization_lag_blocks").set(lag as f64);
}

/// Record the signed distance from execution HEAD to the selected consensus parent.
pub fn record_executor_convergence_depth_blocks(depth: i64) {
    gauge!("outbe_consensus_executor_convergence_depth_blocks").set(depth as f64);
}

#[cfg(test)]
mod tests {
    use super::*;
    use metrics_util::debugging::{DebugValue, DebuggingRecorder};

    #[test]
    fn executor_metrics_preserve_units_and_signed_depth() {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        metrics::with_local_recorder(&recorder, || {
            record_executor_queue_entries(3);
            record_executor_queue_wait_ms(Duration::from_micros(1500));
            record_block_verify_execution_ms(Duration::from_micros(2500));
            record_executor_finalization_lag_blocks(7);
            record_executor_convergence_depth_blocks(-5);
        });
        let values: std::collections::BTreeMap<_, _> = snapshotter
            .snapshot()
            .into_vec()
            .into_iter()
            .map(|(key, _, _, value)| {
                assert_eq!(key.key().labels().count(), 0);
                (key.key().name().to_string(), value)
            })
            .collect();
        let expected = [
            (
                "outbe_consensus_executor_queue_entries",
                DebugValue::Gauge(3.0.into()),
            ),
            (
                "outbe_consensus_executor_queue_wait_ms",
                DebugValue::Histogram(vec![1.5.into()]),
            ),
            (
                "outbe_consensus_block_verify_execution_ms",
                DebugValue::Histogram(vec![2.5.into()]),
            ),
            (
                "outbe_consensus_executor_finalization_lag_blocks",
                DebugValue::Gauge(7.0.into()),
            ),
            (
                "outbe_consensus_executor_convergence_depth_blocks",
                DebugValue::Gauge((-5.0).into()),
            ),
        ];
        assert_eq!(values.len(), expected.len());
        for (name, value) in expected {
            assert_eq!(values.get(name), Some(&value));
        }
    }
}
