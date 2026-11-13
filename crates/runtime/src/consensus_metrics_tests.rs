use super::{spawn_consensus_metrics_server, ConsensusMetricsServerError};
use commonware_runtime::{Runner as _, Supervisor as _};
// Thread-local recorder dependency is staged for runtime exporter wiring tests.
use metrics_util as _;
use tokio_util::sync::CancellationToken;

#[test]
#[cfg_attr(miri, ignore)]
fn consensus_metrics_accepts_commonware_context_without_clone() {
    let tempdir = tempfile::tempdir().unwrap();
    let config =
        commonware_runtime::tokio::Config::default().with_storage_directory(tempdir.path());
    commonware_runtime::tokio::Runner::new(config).start(async move |ctx| {
        let shutdown = CancellationToken::new();
        let handle = spawn_consensus_metrics_server(
            ctx.child("metrics"),
            "127.0.0.1:0".parse().unwrap(),
            shutdown.clone(),
        )
        .unwrap();
        assert!(handle.local_addr().ip().is_loopback());
        assert_ne!(handle.local_addr().port(), 0);
        tokio::net::TcpStream::connect(handle.local_addr())
            .await
            .unwrap();
        shutdown.cancel();
    });
}

#[test]
#[cfg_attr(miri, ignore)]
fn consensus_metrics_reports_active_bind_conflict() {
    let tempdir = tempfile::tempdir().unwrap();
    let config =
        commonware_runtime::tokio::Config::default().with_storage_directory(tempdir.path());
    commonware_runtime::tokio::Runner::new(config).start(async move |ctx| {
        let shutdown = CancellationToken::new();
        let handle = spawn_consensus_metrics_server(
            ctx.child("metrics"),
            "127.0.0.1:0".parse().unwrap(),
            shutdown.clone(),
        )
        .unwrap();
        let error = spawn_consensus_metrics_server(
            ctx.child("conflict"),
            handle.local_addr(),
            shutdown.clone(),
        )
        .unwrap_err();
        assert!(std::error::Error::source(&error).is_some());
        match error {
            ConsensusMetricsServerError::Bind { source, .. } => {
                assert_eq!(source.kind(), std::io::ErrorKind::AddrInUse);
            }
            other => panic!("expected bind error, got {other:?}"),
        }
        shutdown.cancel();
    });
}
