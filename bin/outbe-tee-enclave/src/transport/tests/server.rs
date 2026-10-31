use std::io::ErrorKind;
use std::path::Path;
use std::sync::Mutex;
use std::thread;
use std::time::Instant;

use crate::initialization::RemoteSessionAuthorization;
use crate::transport::tests::*;

const LIVE_CAP_UNDER_TEST: usize = 4;
const CLIENT_BOUND: std::time::Duration = std::time::Duration::from_secs(2);
const SLOT_RETRY: std::time::Duration = std::time::Duration::from_millis(10);

struct RecordingStream {
    inner: UnixStream,
    timeouts: Arc<Mutex<Vec<Option<Duration>>>>,
}

impl RecordingStream {
    fn new(inner: UnixStream, timeouts: Arc<Mutex<Vec<Option<Duration>>>>) -> Self {
        Self { inner, timeouts }
    }
}

impl Read for RecordingStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

impl Write for RecordingStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

impl EnclaveTransportStream for RecordingStream {
    fn set_session_read_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        self.inner.set_read_timeout(timeout)?;
        self.timeouts.lock().expect("timeout log").push(timeout);
        Ok(())
    }
}

struct ProductionEnclave {
    _root: tempfile::TempDir,
    boot: Arc<EnclaveBootConfig>,
    keys: Arc<EnclaveKeys>,
    initialization: Arc<InitializationState>,
    offer_key: SharedTributeOfferKey,
}

impl ProductionEnclave {
    fn open() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let boot = Arc::new(EnclaveBootConfig::new(
            testnet_chain_word(),
            root.path().to_path_buf(),
            0,
        ));
        let keys = Arc::new(EnclaveKeys::new([0x21; 32], Some([0x21; 32])).expect("keys"));
        let initialization = Arc::new(production_dcap_state(Arc::clone(&boot), &keys));
        let challenge = match initialization.challenge_response(&keys).expect("challenge") {
            EnclaveResponse::InitializationChallenge { challenge, .. } => challenge,
            response => panic!("unexpected challenge response: {response:?}"),
        };
        let (manifest, node_signature) =
            signed_initialization_manifest(&keys, challenge, [0x32; 32]);
        let pending = initialization
            .prepare(
                &manifest.encode_canonical().expect("manifest bytes"),
                &node_signature,
                &keys,
            )
            .expect("prepare");
        initialization.commit(pending, &keys).expect("commit");
        Self {
            _root: root,
            boot,
            keys,
            initialization,
            offer_key: Arc::new(OnceLock::new()),
        }
    }

    fn context(&self) -> ServerContext {
        ServerContext {
            chain_id: B256::from(testnet_chain_word()),
            initialization: Arc::clone(&self.initialization),
            offer_key: Arc::clone(&self.offer_key),
            boot: Some(Arc::clone(&self.boot)),
            keys: Arc::clone(&self.keys),
        }
    }
}

fn development_context() -> ServerContext {
    ServerContext {
        chain_id: B256::ZERO,
        initialization: Arc::new(crate::initialization::factory::development()),
        offer_key: Arc::new(OnceLock::new()),
        boot: None,
        keys: Arc::new(EnclaveKeys::new([0x31; 32], None).expect("dev keys")),
    }
}

fn peer_closed(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        ErrorKind::ConnectionReset
            | ErrorKind::ConnectionAborted
            | ErrorKind::BrokenPipe
            | ErrorKind::UnexpectedEof
            | ErrorKind::NotConnected
    )
}

#[test]
fn handshake_read_timeout_is_set_and_cleared_for_local_and_clamped_for_remote() {
    assert_eq!(
        local_session_timeouts(),
        vec![Some(HANDSHAKE_READ_TIMEOUT), None]
    );
    assert_clamped_remote_timeouts(&remote_session_timeouts());
}

fn local_session_timeouts() -> Vec<Option<Duration>> {
    let root = tempfile::tempdir().expect("tempdir");
    let socket = root.path().join("local.sock");
    let listener = UnixListener::bind(&socket).expect("bind");
    let timeouts = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&timeouts);
    let keys = Arc::new(EnclaveKeys::new([0x11; 32], None).expect("keys"));
    let initialization = Arc::new(crate::initialization::factory::development());
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept");
        let offer_key: SharedTributeOfferKey = Arc::new(OnceLock::new());
        serve_connection_with(
            RecordingStream::new(stream, recorded),
            &keys,
            &offer_key,
            None,
            &initialization,
        )
        .expect("local session");
    });
    let mut client = outbe_tee::EnclaveClient::connect(&socket).expect("connect");
    client
        .request(&EnclaveRequest::GetPublicKeys)
        .expect("public keys");
    drop(client);
    server.join().expect("server thread");
    let recorded = timeouts.lock().expect("timeout log").clone();
    recorded
}

fn remote_session_timeouts() -> Vec<Option<Duration>> {
    let enclave = ProductionEnclave::open();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    let initiator_private = [0x44; 32];
    let initiator_public =
        x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(initiator_private))
            .to_bytes();
    let ticket_id = B256::repeat_byte(0xAB);
    enclave
        .initialization
        .authorize_remote_session(
            RemoteSessionAuthorization {
                ticket_id,
                initiator_static_x25519: initiator_public,
                responder_static_x25519: enclave.keys.noise_public(),
                deadline: now + 120,
                finalized_block_hash: B256::repeat_byte(0xCD),
            },
            &enclave.keys,
        )
        .expect("authorize remote session");

    let root = tempfile::tempdir().expect("tempdir");
    let socket = root.path().join("remote.sock");
    let listener = UnixListener::bind(&socket).expect("bind");
    let timeouts = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&timeouts);
    let keys = Arc::clone(&enclave.keys);
    let boot = Arc::clone(&enclave.boot);
    let initialization = Arc::clone(&enclave.initialization);
    let offer_key = Arc::clone(&enclave.offer_key);
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept");
        serve_connection_with(
            RecordingStream::new(stream, recorded),
            &keys,
            &offer_key,
            Some(&boot),
            &initialization,
        )
        .expect("remote session");
    });

    let mut client = UnixStream::connect(&socket).expect("connect");
    complete_remote_public_keys(
        &mut client,
        ticket_id,
        initiator_private,
        enclave.keys.noise_public(),
    );
    drop(client);
    server.join().expect("server thread");
    let recorded = timeouts.lock().expect("timeout log").clone();
    recorded
}

fn complete_remote_public_keys(
    stream: &mut UnixStream,
    ticket_id: B256,
    initiator_private: [u8; 32],
    responder_public: [u8; 32],
) {
    let preamble =
        outbe_tee::codec::encode_request(&EnclaveRequest::OpenRemoteSessionV1 { ticket_id })
            .expect("preamble");
    outbe_tee::codec::write_frame(stream, &preamble).expect("write preamble");
    let mut handshake = snow::Builder::new(NOISE_PARAMS.parse().expect("noise params"))
        .local_private_key(&initiator_private)
        .remote_public_key(&responder_public)
        .build_initiator()
        .expect("initiator");
    let mut buffer = [0u8; 1024];
    let message_len = handshake.write_message(&[], &mut buffer).expect("msg1");
    outbe_tee::codec::write_frame(stream, &buffer[..message_len]).expect("write msg1");
    let reply = outbe_tee::codec::read_frame(stream).expect("msg2");
    handshake
        .read_message(&reply, &mut buffer)
        .expect("read msg2");
    let mut noise = handshake.into_transport_mode().expect("transport");
    let plain = outbe_tee::codec::encode_call(
        outbe_tee::call_context::EnclaveCallContextV1::default(),
        &EnclaveRequest::GetPublicKeys,
    )
    .expect("call");
    let mut ciphertext = vec![0u8; plain.len() + 64];
    let length = noise
        .write_message(&plain, &mut ciphertext)
        .expect("encrypt");
    outbe_tee::codec::write_frame(stream, &ciphertext[..length]).expect("write call");
    let frame = outbe_tee::codec::read_frame(stream).expect("response");
    let mut plaintext = vec![0u8; frame.len()];
    let length = noise.read_message(&frame, &mut plaintext).expect("decrypt");
    assert!(matches!(
        outbe_tee::codec::decode_response(&plaintext[..length]).expect("response"),
        EnclaveResponse::PublicKeys { .. }
    ));
}

fn assert_clamped_remote_timeouts(timeouts: &[Option<Duration>]) {
    let mut handshake_cap_count = 0usize;
    let mut saw_live_deadline = false;
    for timeout in timeouts {
        let Some(timeout) = *timeout else {
            panic!("remote session cleared the read timeout: {timeouts:?}");
        };
        if saw_live_deadline {
            assert!(
                timeout > HANDSHAKE_READ_TIMEOUT,
                "live deadline fell back to the handshake cap: {timeouts:?}"
            );
        } else if timeout == HANDSHAKE_READ_TIMEOUT {
            handshake_cap_count += 1;
        } else if timeout > HANDSHAKE_READ_TIMEOUT {
            assert!(
                handshake_cap_count >= 2,
                "both pre-preamble and pre-negotiate reads must clamp to HANDSHAKE_READ_TIMEOUT: {timeouts:?}"
            );
            saw_live_deadline = true;
        } else {
            panic!("timeout below the handshake cap: {timeout:?} in {timeouts:?}");
        }
    }
    assert!(
        saw_live_deadline,
        "read_live_frame did not install the full lease: {timeouts:?}"
    );
}

#[test]
fn idle_connection_is_dropped_after_handshake_timeout() {
    let enclave = ProductionEnclave::open();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let context = enclave.context();
    let _server = thread::spawn(move || {
        serve_tcp_with_limits(
            &listener,
            context,
            MAX_LIVE_CONNECTIONS,
            Duration::from_millis(50),
        )
        .expect("tcp server");
    });
    expect_idle_close(addr, |_| {});
    expect_idle_close(addr, send_open_session);
}

fn send_open_session(stream: &mut TcpStream) {
    let preamble =
        outbe_tee::codec::encode_request(&EnclaveRequest::OpenSession).expect("preamble");
    outbe_tee::codec::write_frame(stream, &preamble).expect("write preamble");
}

fn expect_idle_close(addr: std::net::SocketAddr, prepare: impl FnOnce(&mut TcpStream)) {
    let mut client = TcpStream::connect(addr).expect("connect");
    client
        .set_read_timeout(Some(Duration::from_secs(1)))
        .expect("client timeout");
    prepare(&mut client);
    let started = Instant::now();
    let mut buf = [0u8; 1];
    let read = client.read(&mut buf);
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(40),
        "closed in {elapsed:?}, before the handshake timeout: {read:?}"
    );
    assert!(
        elapsed < Duration::from_secs(1),
        "client read hit its 1s safety timeout: {read:?}"
    );
    match read {
        Ok(0) => {}
        Err(err) if peer_closed(&err) => {}
        other => panic!("expected the server to close the idle socket, got {other:?}"),
    }
}

#[test]
fn live_connections_are_capped() {
    assert_tcp_live_cap();
    assert_unix_live_cap();
}

fn assert_tcp_live_cap() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let _server = thread::spawn(move || {
        serve_tcp_with_limits(
            &listener,
            development_context(),
            LIVE_CAP_UNDER_TEST,
            HANDSHAKE_READ_TIMEOUT,
        )
        .expect("tcp server");
    });
    assert_live_connections_are_capped(|| TcpStream::connect(addr).expect("holder"));
}

fn assert_unix_live_cap() {
    let root = tempfile::tempdir().expect("tempdir");
    let socket = root.path().join("cap.sock");
    let listener = UnixListener::bind(&socket).expect("bind");
    let _server = thread::spawn(move || {
        serve_with_limits(
            &listener,
            development_context(),
            LIVE_CAP_UNDER_TEST,
            HANDSHAKE_READ_TIMEOUT,
        )
        .expect("uds server");
    });
    assert_live_connections_are_capped(|| UnixStream::connect(&socket).expect("holder"));
}

trait ProbeStream: Read + Write {
    fn set_read_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()>;
    fn set_write_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()>;
}

impl ProbeStream for TcpStream {
    fn set_read_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        TcpStream::set_read_timeout(self, timeout)
    }

    fn set_write_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        TcpStream::set_write_timeout(self, timeout)
    }
}

impl ProbeStream for UnixStream {
    fn set_read_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        UnixStream::set_read_timeout(self, timeout)
    }

    fn set_write_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        UnixStream::set_write_timeout(self, timeout)
    }
}

fn assert_live_connections_are_capped<S: ProbeStream>(connect: impl Fn() -> S) {
    let mut held = Vec::with_capacity(LIVE_CAP_UNDER_TEST);
    for _ in 0..LIVE_CAP_UNDER_TEST {
        held.push(connect());
    }
    // The accept loop takes slots after `connect` returns. Give it one scheduling slice.
    thread::sleep(Duration::from_millis(50));
    await_overflow_rejection(&connect);
    assert_active_client_overflow_rejected(&connect);
    drop(held.pop());
    admit_public_keys(|| request_public_keys_bounded(connect()));
}

fn await_overflow_rejection<S: ProbeStream>(connect: &impl Fn() -> S) {
    let deadline = Instant::now() + CLIENT_BOUND;
    let mut last = String::from("no probe");
    while Instant::now() < deadline {
        let mut stream = connect();
        stream
            .set_read_timeout(Some(CLIENT_BOUND))
            .expect("probe timeout");
        let started = Instant::now();
        match probe_rejected(&mut stream) {
            Ok(()) => {
                assert!(
                    started.elapsed() < CLIENT_BOUND,
                    "overflow close waited for the client timeout"
                );
                return;
            }
            Err(detail) => last = detail,
        }
        thread::sleep(SLOT_RETRY);
    }
    panic!("overflow connection was not rejected within 2s: {last}");
}

fn assert_active_client_overflow_rejected<S: ProbeStream>(connect: &impl Fn() -> S) {
    let started = Instant::now();
    let result = request_public_keys_bounded(connect());
    let elapsed = started.elapsed();
    assert!(
        elapsed < CLIENT_BOUND,
        "overflow client waited for the 2s read timeout: {result:?}"
    );
    match result {
        Err(TransportError::Io(err)) if peer_closed(&err) => {}
        other => panic!("expected immediate transport close for overflow client, got {other:?}"),
    }
}

fn request_public_keys_bounded(
    mut stream: impl ProbeStream,
) -> Result<EnclaveResponse, TransportError> {
    stream.set_read_timeout(Some(CLIENT_BOUND))?;
    stream.set_write_timeout(Some(CLIENT_BOUND))?;
    let quote_req =
        outbe_tee::codec::encode_request(&EnclaveRequest::GetQuote { nonce: [0x5A; 32] })?;
    outbe_tee::codec::write_frame(&mut stream, &quote_req)?;
    let quote_frame = outbe_tee::codec::read_frame(&mut stream)?;
    let EnclaveResponse::Quote {
        noise_static_pub, ..
    } = outbe_tee::codec::decode_response(&quote_frame)?
    else {
        return Err(TransportError::UnexpectedResponse);
    };
    let params = NOISE_PARAMS.parse().expect("noise params");
    let builder = snow::Builder::new(params);
    let host_keys = builder.generate_keypair().expect("host keypair");
    let mut handshake = builder
        .local_private_key(&host_keys.private)
        .remote_public_key(&noise_static_pub)
        .build_initiator()
        .expect("initiator");
    let mut buf = [0u8; 1024];
    let msg1_len = handshake.write_message(&[], &mut buf).expect("msg1");
    outbe_tee::codec::write_frame(&mut stream, &buf[..msg1_len])?;
    let msg2 = outbe_tee::codec::read_frame(&mut stream)?;
    handshake.read_message(&msg2, &mut buf).expect("msg2");
    let mut noise = handshake.into_transport_mode().expect("transport mode");
    let plain = outbe_tee::codec::encode_call(
        outbe_tee::call_context::EnclaveCallContextV1::default(),
        &EnclaveRequest::GetPublicKeys,
    )?;
    let mut ct = vec![0u8; plain.len() + 64];
    let ct_len = noise.write_message(&plain, &mut ct).expect("encrypt");
    outbe_tee::codec::write_frame(&mut stream, &ct[..ct_len])?;
    let resp_frame = outbe_tee::codec::read_frame(&mut stream)?;
    let mut pt = vec![0u8; resp_frame.len()];
    let pt_len = noise.read_message(&resp_frame, &mut pt).expect("decrypt");
    outbe_tee::codec::decode_response(&pt[..pt_len])
}

fn probe_rejected(stream: &mut impl ProbeStream) -> Result<(), String> {
    let mut buf = [0u8; 1];
    match stream.read(&mut buf) {
        Ok(0) => Ok(()),
        Ok(n) => Err(format!("server accepted the overflow read ({n} bytes)")),
        Err(err) if peer_closed(&err) => Ok(()),
        // A timeout means the server kept the socket. Do not write: a partial
        // frame would pin the slot until the handshake timeout.
        Err(err) if err.kind() == ErrorKind::TimedOut || err.kind() == ErrorKind::WouldBlock => {
            Err(err.to_string())
        }
        Err(err) => match stream.write(&[0]) {
            Err(write_err) if peer_closed(&write_err) => Ok(()),
            _ => Err(err.to_string()),
        },
    }
}

fn admit_public_keys(admit: impl Fn() -> Result<EnclaveResponse, TransportError>) {
    let started = Instant::now();
    let deadline = started + CLIENT_BOUND;
    let mut last = String::from("no attempt");
    while Instant::now() < deadline {
        match admit() {
            Ok(EnclaveResponse::PublicKeys { .. }) => {
                assert!(
                    started.elapsed() <= CLIENT_BOUND,
                    "recovery exceeded the 2s deadline: {:?}",
                    started.elapsed()
                );
                return;
            }
            Ok(other) => panic!("unexpected public-key response: {other:?}"),
            Err(err) => last = err.to_string(),
        }
        thread::sleep(SLOT_RETRY);
    }
    panic!("live slot did not recover within 2s: {last}");
}

#[test]
fn max_live_connections_fits_within_release_sgx_max_threads() {
    for bundle in ["testnet-sgx-bundle-v1.json", "mainnet-sgx-bundle-v1.json"] {
        let max_threads = release_sgx_max_threads(bundle);
        assert!(
            MAX_LIVE_CONNECTIONS + 1 < max_threads,
            "{bundle}: MAX_LIVE_CONNECTIONS ({MAX_LIVE_CONNECTIONS}) + 1 must be < sgx.max_threads ({max_threads})"
        );
    }
}

fn release_sgx_max_threads(name: &str) -> usize {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../release")
        .join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    let value: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|err| panic!("parse {}: {err}", path.display()));
    let max_threads = value
        .get("sgx")
        .and_then(|sgx| sgx.get("max_threads"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_else(|| panic!("{name} is missing sgx.max_threads"));
    usize::try_from(max_threads).expect("max_threads fits usize")
}
