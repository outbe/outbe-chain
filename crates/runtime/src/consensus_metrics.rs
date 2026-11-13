//! Clone-free binding contract for the Commonware consensus metrics server.

// HTTP dependencies are staged for the OpenMetrics HTTP serving loop.
use std::{net::SocketAddr, time::Duration};
use tokio::net::{TcpListener, TcpSocket};
use {bytes as _, http as _, http_body_util as _, hyper as _, hyper_util as _};

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

/// Binds a metrics socket and keeps it alive under Commonware supervision.
pub fn spawn_consensus_metrics_server<C>(
    metrics_ctx: C,
    listen_addr: SocketAddr,
    shutdown: tokio_util::sync::CancellationToken,
) -> Result<ConsensusMetricsHandle, ConsensusMetricsServerError>
where
    C: commonware_runtime::Metrics + commonware_runtime::Spawner,
{
    let (listener, local_addr) = bind_reuseaddr_listener(listen_addr)?;
    metrics_ctx.spawn(move |_ctx| async move {
        // shortcut: retain the listener until cancellation; replace with the HTTP serving loop.
        let _keep = listener;
        shutdown.cancelled().await;
    });
    Ok(ConsensusMetricsHandle { local_addr })
}

#[cfg(test)]
#[path = "consensus_metrics_tests.rs"]
mod tests;
