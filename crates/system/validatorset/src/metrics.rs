//! Prometheus metrics for ValidatorSet state transitions.
//!
//! Names follow `outbe_validatorset_*`; counters end in `_total`, while
//! gauges use validator plurals or status/pending predicates, not `_total`,
//! `_count`, or `_size`. Canonical taxonomy: `crates/runtime/src/metrics_recorder.rs`.
//!
//! The mutation paths under `runtime/` emit these metrics.
//! Operators then see the validator lifecycle in real time and do not
//! need to poll on-chain state.
//!
//! Per-validator labels: `addr` is the validator address rendered as
//! `0x{40-char-hex}`. The configured maximum validator count
//! (`config_max_validators`, default 128) bounds the cardinality.

use alloy_primitives::Address;
use metrics::{counter, gauge};

fn addr_label(addr: Address) -> String {
    format!("{addr:?}")
}

/// Per-validator current status, one of the values from
/// [`crate::runtime::status`]:
/// `1=PENDING`, `2=ACTIVE`, `3=EXITING`,
/// `4=UNBONDING`, `5=INACTIVE`, `6=JAILED`.
pub fn record_validator_status(addr: Address, status: u8) {
    gauge!("outbe_validatorset_validator_status", "addr" => addr_label(addr))
        .set(f64::from(status));
}

/// Cumulative punishment events per validator.
/// Both jail and force-exit increment this counter.
pub fn record_validator_force_exit(addr: Address) {
    counter!("outbe_validatorset_force_exit_total", "addr" => addr_label(addr)).increment(1);
}

/// Cumulative voluntary deactivations per validator.
pub fn record_validator_deactivate(addr: Address) {
    counter!("outbe_validatorset_deactivate_total", "addr" => addr_label(addr)).increment(1);
}

/// One registration event (first-time or re-register).
pub fn record_validator_register(addr: Address, reregister: bool) {
    counter!(
        "outbe_validatorset_register_total",
        "addr" => addr_label(addr),
        "kind" => if reregister { "reregister" } else { "first" },
    )
    .increment(1);
}

/// One DKG reshare activation. `transitioned_to_unbonding` is the
/// number of validators transitioned EXITING->UNBONDING this round.
pub fn record_reshared_set_activated(active_count: u32, transitioned_to_unbonding: usize) {
    counter!("outbe_validatorset_reshared_set_activated_total").increment(1);
    gauge!("outbe_validatorset_reshared_active_validators").set(f64::from(active_count));
    gauge!("outbe_validatorset_last_reshare_unbonding_validators")
        .set(transitioned_to_unbonding as f64);
}

/// Aggregate validator-status counts. Sample once per relevant
/// transition. This is cheap because the validator-set size is bounded.
pub fn record_aggregate_status_counts(active: usize, exiting: usize, unbonding: usize) {
    gauge!("outbe_validatorset_active_validators").set(active as f64);
    gauge!("outbe_validatorset_exiting_validators").set(exiting as f64);
    gauge!("outbe_validatorset_unbonding_validators").set(unbonding as f64);
}

/// Pending-set-change flag. 0/1.
pub fn record_pending_set_change(pending: bool) {
    gauge!("outbe_validatorset_set_change_pending").set(if pending { 1.0 } else { 0.0 });
}

/// One certified freeze-height TEE expiry transition per affected validator.
pub fn record_validator_tee_expiry(addr: Address, action: &'static str) {
    counter!(
        "outbe_validatorset_tee_expiry_total",
        "addr" => addr_label(addr),
        "action" => action,
    )
    .increment(1);
}

/// Size of the most recently applied narrow TEE-expiry transition.
pub fn record_tee_expiry_exclusions(active_demoted: usize, pending_cleared: usize) {
    gauge!("outbe_validatorset_last_tee_expired_active_demoted_validators")
        .set(active_demoted as f64);
    gauge!("outbe_validatorset_last_tee_expired_pending_cleared_validators")
        .set(pending_cleared as f64);
}

#[cfg(test)]
mod tests {
    use super::*;
    use metrics::{
        Counter, Gauge, Histogram, Key, KeyName, Metadata, Recorder, SharedString, Unit,
    };
    use std::{cell::RefCell, collections::HashSet};

    #[derive(Default)]
    struct TestRecorder(RefCell<HashSet<(&'static str, Key)>>);

    impl Recorder for TestRecorder {
        fn describe_counter(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

        fn describe_gauge(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

        fn describe_histogram(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

        fn register_counter(&self, key: &Key, _: &Metadata<'_>) -> Counter {
            self.0.borrow_mut().insert(("counter", key.clone()));
            Counter::noop()
        }

        fn register_gauge(&self, key: &Key, _: &Metadata<'_>) -> Gauge {
            self.0.borrow_mut().insert(("gauge", key.clone()));
            Gauge::noop()
        }

        fn register_histogram(&self, _: &Key, _: &Metadata<'_>) -> Histogram {
            panic!("validatorset metrics must not register histograms")
        }
    }

    fn expected_series() -> HashSet<(&'static str, Key)> {
        let addr = "0x1111111111111111111111111111111111111111";
        let mut expected: HashSet<_> = [
            "outbe_validatorset_reshared_active_validators",
            "outbe_validatorset_last_reshare_unbonding_validators",
            "outbe_validatorset_active_validators",
            "outbe_validatorset_exiting_validators",
            "outbe_validatorset_unbonding_validators",
            "outbe_validatorset_set_change_pending",
            "outbe_validatorset_last_tee_expired_active_demoted_validators",
            "outbe_validatorset_last_tee_expired_pending_cleared_validators",
        ]
        .into_iter()
        .map(|name| ("gauge", Key::from_name(name)))
        .collect();
        expected.insert((
            "gauge",
            Key::from_parts("outbe_validatorset_validator_status", &[("addr", addr)]),
        ));
        for name in [
            "outbe_validatorset_force_exit_total",
            "outbe_validatorset_deactivate_total",
        ] {
            expected.insert(("counter", Key::from_parts(name, &[("addr", addr)])));
        }
        expected.insert((
            "counter",
            Key::from_name("outbe_validatorset_reshared_set_activated_total"),
        ));
        for kind in ["first", "reregister"] {
            expected.insert((
                "counter",
                Key::from_parts(
                    "outbe_validatorset_register_total",
                    &[("addr", addr), ("kind", kind)],
                ),
            ));
        }
        for action in ["active_demoted", "pending_cleared"] {
            expected.insert((
                "counter",
                Key::from_parts(
                    "outbe_validatorset_tee_expiry_total",
                    &[("addr", addr), ("action", action)],
                ),
            ));
        }
        expected
    }

    #[test]
    fn metrics_names_types_and_labels_match_taxonomy() {
        let recorder = TestRecorder::default();
        let addr = Address::from([0x11; 20]);
        metrics::with_local_recorder(&recorder, || {
            record_validator_status(addr, 2);
            record_validator_force_exit(addr);
            record_validator_deactivate(addr);
            record_validator_register(addr, false);
            record_validator_register(addr, true);
            record_reshared_set_activated(3, 4);
            record_aggregate_status_counts(3, 2, 1);
            record_pending_set_change(false);
            record_pending_set_change(true);
            record_validator_tee_expiry(addr, "active_demoted");
            record_validator_tee_expiry(addr, "pending_cleared");
            record_tee_expiry_exclusions(2, 1);
        });

        let actual = recorder.0.into_inner();
        assert_eq!(actual, expected_series());
        let names: HashSet<_> = actual.iter().map(|(_, key)| key.name()).collect();
        assert_eq!(names.len(), 14);
        for (kind, key) in &actual {
            assert!(key.name().starts_with("outbe_validatorset_"));
            if *kind == "counter" {
                assert!(key.name().ends_with("_total"));
            } else {
                assert!(!key.name().ends_with("_total"));
                assert!(!key.name().ends_with("_count"));
            }
        }
    }
}
