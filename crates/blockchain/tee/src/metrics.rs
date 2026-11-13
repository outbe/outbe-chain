//! Prometheus series for the node-side enclave channel, emitted through the
//! global `metrics` recorder (exposed by reth's `--metrics` exporter - same
//! pattern as `outbe-consensus`'s metrics module).
//!
//! Taxonomy: `outbe_tee_client_*`; counters end in `_total`, latency histograms
//! in `_ms`, and gauges never in `_total` or `_count`. Canonical rules live in
//! `crates/runtime/src/metrics_recorder.rs`.

use std::time::Duration;

use metrics::{counter, gauge, histogram};

/// One attempt's round-trip duration, labelled by the request's stable
/// [`crate::protocol::EnclaveRequest::label`]. A retried request records two
/// samples - one per attempt.
pub fn record_request_duration(request: &'static str, duration: Duration) {
    histogram!("outbe_tee_client_request_duration_ms", "request" => request)
        .record(duration.as_secs_f64() * 1000.0);
}

/// One failed attempt, labelled by request and
/// [`crate::errors::TransportError::metric_class`].
pub fn record_request_error(request: &'static str, class: &'static str) {
    counter!("outbe_tee_client_request_errors_total", "request" => request, "class" => class)
        .increment(1);
}

/// One reconnect attempt outcome: `ok` | `connect_failed` | `identity_mismatch`.
pub fn record_reconnect(result: &'static str) {
    counter!("outbe_tee_client_reconnects_total", "result" => result).increment(1);
}

/// The session generation (bumped on every successful reconnect). A step here
/// with no node restart marks an enclave-side restart or connection loss.
pub fn record_session_generation(generation: u64) {
    // u64 -> f64 is lossless for any realistic reconnect count (< 2^53).
    gauge!("outbe_tee_client_session_generation").set(generation as f64);
}

#[cfg(test)]
mod tests {
    use super::*;
    use metrics::{
        Counter, Gauge, Histogram, Key, KeyName, Metadata, Recorder, SharedString, Unit,
    };
    use std::cell::RefCell;

    #[derive(Default)]
    struct MetricRecorder(RefCell<Vec<(&'static str, Key)>>);

    impl Recorder for MetricRecorder {
        fn describe_counter(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
        fn describe_gauge(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
        fn describe_histogram(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

        fn register_counter(&self, key: &Key, _: &Metadata<'_>) -> Counter {
            self.0.borrow_mut().push(("counter", key.clone()));
            Counter::noop()
        }

        fn register_gauge(&self, key: &Key, _: &Metadata<'_>) -> Gauge {
            self.0.borrow_mut().push(("gauge", key.clone()));
            Gauge::noop()
        }

        fn register_histogram(&self, key: &Key, _: &Metadata<'_>) -> Histogram {
            self.0.borrow_mut().push(("histogram", key.clone()));
            Histogram::noop()
        }
    }

    #[test]
    fn metrics_use_client_names_types_and_labels() {
        let recorder = MetricRecorder::default();
        metrics::with_local_recorder(&recorder, || {
            record_request_duration("health", Duration::from_millis(5));
            record_request_error("health", "io_timeout");
            record_reconnect("ok");
            record_session_generation(2);
        });
        assert_eq!(
            *recorder.0.borrow(),
            vec![
                (
                    "histogram",
                    Key::from_parts(
                        "outbe_tee_client_request_duration_ms",
                        &[("request", "health")]
                    )
                ),
                (
                    "counter",
                    Key::from_parts(
                        "outbe_tee_client_request_errors_total",
                        &[("request", "health"), ("class", "io_timeout")]
                    )
                ),
                (
                    "counter",
                    Key::from_parts("outbe_tee_client_reconnects_total", &[("result", "ok")])
                ),
                (
                    "gauge",
                    Key::from_name("outbe_tee_client_session_generation")
                ),
            ]
        );
    }
}
