mod commands;
mod handshake;

use crate::transport::*;

/// Transport carriers supported by the production enclave server. Remote
/// sessions use the socket read timeout to close an otherwise idle connection
/// at its exclusive finalized-lease deadline.
pub trait EnclaveTransportStream: Read + Write {
    fn set_session_read_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()>;
}

impl EnclaveTransportStream for UnixStream {
    fn set_session_read_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        UnixStream::set_read_timeout(self, timeout)
    }
}

impl EnclaveTransportStream for TcpStream {
    fn set_session_read_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        TcpStream::set_read_timeout(self, timeout)
    }
}

/// Serve a single client connection end-to-end (no boot config / sealing).
/// Thin wrapper over [`serve_connection_with`]; kept for tests and callers that
/// do not seal.
pub fn serve_connection<S: EnclaveTransportStream>(
    stream: S,
    keys: &EnclaveKeys,
    offer_key: &SharedTributeOfferKey,
) -> Result<(), TransportError> {
    let initialization = InitializationState::development();
    serve_connection_with(stream, keys, offer_key, None, &initialization)
}

/// Hardware-free network-bound transport seam for integration tests. It is
/// unavailable from production builds.
#[cfg(feature = "mock")]
pub fn serve_connection_for_network_test<S: EnclaveTransportStream>(
    stream: S,
    keys: &EnclaveKeys,
    offer_key: &SharedTributeOfferKey,
    network_binding: outbe_primitives::tee_attestation_v1::NetworkBindingV1,
) -> Result<(), TransportError> {
    let initialization = InitializationState::development_for_network(network_binding);
    serve_connection_with(stream, keys, offer_key, None, &initialization)
}

/// Serve a single client connection end-to-end. `offer_key` is the shared,
/// write-once DKG-derived offer key slot (populated by the DKG connection's
/// Seam F, read by the offer-decrypt path). `boot` carries the seal/unseal
/// configuration (chain_id / tee-dir / isv_svn). When `Some`, the sealing path
/// persists the offer secret + group threshold signature after Seam F. The production
/// accept loop passes the resident chain independently because non-sealing
/// enclaves still need a chain-scoped state-key domain.
pub fn serve_connection_with<S: EnclaveTransportStream>(
    stream: S,
    keys: &EnclaveKeys,
    offer_key: &SharedTributeOfferKey,
    boot: Option<&EnclaveBootConfig>,
    initialization: &InitializationState,
) -> Result<(), TransportError> {
    let chain_id = initialization
        .network_binding()
        .ok()
        .flatten()
        .map(|binding| B256::from(binding.chain_id))
        .unwrap_or(alloy_primitives::B256::ZERO);
    serve_connection_with_resident_chain(
        stream,
        ConnectionContext {
            keys,
            offer_key,
            boot,
            initialization,
            chain_id,
            quote_generator: crate::gramine::dcap_quote,
        },
    )
}

/// Explicit hardware-free lifecycle harness. The synthetic quote only carries
/// the requested REPORT_DATA through the normal candidate transport path; it is
/// never a QVL-positive or hardware-evidence seam. The production binary is
/// built without `mock`, so this entrypoint and generator are absent from it.
#[cfg(feature = "mock")]
pub fn serve_connection_with_synthetic_dcap<S: EnclaveTransportStream>(
    stream: S,
    keys: &EnclaveKeys,
    offer_key: &SharedTributeOfferKey,
    boot: Option<&EnclaveBootConfig>,
    initialization: &InitializationState,
) -> Result<(), TransportError> {
    let chain_id = initialization
        .network_binding()
        .ok()
        .flatten()
        .map(|binding| B256::from(binding.chain_id))
        .unwrap_or(alloy_primitives::B256::ZERO);
    serve_connection_with_resident_chain(
        stream,
        ConnectionContext {
            keys,
            offer_key,
            boot,
            initialization,
            chain_id,
            quote_generator: synthetic_dcap_quote,
        },
    )
}

#[derive(Clone, Copy)]
pub(in crate::transport) struct ConnectionContext<'a> {
    pub(in crate::transport) keys: &'a EnclaveKeys,
    pub(in crate::transport) offer_key: &'a SharedTributeOfferKey,
    pub(in crate::transport) boot: Option<&'a EnclaveBootConfig>,
    pub(in crate::transport) initialization: &'a InitializationState,
    pub(in crate::transport) chain_id: B256,
    pub(in crate::transport) quote_generator: fn(&[u8; 64]) -> Result<Vec<u8>, String>,
}

pub(in crate::transport) fn serve_connection_with_resident_chain<S: EnclaveTransportStream>(
    mut stream: S,
    context: ConnectionContext<'_>,
) -> Result<(), TransportError> {
    let Some(preamble) = handshake::read_preamble(&mut stream, context)? else {
        return Ok(());
    };
    let remote = preamble.remote;
    let authority = remote.map_or(SessionAuthorityV1::LocalNodeHost, |session| {
        SessionAuthorityV1::RemoteActiveNode {
            deadline: session.deadline(),
        }
    });
    set_remote_read_deadline(&stream, remote)?;
    let noise = handshake::negotiate(&mut stream, context, preamble)?;
    serve_requests(stream, context, noise, remote, authority)
}

fn serve_requests<S: EnclaveTransportStream>(
    mut stream: S,
    context: ConnectionContext<'_>,
    mut noise: snow::TransportState,
    remote: Option<PendingRemoteSessionV1>,
    authority: SessionAuthorityV1,
) -> Result<(), TransportError> {
    let peer = if context.initialization.mode() == InitializationMode::Development {
        "dev"
    } else if remote.is_some() {
        "remote"
    } else {
        "local"
    };
    let mut commands = commands::CommandSession::new();
    let mut call_stream = outbe_tee::call_context::StreamContext::default();
    while let Some(frame) = read_live_frame(&mut stream, context, remote, authority)? {
        let mut pt = vec![0u8; frame.len()];
        let n = noise
            .read_message(&frame, &mut pt)
            .map_err(|e| TransportError::Noise(e.to_string()))?;
        let call = outbe_tee::codec::decode_call(&pt[..n])?;
        call_stream.accept(&call.request, call.ctx)?;
        let _call_context = outbe_tee::call_context::ContextScope::enter(call.ctx);
        let response = match commands.prepare_response(call.request, context, authority, peer) {
            commands::Response::Immediate(response) => response,
            commands::Response::Admitted(response) => {
                ensure_admission(context, remote, authority)?;
                response
            }
        };
        write_encrypted_response(&mut stream, &mut noise, &response)?;
    }
    Ok(())
}

fn ensure_admission(
    context: ConnectionContext<'_>,
    remote: Option<PendingRemoteSessionV1>,
    authority: SessionAuthorityV1,
) -> Result<(), TransportError> {
    if let Some(session) = remote {
        context
            .initialization
            .ensure_remote_admission_current(session)
            .map_err(TransportError::Handshake)?;
    }
    authority
        .ensure_live()
        .map_err(|message| TransportError::Handshake(message.to_string()))
}

fn read_live_frame(
    stream: &mut impl EnclaveTransportStream,
    context: ConnectionContext<'_>,
    remote: Option<PendingRemoteSessionV1>,
    authority: SessionAuthorityV1,
) -> Result<Option<Vec<u8>>, TransportError> {
    ensure_admission(context, remote, authority)?;
    set_remote_read_deadline(stream, remote)?;
    let frame = match read_frame(stream) {
        Ok(frame) => frame,
        Err(TransportError::Io(error))
            if matches!(
                error.kind(),
                std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::BrokenPipe
            ) =>
        {
            return Ok(None)
        }
        Err(TransportError::Io(error))
            if remote.is_some()
                && matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
        {
            authority
                .ensure_live()
                .map_err(|message| TransportError::Handshake(message.to_string()))?;
            return Err(TransportError::Io(error));
        }
        Err(error) => return Err(error),
    };
    ensure_admission(context, remote, authority)?;
    Ok(Some(frame))
}

fn write_encrypted_response(
    stream: &mut impl EnclaveTransportStream,
    noise: &mut snow::TransportState,
    response: &EnclaveResponse,
) -> Result<(), TransportError> {
    let plain = encode_response(response)?;
    let mut ct = vec![0u8; plain.len() + 64];
    let n = noise
        .write_message(&plain, &mut ct)
        .map_err(|e| TransportError::Noise(e.to_string()))?;
    write_frame(stream, &ct[..n])?;
    Ok(())
}

fn set_remote_read_deadline(
    stream: &impl EnclaveTransportStream,
    remote_session: Option<PendingRemoteSessionV1>,
) -> Result<(), TransportError> {
    let Some(session) = remote_session else {
        return Ok(());
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| TransportError::Handshake("system time precedes Unix epoch".into()))?;
    let remaining = Duration::from_secs(session.deadline())
        .checked_sub(now)
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| TransportError::Handshake("remote session lease expired".into()))?;
    stream.set_session_read_timeout(Some(remaining))?;
    Ok(())
}
