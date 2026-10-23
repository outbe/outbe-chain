//! Outbe profile wrapper around the native Heartwood runtime.
mod collector;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use radicle::crypto::ssh::Keystore;
use radicle::identity::RepoId;
use radicle::node::config::{
    DefaultSeedingPolicy, LimitConnectionsInbound, LimitConnectionsOutbound, Network, PeerConfig,
    Relay,
};
use radicle::node::{Address, Alias, Config, NodeId};
use radicle::profile::Home;
use radicle::storage::{ReadRepository, ReadStorage};
use radicle_signals::Signal;
pub mod errors;
pub use errors::{Error, RepositoryAvailabilityError};

const MAX_STATUS_CONNECTIONS: usize = 32;
const STATUS_DEADLINE: Duration = Duration::from_secs(5);
const MAX_REQUEST_BYTES: usize = 4096;

#[derive(Debug, Clone)]
pub struct Options {
    home: PathBuf,
    control_socket: PathBuf,
    listen: SocketAddr,
    advertise: Vec<Address>,
    max_validators: usize,
    external_inbound_reserve: usize,
    status_listen: SocketAddr,
}

impl Options {
    pub fn parse_from<I, T>(args: I) -> Result<Self, Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString>,
    {
        use lexopt::prelude::*;

        let mut parser = lexopt::Parser::from_args(args);
        let mut home = None;
        let mut control_socket = None;
        let mut listen: Option<SocketAddr> = None;
        let mut advertise = Vec::new();
        let mut max_validators: Option<usize> = None;
        let mut external_inbound_reserve: Option<usize> = None;
        let mut status_listen: Option<SocketAddr> = None;

        while let Some(arg) = parser.next()? {
            match arg {
                Long("home") => home = Some(PathBuf::from(parser.value()?)),
                Long("control-socket") => {
                    control_socket = Some(PathBuf::from(parser.value()?));
                }
                Long("listen") => listen = Some(parse_value(&mut parser, "--listen")?),
                Long("advertise") => {
                    advertise.push(parse_value(&mut parser, "--advertise")?);
                }
                Long("max-validators") => {
                    max_validators = Some(parse_value(&mut parser, "--max-validators")?);
                }
                Long("external-inbound-reserve") => {
                    external_inbound_reserve =
                        Some(parse_value(&mut parser, "--external-inbound-reserve")?);
                }
                Long("status-listen") => {
                    status_listen = Some(parse_value(&mut parser, "--status-listen")?);
                }
                _ => return Err(arg.unexpected().into()),
            }
        }

        let max_validators = max_validators.ok_or(Error::Missing("--max-validators"))?;
        if max_validators == 0 {
            return Err(Error::InvalidValue {
                option: "--max-validators",
                value: "0".to_owned(),
            });
        }
        let status_listen = status_listen.ok_or(Error::Missing("--status-listen"))?;
        if !status_listen.ip().is_loopback() {
            return Err(Error::InvalidValue {
                option: "--status-listen",
                value: status_listen.to_string(),
            });
        }
        if advertise.is_empty() {
            return Err(Error::Missing("--advertise"));
        }

        Ok(Self {
            home: home.ok_or(Error::Missing("--home"))?,
            control_socket: control_socket.ok_or(Error::Missing("--control-socket"))?,
            listen: listen.ok_or(Error::Missing("--listen"))?,
            advertise,
            max_validators,
            external_inbound_reserve: external_inbound_reserve
                .ok_or(Error::Missing("--external-inbound-reserve"))?,
            status_listen,
        })
    }

    pub fn from_env() -> Result<Self, Error> {
        Self::parse_from(std::env::args_os().skip(1))
    }

    pub fn control_socket(&self) -> &Path {
        &self.control_socket
    }

    pub fn node_config(&self) -> Result<Config, Error> {
        let managed_peers = self.max_validators - 1;
        let inbound = managed_peers
            .checked_add(self.external_inbound_reserve)
            .ok_or_else(|| Error::InvalidValue {
                option: "--external-inbound-reserve",
                value: self.external_inbound_reserve.to_string(),
            })?;
        let mut config = Config::new(Alias::new("outbe"));
        config.network = Network::Outbe;
        config.peers = PeerConfig::Static;
        config.relay = Relay::Always;
        config.seeding_policy = DefaultSeedingPolicy::Block;
        config.listen = vec![self.listen];
        config.external_addresses = self.advertise.clone();
        config.limits.connection.outbound = LimitConnectionsOutbound::from(managed_peers);
        config.limits.connection.inbound = LimitConnectionsInbound::from(inbound);
        Ok(config)
    }
}

fn parse_value<T>(parser: &mut lexopt::Parser, option: &'static str) -> Result<T, Error>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    let value = parser.value()?;
    let display = value.to_string_lossy().into_owned();
    display.parse().map_err(|_| Error::InvalidValue {
        option,
        value: display,
    })
}

pub trait RepositoryAvailability: Send + Sync + 'static {
    fn contains(&self, rid: &RepoId) -> Result<bool, RepositoryAvailabilityError>;
}

impl RepositoryAvailability for radicle::storage::git::Storage {
    fn contains(&self, rid: &RepoId) -> Result<bool, RepositoryAvailabilityError> {
        if !ReadStorage::contains(self, rid)? {
            return Ok(false);
        }
        ReadStorage::repository(self, *rid)
            .and_then(|repository| repository.identity_doc().map(|_| ()))
            .map(|()| true)
            .map_err(RepositoryAvailabilityError::from)
    }
}

pub struct StatusServer {
    address: SocketAddr,
    shutdown: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl StatusServer {
    pub fn start(
        address: SocketAddr,
        availability: impl RepositoryAvailability,
    ) -> Result<Self, Error> {
        if !address.ip().is_loopback() {
            return Err(Error::InvalidValue {
                option: "--status-listen",
                value: address.to_string(),
            });
        }
        let listener = TcpListener::bind(address)?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let availability: Arc<dyn RepositoryAvailability> = Arc::new(availability);
        let shutdown = Arc::new(AtomicBool::new(false));
        let running = shutdown.clone();
        let join = thread::Builder::new()
            .name("outbe-radicle-status".to_owned())
            .spawn(move || serve_status(listener, availability, running))?;

        Ok(Self {
            address,
            shutdown,
            join: Some(join),
        })
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }
}

impl Drop for StatusServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        let _ = TcpStream::connect_timeout(&self.address, Duration::from_millis(100));
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn serve_status(
    listener: TcpListener,
    availability: Arc<dyn RepositoryAvailability>,
    shutdown: Arc<AtomicBool>,
) {
    let active = Arc::new(AtomicUsize::new(0));
    while !shutdown.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => {
                if active.fetch_add(1, Ordering::AcqRel) >= MAX_STATUS_CONNECTIONS {
                    active.fetch_sub(1, Ordering::AcqRel);
                    let _ = respond(stream, 503, r#"{"version":1,"code":"busy"}"#);
                    continue;
                }
                let availability = availability.clone();
                let worker_active = active.clone();
                if let Err(error) = thread::Builder::new()
                    .name("outbe-radicle-status-request".to_owned())
                    .spawn(move || {
                        handle_status(stream, availability.as_ref());
                        worker_active.fetch_sub(1, Ordering::AcqRel);
                    })
                {
                    active.fetch_sub(1, Ordering::AcqRel);
                    log::error!(target: "outbe-radicle", "status worker spawn failed: {error}");
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => {
                log::error!(target: "outbe-radicle", "status accept failed: {error}");
                break;
            }
        }
    }
}

fn handle_status(mut stream: TcpStream, availability: &dyn RepositoryAvailability) {
    // Accepted sockets inherit the listener's nonblocking mode on some platforms.
    if stream.set_nonblocking(false).is_err()
        || stream.set_write_timeout(Some(STATUS_DEADLINE)).is_err()
    {
        return;
    }
    let mut request = [0_u8; MAX_REQUEST_BYTES];
    let length = match read_status_request(&mut stream, &mut request) {
        Ok(Some(length)) => length,
        Ok(None) => {
            let _ = respond(stream, 400, r#"{"version":1,"code":"invalid_request"}"#);
            return;
        }
        Err(_) => return,
    };
    let Some(path) = std::str::from_utf8(&request[..length])
        .ok()
        .and_then(status_path)
    else {
        let _ = respond(stream, 400, r#"{"version":1,"code":"invalid_request"}"#);
        return;
    };
    respond_status_path(stream, availability, path);
}

fn read_status_request(
    stream: &mut TcpStream,
    request: &mut [u8],
) -> std::io::Result<Option<usize>> {
    let deadline = Instant::now() + STATUS_DEADLINE;
    let mut length = 0;
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or(std::io::ErrorKind::TimedOut)?;
        stream.set_read_timeout(Some(remaining))?;
        let read = stream.read(&mut request[length..])?;
        if read == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        length += read;
        if request[..length]
            .windows(4)
            .any(|window| window == b"\r\n\r\n")
        {
            return Ok(Some(length));
        }
        if length == request.len() {
            return Ok(None);
        }
    }
}

fn status_path(request: &str) -> Option<&str> {
    if request.lines().skip(1).any(|line| {
        line.eq_ignore_ascii_case("transfer-encoding: chunked") || {
            line.split_once(':').is_some_and(|(name, value)| {
                name.eq_ignore_ascii_case("content-length") && value.trim() != "0"
            })
        }
    }) {
        return None;
    }
    request
        .lines()
        .next()?
        .strip_prefix("GET ")
        .and_then(|line| line.strip_suffix(" HTTP/1.1"))
}

fn respond_status_path(stream: TcpStream, availability: &dyn RepositoryAvailability, path: &str) {
    if path == "/healthz" {
        let _ = respond(stream, 200, r#"{"status":"ok"}"#);
        return;
    }
    let Some(encoded) = path.strip_prefix("/v1/repositories/") else {
        let _ = respond(stream, 404, r#"{"version":1,"code":"not_found"}"#);
        return;
    };
    if encoded.len() != 40
        || !encoded
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        let _ = respond(stream, 400, r#"{"version":1,"code":"invalid_repo_id"}"#);
        return;
    }
    let Ok(oid) = radicle::git::Oid::from_str(encoded) else {
        let _ = respond(stream, 400, r#"{"version":1,"code":"invalid_repo_id"}"#);
        return;
    };
    let rid = RepoId::from(oid);
    match availability.contains(&rid) {
        Ok(available) => {
            let body = format!(r#"{{"version":1,"repo_id":"{encoded}","available":{available}}}"#);
            let _ = respond(stream, 200, &body);
        }
        Err(error) => {
            log::warn!(target: "outbe-radicle", "repository availability failed: {error}");
            let _ = respond(stream, 500, r#"{"version":1,"code":"storage_unavailable"}"#);
        }
    }
}

fn respond(mut stream: TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Unknown",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

pub struct PreparedSidecar {
    runtime: radicle_node::runtime::Runtime,
    status: StatusServer,
    control_socket: PathBuf,
}

impl PreparedSidecar {
    pub fn prepare(options: Options, signals: mpsc::Receiver<Signal>) -> Result<Self, Error> {
        let home = Home::load(&options.home)?;
        let control_socket = options.control_socket.clone();
        protect_control_parent(&options.control_socket)?;
        let secret_path = home.keys().join("radicle");
        let secret = Keystore::from_secret_path(&secret_path)
            .secret_key(None)?
            .ok_or_else(|| Error::MissingKey(secret_path.clone()))?;
        let runtime = radicle_node::runtime::Runtime::init(
            home,
            options.node_config()?,
            options.control_socket,
            vec![options.listen],
            signals,
            secret,
        )?;
        let status = StatusServer::start(options.status_listen, runtime.storage.clone())?;
        Ok(Self {
            runtime,
            status,
            control_socket,
        })
    }

    pub fn node_id(&self) -> NodeId {
        self.runtime.id
    }

    pub fn status_address(&self) -> SocketAddr {
        self.status.address()
    }

    pub fn run(self) -> Result<(), Error> {
        let Self {
            runtime,
            status,
            control_socket,
        } = self;
        let collector = collector::Worker::start(
            control_socket,
            runtime.storage.clone(),
            runtime.home.node().join("outbe-governments"),
            runtime.handle.clone(),
        )?;
        let result = runtime.run().map_err(Error::from);
        drop(collector);
        drop(status);
        result
    }
}

#[cfg(unix)]
fn protect_control_parent(socket: &Path) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;

    let parent = socket.parent().ok_or_else(|| Error::InvalidValue {
        option: "--control-socket",
        value: socket.display().to_string(),
    })?;
    std::fs::create_dir_all(parent)?;
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn protect_control_parent(socket: &Path) -> Result<(), Error> {
    let parent = socket.parent().ok_or_else(|| Error::InvalidValue {
        option: "--control-socket",
        value: socket.display().to_string(),
    })?;
    std::fs::create_dir_all(parent)?;
    Ok(())
}
