//! Bounded, supervised HTTP export of Commonware OpenMetrics snapshots.

use bytes::Bytes;
use http::{header, HeaderValue, Method, Request, Response, StatusCode};
use http_body_util::Full;
use hyper::{server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;
use std::{convert::Infallible, net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpSocket, TcpStream},
    sync::{Mutex, Semaphore},
    time::Instant,
};
use tokio_util::sync::CancellationToken;

/// Maximum number of simultaneous metrics requests.
pub const MAX_CONCURRENT_REQUESTS: usize = 8;
/// Equals hyper 1.9's MINIMUM_MAX_BUFFER_SIZE = 8192; smaller values panic in
/// http1::Builder::max_buf_size.
pub const MAX_HEADER_BUF_BYTES: usize = 8 * 1024;
/// Maximum time allowed for a metrics request.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// Lifetime of a cached metrics encoding.
pub const ENCODE_CACHE_TTL: Duration = Duration::from_millis(250);
/// Media type of the Commonware OpenMetrics encoding.
pub const OPENMETRICS_CONTENT_TYPE: &str =
    "application/openmetrics-text; version=1.0.0; charset=utf-8";

/// Socket setup failures retain their original I/O error.
#[derive(Debug, thiserror::Error)]
pub enum ConsensusMetricsServerError {
    #[error("failed to create consensus metrics socket")]
    SocketCreate {
        #[source]
        source: std::io::Error,
    },
    #[error("failed to enable consensus metrics socket address reuse")]
    SetReuseAddr {
        #[source]
        source: std::io::Error,
    },
    #[error("failed to bind consensus metrics socket at {listen_addr}")]
    Bind {
        listen_addr: SocketAddr,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to listen on consensus metrics socket")]
    Listen {
        #[source]
        source: std::io::Error,
    },
    #[error("failed to read consensus metrics socket address")]
    LocalAddr {
        #[source]
        source: std::io::Error,
    },
}

/// Bound address of a supervised consensus metrics server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsensusMetricsHandle {
    local_addr: SocketAddr,
}

impl ConsensusMetricsHandle {
    /// Returns the actual bound address, including an OS-assigned port.
    pub const fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }
}

fn bind_reuseaddr_listener(
    listen_addr: SocketAddr,
) -> Result<(TcpListener, SocketAddr), ConsensusMetricsServerError> {
    let socket = if listen_addr.is_ipv4() {
        TcpSocket::new_v4()
    } else {
        TcpSocket::new_v6()
    }
    .map_err(|source| ConsensusMetricsServerError::SocketCreate { source })?;
    socket
        .set_reuseaddr(true)
        .map_err(|source| ConsensusMetricsServerError::SetReuseAddr { source })?;
    socket
        .bind(listen_addr)
        .map_err(|source| ConsensusMetricsServerError::Bind {
            listen_addr,
            source,
        })?;
    let listener = socket
        .listen(128)
        .map_err(|source| ConsensusMetricsServerError::Listen { source })?;
    let local_addr = listener
        .local_addr()
        .map_err(|source| ConsensusMetricsServerError::LocalAddr { source })?;
    Ok((listener, local_addr))
}

#[derive(Clone, Copy)]
struct ServerLimits {
    request_timeout: Duration,
    encode_cache_ttl: Duration,
}

impl Default for ServerLimits {
    fn default() -> Self {
        Self {
            request_timeout: REQUEST_TIMEOUT,
            encode_cache_ttl: ENCODE_CACHE_TTL,
        }
    }
}

type SnapshotCache = Arc<Mutex<Option<(Instant, Bytes)>>>;

#[derive(Clone)]
struct ServerState {
    permits: Arc<Semaphore>,
    cache: SnapshotCache,
    shutdown: CancellationToken,
    limits: ServerLimits,
}

const TEXT_CONTENT_TYPE: &str = "text/plain; charset=utf-8";
const SATURATED_RESPONSE: &[u8] = b"HTTP/1.1 503 Service Unavailable\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: 20\r\nConnection: close\r\n\r\nservice unavailable\n";

/// Binds a metrics socket and serves requests under Commonware supervision.
pub fn spawn_consensus_metrics_server<C>(
    metrics_ctx: C,
    listen_addr: SocketAddr,
    shutdown: CancellationToken,
) -> Result<ConsensusMetricsHandle, ConsensusMetricsServerError>
where
    C: commonware_runtime::Metrics + commonware_runtime::Spawner,
{
    spawn_with_limits(metrics_ctx, listen_addr, shutdown, ServerLimits::default())
}

fn spawn_with_limits<C>(
    metrics_ctx: C,
    listen_addr: SocketAddr,
    shutdown: CancellationToken,
    limits: ServerLimits,
) -> Result<ConsensusMetricsHandle, ConsensusMetricsServerError>
where
    C: commonware_runtime::Metrics + commonware_runtime::Spawner,
{
    let (listener, local_addr) = bind_reuseaddr_listener(listen_addr)?;
    metrics_ctx.spawn(move |ctx| async move {
        let state = ServerState {
            permits: Arc::new(Semaphore::new(MAX_CONCURRENT_REQUESTS)),
            cache: Arc::new(Mutex::new(None)),
            shutdown: shutdown.clone(),
            limits,
        };
        loop {
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => break,
                accepted = listener.accept() => match accepted {
                    Ok((stream, _)) => {
                        let _ = stream.set_nodelay(true);
                        dispatch_connection(&ctx, stream, &state);
                    }
                    Err(error) => {
                        tracing::warn!(%error, "consensus metrics accept failed");
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }
            }
        }
    });
    Ok(ConsensusMetricsHandle { local_addr })
}

fn dispatch_connection<C>(ctx: &C, stream: TcpStream, state: &ServerState)
where
    C: commonware_runtime::Metrics + commonware_runtime::Spawner,
{
    let permit = match state.permits.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            ctx.child("rejected")
                .spawn(move |_| reject_saturated(stream));
            return;
        }
    };
    let state = state.clone();
    ctx.child("connection").spawn(move |conn_ctx| async move {
        let _permit = permit;
        serve_connection(conn_ctx, stream, state).await;
    });
}

async fn reject_saturated(mut stream: TcpStream) {
    let _ = tokio::time::timeout(
        Duration::from_millis(250),
        stream.write_all(SATURATED_RESPONSE),
    )
    .await;
    shutdown_and_drain(&mut stream).await;
}

async fn serve_connection<C: commonware_runtime::Metrics>(
    conn_ctx: C,
    mut stream: TcpStream,
    state: ServerState,
) {
    let service =
        service_fn(|request| route_request(&conn_ctx, request, &state.cache, state.limits));
    let mut builder = http1::Builder::new();
    let completed = {
        let conn = builder
            .keep_alive(false)
            .max_buf_size(MAX_HEADER_BUF_BYTES)
            .serve_connection(TokioIo::new(&mut stream), service);
        tokio::select! {
            biased;
            _ = state.shutdown.cancelled() => false,
            result = tokio::time::timeout(state.limits.request_timeout, conn) => result.is_ok(),
        }
    };
    if completed {
        shutdown_and_drain(&mut stream).await;
    }
}

async fn shutdown_and_drain(stream: &mut TcpStream) {
    // Unread socket bytes cause BSD/macOS to reset TCP and discard the HTTP response on close.
    let _ = stream.shutdown().await;
    let mut limited = stream.take(MAX_HEADER_BUF_BYTES as u64);
    let _ = tokio::time::timeout(
        Duration::from_millis(50),
        tokio::io::copy(&mut limited, &mut tokio::io::sink()),
    )
    .await;
}

async fn encoded_snapshot<C: commonware_runtime::Metrics>(
    ctx: &C,
    cache: &SnapshotCache,
    ttl: Duration,
) -> Bytes {
    let mut snapshot = cache.lock().await;
    if let Some((created, bytes)) = snapshot.as_ref() {
        if created.elapsed() < ttl {
            return bytes.clone();
        }
    }
    let bytes = Bytes::from(ctx.encode());
    *snapshot = Some((Instant::now(), bytes.clone()));
    bytes
}

async fn route_request<C: commonware_runtime::Metrics>(
    ctx: &C,
    request: Request<hyper::body::Incoming>,
    cache: &SnapshotCache,
    limits: ServerLimits,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let head = request.method() == Method::HEAD;
    let response = if has_request_body(&request) {
        text_response(StatusCode::BAD_REQUEST, "bad request\n", head)
    } else if request.method() != Method::GET && !head {
        let mut response = text_response(
            StatusCode::METHOD_NOT_ALLOWED,
            "method not allowed\n",
            false,
        );
        response
            .headers_mut()
            .insert(header::ALLOW, HeaderValue::from_static("GET, HEAD"));
        response
    } else {
        match request.uri().path() {
            "/metrics" => make_response(
                StatusCode::OK,
                OPENMETRICS_CONTENT_TYPE,
                encoded_snapshot(ctx, cache, limits.encode_cache_ttl).await,
                head,
            ),
            "/healthz" => text_response(StatusCode::OK, "ok\n", head),
            _ => text_response(StatusCode::NOT_FOUND, "not found\n", head),
        }
    };
    Ok(response)
}

fn has_request_body<B>(request: &Request<B>) -> bool {
    request.headers().contains_key(header::TRANSFER_ENCODING)
        || request
            .headers()
            .get_all(header::CONTENT_LENGTH)
            .iter()
            .any(|value| value != "0")
}

fn text_response(status: StatusCode, body: &'static str, head: bool) -> Response<Full<Bytes>> {
    make_response(
        status,
        TEXT_CONTENT_TYPE,
        Bytes::from_static(body.as_bytes()),
        head,
    )
}

fn make_response(
    status: StatusCode,
    content_type: &'static str,
    bytes: Bytes,
    head: bool,
) -> Response<Full<Bytes>> {
    let content_length = bytes.len();
    let mut response = Response::new(Full::new(if head { Bytes::new() } else { bytes }));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(content_length));
    response
}

#[cfg(test)]
#[path = "consensus_metrics_tests.rs"]
mod tests;
