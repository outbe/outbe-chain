use super::*;

pub(super) struct Preamble {
    pending: Option<PendingInitialization>,
    pub(super) remote: Option<PendingRemoteSessionV1>,
}

pub(super) fn read_preamble(
    stream: &mut impl EnclaveTransportStream,
    context: ConnectionContext<'_>,
) -> Result<Option<Preamble>, TransportError> {
    let ConnectionContext {
        keys,
        initialization,
        ..
    } = context;
    let first = decode_request(&read_frame(stream)?)?;
    let mut remote_session: Option<PendingRemoteSessionV1> = None;
    let pending: Option<PendingInitialization> = match (initialization.mode(), first) {
        (InitializationMode::Development, EnclaveRequest::GetQuote { nonce }) => {
            write_frame(stream, &encode_response(&keys.quote(nonce))?)?;
            None
        }
        (InitializationMode::Production, EnclaveRequest::GetInitializationChallenge) => {
            let response = initialization
                .challenge_response(keys)
                .map_err(TransportError::Handshake)?;
            write_frame(stream, &encode_response(&response)?)?;
            return Ok(None);
        }
        (
            InitializationMode::Production,
            EnclaveRequest::Initialize {
                manifest,
                node_signature,
            },
        ) => Some(
            initialization
                .prepare(&manifest, &node_signature, keys)
                .map_err(TransportError::Handshake)?,
        ),
        (InitializationMode::Production, EnclaveRequest::OpenSession) => {
            initialization
                .expected_node_host()
                .map_err(TransportError::Handshake)?;
            None
        }
        (InitializationMode::Production, EnclaveRequest::OpenRemoteSessionV1 { ticket_id }) => {
            remote_session = Some(
                initialization
                    .take_remote_session(ticket_id)
                    .map_err(TransportError::Handshake)?,
            );
            None
        }
        (InitializationMode::Development, _) => {
            return Err(TransportError::Handshake(
                "development transport expected GetQuote before handshake".to_string(),
            ));
        }
        (InitializationMode::Production, _) => {
            return Err(TransportError::Handshake(
                "production transport expected initialization discovery, Initialize, OpenSession, or OpenRemoteSessionV1"
                    .to_string(),
            ));
        }
    };

    Ok(Some(Preamble {
        pending,
        remote: remote_session,
    }))
}

pub(super) fn negotiate(
    stream: &mut impl EnclaveTransportStream,
    context: ConnectionContext<'_>,
    preamble: Preamble,
) -> Result<snow::TransportState, TransportError> {
    let ConnectionContext {
        keys,
        initialization,
        ..
    } = context;
    let Preamble {
        pending,
        remote: remote_session,
    } = preamble;
    // 2. Noise-IK responder handshake.
    let params = NOISE_PARAMS
        .parse()
        .map_err(|e| TransportError::Noise(format!("{e:?}")))?;
    let mut handshake = snow::Builder::new(params)
        .local_private_key(keys.noise_private())
        .build_responder()
        .map_err(|e| TransportError::Handshake(e.to_string()))?;

    let mut buf = [0u8; 1024];
    let msg1 = read_frame(stream)?;
    handshake
        .read_message(&msg1, &mut buf)
        .map_err(|e| TransportError::Handshake(e.to_string()))?;

    authenticate_initiator(&handshake, context, &pending, remote_session)?;

    let initialized_this_connection = pending.is_some();
    if let Some(pending) = pending {
        initialization
            .commit(pending, keys)
            .map_err(TransportError::Handshake)?;
    }

    let n = handshake
        .write_message(&[], &mut buf)
        .map_err(|e| TransportError::Handshake(e.to_string()))?;
    write_frame(stream, &buf[..n])?;

    let mut noise = handshake
        .into_transport_mode()
        .map_err(|e| TransportError::Handshake(e.to_string()))?;

    // Initialization success is disclosed only inside the newly authenticated
    // channel. OpenSession and the dev path wait for the first explicit command.
    if initialized_this_connection {
        let response = initialization
            .initialized_response()
            .map_err(TransportError::Handshake)?;
        write_encrypted_response(stream, &mut noise, &response)?;
    }

    Ok(noise)
}

fn authenticate_initiator(
    handshake: &snow::HandshakeState,
    context: ConnectionContext<'_>,
    pending: &Option<PendingInitialization>,
    remote_session: Option<PendingRemoteSessionV1>,
) -> Result<(), TransportError> {
    let initialization = context.initialization;
    // Noise IK authenticates the initiator static in message 1. Reject it here,
    // before message 2, transport mode, encrypted request decoding, or effects.
    if initialization.mode() == InitializationMode::Production {
        let expected = match remote_session {
            Some(session) => session.initiator_static_x25519(),
            None => pending
                .as_ref()
                .map(PendingInitialization::node_host_noise_x25519)
                .map(Ok)
                .unwrap_or_else(|| initialization.expected_node_host())
                .map_err(TransportError::Handshake)?,
        };
        let remote = handshake.get_remote_static().ok_or_else(|| {
            TransportError::Handshake("Noise IK message 1 omitted initiator static key".to_string())
        })?;
        if remote != expected {
            return Err(TransportError::Handshake(
                "Noise IK initiator is not the authorized NodeHost".to_string(),
            ));
        }
    }

    Ok(())
}
