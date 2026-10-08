use crate::transport::*;

/// Resident authority shared by the server connections.
pub struct ServerContext {
    pub chain_id: alloy_primitives::B256,
    pub initialization: Arc<InitializationState>,
    pub offer_key: SharedTributeOfferKey,
    pub boot: Option<Arc<EnclaveBootConfig>>,
    pub keys: Arc<EnclaveKeys>,
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
    // The DKG-derived offer key is shared across all connection threads: the DKG
    // ceremony connection writes it (Seam F), the offer-decrypt connection reads
    // it. `main` may pre-seed it from a sealed blob (restart fast-path).
    for conn in listener.incoming() {
        let stream = conn?;
        let keys = Arc::clone(&context.keys);
        let offer_key = Arc::clone(&context.offer_key);
        let boot = context.boot.clone();
        let chain_id = context.chain_id;
        let initialization = Arc::clone(&context.initialization);
        std::thread::spawn(move || {
            // PoC: surface to stderr. One bad client must not kill the enclave.
            if let Err(err) = serve_connection_with_resident_chain(
                stream,
                ConnectionContext {
                    keys: &keys,
                    offer_key: &offer_key,
                    boot: boot.as_deref(),
                    initialization: &initialization,
                    chain_id,
                    quote_generator: crate::gramine::dcap_quote,
                },
            ) {
                eprintln!("tee enclave: connection error: {err}");
            }
        });
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
    for conn in listener.incoming() {
        let stream = conn?;
        // Low-latency request/response (the protocol is many small round-trips).
        let _ = stream.set_nodelay(true);
        let keys = Arc::clone(&context.keys);
        let offer_key = Arc::clone(&context.offer_key);
        let boot = context.boot.clone();
        let chain_id = context.chain_id;
        let initialization = Arc::clone(&context.initialization);
        std::thread::spawn(move || {
            if let Err(err) = serve_connection_with_resident_chain(
                stream,
                ConnectionContext {
                    keys: &keys,
                    offer_key: &offer_key,
                    boot: boot.as_deref(),
                    initialization: &initialization,
                    chain_id,
                    quote_generator: crate::gramine::dcap_quote,
                },
            ) {
                eprintln!("tee enclave: connection error: {err}");
            }
        });
    }
    Ok(())
}
