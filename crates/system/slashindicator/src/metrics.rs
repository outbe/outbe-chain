//! Prometheus metrics for SlashIndicator state transitions.
//!
//! Canonical taxonomy: `crates/runtime/src/metrics_recorder.rs`.
//! All names use `outbe_slashing_*`; counters end in `_total`, while gauges
//! use plurals and never end in `_total` or `_count`.
//!
//! The corresponding mutation paths in `runtime.rs` emit these gauges
//! and counters. With them, operators can alert on miss-count growth
//! before the felony threshold is reached. Operators can also observe
//! cumulative slash/felony events over time.
//!
//! Per-validator labels: `addr` is the validator address rendered as
//! `0x{40-char-hex}`. Cardinality is bounded by the configured maximum
//! validator count (`config_max_validators`, default 128).

use alloy_primitives::Address;
use metrics::{counter, gauge};

fn addr_label(addr: Address) -> String {
    format!("{addr:?}")
}

/// Per-validator current-epoch proposer miss count. Resets at every
/// epoch boundary via `reset_epoch_counters`. Use [`record_proposer_miss_event`]
/// to increment the cumulative counter on every miss.
pub fn record_proposer_miss_count(addr: Address, count: u64) {
    gauge!("outbe_slashing_proposer_misses", "addr" => addr_label(addr)).set(count as f64);
}

/// Cumulative counter - one increment per `slash_proposer` call.
pub fn record_proposer_miss_event(addr: Address) {
    counter!("outbe_slashing_proposer_missed_views_total", "addr" => addr_label(addr)).increment(1);
}

/// Per-validator current-epoch voter miss count. Same reset semantics.
pub fn record_voter_miss_count(addr: Address, count: u64) {
    gauge!("outbe_slashing_voter_misses", "addr" => addr_label(addr)).set(count as f64);
}

/// Cumulative counter - one increment per `slash_voter` call.
pub fn record_voter_miss_event(addr: Address) {
    counter!("outbe_slashing_voter_missed_votes_total", "addr" => addr_label(addr)).increment(1);
}

/// Per-validator cumulative felony count (never resets).
pub fn record_felony_count(addr: Address, count: u64) {
    gauge!("outbe_slashing_felonies", "addr" => addr_label(addr)).set(count as f64);
}

/// One slash event was applied to `addr`. `reason` is one of:
/// `proposer_felony` | `voter_felony` | `evidence_felony` | `byzantine`.
pub fn record_validator_slashed(addr: Address, reason: &'static str) {
    counter!(
        "outbe_slashing_validator_slashed_total",
        "addr" => addr_label(addr),
        "reason" => reason,
    )
    .increment(1);
}

/// Reset of per-epoch counters at an epoch boundary.
pub fn record_epoch_counters_reset(validator_count: usize) {
    counter!("outbe_slashing_epoch_resets_total").increment(1);
    gauge!("outbe_slashing_last_epoch_reset_validators").set(validator_count as f64);
}

#[cfg(test)]
mod tests {
    use super::*;
    use metrics::{
        Counter, Gauge, Histogram, Key, KeyName, Label, Metadata, Recorder, SharedString, Unit,
    };
    use std::sync::Mutex;

    #[derive(Default)]
    struct TestRecorder(Mutex<Vec<(&'static str, Key)>>);

    impl Recorder for TestRecorder {
        fn describe_counter(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

        fn describe_gauge(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

        fn describe_histogram(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

        fn register_counter(&self, key: &Key, _: &Metadata<'_>) -> Counter {
            self.0.lock().unwrap().push(("counter", key.clone()));
            Counter::noop()
        }

        fn register_gauge(&self, key: &Key, _: &Metadata<'_>) -> Gauge {
            self.0.lock().unwrap().push(("gauge", key.clone()));
            Gauge::noop()
        }

        fn register_histogram(&self, key: &Key, _: &Metadata<'_>) -> Histogram {
            self.0.lock().unwrap().push(("histogram", key.clone()));
            Histogram::noop()
        }
    }

    #[test]
    fn metrics_use_canonical_names_types_and_labels() {
        let recorder = TestRecorder::default();
        let addr = Address::repeat_byte(0x11);
        metrics::with_local_recorder(&recorder, || {
            record_proposer_miss_count(addr, 2);
            record_proposer_miss_event(addr);
            record_voter_miss_count(addr, 3);
            record_voter_miss_event(addr);
            record_felony_count(addr, 4);
            record_validator_slashed(addr, "proposer_felony");
            record_epoch_counters_reset(5);
        });

        let addr = Label::new("addr", "0x1111111111111111111111111111111111111111");
        let validator_key = |name| Key::from_parts(name, vec![addr.clone()]);
        assert_eq!(
            recorder.0.into_inner().unwrap(),
            vec![
                ("gauge", validator_key("outbe_slashing_proposer_misses")),
                (
                    "counter",
                    validator_key("outbe_slashing_proposer_missed_views_total"),
                ),
                ("gauge", validator_key("outbe_slashing_voter_misses")),
                (
                    "counter",
                    validator_key("outbe_slashing_voter_missed_votes_total"),
                ),
                ("gauge", validator_key("outbe_slashing_felonies")),
                (
                    "counter",
                    Key::from_parts(
                        "outbe_slashing_validator_slashed_total",
                        vec![addr, Label::new("reason", "proposer_felony")],
                    ),
                ),
                (
                    "counter",
                    Key::from_name("outbe_slashing_epoch_resets_total")
                ),
                (
                    "gauge",
                    Key::from_name("outbe_slashing_last_epoch_reset_validators"),
                ),
            ],
        );
    }
}
