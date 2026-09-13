use crate::transport::*;

/// Accept loop (used by the enclave binary). Each connection is served on its
/// own thread so multiple long-lived clients are handled concurrently - the node
/// keeps one connection open for offer decryption for its whole lifetime *and*
/// opens a second one for the startup TEE-bootstrap registration fetch; a
/// sequential loop would deadlock the second behind the first. `keys` is
/// read-only and shared via `Arc`; each connection still keeps its own
/// `DkgSessionStore`. A per-connection error is logged and never stops the
/// server.
pub fn serve(
    listener: &UnixListener,
    keys: Arc<EnclaveKeys>,
    boot: Option<Arc<EnclaveBootConfig>>,
    offer_key: SharedTributeOfferKey,
    initialization: Arc<InitializationState>,
    chain_id: alloy_primitives::B256,
) -> Result<(), TransportError> {
    // The DKG-derived offer key is shared across all connection threads: the DKG
    // ceremony connection writes it (Seam F), the offer-decrypt connection reads
    // it. `main` may pre-seed it from a sealed blob (restart fast-path).
    for conn in listener.incoming() {
        let stream = conn?;
        let keys = Arc::clone(&keys);
        let offer_key = Arc::clone(&offer_key);
        let boot = boot.clone();
        let initialization = Arc::clone(&initialization);
        std::thread::spawn(move || {
            // PoC: surface to stderr; one bad client must not kill the enclave.
            if let Err(err) = serve_connection_with_resident_chain(
                stream,
                &keys,
                &offer_key,
                boot.as_deref(),
                &initialization,
                chain_id,
                crate::gramine::dcap_quote,
            ) {
                eprintln!("tee enclave: connection error: {err}");
            }
        });
    }
    Ok(())
}

/// TCP accept loop - same thread-per-connection model as [`serve`], but over
/// TCP. Used when the enclave runs under Gramine, where pathname Unix domain
/// sockets are process-internal and a host process (the node) cannot reach them;
/// Gramine passes TCP through to the host network. The Noise-IK handshake still
/// authenticates + encrypts every byte, so TCP only changes the carrier, not the
/// confidentiality of the channel.
pub fn serve_tcp(
    listener: &TcpListener,
    keys: Arc<EnclaveKeys>,
    boot: Option<Arc<EnclaveBootConfig>>,
    offer_key: SharedTributeOfferKey,
    initialization: Arc<InitializationState>,
    chain_id: alloy_primitives::B256,
) -> Result<(), TransportError> {
    for conn in listener.incoming() {
        let stream = conn?;
        // Low-latency request/response (the protocol is many small round-trips).
        let _ = stream.set_nodelay(true);
        let keys = Arc::clone(&keys);
        let offer_key = Arc::clone(&offer_key);
        let boot = boot.clone();
        let initialization = Arc::clone(&initialization);
        std::thread::spawn(move || {
            if let Err(err) = serve_connection_with_resident_chain(
                stream,
                &keys,
                &offer_key,
                boot.as_deref(),
                &initialization,
                chain_id,
                crate::gramine::dcap_quote,
            ) {
                eprintln!("tee enclave: connection error: {err}");
            }
        });
    }
    Ok(())
}
