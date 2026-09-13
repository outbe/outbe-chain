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
/// configuration (chain_id / tee-dir / isv_svn); when `Some`, the sealing path
/// persists the offer secret + threshold share after Seam F. The production
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
        keys,
        offer_key,
        boot,
        initialization,
        chain_id,
        crate::gramine::dcap_quote,
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
        keys,
        offer_key,
        boot,
        initialization,
        chain_id,
        synthetic_dcap_quote,
    )
}

pub(in crate::transport) fn serve_connection_with_resident_chain<S: EnclaveTransportStream>(
    mut stream: S,
    keys: &EnclaveKeys,
    offer_key: &SharedTributeOfferKey,
    boot: Option<&EnclaveBootConfig>,
    initialization: &InitializationState,
    chain_id: alloy_primitives::B256,
    quote_generator: fn(&[u8; 64]) -> Result<Vec<u8>, String>,
) -> Result<(), TransportError> {
    // 1. Minimal cleartext preamble. Production never emits a quote here.
    let first = decode_request(&read_frame(&mut stream)?)?;
    let mut remote_session: Option<PendingRemoteSessionV1> = None;
    let pending: Option<PendingInitialization> = match (initialization.mode(), first) {
        (InitializationMode::Development, EnclaveRequest::GetQuote { nonce }) => {
            write_frame(&mut stream, &encode_response(&keys.quote(nonce))?)?;
            None
        }
        (InitializationMode::Production, EnclaveRequest::GetInitializationChallenge) => {
            let response = initialization
                .challenge_response(keys)
                .map_err(TransportError::Handshake)?;
            write_frame(&mut stream, &encode_response(&response)?)?;
            return Ok(());
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
    let session_authority = remote_session.map_or(SessionAuthorityV1::LocalNodeHost, |session| {
        SessionAuthorityV1::RemoteActiveNode {
            deadline: session.deadline(),
        }
    });
    set_remote_read_deadline(&stream, remote_session)?;

    // 2. Noise-IK responder handshake.
    let params = NOISE_PARAMS
        .parse()
        .map_err(|e| TransportError::Noise(format!("{e:?}")))?;
    let mut handshake = snow::Builder::new(params)
        .local_private_key(keys.noise_private())
        .build_responder()
        .map_err(|e| TransportError::Handshake(e.to_string()))?;

    let mut buf = [0u8; 1024];
    let msg1 = read_frame(&mut stream)?;
    handshake
        .read_message(&msg1, &mut buf)
        .map_err(|e| TransportError::Handshake(e.to_string()))?;

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

    let initialized_this_connection = pending.is_some();
    if let Some(pending) = pending {
        initialization
            .commit(pending, keys)
            .map_err(TransportError::Handshake)?;
    }

    let n = handshake
        .write_message(&[], &mut buf)
        .map_err(|e| TransportError::Handshake(e.to_string()))?;
    write_frame(&mut stream, &buf[..n])?;

    let mut noise = handshake
        .into_transport_mode()
        .map_err(|e| TransportError::Handshake(e.to_string()))?;

    // Initialization success is disclosed only inside the newly authenticated
    // channel. OpenSession and the dev path wait for the first explicit command.
    if initialized_this_connection {
        let response = initialization
            .initialized_response()
            .map_err(TransportError::Handshake)?;
        let plain = encode_response(&response)?;
        let mut ct = vec![0u8; plain.len() + 64];
        let n = noise
            .write_message(&plain, &mut ct)
            .map_err(|e| TransportError::Noise(e.to_string()))?;
        write_frame(&mut stream, &ct[..n])?;
    }

    // Telemetry-only peer class for the per-request log line.
    let peer: &'static str = if initialization.mode() == InitializationMode::Development {
        "dev"
    } else if remote_session.is_some() {
        "remote"
    } else {
        "local"
    };

    // Resident DKG ceremonies for this connection. A ceremony spans many
    // request/response round-trips on one connection (PoC: one connection per
    // enclave for the whole ceremony).
    let mut dkg = DkgSessionStore::new();
    let mut dcap_verification = DcapVerificationSessionV1::default();
    let mut onboarding_upload = OnboardingArtifactUploadSessionV1::default();
    // Seal the DKG-derived offer key + share once installed (Seam F). Tracked
    // per-connection so we attempt the write-once seal at most once here.

    // 3. Encrypted request/response loop. Exits when the peer closes (read EOF).
    // Remote traffic is checked both before and after every blocking read, so a
    // frame arriving at or after the exclusive lease deadline is never decoded.
    loop {
        session_authority
            .ensure_live()
            .map_err(|message| TransportError::Handshake(message.to_string()))?;
        set_remote_read_deadline(&stream, remote_session)?;
        let frame = match read_frame(&mut stream) {
            Ok(frame) => frame,
            Err(TransportError::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::UnexpectedEof
                        | std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::BrokenPipe
                ) =>
            {
                break;
            }
            Err(TransportError::Io(error))
                if remote_session.is_some()
                    && matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
            {
                session_authority
                    .ensure_live()
                    .map_err(|message| TransportError::Handshake(message.to_string()))?;
                return Err(TransportError::Io(error));
            }
            Err(error) => return Err(error),
        };
        session_authority
            .ensure_live()
            .map_err(|message| TransportError::Handshake(message.to_string()))?;
        let mut pt = vec![0u8; frame.len()];
        let n = noise
            .read_message(&frame, &mut pt)
            .map_err(|e| TransportError::Noise(e.to_string()))?;
        let req = decode_request(&pt[..n])?;
        let req_label = req.label();
        let req_class = crate::initialization::request_class_label(&req);
        let req_started = std::time::SystemTime::now();
        let is_onboarding_upload_request = matches!(
            req,
            EnclaveRequest::BeginDcapOnboardingArtifactIngestV1 { .. }
                | EnclaveRequest::DcapOnboardingArtifactChunkV1 { .. }
                | EnclaveRequest::CommitDcapOnboardingArtifactRecordV1 { .. }
                | EnclaveRequest::FinishDcapOnboardingArtifactIngestV1 { .. }
        );
        if onboarding_upload.is_active() && !is_onboarding_upload_request {
            onboarding_upload.abort();
            let response = EnclaveResponse::Error {
                message: "onboarding artifact upload cannot be interleaved with another command"
                    .into(),
            };
            let plain = encode_response(&response)?;
            let mut ct = vec![0u8; plain.len() + 64];
            let n = noise
                .write_message(&plain, &mut ct)
                .map_err(|e| TransportError::Noise(e.to_string()))?;
            write_frame(&mut stream, &ct[..n])?;
            continue;
        }
        if let Err(message) =
            initialization.authorize_command(&req, offer_key.get().is_some(), session_authority)
        {
            if is_onboarding_upload_request {
                onboarding_upload.abort();
            }
            let (ts, dur_ms) = crate::telemetry::now_unix_and_elapsed_ms(req_started);
            crate::telemetry::record_request(req_class, crate::telemetry::RequestOutcome::Denied);
            eprintln!(
                "{}",
                crate::telemetry::format_request_log(
                    ts,
                    req_label,
                    peer,
                    crate::telemetry::RequestOutcome::Denied,
                    dur_ms,
                )
            );
            let response = EnclaveResponse::Error {
                message: message.to_string(),
            };
            let plain = encode_response(&response)?;
            let mut ct = vec![0u8; plain.len() + 64];
            let n = noise
                .write_message(&plain, &mut ct)
                .map_err(|e| TransportError::Noise(e.to_string()))?;
            write_frame(&mut stream, &ct[..n])?;
            continue;
        }

        let resp = match req {
            EnclaveRequest::PrepareGramineDirectDevOnboardingArtifactV1 {
                request_hash,
                context,
            } => match initialization.manifest() {
                Ok(manifest) => complete_gramine_direct_dev_onboarding_response(
                    request_hash,
                    &context,
                    offer_key.get(),
                    manifest.as_ref(),
                ),
                Err(message) => EnclaveResponse::Error { message },
            },
            request @ (EnclaveRequest::BeginDcapVerificationV1 { .. }
            | EnclaveRequest::BeginDcapOnboardingVerificationV1 { .. }
            | EnclaveRequest::DcapVerificationChunkV1 { .. }
            | EnclaveRequest::FinishDcapVerificationV1 { .. }) => {
                if initialization.mode() != InitializationMode::Production {
                    EnclaveResponse::Error {
                        message: "DCAP verification requires initialized production state"
                            .to_string(),
                    }
                } else {
                    match dcap_verification.handle(request) {
                        Ok(DcapVerificationProgressV1::Started { request_hash }) => {
                            EnclaveResponse::DcapVerificationStartedV1 { request_hash }
                        }
                        Ok(DcapVerificationProgressV1::ChunkAccepted {
                            request_hash,
                            next_offset,
                        }) => EnclaveResponse::DcapVerificationChunkAcceptedV1 {
                            request_hash,
                            next_offset,
                        },
                        Ok(DcapVerificationProgressV1::Complete(request)) => {
                            match initialization.manifest() {
                                Ok(manifest) => complete_verification_response(
                                    *request,
                                    keys,
                                    offer_key.get(),
                                    manifest.as_ref(),
                                ),
                                Err(message) => EnclaveResponse::Error { message },
                            }
                        }
                        Err(message) => EnclaveResponse::Error {
                            message: message.to_string(),
                        },
                    }
                }
            }
            request @ (EnclaveRequest::BeginDcapOnboardingArtifactIngestV1 { .. }
            | EnclaveRequest::DcapOnboardingArtifactChunkV1 { .. }
            | EnclaveRequest::CommitDcapOnboardingArtifactRecordV1 { .. }
            | EnclaveRequest::FinishDcapOnboardingArtifactIngestV1 { .. }) => {
                match onboarding_upload.handle(request, initialization.trusted_network_descriptor())
                {
                    Ok(OnboardingArtifactUploadProgressV1::Started { request_hash }) => {
                        EnclaveResponse::DcapOnboardingArtifactIngestStartedV1 { request_hash }
                    }
                    Ok(OnboardingArtifactUploadProgressV1::ChunkAccepted {
                        request_hash,
                        next_offset,
                    }) => EnclaveResponse::DcapOnboardingArtifactChunkAcceptedV1 {
                        request_hash,
                        next_offset,
                    },
                    Ok(OnboardingArtifactUploadProgressV1::RecordAccepted {
                        request_hash,
                        kind,
                    }) => EnclaveResponse::DcapOnboardingArtifactRecordAcceptedV1 {
                        request_hash,
                        kind,
                    },
                    Ok(OnboardingArtifactUploadProgressV1::Complete(complete)) => {
                        complete_onboarding_artifact_ingest_response(
                            *complete,
                            keys,
                            offer_key,
                            boot,
                            initialization,
                        )
                    }
                    Err(message) => EnclaveResponse::Error {
                        message: message.to_string(),
                    },
                }
            }
            request => dispatch_with_initialization(
                request,
                keys,
                &mut dkg,
                offer_key,
                chain_id,
                DispatchInitializationContext {
                    boot,
                    initialization: Some(initialization),
                    quote_generator,
                },
            ),
        };

        let outcome = if matches!(resp, EnclaveResponse::Error { .. }) {
            crate::telemetry::RequestOutcome::Err
        } else {
            crate::telemetry::RequestOutcome::Ok
        };
        let (ts, dur_ms) = crate::telemetry::now_unix_and_elapsed_ms(req_started);
        crate::telemetry::record_request(req_class, outcome);
        eprintln!(
            "{}",
            crate::telemetry::format_request_log(ts, req_label, peer, outcome, dur_ms)
        );

        session_authority
            .ensure_live()
            .map_err(|message| TransportError::Handshake(message.to_string()))?;

        let plain = encode_response(&resp)?;
        let mut ct = vec![0u8; plain.len() + 64];
        let n = noise
            .write_message(&plain, &mut ct)
            .map_err(|e| TransportError::Noise(e.to_string()))?;
        write_frame(&mut stream, &ct[..n])?;
    }
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
