use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpStream};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixStream;
use std::str::FromStr as _;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use outbe_radicle_runtime::{
    Options, PreparedSidecar, RepositoryAvailability, RepositoryAvailabilityError, StatusServer,
};
use radicle::crypto::ssh::Keystore;
use radicle::crypto::{Seed, Signer as _, SigningKey};
use radicle::identity::RepoId;
use radicle::node::config::{DefaultSeedingPolicy, Network, PeerConfig, Relay};
use radicle::node::Handle as _;
use radicle::node::{
    Address, Command, ConnectOptions, ConnectResult, DisconnectDisposition, NodeId,
};
use radicle::profile::Home;
use radicle::storage::ReadStorage as _;

const RID_HEX: &str = "0123456789abcdef0123456789abcdef01234567";

fn valid_args(home: &Home) -> Vec<OsString> {
    [
        "--home".into(),
        home.path().as_os_str().to_owned(),
        "--control-socket".into(),
        home.node().join("outbe-control.sock").into_os_string(),
        "--listen".into(),
        "127.0.0.1:0".into(),
        "--advertise".into(),
        "validator.test:8776".into(),
        "--max-validators".into(),
        "32".into(),
        "--external-inbound-reserve".into(),
        "7".into(),
        "--status-listen".into(),
        "127.0.0.1:0".into(),
    ]
    .into()
}

#[test]
fn every_sidecar_flag_is_required() {
    let temp = tempfile::tempdir().unwrap();
    let home = Home::new(temp.path()).unwrap();
    let args = valid_args(&home);

    for flag in [
        "--home",
        "--control-socket",
        "--listen",
        "--advertise",
        "--max-validators",
        "--external-inbound-reserve",
        "--status-listen",
    ] {
        let index = args.iter().position(|arg| arg == flag).unwrap();
        let mut missing = args.clone();
        missing.drain(index..=index + 1);
        assert!(
            Options::parse_from(missing).is_err(),
            "{flag} must be required"
        );
    }
}

#[test]
fn options_build_the_exact_outbe_profile_and_limits() {
    let temp = tempfile::tempdir().unwrap();
    let home = Home::new(temp.path()).unwrap();
    let options = Options::parse_from(valid_args(&home)).unwrap();
    let config = options.node_config().unwrap();

    assert_eq!(config.network, Network::Outbe);
    assert!(matches!(config.peers, PeerConfig::Static));
    assert!(matches!(config.relay, Relay::Always));
    assert_eq!(config.seeding_policy, DefaultSeedingPolicy::Block);
    assert_eq!(usize::from(config.limits.connection.outbound), 31);
    assert_eq!(usize::from(config.limits.connection.inbound), 38);
    assert_eq!(config.external_addresses.len(), 1);
    assert_eq!(
        config.external_addresses[0].to_string(),
        "validator.test:8776"
    );
}

#[test]
fn every_advertise_address_is_published() {
    let temp = tempfile::tempdir().unwrap();
    let home = Home::new(temp.path()).unwrap();
    let mut args = valid_args(&home);
    args.extend(["--advertise".into(), "198.51.100.10:8776".into()]);

    let config = Options::parse_from(args).unwrap().node_config().unwrap();
    assert_eq!(config.external_addresses.len(), 2);
    assert_eq!(
        config.external_addresses[1].to_string(),
        "198.51.100.10:8776"
    );
}

#[derive(Clone)]
struct Availability {
    repositories: BTreeSet<RepoId>,
}

#[derive(Clone)]
struct UnavailableStorage;

impl RepositoryAvailability for UnavailableStorage {
    fn contains(&self, _rid: &RepoId) -> Result<bool, RepositoryAvailabilityError> {
        Err(RepositoryAvailabilityError::Offline)
    }
}

impl RepositoryAvailability for Availability {
    fn contains(&self, rid: &RepoId) -> Result<bool, RepositoryAvailabilityError> {
        Ok(self.repositories.contains(rid))
    }
}

fn request(address: SocketAddr, path: &str) -> String {
    try_request(address, path).unwrap()
}

fn try_request(address: SocketAddr, path: &str) -> std::io::Result<String> {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(1))?;
    stream.write_all(
        format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").as_bytes(),
    )?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(response)
}

fn request_with_body(address: SocketAddr, path: &str) -> String {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(1)).unwrap();
    stream
        .write_all(
            format!(
                "GET {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 1\r\nConnection: close\r\n\r\nx"
            )
            .as_bytes(),
        )
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

#[test]
fn status_server_is_loopback_read_only_and_reports_repository_availability() {
    let rid = RepoId::from(radicle::git::Oid::from_str(RID_HEX).unwrap());
    let server = StatusServer::start(
        "127.0.0.1:0".parse().unwrap(),
        Availability {
            repositories: BTreeSet::from([rid]),
        },
    )
    .unwrap();

    let health = request(server.address(), "/healthz");
    assert!(health.starts_with("HTTP/1.1 200"));
    assert!(health.contains(r#"{"status":"ok"}"#));

    let available = request(server.address(), &format!("/v1/repositories/{RID_HEX}"));
    assert!(available.starts_with("HTTP/1.1 200"));
    assert!(available.contains(r#""version":1"#));
    assert!(available.contains(r#""available":true"#));

    let absent_hex = "1123456789abcdef0123456789abcdef01234567";
    let absent = request(server.address(), &format!("/v1/repositories/{absent_hex}"));
    assert!(absent.starts_with("HTTP/1.1 200"));
    assert!(absent.contains(r#""available":false"#));

    let malformed = request(server.address(), "/v1/repositories/not-a-repo-id");
    assert!(malformed.starts_with("HTTP/1.1 400"));
    assert!(malformed.contains(r#""code":"invalid_repo_id""#));

    let uppercase = request(
        server.address(),
        "/v1/repositories/0123456789ABCDEF0123456789ABCDEF01234567",
    );
    assert!(uppercase.starts_with("HTTP/1.1 400"));

    assert!(StatusServer::start(
        "0.0.0.0:0".parse().unwrap(),
        Availability {
            repositories: BTreeSet::new(),
        },
    )
    .is_err());

    let body = request_with_body(server.address(), "/healthz");
    assert!(body.starts_with("HTTP/1.1 400"));

    let unavailable =
        StatusServer::start("127.0.0.1:0".parse().unwrap(), UnavailableStorage).unwrap();
    let failed = request(
        unavailable.address(),
        &format!("/v1/repositories/{RID_HEX}"),
    );
    assert!(failed.starts_with("HTTP/1.1 500"));
    assert!(failed.contains(r#""code":"storage_unavailable""#));
}

#[test]
fn status_server_enforces_connection_and_request_deadline_bounds() {
    let started = Instant::now();
    let server = StatusServer::start(
        "127.0.0.1:0".parse().unwrap(),
        Availability {
            repositories: BTreeSet::new(),
        },
    )
    .unwrap();
    assert!(started.elapsed() < Duration::from_secs(10));

    let mut held = Vec::new();
    for _ in 0..32 {
        let mut stream = TcpStream::connect(server.address()).unwrap();
        stream.write_all(b"GET /healthz HTTP/1.1\r\n").unwrap();
        held.push(stream);
    }
    thread::sleep(Duration::from_millis(100));

    let deadline = Instant::now() + Duration::from_secs(2);
    let mut rejected = false;
    while Instant::now() < deadline {
        match try_request(server.address(), "/healthz") {
            Ok(response) if response.starts_with("HTTP/1.1 503") => {
                rejected = true;
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {
                rejected = true;
                break;
            }
            _ => {}
        }
    }
    assert!(rejected, "the 33rd concurrent connection must be rejected");
    drop(held);

    let mut stalled = TcpStream::connect(server.address()).unwrap();
    stalled.write_all(b"GET /healthz HTTP/1.1\r\n").unwrap();
    let started = Instant::now();
    let mut response = Vec::new();
    stalled.read_to_end(&mut response).unwrap();
    assert!(started.elapsed() >= Duration::from_secs(4));
    assert!(started.elapsed() < Duration::from_secs(7));
}

#[test]
fn native_storage_availability_requires_a_readable_identity_and_head() {
    let temp = tempfile::tempdir().unwrap();
    let signer = SigningKey::mock(71);
    let storage = radicle::test::fixtures::storage(temp.path(), &signer).unwrap();
    let repositories = storage.repositories().unwrap();
    let rid = repositories.first().unwrap().rid;
    let absent = RepoId::from(radicle::git::Oid::from_str(RID_HEX).unwrap());

    assert!(RepositoryAvailability::contains(&storage, &rid).unwrap());
    assert!(!RepositoryAvailability::contains(&storage, &absent).unwrap());
}

#[test]
fn prepare_requires_an_existing_key_and_never_generates_one() {
    let temp = tempfile::tempdir().unwrap();
    let home = Home::new(temp.path()).unwrap();
    let options = Options::parse_from(valid_args(&home)).unwrap();
    let (_signal_tx, signal_rx) = mpsc::sync_channel(1);

    assert!(PreparedSidecar::prepare(options, signal_rx).is_err());
    assert!(!home.keys().join("radicle").exists());
}

struct RunningSidecar {
    _home: tempfile::TempDir,
    _signal_tx: mpsc::SyncSender<radicle_signals::Signal>,
    control_socket: std::path::PathBuf,
    controller: radicle::node::Node,
    node_id: NodeId,
    join: Option<thread::JoinHandle<Result<(), outbe_radicle_runtime::Error>>>,
}

impl RunningSidecar {
    fn start(seed_byte: u8) -> Self {
        let home_dir = tempfile::tempdir().unwrap();
        let home = Home::new(home_dir.path()).unwrap();
        Keystore::new(&home.keys())
            .init("outbe-test", None, Seed::new([seed_byte; 32]))
            .unwrap();
        let options = Options::parse_from(valid_args(&home)).unwrap();
        let control_socket = options.control_socket().to_path_buf();
        let (signal_tx, signal_rx) = mpsc::sync_channel(1);
        let sidecar = PreparedSidecar::prepare(options, signal_rx).unwrap();
        let node_id = sidecar.node_id();
        let join = thread::spawn(move || sidecar.run());
        let controller = radicle::node::Node::new(&control_socket);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !controller.is_running() {
            assert!(
                Instant::now() < deadline,
                "native control socket did not become ready"
            );
            thread::sleep(Duration::from_millis(20));
        }
        Self {
            _home: home_dir,
            _signal_tx: signal_tx,
            control_socket,
            controller,
            node_id,
            join: Some(join),
        }
    }
}

impl Drop for RunningSidecar {
    fn drop(&mut self) {
        let _ = self.controller.clone().shutdown();
        if let Some(join) = self.join.take() {
            join.join().unwrap().unwrap();
        }
    }
}

#[test]
fn native_uds_disconnect_is_bounded_for_every_frozen_disposition() {
    let mut amy = RunningSidecar::start(31);
    let mut bob = RunningSidecar::start(32);
    let bob_address = Address::from(bob.controller.listen_addrs().unwrap()[0]);
    let amy_address = Address::from(amy.controller.listen_addrs().unwrap()[0]);
    let timeout = Duration::from_secs(2);

    assert_eq!(
        amy.controller
            .disconnect_bounded(bob.node_id, Some(bob_address.clone()), timeout)
            .unwrap(),
        DisconnectDisposition::AlreadyAbsent
    );

    assert!(matches!(
        amy.controller
            .connect(
                bob.node_id,
                bob_address.clone(),
                ConnectOptions {
                    persistent: false,
                    timeout,
                },
            )
            .unwrap(),
        ConnectResult::Connected
    ));
    assert_eq!(
        bob.controller
            .disconnect_bounded(amy.node_id, Some(amy_address), timeout)
            .unwrap(),
        DisconnectDisposition::Inbound
    );
    let changed: Address = "198.18.0.11:6553".parse().unwrap();
    assert_eq!(
        amy.controller
            .disconnect_bounded(bob.node_id, Some(changed), timeout)
            .unwrap(),
        DisconnectDisposition::AddressChanged
    );
    assert_eq!(
        amy.controller
            .disconnect_bounded(bob.node_id, Some(bob_address.clone()), timeout)
            .unwrap(),
        DisconnectDisposition::Disconnected
    );

    let pending_id = NodeId::from(*SigningKey::mock(99).public_key());
    let pending_address: Address = "203.0.113.1:9".parse().unwrap();
    let mut raw = UnixStream::connect(&amy.control_socket).unwrap();
    Command::Connect {
        addr: (pending_id, pending_address.clone()).into(),
        opts: ConnectOptions {
            persistent: false,
            timeout: Duration::from_secs(5),
        },
    }
    .to_writer(&mut raw)
    .unwrap();
    thread::sleep(Duration::from_millis(50));
    let started = Instant::now();
    assert_eq!(
        amy.controller
            .disconnect_bounded(pending_id, Some(pending_address), timeout)
            .unwrap(),
        DisconnectDisposition::NotConnected
    );
    assert!(started.elapsed() <= timeout);
}

#[test]
fn native_runtime_starts_with_protected_uds_and_native_control_operations() {
    let temp = tempfile::tempdir().unwrap();
    let home = Home::new(temp.path()).unwrap();
    Keystore::new(&home.keys())
        .init("outbe-test", None, Seed::new([7; 32]))
        .unwrap();
    let options = Options::parse_from(valid_args(&home)).unwrap();
    let control_socket = options.control_socket().to_path_buf();
    let (signal_tx, signal_rx) = mpsc::sync_channel(1);
    let sidecar = PreparedSidecar::prepare(options, signal_rx).unwrap();
    let expected_id = sidecar.node_id();
    let status_address = sidecar.status_address();
    let join = thread::spawn(move || sidecar.run());
    let mut controller = radicle::node::Node::new(&control_socket);

    let deadline = Instant::now() + Duration::from_secs(10);
    while !controller.is_running() {
        assert!(
            Instant::now() < deadline,
            "native control socket did not become ready"
        );
        thread::sleep(Duration::from_millis(20));
    }

    assert_eq!(controller.nid().unwrap(), expected_id);
    assert_eq!(controller.config().unwrap().network, Network::Outbe);
    assert!(!controller.listen_addrs().unwrap().is_empty());
    assert!(controller.sessions().unwrap().is_empty());
    assert_eq!(
        controller
            .disconnect_bounded(expected_id, None, Duration::from_secs(1))
            .unwrap(),
        radicle::node::DisconnectDisposition::AlreadyAbsent
    );
    assert!(request(status_address, "/healthz").starts_with("HTTP/1.1 200"));
    assert_eq!(
        control_socket
            .parent()
            .unwrap()
            .metadata()
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );

    controller.shutdown().unwrap();
    drop(signal_tx);
    join.join().unwrap().unwrap();
}
