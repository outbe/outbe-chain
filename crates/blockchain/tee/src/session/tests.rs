use super::*;
use crate::codec::{decode_request, encode_response};
use crate::codec::{read_frame, write_frame};
use alloy_primitives::keccak256;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Scriptable fake dev enclave: real cleartext-quote + Noise-IK responder
/// wire, per-connection behavior driven by [`ServerScript`].
struct FakeEnclave {
    noise_private: Vec<u8>,
    noise_public: [u8; 32],
    recipient: [u8; 32],
    attestation_pub: [u8; 32],
    script: Arc<ServerScript>,
}

#[derive(Default)]
struct ServerScript {
    /// Hold a Health response until the test releases it. Other connections
    /// must remain usable while this request is in flight.
    health_gate: Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
    /// Offer pubkey reported by `GetPublicKeys`. `None` = keyless.
    offer_key: Mutex<Option<[u8; 32]>>,
    /// Drop the connection after serving this many post-handshake requests
    /// (0 = unlimited).
    drop_after_requests: AtomicU64,
    /// Answer every non-GetPublicKeys request with `EnclaveResponse::Error`.
    error_mode: std::sync::atomic::AtomicBool,
    /// Labels of every post-handshake request served, across connections.
    served: Mutex<Vec<&'static str>>,
    contexts: Mutex<Vec<crate::call_context::EnclaveCallContextV1>>,
}

impl FakeEnclave {
    fn generate(script: Arc<ServerScript>) -> Self {
        let builder = snow::Builder::new(crate::NOISE_PARAMS.parse().expect("noise params"));
        let keys = builder.generate_keypair().expect("keypair");
        let noise_public: [u8; 32] = keys.public.as_slice().try_into().expect("32-byte key");
        Self {
            noise_private: keys.private,
            noise_public,
            recipient: [0x0B; 32],
            attestation_pub: [0xAA; 32],
            script,
        }
    }

    fn quote_response(&self) -> EnclaveResponse {
        let mut preimage = Vec::with_capacity(96);
        preimage.extend_from_slice(&self.noise_public);
        preimage.extend_from_slice(&self.recipient);
        preimage.extend_from_slice(&self.attestation_pub);
        EnclaveResponse::Quote {
            mrenclave: alloy_primitives::B256::repeat_byte(0x01),
            mrsigner: alloy_primitives::B256::repeat_byte(0x02),
            isv_svn: 1,
            report_data: keccak256(&preimage),
            recipient_x25519_pub: self.recipient,
            attestation_pub: self.attestation_pub,
            noise_static_pub: self.noise_public,
            quote_body: Vec::new(),
            attestation: "none (session-test)".to_string(),
        }
    }

    fn serve_connection(&self, mut stream: UnixStream) -> Result<(), String> {
        let mut noise = self.establish_session(&mut stream)?;
        self.serve_encrypted_requests(&mut stream, &mut noise)
    }

    fn establish_session(
        &self,
        mut stream: &mut UnixStream,
    ) -> Result<snow::TransportState, String> {
        // 1. Cleartext GetQuote.
        let frame = read_frame(&mut stream).map_err(|e| e.to_string())?;
        let request = decode_request(&frame).map_err(|e| e.to_string())?;
        if !matches!(request, EnclaveRequest::GetQuote { .. }) {
            return Err("expected GetQuote".into());
        }
        let body = encode_response(&self.quote_response()).map_err(|e| e.to_string())?;
        write_frame(&mut stream, &body).map_err(|e| e.to_string())?;
        // 2. Noise-IK responder handshake.
        let mut handshake = snow::Builder::new(
            crate::NOISE_PARAMS
                .parse()
                .map_err(|_| "params".to_string())?,
        )
        .local_private_key(&self.noise_private)
        .build_responder()
        .map_err(|e| e.to_string())?;
        let msg1 = read_frame(&mut stream).map_err(|e| e.to_string())?;
        let mut buf = [0u8; 1024];
        handshake
            .read_message(&msg1, &mut buf)
            .map_err(|e| e.to_string())?;
        let n = handshake
            .write_message(&[], &mut buf)
            .map_err(|e| e.to_string())?;
        write_frame(&mut stream, &buf[..n]).map_err(|e| e.to_string())?;
        handshake.into_transport_mode().map_err(|e| e.to_string())
    }

    fn read_encrypted_request(
        &self,
        stream: &mut UnixStream,
        noise: &mut snow::TransportState,
    ) -> Result<Option<EnclaveRequest>, String> {
        let Ok(frame) = read_frame(stream) else {
            return Ok(None); // client closed
        };
        let mut pt = vec![0u8; frame.len()];
        let n = noise
            .read_message(&frame, &mut pt)
            .map_err(|e| e.to_string())?;
        let call = crate::codec::decode_call(&pt[..n]).map_err(|e| e.to_string())?;
        self.script.contexts.lock().unwrap().push(call.ctx);
        let request = call.request;
        if let Ok(mut served) = self.script.served.lock() {
            served.push(request.label());
        }
        Ok(Some(request))
    }

    fn wait_for_health_gate(&self, request: &EnclaveRequest) -> Result<(), String> {
        if matches!(request, EnclaveRequest::Health) {
            let gate = self.script.health_gate.lock().expect("health gate").take();
            if let Some((entered, release)) = gate {
                entered.send(()).map_err(|e| e.to_string())?;
                release.recv().map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }

    fn scripted_response(&self, request: &EnclaveRequest) -> Result<EnclaveResponse, String> {
        let response = match request {
            EnclaveRequest::GetPublicKeys => {
                let offer = *self
                    .script
                    .offer_key
                    .lock()
                    .map_err(|_| "offer_key lock".to_string())?;
                EnclaveResponse::PublicKeys {
                    offer_key_ready: offer.is_some(),
                    recipient_x25519_pub: offer.unwrap_or(self.recipient),
                    attestation_pub: self.attestation_pub,
                    noise_static_pub: self.noise_public,
                    tee_bls_pub: Vec::new(),
                    dkg_enc_pub: [0x0D; 32],
                    dkg_enc_sig: Vec::new(),
                }
            }
            _ if self.script.error_mode.load(Ordering::Relaxed) => EnclaveResponse::Error {
                message: "scripted deterministic error".to_string(),
            },
            _ => EnclaveResponse::Ack,
        };
        Ok(response)
    }

    fn write_encrypted_response(
        stream: &mut UnixStream,
        noise: &mut snow::TransportState,
        response: &EnclaveResponse,
    ) -> Result<(), String> {
        let plain = encode_response(response).map_err(|e| e.to_string())?;
        let mut ct = vec![0u8; plain.len() + 64];
        let n = noise
            .write_message(&plain, &mut ct)
            .map_err(|e| e.to_string())?;
        write_frame(stream, &ct[..n]).map_err(|e| e.to_string())
    }

    fn serve_encrypted_requests(
        &self,
        stream: &mut UnixStream,
        noise: &mut snow::TransportState,
    ) -> Result<(), String> {
        // 3. Encrypted request loop.
        let mut served_here: u64 = 0;
        loop {
            let Some(request) = self.read_encrypted_request(stream, noise)? else {
                return Ok(());
            };
            self.wait_for_health_gate(&request)?;
            served_here += 1;
            let drop_after = self.script.drop_after_requests.load(Ordering::Relaxed);
            if drop_after != 0 && served_here > drop_after {
                return Ok(()); // simulate a mid-session connection loss
            }
            let response = self.scripted_response(&request)?;
            Self::write_encrypted_response(stream, noise, &response)?;
        }
    }
}

struct RunningServer {
    endpoint: String,
    stop: Arc<std::sync::atomic::AtomicBool>,
    _dir: tempfile::TempDir,
}

fn spawn_server(enclave: Arc<FakeEnclave>) -> RunningServer {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("enclave.sock");
    let endpoint = path.to_string_lossy().into_owned();
    let listener = UnixListener::bind(&path).expect("bind");
    listener.set_nonblocking(true).expect("nonblocking");
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_flag = Arc::clone(&stop);
    std::thread::spawn(move || {
        while !stop_flag.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false).ok();
                    let enclave = Arc::clone(&enclave);
                    std::thread::spawn(move || {
                        let _ = enclave.serve_connection(stream);
                    });
                }
                Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(_) => break,
            }
        }
    });
    RunningServer {
        endpoint,
        stop,
        _dir: dir,
    }
}

fn ready_script() -> Arc<ServerScript> {
    let script = Arc::new(ServerScript::default());
    if let Ok(mut key) = script.offer_key.lock() {
        *key = Some([0x0E; 32]);
    }
    script
}

fn connect_session(server: &RunningServer) -> EnclaveSession {
    let client = EnclaveClient::connect_endpoint(&server.endpoint).expect("connect");
    EnclaveSession::development(client, server.endpoint.clone()).expect("session")
}

#[test]
fn blocked_canary_does_not_block_execution_connection() {
    use crate::client_global::EnclaveSessions;
    use std::sync::mpsc::channel;
    use std::time::Duration;

    let script = ready_script();
    let server = spawn_server(Arc::new(FakeEnclave::generate(Arc::clone(&script))));
    let sessions = Arc::new(EnclaveSessions::new(connect_session(&server)));
    let (entered_tx, entered_rx) = channel();
    let (release_tx, release_rx) = channel();
    *script.health_gate.lock().unwrap() = Some((entered_tx, release_rx));
    let canary_sessions = Arc::clone(&sessions);
    let canary =
        std::thread::spawn(move || canary_sessions.canary_request(&EnclaveRequest::Health));
    entered_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("canary in flight");
    let execution_sessions = Arc::clone(&sessions);
    let (finished_tx, finished_rx) = channel();
    let execution = std::thread::spawn(move || {
        let result = execution_sessions
            .with_execution(|session| session.request(&EnclaveRequest::GetPublicKeys));
        finished_tx.send(result).unwrap();
    });
    let completed_while_canary_blocked = finished_rx.recv_timeout(Duration::from_secs(5));
    // Release before asserting so a broken implementation cannot strand
    // either request thread holding a session lock.
    release_tx.send(()).unwrap();
    canary.join().unwrap().expect("canary response");
    execution.join().unwrap();
    assert!(matches!(
        completed_while_canary_blocked.unwrap().unwrap(),
        EnclaveResponse::PublicKeys { .. }
    ));
    server.stop.store(true, Ordering::Relaxed);
}

#[test]
fn canary_rejects_non_probe_requests_without_transport_io() {
    let script = ready_script();
    let server = spawn_server(Arc::new(FakeEnclave::generate(Arc::clone(&script))));
    let sessions = crate::client_global::EnclaveSessions::new(connect_session(&server));
    let before = script.served.lock().unwrap().clone();
    let error = sessions
        .canary_request(&EnclaveRequest::GetQuote { nonce: [0; 32] })
        .expect_err("not a canary probe");
    assert!(matches!(error, TransportError::EnclaveError(_)));
    assert_eq!(*script.served.lock().unwrap(), before);
    server.stop.store(true, Ordering::Relaxed);
}

#[test]
fn forked_connection_preserves_offer_key_pin_and_revocation() {
    let script = ready_script();
    let server = spawn_server(Arc::new(FakeEnclave::generate(Arc::clone(&script))));
    let installed = connect_session(&server);
    let mut fork = installed.fork_connection();
    assert_eq!(fork.attestation_pub(), installed.attestation_pub());
    *script.offer_key.lock().unwrap() = Some([0x99; 32]);
    assert!(matches!(
        fork.request(&EnclaveRequest::GetPublicKeys),
        Err(TransportError::IdentityMismatch(_))
    ));
    assert!(matches!(
        fork.request(&EnclaveRequest::Health),
        Err(TransportError::SessionRevoked(_))
    ));
    assert!(matches!(
        fork.fork_connection().request(&EnclaveRequest::Health),
        Err(TransportError::SessionRevoked(_))
    ));
    server.stop.store(true, Ordering::Relaxed);
}

#[test]
fn exact_call_context_survives_noise_reconnect_and_historical_replay() {
    use crate::call_context::{EnclaveCallContextV1, EnclaveContextKindV1};
    let script = ready_script();
    script.drop_after_requests.store(2, Ordering::Relaxed);
    let server = spawn_server(Arc::new(FakeEnclave::generate(Arc::clone(&script))));
    let mut session = connect_session(&server);
    let next = EnclaveCallContextV1 {
        kind: EnclaveContextKindV1::Execution,
        chain_id: 42,
        genesis_hash: B256::repeat_byte(7),
        block_number: 100,
        block_timestamp: 200,
        protocol_version: 2,
    };
    session
        .request_with_context(next, &EnclaveRequest::Health)
        .unwrap();
    // Historical replay is valid even after observing a newer version.
    let old = EnclaveCallContextV1 {
        block_number: 99,
        block_timestamp: 198,
        protocol_version: 1,
        ..next
    };
    session
        .request_with_context(old, &EnclaveRequest::Health)
        .unwrap();
    let contexts = script.contexts.lock().unwrap();
    assert_eq!(contexts[1], next);
    // Failed request, reconnect probe and successful retry all retain old.
    assert_eq!(&contexts[2..], &[old, old, old]);
    drop(contexts);
    server.stop.store(true, Ordering::Relaxed);
}

#[test]
fn transport_fault_reconnects_and_retries_idempotent_request() {
    let script = ready_script();
    script.drop_after_requests.store(2, Ordering::Relaxed);
    let enclave = Arc::new(FakeEnclave::generate(Arc::clone(&script)));
    let server = spawn_server(Arc::clone(&enclave));
    let mut session = connect_session(&server);
    assert_eq!(session.generation(), 1);
    // Install consumed request #1 (the probe). #2 succeeds. #3 gets the
    // connection dropped mid-request -> reconnect (fresh connection, its own
    // probe) -> retry succeeds.
    let first = session.request(&EnclaveRequest::GetPublicKeys).expect("ok");
    assert!(matches!(first, EnclaveResponse::PublicKeys { .. }));
    // Request #3 on connection 1 exceeds the per-connection budget -> the
    // server drops it mid-request. The reconnect's fresh connection serves
    // its probe + the retry within the same budget.
    let retried = session
        .request(&EnclaveRequest::GetPublicKeys)
        .expect("retried after reconnect");
    assert!(matches!(retried, EnclaveResponse::PublicKeys { .. }));
    assert_eq!(session.generation(), 2, "exactly one reconnect");
    server.stop.store(true, Ordering::Relaxed);
}

#[test]
fn failed_reconnect_returns_error_without_revoking() {
    let script = ready_script();
    let enclave = Arc::new(FakeEnclave::generate(Arc::clone(&script)));
    let server = spawn_server(Arc::clone(&enclave));
    let mut session = connect_session(&server);
    // Kill the listener. The pinned connection dies with the next fault.
    server.stop.store(true, Ordering::Relaxed);
    std::thread::sleep(std::time::Duration::from_millis(20));
    drop(server);
    // The old connection is still alive server-side (thread-per-conn), so
    // force a fresh reconnect target by dropping the client state:
    session.client = None;
    let error = session
        .request(&EnclaveRequest::GetPublicKeys)
        .expect_err("reconnect target is gone");
    assert!(
        !matches!(error, TransportError::SessionRevoked(_)),
        "connect failure must stay retryable, got: {error}"
    );
    // The test cannot rebuild a new server at the SAME endpoint path (tempdir
    // dropped). The point above (no revocation) is the invariant.
}

#[test]
fn identity_mismatch_on_reconnect_revokes_permanently() {
    let script = ready_script();
    let enclave = Arc::new(FakeEnclave::generate(Arc::clone(&script)));
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("enclave.sock");
    let endpoint = path.to_string_lossy().into_owned();

    // Server #1: original identity.
    let listener = UnixListener::bind(&path).expect("bind");
    let enclave1 = Arc::clone(&enclave);
    std::thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            let _ = enclave1.serve_connection(stream);
        }
        // one connection only, then stop accepting
    });
    let client = EnclaveClient::connect_endpoint(&endpoint).expect("connect");
    let mut session = EnclaveSession::development(client, endpoint.clone()).expect("session");

    // Server #2 on the same path: DIFFERENT identity keys.
    std::fs::remove_file(&path).expect("remove socket");
    let impostor = Arc::new(FakeEnclave::generate(ready_script()));
    let listener = UnixListener::bind(&path).expect("rebind");
    let impostor_for_conn = Arc::clone(&impostor);
    std::thread::spawn(move || {
        while let Ok((stream, _)) = listener.accept() {
            let enclave = Arc::clone(&impostor_for_conn);
            std::thread::spawn(move || {
                let _ = enclave.serve_connection(stream);
            });
        }
    });

    session.client = None; // force reconnect against the impostor
    let error = session
        .request(&EnclaveRequest::GetPublicKeys)
        .expect_err("identity mismatch");
    assert!(
        matches!(error, TransportError::IdentityMismatch(_)),
        "got: {error}"
    );
    // Fail-closed: every later request refuses fast, no new connection.
    let error = session
        .request(&EnclaveRequest::GetPublicKeys)
        .expect_err("revoked");
    assert!(matches!(error, TransportError::SessionRevoked(_)));
}

#[test]
fn offer_key_pin_upgrades_from_keyless_but_revokes_on_change() {
    // Start keyless.
    let script = Arc::new(ServerScript::default());
    let enclave = Arc::new(FakeEnclave::generate(Arc::clone(&script)));
    let server = spawn_server(Arc::clone(&enclave));
    let mut session = connect_session(&server);
    assert!(session.pinned_offer_public.is_none());

    // Key appears (post-DKG): reconnect upgrades the pin.
    if let Ok(mut key) = script.offer_key.lock() {
        *key = Some([0x0E; 32]);
    }
    session.client = None;
    session.request(&EnclaveRequest::GetPublicKeys).expect("ok");
    assert_eq!(
        session.pinned_offer_public,
        Some(B256::from([0x0E; 32])),
        "None -> Some upgrades the pin"
    );

    // Key changes: reconnect must revoke.
    if let Ok(mut key) = script.offer_key.lock() {
        *key = Some([0x0F; 32]);
    }
    session.client = None;
    let error = session
        .request(&EnclaveRequest::GetPublicKeys)
        .expect_err("offer key changed");
    assert!(matches!(error, TransportError::IdentityMismatch(_)));
    assert!(matches!(
        session.request(&EnclaveRequest::GetPublicKeys),
        Err(TransportError::SessionRevoked(_))
    ));
    server.stop.store(true, Ordering::Relaxed);
}

#[test]
fn reconnect_revokes_when_a_pinned_offer_key_disappears() {
    let script = ready_script();
    let enclave = Arc::new(FakeEnclave::generate(Arc::clone(&script)));
    let server = spawn_server(Arc::clone(&enclave));
    let mut session = connect_session(&server);
    assert!(session.pinned_offer_public.is_some());
    *script.offer_key.lock().expect("offer key") = None;
    session.client = None;

    let error = session
        .request(&EnclaveRequest::GetPublicKeys)
        .expect_err("pinned key disappeared");
    assert!(matches!(error, TransportError::IdentityMismatch(_)));
    assert!(matches!(
        session.request(&EnclaveRequest::GetPublicKeys),
        Err(TransportError::SessionRevoked(_))
    ));
    assert_eq!(session.generation(), 1, "failed reconnect cannot advance");
    server.stop.store(true, Ordering::Relaxed);
}

#[test]
fn reconnect_rejects_ready_but_zero_offer_key_without_revoking() {
    let script = Arc::new(ServerScript::default());
    let enclave = Arc::new(FakeEnclave::generate(Arc::clone(&script)));
    let server = spawn_server(Arc::clone(&enclave));
    let mut session = connect_session(&server);
    assert!(session.pinned_offer_public.is_none());
    *script.offer_key.lock().expect("offer key") = Some([0; 32]);
    session.client = None;

    let error = session
        .request(&EnclaveRequest::GetPublicKeys)
        .expect_err("ready zero key is invalid");
    assert!(matches!(
        error,
        TransportError::EnclaveError(message)
            if message == "local enclave reports a ready but zero permanent offer key"
    ));
    assert!(session.revoked.is_none(), "probe fault remains retryable");
    assert_eq!(session.generation(), 1, "failed reconnect cannot advance");
    server.stop.store(true, Ordering::Relaxed);
}

#[test]
fn non_idempotent_request_is_not_resent_after_reconnect() {
    let script = ready_script();
    let enclave = Arc::new(FakeEnclave::generate(Arc::clone(&script)));
    let server = spawn_server(Arc::clone(&enclave));
    let mut session = connect_session(&server);
    // Drop the connection at the next (non-idempotent) request.
    script.drop_after_requests.store(1, Ordering::Relaxed);
    // Reset so the NEXT connection (reconnect) is unlimited again.
    let request = EnclaveRequest::DkgStartDealer {
        ceremony_id: alloy_primitives::B256::repeat_byte(0x33),
    };
    assert!(!request.is_idempotent());
    let before_generation = session.generation();
    let error = session.request(&request);
    // Restore unlimited serving for the reconnect probe.
    script.drop_after_requests.store(0, Ordering::Relaxed);
    assert!(error.is_err(), "original transport error must propagate");
    let served = script.served.lock().expect("served").clone();
    assert_eq!(
        served
            .iter()
            .filter(|label| **label == "dkg_start_dealer")
            .count(),
        1,
        "non-idempotent request must never be re-sent: {served:?}"
    );
    // The session healed for the next caller (reconnect happened).
    assert!(session.generation() > before_generation);
    let ok = session.request(&EnclaveRequest::GetPublicKeys);
    assert!(ok.is_ok(), "session must be healed: {ok:?}");
    server.stop.store(true, Ordering::Relaxed);
}

#[test]
fn enclave_error_response_does_not_reconnect() {
    let script = ready_script();
    script.error_mode.store(true, Ordering::Relaxed);
    let enclave = Arc::new(FakeEnclave::generate(Arc::clone(&script)));
    let server = spawn_server(Arc::clone(&enclave));
    let mut session = connect_session(&server);
    let generation = session.generation();
    let error = session
        .request(&EnclaveRequest::Health)
        .expect_err("scripted error");
    assert!(matches!(error, TransportError::EnclaveError(_)));
    assert_eq!(session.generation(), generation, "no reconnect");
    server.stop.store(true, Ordering::Relaxed);
}

#[test]
fn poison_recovery_forces_one_clean_reconnect() {
    let script = ready_script();
    let enclave = Arc::new(FakeEnclave::generate(Arc::clone(&script)));
    let server = spawn_server(Arc::clone(&enclave));
    let session = connect_session(&server);
    let mutex = Arc::new(Mutex::new(session));
    // Poison the mutex.
    let poisoner = Arc::clone(&mutex);
    let _ = std::thread::spawn(move || {
        let _guard = poisoner.lock().expect("lock");
        panic!("poison");
    })
    .join();
    assert!(mutex.lock().is_err(), "mutex must be poisoned");
    // Recovery path (mirrors client_global::try_with_enclave).
    let mut guard = match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            let mut guard = poisoned.into_inner();
            guard.recover_from_poison();
            guard
        }
    };
    assert!(guard.client.is_none(), "poison drops the connection once");
    assert!(guard.poison_recovered());
    let response = guard.request(&EnclaveRequest::GetPublicKeys);
    assert!(
        response.is_ok(),
        "clean reconnect after poison: {response:?}"
    );
    assert_eq!(guard.generation(), 2);
    // The latch: a second recovery call must not drop the healed client.
    guard.recover_from_poison();
    assert!(
        guard.client.is_some(),
        "latch protects the reconnected client"
    );
    server.stop.store(true, Ordering::Relaxed);
}

#[test]
fn multi_frame_dcap_requests_are_guarded() {
    let script = ready_script();
    let enclave = Arc::new(FakeEnclave::generate(Arc::clone(&script)));
    let server = spawn_server(Arc::clone(&enclave));
    let mut session = connect_session(&server);
    for request in [
        EnclaveRequest::FinishDcapVerificationV1 {
            request_hash: alloy_primitives::B256::ZERO,
        },
        EnclaveRequest::BeginUpgradeKeyTransferV1 {
            request_hash: alloy_primitives::B256::ZERO,
            artifact: vec![],
            anchor_outcome: vec![],
            export: true,
        },
    ] {
        let error = session
            .request(&request)
            .expect_err("guarded before sending any frame");
        assert!(matches!(error, TransportError::DcapVerification(_)));
    }
    server.stop.store(true, Ordering::Relaxed);
}
