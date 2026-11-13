use super::{
    spawn_consensus_metrics_server, spawn_with_limits, ConsensusMetricsHandle,
    ConsensusMetricsServerError, ServerLimits, ENCODE_CACHE_TTL, MAX_CONCURRENT_REQUESTS,
    OPENMETRICS_CONTENT_TYPE,
};
use commonware_runtime::{
    telemetry::metrics::raw::Counter, tokio::Context, Metrics as _, Runner as _, Supervisor as _,
};
use std::{future::Future, net::SocketAddr, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};
use tokio_util::sync::CancellationToken;

const METRICS_REQUEST: &str = "GET /metrics HTTP/1.1\r\nHost: localhost\r\n\r\n";
const PARTIAL_REQUEST: &[u8] = b"GET /metrics HTTP/1.1\r\nHost: localhost\r\n";

fn run_test<F, Fut>(test: F)
where
    F: FnOnce(Context) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let tempdir = tempfile::tempdir().unwrap();
    let config =
        commonware_runtime::tokio::Config::default().with_storage_directory(tempdir.path());
    commonware_runtime::tokio::Runner::new(config).start(async move |ctx| {
        tokio::time::timeout(Duration::from_secs(10), test(ctx))
            .await
            .expect("HTTP test exceeded its outer timeout");
    });
}

fn start_server(
    ctx: &Context,
    limits: ServerLimits,
) -> (ConsensusMetricsHandle, CancellationToken) {
    let shutdown = CancellationToken::new();
    let handle = spawn_with_limits(
        ctx.child("metrics"),
        "127.0.0.1:0".parse().unwrap(),
        shutdown.clone(),
        limits,
    )
    .unwrap();
    assert!(handle.local_addr().ip().is_loopback());
    assert_ne!(handle.local_addr().port(), 0);
    (handle, shutdown)
}

async fn request(addr: SocketAddr, request: &str) -> String {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    response
}

fn body(response: &str) -> &str {
    response.split_once("\r\n\r\n").unwrap().1
}

fn header<'a>(response: &'a str, name: &str) -> &'a str {
    response
        .split_once("\r\n\r\n")
        .unwrap()
        .0
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .unwrap_or_else(|| panic!("missing {name} in {response}"))
        .1
        .trim()
}

fn assert_status(response: &str, status: &str) {
    assert!(
        response.starts_with(&format!("HTTP/1.1 {status}\r\n")),
        "{response}"
    );
}

async fn stalled_connection(addr: SocketAddr) -> TcpStream {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(PARTIAL_REQUEST).await.unwrap();
    stream
}

#[test]
#[cfg_attr(miri, ignore)]
fn consensus_metrics_exports_registered_counter_without_clone() {
    run_test(|ctx| async move {
        let votes =
            ctx.child("simplex_test")
                .register("votes", "test votes", Counter::<u64>::default());
        votes.inc();
        let shutdown = CancellationToken::new();
        let handle = spawn_consensus_metrics_server(
            ctx.child("metrics"),
            "127.0.0.1:0".parse().unwrap(),
            shutdown.clone(),
        )
        .unwrap();
        let response = request(handle.local_addr(), METRICS_REQUEST).await;
        assert_status(&response, "200 OK");
        assert_eq!(header(&response, "Content-Type"), OPENMETRICS_CONTENT_TYPE);
        let encoded = body(&response);
        assert!(encoded.contains("# TYPE"));
        assert!(encoded.contains("runtime_tasks_running"));
        assert!(
            encoded.contains("simplex_test_votes_total 1\n"),
            "{encoded}"
        );
        assert!(encoded.ends_with("# EOF\n"));
        assert_eq!(
            header(&response, "Content-Length")
                .parse::<usize>()
                .unwrap(),
            encoded.len()
        );
        votes.inc();
        shutdown.cancel();
    });
}

#[test]
#[cfg_attr(miri, ignore)]
fn consensus_metrics_head_preserves_representation_headers() {
    run_test(|ctx| async move {
        let (handle, shutdown) = start_server(&ctx, ServerLimits::default());
        let response = request(
            handle.local_addr(),
            "HEAD /metrics HTTP/1.1\r\nHost: localhost\r\n\r\n",
        )
        .await;
        assert_status(&response, "200 OK");
        assert_eq!(header(&response, "Content-Type"), OPENMETRICS_CONTENT_TYPE);
        assert!(
            header(&response, "Content-Length")
                .parse::<usize>()
                .unwrap()
                > 0
        );
        assert!(body(&response).is_empty());
        shutdown.cancel();
    });
}

async fn assert_route(addr: SocketAddr, method: &str, path: &str, status: &str, expected: &str) {
    let response = request(
        addr,
        &format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\n\r\n"),
    )
    .await;
    assert_status(&response, status);
    assert_eq!(
        header(&response, "Content-Type"),
        "text/plain; charset=utf-8"
    );
    assert_eq!(body(&response), expected);
    if method == "HEAD" {
        assert_eq!(
            header(&response, "Content-Length"),
            if path == "/healthz" { "3" } else { "10" }
        );
    }
}

async fn assert_protocol_errors(addr: SocketAddr) {
    let response = request(addr, "POST /metrics HTTP/1.1\r\nHost: localhost\r\n\r\n").await;
    assert_status(&response, "405 Method Not Allowed");
    assert_eq!(header(&response, "Allow"), "GET, HEAD");
    for body_header in ["Content-Length: 4", "Transfer-Encoding: chunked"] {
        let response = request(
            addr,
            &format!("GET /metrics HTTP/1.1\r\nHost: localhost\r\n{body_header}\r\n\r\n"),
        )
        .await;
        assert_status(&response, "400 Bad Request");
    }
    let response = request(
        addr,
        &format!(
            "GET /metrics HTTP/1.1\r\nHost: localhost\r\nX-Large: {}\r\n\r\n",
            "x".repeat(9000)
        ),
    )
    .await;
    assert_status(&response, "431 Request Header Fields Too Large");
    let response = request(
        addr,
        "GET /metrics HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n",
    )
    .await;
    assert_status(&response, "200 OK");
}

#[test]
#[cfg_attr(miri, ignore)]
fn consensus_metrics_routes_and_rejects_invalid_requests() {
    run_test(|ctx| async move {
        let (handle, shutdown) = start_server(&ctx, ServerLimits::default());
        let addr = handle.local_addr();
        assert_route(addr, "GET", "/healthz", "200 OK", "ok\n").await;
        assert_route(addr, "HEAD", "/healthz", "200 OK", "").await;
        assert_route(addr, "GET", "/unknown", "404 Not Found", "not found\n").await;
        assert_route(addr, "HEAD", "/unknown", "404 Not Found", "").await;
        assert_protocol_errors(addr).await;
        shutdown.cancel();
    });
}

#[test]
#[cfg_attr(miri, ignore)]
fn consensus_metrics_isolates_slow_clients_until_timeout() {
    run_test(|ctx| async move {
        let limits = ServerLimits {
            request_timeout: Duration::from_secs(1),
            ..ServerLimits::default()
        };
        let (handle, shutdown) = start_server(&ctx, limits);
        let mut stalled = stalled_connection(handle.local_addr()).await;
        let mut byte = [0];
        let response = tokio::select! {
            response = request(handle.local_addr(), METRICS_REQUEST) => response,
            result = stalled.read(&mut byte) => panic!("stalled client closed before healthy response: {result:?}"),
        };
        assert_status(&response, "200 OK");
        assert_eq!(stalled.read(&mut byte).await.unwrap(), 0);
        shutdown.cancel();
    });
}

#[test]
#[cfg_attr(miri, ignore)]
fn consensus_metrics_rejects_saturation_without_parsing_and_recovers() {
    run_test(|ctx| async move {
        let (handle, shutdown) = start_server(&ctx, ServerLimits::default());
        let mut stalled = Vec::new();
        for _ in 0..MAX_CONCURRENT_REQUESTS {
            stalled.push(stalled_connection(handle.local_addr()).await);
        }
        for input in [
            "",
            std::str::from_utf8(PARTIAL_REQUEST).unwrap(),
            METRICS_REQUEST,
        ] {
            let response = request(handle.local_addr(), input).await;
            assert_status(&response, "503 Service Unavailable");
            assert_eq!(
                header(&response, "Content-Type"),
                "text/plain; charset=utf-8"
            );
            assert_eq!(header(&response, "Content-Length"), "20");
            assert_eq!(header(&response, "Connection"), "close");
            assert_eq!(body(&response), "service unavailable\n");
        }
        drop(stalled.pop());
        loop {
            let response = request(handle.local_addr(), METRICS_REQUEST).await;
            if response.starts_with("HTTP/1.1 200 OK\r\n") {
                break;
            }
            assert_status(&response, "503 Service Unavailable");
            tokio::task::yield_now().await;
        }
        shutdown.cancel();
    });
}

#[test]
#[cfg_attr(miri, ignore)]
fn consensus_metrics_preserves_bind_errors_and_closes_on_shutdown() {
    run_test(|ctx| async move {
        let (handle, shutdown) = start_server(&ctx, ServerLimits::default());
        let error = spawn_consensus_metrics_server(
            ctx.child("conflict"),
            handle.local_addr(),
            shutdown.clone(),
        )
        .unwrap_err();
        assert!(std::error::Error::source(&error).is_some());
        assert!(
            matches!(error, ConsensusMetricsServerError::Bind { source, .. } if source.kind() == std::io::ErrorKind::AddrInUse)
        );
        assert_status(
            &request(handle.local_addr(), METRICS_REQUEST).await,
            "200 OK",
        );
        let mut stalled = stalled_connection(handle.local_addr()).await;
        // A completed request establishes that the earlier stalled socket was accepted.
        assert_status(
            &request(handle.local_addr(), METRICS_REQUEST).await,
            "200 OK",
        );
        shutdown.cancel();
        assert_eq!(stalled.read(&mut [0]).await.unwrap(), 0);
        while TcpStream::connect(handle.local_addr()).await.is_ok() {
            tokio::task::yield_now().await;
        }
    });
}

#[test]
#[cfg_attr(miri, ignore)]
fn consensus_metrics_reuses_snapshot_and_refreshes_after_expiration() {
    run_test(|ctx| async move {
        let votes =
            ctx.child("simplex_test")
                .register("votes", "test votes", Counter::<u64>::default());
        votes.inc();
        let limits = ServerLimits {
            encode_cache_ttl: Duration::from_secs(3600),
            ..ServerLimits::default()
        };
        let (cached, shutdown) = start_server(&ctx, limits);
        let first = request(cached.local_addr(), METRICS_REQUEST).await;
        votes.inc();
        let second = request(cached.local_addr(), METRICS_REQUEST).await;
        assert_eq!(body(&first), body(&second));
        assert!(body(&second).contains("simplex_test_votes_total 1\n"));
        shutdown.cancel();
        let (fresh, shutdown) = start_server(&ctx, ServerLimits::default());
        let response = request(fresh.local_addr(), METRICS_REQUEST).await;
        assert!(body(&response).contains("simplex_test_votes_total 2\n"));
        votes.inc();
        tokio::time::sleep(ENCODE_CACHE_TTL).await;
        let response = request(fresh.local_addr(), METRICS_REQUEST).await;
        assert!(body(&response).contains("simplex_test_votes_total 3\n"));
        shutdown.cancel();
    });
}
