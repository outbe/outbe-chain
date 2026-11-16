use super::maybe_start_consensus_metrics_exporter;
use commonware_runtime::{Runner as _, Supervisor as _};
use metrics_util::debugging::{DebugValue, DebuggingRecorder, Snapshot};
use std::{
    net::{SocketAddr, TcpListener},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};
use tokio_util::sync::CancellationToken;
use tracing::{span, Event, Metadata, Subscriber};

#[test]
#[cfg_attr(miri, ignore)]
fn disabled_exporter_does_not_record_metrics() {
    let tempdir = tempfile::tempdir().unwrap();
    let config =
        commonware_runtime::tokio::Config::default().with_storage_directory(tempdir.path());
    commonware_runtime::tokio::Runner::new(config).start(async move |ctx| {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        let handle = metrics::with_local_recorder(&recorder, || {
            maybe_start_consensus_metrics_exporter(
                ctx.child("metrics"),
                None,
                CancellationToken::new(),
            )
        });
        assert!(handle.is_none());
        assert!(snapshotter.snapshot().into_vec().is_empty());
    });
}

#[test]
#[cfg_attr(miri, ignore)]
fn bound_exporter_serves_openmetrics_and_records_up() {
    let tempdir = tempfile::tempdir().unwrap();
    let config =
        commonware_runtime::tokio::Config::default().with_storage_directory(tempdir.path());
    commonware_runtime::tokio::Runner::new(config).start(async move |ctx| {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        let shutdown = CancellationToken::new();
        let handle = metrics::with_local_recorder(&recorder, || {
            maybe_start_consensus_metrics_exporter(
                ctx.child("metrics"),
                Some("127.0.0.1:0".parse().unwrap()),
                shutdown.clone(),
            )
        })
        .unwrap();
        assert!(handle.local_addr().ip().is_loopback());
        assert_ne!(handle.local_addr().port(), 0);
        let response = tokio::time::timeout(Duration::from_secs(10), async {
            let mut stream = TcpStream::connect(handle.local_addr()).await.unwrap();
            stream
                .write_all(b"GET /metrics HTTP/1.1\r\nHost: localhost\r\n\r\n")
                .await
                .unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).await.unwrap();
            response
        })
        .await
        .expect("HTTP test exceeded its outer timeout");
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"), "{response}");
        let (headers, body) = response.split_once("\r\n\r\n").unwrap();
        assert!(headers.lines().any(|line| line.eq_ignore_ascii_case(
            "content-type: application/openmetrics-text; version=1.0.0; charset=utf-8"
        )));
        assert!(body.contains("runtime_tasks_running"), "{body}");
        assert!(body.ends_with("# EOF\n"), "{body}");
        assert_exporter_gauge(snapshotter.snapshot(), 1.0);
        shutdown.cancel();
    });
}

#[test]
#[cfg_attr(miri, ignore)]
fn bind_conflict_records_down_without_failing() {
    let tempdir = tempfile::tempdir().unwrap();
    let config =
        commonware_runtime::tokio::Config::default().with_storage_directory(tempdir.path());
    commonware_runtime::tokio::Runner::new(config).start(async move |ctx| {
        let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        let handle = metrics::with_local_recorder(&recorder, || {
            maybe_start_consensus_metrics_exporter(
                ctx.child("metrics"),
                Some(occupied.local_addr().unwrap()),
                CancellationToken::new(),
            )
        });
        assert!(handle.is_none());
        assert_exporter_gauge(snapshotter.snapshot(), 0.0);
    });
}

#[test]
#[cfg_attr(miri, ignore)]
fn only_non_loopback_addresses_emit_exposure_warning() {
    let tempdir = tempfile::tempdir().unwrap();
    let config =
        commonware_runtime::tokio::Config::default().with_storage_directory(tempdir.path());
    commonware_runtime::tokio::Runner::new(config).start(async move |ctx| {
        for (address, expected_warnings) in [
            ("127.0.0.1:0", 0),
            ("[::1]:0", 0),
            ("0.0.0.0:0", 1),
            ("10.0.0.5:0", 1),
        ] {
            let address: SocketAddr = address.parse().unwrap();
            assert_eq!(address.ip().is_loopback(), expected_warnings == 0);
            let warnings = Arc::new(AtomicUsize::new(0));
            let subscriber = WarningCapture(Arc::clone(&warnings));
            let recorder = DebuggingRecorder::new();
            let shutdown = CancellationToken::new();
            tracing::subscriber::with_default(subscriber, || {
                metrics::with_local_recorder(&recorder, || {
                    let _ = maybe_start_consensus_metrics_exporter(
                        ctx.child("metrics"),
                        Some(address),
                        shutdown.clone(),
                    );
                });
            });
            assert_eq!(warnings.load(Ordering::Relaxed), expected_warnings);
            shutdown.cancel();
        }
    });
}

fn assert_exporter_gauge(snapshot: Snapshot, expected: f64) {
    let snapshot = snapshot.into_vec();
    assert_eq!(snapshot.len(), 1);
    let (key, _, _, value) = &snapshot[0];
    assert_eq!(key.key().name(), "outbe_consensus_metrics_exporter_up");
    assert_eq!(key.key().labels().count(), 0);
    assert_eq!(*value, DebugValue::Gauge(expected.into()));
}

struct WarningCapture(Arc<AtomicUsize>);

impl Subscriber for WarningCapture {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        *metadata.level() == tracing::Level::WARN
    }

    fn new_span(&self, _span: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(1)
    }

    fn record(&self, _span: &span::Id, _values: &span::Record<'_>) {}

    fn record_follows_from(&self, _span: &span::Id, _follows: &span::Id) {}

    fn event(&self, _event: &Event<'_>) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }

    fn enter(&self, _span: &span::Id) {}

    fn exit(&self, _span: &span::Id) {}
}
