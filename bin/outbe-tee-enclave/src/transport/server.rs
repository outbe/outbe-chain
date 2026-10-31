use std::sync::atomic::{AtomicUsize, Ordering};

use crate::transport::*;

/// Resident authority shared by the server connections.
pub struct ServerContext {
    pub chain_id: alloy_primitives::B256,
    pub initialization: Arc<InitializationState>,
    pub offer_key: SharedTributeOfferKey,
    pub boot: Option<Arc<EnclaveBootConfig>>,
    pub keys: Arc<EnclaveKeys>,
}

/// Hard cap on concurrently served enclave connections. Kept strictly below
/// the signed release manifest's `sgx.max_threads = 16` ceiling so unauthenticated
/// connection floods cannot exhaust SGX TCS slots needed by internal runtime threads.
pub const MAX_LIVE_CONNECTIONS: usize = 12;

struct LiveConnectionGuard(Arc<AtomicUsize>);

impl LiveConnectionGuard {
    fn try_acquire(counter: &Arc<AtomicUsize>, max_live: usize) -> Option<Self> {
        counter
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (current < max_live).then_some(current + 1)
            })
            .ok()
            .map(|_| Self(Arc::clone(counter)))
    }
}

impl Drop for LiveConnectionGuard {
    fn drop(&mut self) {
        let _ = self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Accept loop (used by the enclave binary). The loop serves each connection on
/// its own thread, so it handles multiple long-lived clients concurrently. The
/// node keeps one connection open for offer decryption for its whole lifetime
/// *and* opens a second one for the startup TEE-bootstrap registration fetch. A
/// sequential loop would deadlock the second behind the first. `keys` is
/// read-only and shared via `Arc`. Each connection still keeps its own
/// `DkgSessionStore`. The loop logs a per-connection error, and that error never
/// stops the server.
pub fn serve(listener: &UnixListener, context: ServerContext) -> Result<(), TransportError> {
    serve_with_limits(
        listener,
        context,
        MAX_LIVE_CONNECTIONS,
        HANDSHAKE_READ_TIMEOUT,
    )
}

pub(in crate::transport) fn serve_with_limits(
    listener: &UnixListener,
    context: ServerContext,
    max_live_connections: usize,
    handshake_timeout: Duration,
) -> Result<(), TransportError> {
    // The DKG-derived offer key is shared across all connection threads: the DKG
    // ceremony connection writes it (Seam F), the offer-decrypt connection reads
    // it. `main` may pre-seed it from a sealed blob (restart fast-path).
    let live = Arc::new(AtomicUsize::new(0));
    for conn in listener.incoming() {
        let stream = conn?;
        let Some(guard) = LiveConnectionGuard::try_acquire(&live, max_live_connections) else {
            // Drop the accepted socket. Do not log; a connection storm must not flood stderr.
            continue;
        };
        spawn_connection_worker(stream, guard, &context, handshake_timeout);
    }
    Ok(())
}

/// TCP accept loop. It uses the same thread-per-connection model as [`serve`],
/// but over TCP. This loop applies when the enclave runs under Gramine. There,
/// pathname Unix domain sockets are process-internal, and a host process (the
/// node) cannot reach them. Gramine passes TCP through to the host network.
/// The Noise-IK handshake still authenticates + encrypts every byte, so TCP only
/// changes the carrier, not the confidentiality of the channel.
pub fn serve_tcp(listener: &TcpListener, context: ServerContext) -> Result<(), TransportError> {
    serve_tcp_with_limits(
        listener,
        context,
        MAX_LIVE_CONNECTIONS,
        HANDSHAKE_READ_TIMEOUT,
    )
}

pub(in crate::transport) fn serve_tcp_with_limits(
    listener: &TcpListener,
    context: ServerContext,
    max_live_connections: usize,
    handshake_timeout: Duration,
) -> Result<(), TransportError> {
    let live = Arc::new(AtomicUsize::new(0));
    for conn in listener.incoming() {
        let stream = conn?;
        // Low-latency request/response (the protocol is many small round-trips).
        let _ = stream.set_nodelay(true);
        let Some(guard) = LiveConnectionGuard::try_acquire(&live, max_live_connections) else {
            // Drop the accepted socket. Do not log; a connection storm must not flood stderr.
            continue;
        };
        spawn_connection_worker(stream, guard, &context, handshake_timeout);
    }
    Ok(())
}

fn spawn_connection_worker<S>(
    stream: S,
    guard: LiveConnectionGuard,
    context: &ServerContext,
    handshake_timeout: Duration,
) where
    S: EnclaveTransportStream + Send + 'static,
{
    let keys = Arc::clone(&context.keys);
    let offer_key = Arc::clone(&context.offer_key);
    let boot = context.boot.clone();
    let chain_id = context.chain_id;
    let initialization = Arc::clone(&context.initialization);
    std::thread::spawn(move || {
        let _guard = guard;
        // PoC: surface to stderr. One bad client must not kill the enclave.
        if let Err(err) = serve_connection_with_resident_chain_and_timeout(
            stream,
            ConnectionContext {
                keys: &keys,
                offer_key: &offer_key,
                boot: boot.as_deref(),
                initialization: &initialization,
                chain_id,
                quote_generator: crate::gramine::dcap_quote,
            },
            handshake_timeout,
        ) {
            eprintln!("tee enclave: connection error: {err}");
        }
    });
}
