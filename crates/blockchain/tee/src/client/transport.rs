use super::*;

/// The byte carrier under the Noise-IK session: a local Unix domain socket
/// (native sidecar) or TCP (used when the enclave runs under Gramine, whose
/// pathname UDS are process-internal). Noise authenticates + encrypts every
/// byte regardless, so the carrier does not change the channel's security.
pub(super) enum Transport {
    Unix(UnixStream),
    Tcp(TcpStream),
}

impl Read for Transport {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Transport::Unix(s) => s.read(buf),
            Transport::Tcp(s) => s.read(buf),
        }
    }
}

impl Write for Transport {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Transport::Unix(s) => s.write(buf),
            Transport::Tcp(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Transport::Unix(s) => s.flush(),
            Transport::Tcp(s) => s.flush(),
        }
    }
}

/// Bound on a single blocking enclave read/write. A wedged enclave that accepts
/// the connection but never responds surfaces as a timeout error instead of
/// hanging the caller (e.g. node startup) forever. The bound is generous: every
/// real enclave op (quote, Noise handshake, seal, offer-batch decrypt) completes
/// well within it.
const DEFAULT_ENCLAVE_IO_TIMEOUT_SECS: u64 = 30;

/// Hardware SGX can spend substantially longer than gramine-direct servicing a
/// request while EPC pages are reclaimed. The default remains fail-fast for
/// production/dev, while SGX stress/E2E runners may raise the bound explicitly.
pub(super) fn enclave_io_timeout() -> std::time::Duration {
    static TIMEOUT: std::sync::OnceLock<std::time::Duration> = std::sync::OnceLock::new();
    *TIMEOUT.get_or_init(|| {
        let value = std::env::var("OUTBE_TEE_IO_TIMEOUT_SECS").ok();
        let seconds = timeout_seconds_from(value.as_deref());
        std::time::Duration::from_secs(seconds)
    })
}

pub(super) fn timeout_seconds_from(value: Option<&str>) -> u64 {
    value
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .unwrap_or(DEFAULT_ENCLAVE_IO_TIMEOUT_SECS)
}

pub(super) fn with_io_phase<T>(
    result: Result<T, TransportError>,
    operation: &'static str,
) -> Result<T, TransportError> {
    result.map_err(|error| match error {
        TransportError::Io(source)
            if matches!(
                source.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ) =>
        {
            TransportError::IoTimeout {
                operation,
                timeout_secs: enclave_io_timeout().as_secs(),
            }
        }
        other => other,
    })
}

pub(super) fn connect_endpoint_transport(endpoint: &str) -> Result<Transport, TransportError> {
    if endpoint.contains(':') {
        let stream = TcpStream::connect(endpoint)?;
        let _ = stream.set_nodelay(true);
        stream.set_read_timeout(Some(enclave_io_timeout()))?;
        stream.set_write_timeout(Some(enclave_io_timeout()))?;
        Ok(Transport::Tcp(stream))
    } else {
        let stream = UnixStream::connect(endpoint)?;
        stream.set_read_timeout(Some(enclave_io_timeout()))?;
        stream.set_write_timeout(Some(enclave_io_timeout()))?;
        Ok(Transport::Unix(stream))
    }
}
