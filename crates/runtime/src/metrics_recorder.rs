//! Canonical Outbe metric taxonomy and Reth Prometheus recorder configuration.
//!
//! Rust names are `outbe_<subsystem>_<metric_name>`; Reth's `/metrics` socket
//! exports `reth_outbe_<subsystem>_<metric_name>` and `reth_process_*`.
//! Stage 0's closed active list is [`OUTBE_METRIC_SUBSYSTEMS`]: consensus, dkg,
//! vrf, tee, radicle, txpool, validatorset, slashing. The Stage 2 reservations
//! [`OUTBE_RESERVED_STAGE2_SUBSYSTEMS`] (follower, p2p, evm) are not active yet.
//!
//! `counter!` names must end in `_total`. `histogram!` names must end in their
//! measurement unit: millisecond latencies use `_ms`, matched after Reth's prefix
//! by `Matcher::Suffix("_ms")` and exported with [`OUTBE_LATENCY_BUCKETS_MS`].
//! `gauge!` names MUST NOT end in `_total` or `_count`. Use units `_seconds`,
//! `_bytes`, `_ratio`, `_blocks`, `_height`, `_view`; plurals `_validators`,
//! `_peers`, `_entries`, `_votes`, `_misses`, `_felonies`, `_shares`, `_txs`,
//! `_requests`, `_repositories`; or boolean predicates (0.0/1.0) `_known`,
//! `_allowed`, `_suspected`, `_degraded`, `_pending`, `_matched`.
//! Standard label names are `addr`, `signer`, `reason`, `kind`, `result`.
//!
//! Install before Reth starts or metrics are registered. Only this function's
//! recorder is reused; a foreign global recorder is an error, not a fallback.

use metrics_exporter_prometheus::{BuildError, Matcher, PrometheusBuilder};
use reth_node_metrics::recorder::PrometheusRecorder;
use std::sync::{Mutex, OnceLock};

/// Explicit millisecond latency boundaries shared by all Outbe subsystems.
pub const OUTBE_LATENCY_BUCKETS_MS: &[f64] = &[
    0.5, 1.0, 2.5, 5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 2500.0, 5000.0, 10000.0,
];
/// Active subsystem segments accepted on the Reth metrics socket.
pub const OUTBE_METRIC_SUBSYSTEMS: &[&str] = &[
    "consensus",
    "dkg",
    "vrf",
    "tee",
    "radicle",
    "txpool",
    "validatorset",
    "slashing",
];
/// Reserved segments that are not accepted until explicitly activated.
pub const OUTBE_RESERVED_STAGE2_SUBSYSTEMS: &[&str] = &["follower", "p2p", "evm"];

static OUTBE_PROMETHEUS_RECORDER: OnceLock<&'static PrometheusRecorder> = OnceLock::new();
static RECORDER_INSTALL_LOCK: Mutex<()> = Mutex::new(());

/// Failures configuring or installing the Outbe recorder.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MetricsRecorderError {
    /// Invalid histogram bucket configuration.
    #[error("configure Outbe Prometheus latency buckets: {0}")]
    Build(#[from] BuildError),
    /// Reth could not install the global recorder, including a foreign recorder.
    #[error("install Outbe Prometheus recorder: {0}")]
    Install(eyre::Report),
    /// A previous initialization panicked while holding the installation lock.
    #[error("Outbe Prometheus recorder installation lock is poisoned")]
    InstallationPoisoned,
}

/// Install the Reth-owned recorder once, before node startup.
///
/// Concurrent calls reuse the same recorder. A foreign recorder is rejected.
pub fn install_outbe_prometheus_recorder(
) -> Result<&'static PrometheusRecorder, MetricsRecorderError> {
    let _guard = RECORDER_INSTALL_LOCK
        .lock()
        .map_err(|_| MetricsRecorderError::InstallationPoisoned)?;
    if let Some(recorder) = OUTBE_PROMETHEUS_RECORDER.get() {
        return Ok(recorder);
    }
    let builder = PrometheusBuilder::new()
        .set_buckets_for_metric(Matcher::Suffix("_ms".to_string()), OUTBE_LATENCY_BUCKETS_MS)?;
    let recorder =
        reth_node_metrics::recorder::try_install_prometheus_recorder_with_builder(builder)
            .map_err(MetricsRecorderError::Install)?;
    Ok(OUTBE_PROMETHEUS_RECORDER.get_or_init(|| recorder))
}

/// Validate Outbe TYPE declarations and sample names without global state.
///
/// Every diagnostic includes its series, violated rule, and canonical source.
pub fn validate_outbe_metric_taxonomy(rendered: &str) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    for line in rendered.lines().map(str::trim) {
        let (series, kind) = if let Some(declaration) = line.strip_prefix("# TYPE ") {
            let mut fields = declaration.split_whitespace();
            (fields.next().unwrap_or_default(), fields.next())
        } else if line.starts_with('#') {
            continue;
        } else {
            (
                line.split(['{', ' ', '\t']).next().unwrap_or_default(),
                None,
            )
        };
        let Some(name) = series.strip_prefix("reth_outbe_") else {
            continue;
        };
        let subsystem = name.split('_').next().unwrap_or_default();
        if !OUTBE_METRIC_SUBSYSTEMS.contains(&subsystem) {
            errors.push(taxonomy_error(
                series,
                "subsystem must be in OUTBE_METRIC_SUBSYSTEMS",
            ));
        }
        if let Some(kind) = kind {
            validate_metric_type(series, kind, rendered, &mut errors);
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn taxonomy_error(series: &str, rule: &str) -> String {
    format!("{series}: {rule}; see crates/runtime/src/metrics_recorder.rs")
}

fn validate_metric_type(series: &str, kind: &str, rendered: &str, errors: &mut Vec<String>) {
    match kind {
        "counter" if !series.ends_with("_total") => {
            errors.push(taxonomy_error(series, "counter must end in _total"));
        }
        "gauge" if series.ends_with("_total") || series.ends_with("_count") => {
            errors.push(taxonomy_error(
                series,
                "gauge must not end in _total or _count",
            ));
        }
        _ => {}
    }
    if series.ends_with("_ms") {
        if kind != "histogram" {
            errors.push(taxonomy_error(
                series,
                "_ms series must have type histogram",
            ));
        }
        let bucket = format!("{series}_bucket{{le=\"");
        if !rendered
            .lines()
            .any(|line| line.trim_start().starts_with(&bucket))
        {
            errors.push(taxonomy_error(
                series,
                "_ms histogram must have a bucket sample with le",
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_recorder_rejects_foreign_recorder() {
        const CHILD_MODE: &str = "OUTBE_TEST_FOREIGN_RECORDER";
        if let Ok(mode) = std::env::var(CHILD_MODE) {
            if mode == "reth" {
                reth_node_metrics::recorder::install_prometheus_recorder();
            } else {
                metrics::set_global_recorder(PrometheusBuilder::new().build_recorder())
                    .expect("foreign recorder installation");
            }
            for _ in 0..2 {
                assert!(matches!(
                    install_outbe_prometheus_recorder(),
                    Err(MetricsRecorderError::Install(_))
                ));
            }
            return;
        }
        // Global recorder state cannot be reset between tests in one process.
        for mode in ["reth", "foreign"] {
            let output =
                std::process::Command::new(std::env::current_exe().expect("test executable"))
                    .args([
                        "--exact",
                        "metrics_recorder::tests::install_recorder_rejects_foreign_recorder",
                    ])
                    .env(CHILD_MODE, mode)
                    .output()
                    .expect("isolated foreign recorder test");
            assert!(output.status.success(), "{output:?}");
        }
    }

    #[test]
    fn install_recorder_is_idempotent_and_renders_ms_histograms_with_buckets() {
        let recorder = install_outbe_prometheus_recorder().expect("first installation");
        let repeated = install_outbe_prometheus_recorder().expect("repeated installation");
        assert!(std::ptr::eq(recorder, repeated));
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    assert!(std::ptr::eq(
                        recorder,
                        install_outbe_prometheus_recorder().expect("concurrent installation")
                    ));
                });
            }
        });
        metrics::histogram!("outbe_consensus_stage1_probe_duration_ms").record(12.3);
        let rendered = recorder.handle().render();
        let series = "reth_outbe_consensus_stage1_probe_duration_ms";
        assert!(
            rendered.contains(&format!("# TYPE {series} histogram")),
            "{rendered}"
        );
        for boundary in ["0.5", "250", "10000", "+Inf"] {
            assert!(
                rendered.contains(&format!("{series}_bucket{{le=\"{boundary}\"}}")),
                "{rendered}"
            );
        }
        assert!(!rendered.contains("{quantile="), "{rendered}");
        assert_eq!(validate_outbe_metric_taxonomy(&rendered), Ok(()));
    }

    #[test]
    fn validate_taxonomy_rejects_violations_and_accepts_valid_exposition() {
        let valid = "# TYPE reth_outbe_consensus_rounds_total counter\n\
                     reth_outbe_consensus_rounds_total 2\n\
                     # TYPE reth_outbe_dkg_pending gauge\n\
                     reth_outbe_dkg_pending 1\n\
                     # TYPE reth_outbe_tee_duration_ms histogram\n\
                     reth_outbe_tee_duration_ms_bucket{le=\"0.5\"} 1\n\
                     reth_outbe_tee_duration_ms_sum 0.1\n\
                     reth_outbe_tee_duration_ms_count 1\n";
        assert_eq!(validate_outbe_metric_taxonomy(valid), Ok(()));
        assert_eq!(
            validate_outbe_metric_taxonomy(
                "# TYPE reth_process_threads gauge\nreth_process_threads 1"
            ),
            Ok(())
        );
        for (exposition, series, rule) in [
            (
                "# TYPE reth_outbe_unknown_foo_total counter",
                "reth_outbe_unknown_foo_total",
                "subsystem",
            ),
            (
                "reth_outbe_unknown_foo_total{kind=\"x\"} 1",
                "reth_outbe_unknown_foo_total",
                "subsystem",
            ),
            (
                "# TYPE reth_outbe_consensus_rounds counter",
                "reth_outbe_consensus_rounds",
                "_total",
            ),
            (
                "# TYPE reth_outbe_tee_requests_total gauge",
                "reth_outbe_tee_requests_total",
                "gauge",
            ),
            (
                "# TYPE reth_outbe_tee_requests_count gauge",
                "reth_outbe_tee_requests_count",
                "gauge",
            ),
            (
                "# TYPE reth_outbe_vrf_duration_ms summary",
                "reth_outbe_vrf_duration_ms",
                "histogram",
            ),
            (
                "# TYPE reth_outbe_vrf_duration_ms untyped",
                "reth_outbe_vrf_duration_ms",
                "histogram",
            ),
            (
                "# TYPE reth_outbe_vrf_duration_ms histogram",
                "reth_outbe_vrf_duration_ms",
                "bucket",
            ),
            (
                "reth_outbe_follower_rounds_total 1",
                "reth_outbe_follower_rounds_total",
                "subsystem",
            ),
        ] {
            let errors = validate_outbe_metric_taxonomy(exposition).expect_err(exposition);
            assert!(
                errors.iter().any(|error| error.contains(rule)),
                "{errors:?}"
            );
            for error in errors {
                assert!(error.contains(series), "{error}");
                assert!(
                    error.contains("see crates/runtime/src/metrics_recorder.rs"),
                    "{error}"
                );
            }
        }
    }
}
