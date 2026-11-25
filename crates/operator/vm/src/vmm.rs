//! Typed dstack VMM pRPC client over a pluggable transport (local Unix socket or SSH).

use std::{
    collections::BTreeMap,
    fs,
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread,
    time::{Duration, Instant},
};

use eyre::{bail, ensure, Context, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::shell_quote;

/// Embedded `app-compose.json` for `key_provider: none`, no-KMS, no-gateway Outbe VMs.
pub const APP_COMPOSE_JSON: &str = include_str!("../../../../deploy/outbe-vm/app-compose.json");

const DEFAULT_UNIX_IO_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_HTTP_HEADER_BYTES: usize = 64 * 1024;
const MAX_HTTP_BODY_BYTES: usize = 32 * 1024 * 1024;
const HTTP_READ_BUF_CAPACITY: usize = 8 * 1024;
const CANCEL_POLL_SLICE: Duration = Duration::from_millis(25);

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CreateVmRequest {
    pub name: String,
    pub image: String,
    pub compose_file: String,
    pub vcpu: u32,
    /// Memory in MiB (dstack VMM expects MB).
    pub memory: u32,
    /// Disk size in GiB (dstack VMM expects GB).
    pub disk_size: u32,
    pub ports: Vec<PortMapping>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    pub user_config: String,
    pub hugepages: bool,
    pub pin_numa: bool,
    pub stopped: bool,
    pub no_tee: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub networking: Option<NetworkingConfig>,
    pub annotations: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NetworkingConfig {
    pub mode: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub bridge_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PortMapping {
    pub protocol: String,
    pub host_port: u32,
    pub vm_port: u32,
    pub host_address: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct IdMessage {
    id: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct StatusRequest {
    pub ids: Vec<String>,
    pub brief: bool,
    pub keyword: String,
    pub page: u32,
    pub page_size: u32,
    pub status: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct StatusResponse {
    #[serde(default)]
    pub vms: Vec<VmInfo>,
    #[serde(default)]
    pub total: u32,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct GetInfoResponse {
    #[serde(default)]
    pub found: bool,
    #[serde(default)]
    pub info: Option<VmInfo>,
}

impl GetInfoResponse {
    /// Return true when dstack VMM reports that this VM does not exist (`found == false` or `info == null`).
    pub fn is_gone(&self) -> bool {
        !self.found || self.info.is_none()
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct VmConfigurationSummary {
    #[serde(default)]
    pub image: String,
    #[serde(default)]
    pub vcpu: u32,
    #[serde(default)]
    pub memory: u32,
    #[serde(default)]
    pub app_id: Option<String>,
    #[serde(default)]
    pub stopped: Option<bool>,
    #[serde(default)]
    pub annotations: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct VmInfo {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub uptime: String,
    #[serde(default)]
    pub app_id: String,
    #[serde(default)]
    pub exited_at: Option<String>,
    #[serde(default)]
    pub boot_progress: String,
    #[serde(default)]
    pub boot_error: String,
    #[serde(default)]
    pub shutdown_progress: String,
    #[serde(default)]
    pub configuration: Option<VmConfigurationSummary>,
    #[serde(default)]
    pub events: Vec<serde_json::Value>,
    #[serde(default)]
    pub interfaces: Vec<NetworkInterfaceStatus>,
    #[serde(default)]
    pub running: bool,
}

impl VmInfo {
    /// Exact runtime status strings returned by dstack VMM (`vm_runtime_status`).
    pub const STATUS_RUNNING: &'static str = "running";
    /// Guest exited while `.started` remained set (the state reached after graceful `ShutdownVm`).
    pub const STATUS_EXITED: &'static str = "exited";
    pub const STATUS_STOPPING: &'static str = "stopping";
    pub const STATUS_STOPPED: &'static str = "stopped";
    pub const STATUS_REMOVING: &'static str = "removing";

    /// Return true when QEMU is not running and dstack reports a terminal stopped status.
    ///
    /// In dstack VMM (`vm_runtime_status`), a created-stopped or force-stopped VM reports
    /// `"stopped"`, whereas a VM powered off gracefully by `ShutdownVm` reports `"exited"`.
    pub fn is_stopped(&self) -> bool {
        !self.running
            && matches!(
                self.status.as_str(),
                Self::STATUS_STOPPED | Self::STATUS_EXITED
            )
    }

    /// Return true only when dstack reports the VM is administratively stopped (`.started == false`
    /// and QEMU not running), so `auto_restart` will not relaunch it.
    pub fn is_administratively_stopped(&self) -> bool {
        !self.running && self.status == Self::STATUS_STOPPED
    }

    /// Verify that this VM is stopped and has never been started, making initial `hda.img`
    /// substitution safe.
    pub fn ensure_never_started_and_stopped(&self) -> Result<()> {
        ensure!(
            !self.running && self.status == Self::STATUS_STOPPED,
            "refusing to stage hda.img for VM `{}` ({}): VM must be stopped and never started (status={}, running={})",
            self.name,
            self.id,
            self.status,
            self.running
        );
        ensure!(
            self.boot_progress.is_empty(),
            "refusing to stage hda.img for VM `{}` ({}): VM has already booted (boot_progress={})",
            self.name,
            self.id,
            self.boot_progress
        );
        ensure!(
            self.events.is_empty(),
            "refusing to stage hda.img for VM `{}` ({}): VM already has {} guest event(s) indicating prior boot",
            self.name,
            self.id,
            self.events.len()
        );
        let exited_at_clean = self
            .exited_at
            .as_deref()
            .is_none_or(|v| v.is_empty() || v == "never");
        ensure!(
            exited_at_clean,
            "refusing to stage hda.img for VM `{}` ({}): VM has prior exit timestamp ({:?})",
            self.name,
            self.id,
            self.exited_at
        );
        if let Some(cfg) = &self.configuration {
            ensure!(
                cfg.stopped != Some(false),
                "refusing to stage hda.img for VM `{}` ({}): VMM manifest records started state (configuration.stopped=false)",
                self.name,
                self.id
            );
        }
        Ok(())
    }

    /// Return the configured guest image name reported by VMM, if present.
    pub fn image_name(&self) -> Option<&str> {
        self.configuration
            .as_ref()
            .map(|cfg| cfg.image.as_str())
            .filter(|name| !name.is_empty())
    }

    /// Return one VMM manifest annotation value, if present in `configuration.annotations`.
    pub fn annotation(&self, key: &str) -> Option<&str> {
        self.configuration
            .as_ref()
            .and_then(|cfg| cfg.annotations.get(key))
            .map(String::as_str)
    }

    /// Return the effective `app_id` from `configuration.app_id` or top-level `VmInfo.app_id`.
    pub fn effective_app_id(&self) -> Option<&str> {
        self.configuration
            .as_ref()
            .and_then(|cfg| cfg.app_id.as_deref())
            .filter(|s| !s.is_empty())
            .or_else(|| (!self.app_id.is_empty()).then_some(self.app_id.as_str()))
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct NetworkInterfaceStatus {
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub mac: String,
}

/// Transport used by `VmmClient` to call dstack VMM pRPC endpoints and read serial logs.
pub trait VmmTransport {
    /// Send a JSON `POST /prpc/<method>?json` request and return the response body string.
    fn prpc_post(&self, method: &str, payload: &[u8]) -> Result<String>;

    /// Send a JSON `POST /prpc/<method>?json` request bounded by an overall total `deadline`.
    fn prpc_post_with_deadline(
        &self,
        method: &str,
        payload: &[u8],
        deadline: Option<Instant>,
    ) -> Result<String> {
        let _ = deadline;
        self.prpc_post(method, payload)
    }

    /// Read the last `lines` lines of the VM's serial console log.
    fn read_serial_log(&self, vm_id: &str, lines: usize) -> Result<String>;
}

/// Direct local Unix domain socket transport speaking HTTP/1.1 with no `curl` or shell.
///
/// Enforces a total per-request deadline across connect, write, headers, and body (not merely
/// per-read inactivity), honors an optional cancellation flag, and bounds HTTP header and chunk
/// framing bytes before allocation.
#[derive(Clone, Debug)]
pub struct LocalUnixVmmTransport {
    socket_path: PathBuf,
    vmm_home: PathBuf,
    io_timeout: Duration,
    cancel: Option<Arc<AtomicBool>>,
}

impl LocalUnixVmmTransport {
    pub fn new(socket_path: impl Into<PathBuf>, vmm_home: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
            vmm_home: vmm_home.into(),
            io_timeout: DEFAULT_UNIX_IO_TIMEOUT,
            cancel: None,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.io_timeout = timeout;
        self
    }

    pub fn with_cancel(mut self, cancel: Arc<AtomicBool>) -> Self {
        self.cancel = Some(cancel);
        self
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub fn vmm_home(&self) -> &Path {
        &self.vmm_home
    }

    fn http_request(
        &self,
        http_method: &str,
        target: &str,
        content_type: Option<&str>,
        body: &[u8],
        context_label: &str,
        caller_deadline: Option<Instant>,
    ) -> Result<Vec<u8>> {
        let now = Instant::now();
        let default_deadline = now + self.io_timeout;
        let deadline = match caller_deadline {
            Some(d) => d.min(default_deadline),
            None => default_deadline,
        };
        if self.is_canceled() {
            bail!("VMM {context_label} canceled by signal");
        }
        ensure!(
            now < deadline,
            "VMM {context_label} exceeded total deadline before connecting"
        );

        let stream = connect_unix_with_deadline(
            &self.socket_path,
            deadline,
            self.cancel.as_deref(),
            context_label,
        )?;
        let mut deadline_stream = DeadlineUnixStream {
            stream,
            deadline,
            cancel: self.cancel.clone(),
        };

        let mut request =
            format!("{http_method} {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n");
        if let Some(ct) = content_type {
            request.push_str(&format!("Content-Type: {ct}\r\n"));
        }
        if http_method == "POST" || !body.is_empty() {
            request.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        request.push_str("\r\n");

        deadline_stream
            .write_all(request.as_bytes())
            .wrap_err_with(|| format!("write HTTP headers for {context_label}"))?;
        if !body.is_empty() {
            deadline_stream
                .write_all(body)
                .wrap_err_with(|| format!("write HTTP body for {context_label}"))?;
        }
        deadline_stream
            .flush()
            .wrap_err_with(|| format!("flush HTTP request for {context_label}"))?;

        let mut reader = BufReader::with_capacity(HTTP_READ_BUF_CAPACITY, deadline_stream);
        let parsed = parse_http_response(&mut reader)
            .wrap_err_with(|| format!("read HTTP response for {context_label}"))?;
        if !(200..300).contains(&parsed.status_code) {
            let body_text = String::from_utf8_lossy(&parsed.body);
            bail!(
                "VMM {context_label} returned HTTP {}: {}",
                parsed.status_code,
                body_text.trim()
            );
        }
        Ok(parsed.body)
    }

    fn is_canceled(&self) -> bool {
        self.cancel
            .as_deref()
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
    }
}

/// Connect to a Unix domain socket bounded by `deadline` (and optional `cancel`), avoiding
/// indefinite blocking when the listener accept queue is full or stalled.
fn connect_unix_with_deadline(
    socket_path: &Path,
    deadline: Instant,
    cancel: Option<&AtomicBool>,
    context_label: &str,
) -> Result<UnixStream> {
    let path_owned = socket_path.to_path_buf();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let res = UnixStream::connect(&path_owned);
        // If the caller already timed out or canceled and dropped `rx`, `send` returns Err
        // and any late-connected stream is closed immediately on drop.
        let _ = tx.send(res);
    });

    loop {
        if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            bail!(
                "VMM {context_label} canceled while connecting to {}",
                socket_path.display()
            );
        }
        let now = Instant::now();
        if now >= deadline {
            bail!(
                "VMM {context_label} timed out connecting to local VMM Unix socket {}",
                socket_path.display()
            );
        }
        let remaining = deadline.saturating_duration_since(now);
        let wait = if cancel.is_some() {
            remaining.min(CANCEL_POLL_SLICE)
        } else {
            remaining
        };
        match rx.recv_timeout(wait) {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(err)) => {
                return Err(err).wrap_err_with(|| {
                    format!("connect to local VMM Unix socket {}", socket_path.display())
                });
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                bail!(
                    "VMM connect helper exited unexpectedly for {}",
                    socket_path.display()
                );
            }
        }
    }
}

/// Wrapper around `UnixStream` enforcing a monotonic wall-clock deadline and optional signal
/// cancellation across every read/write/flush syscall.
struct DeadlineUnixStream {
    stream: UnixStream,
    deadline: Instant,
    cancel: Option<Arc<AtomicBool>>,
}

impl DeadlineUnixStream {
    fn is_canceled(&self) -> bool {
        self.cancel
            .as_deref()
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
    }

    fn next_slice(&self) -> io::Result<Duration> {
        if self.is_canceled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "VMM HTTP request canceled by signal",
            ));
        }
        let now = Instant::now();
        if now >= self.deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "VMM HTTP request exceeded total deadline",
            ));
        }
        let remaining = self.deadline.saturating_duration_since(now);
        if self.cancel.is_some() {
            Ok(remaining.min(CANCEL_POLL_SLICE))
        } else {
            Ok(remaining)
        }
    }
}

impl Read for DeadlineUnixStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let slice = self.next_slice()?;
            self.stream.set_read_timeout(Some(slice))?;
            match self.stream.read(buf) {
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err)
                    if matches!(
                        err.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    if self.is_canceled() {
                        return Err(io::Error::new(
                            io::ErrorKind::Interrupted,
                            "VMM HTTP request canceled by signal",
                        ));
                    }
                    if Instant::now() >= self.deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "VMM HTTP request timed out",
                        ));
                    }
                    continue;
                }
                other => return other,
            }
        }
    }
}

impl Write for DeadlineUnixStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        loop {
            let slice = self.next_slice()?;
            self.stream.set_write_timeout(Some(slice))?;
            match self.stream.write(buf) {
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err)
                    if matches!(
                        err.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    if self.is_canceled() {
                        return Err(io::Error::new(
                            io::ErrorKind::Interrupted,
                            "VMM HTTP write canceled by signal",
                        ));
                    }
                    if Instant::now() >= self.deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "VMM HTTP write timed out",
                        ));
                    }
                    continue;
                }
                other => return other,
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        let slice = self.next_slice()?;
        self.stream.set_write_timeout(Some(slice))?;
        self.stream.flush()
    }
}

impl VmmTransport for LocalUnixVmmTransport {
    fn prpc_post(&self, method: &str, payload: &[u8]) -> Result<String> {
        self.prpc_post_with_deadline(method, payload, None)
    }

    fn prpc_post_with_deadline(
        &self,
        method: &str,
        payload: &[u8],
        deadline: Option<Instant>,
    ) -> Result<String> {
        ensure!(
            !method.is_empty()
                && method
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.')),
            "invalid VMM pRPC method name: {method}"
        );
        let target = format!("/prpc/{method}?json");
        let body = self.http_request(
            "POST",
            &target,
            Some("application/json"),
            payload,
            &format!("pRPC {method}"),
            deadline,
        )?;
        String::from_utf8(body)
            .wrap_err_with(|| format!("VMM pRPC {method} returned non-UTF-8 body"))
    }

    fn read_serial_log(&self, vm_id: &str, lines: usize) -> Result<String> {
        validate_vm_id(vm_id)?;
        if lines == 0 {
            return Ok(String::new());
        }
        let log_path = self.vmm_home.join("vm").join(vm_id).join("serial.log");
        if let Ok(meta) = fs::symlink_metadata(&log_path) {
            ensure!(
                meta.file_type().is_file(),
                "serial log path is not a regular file: {}",
                log_path.display()
            );
            let bytes = fs::read(&log_path)
                .wrap_err_with(|| format!("read serial log {}", log_path.display()))?;
            let text = String::from_utf8_lossy(&bytes);
            return Ok(tail_lines(&text, lines));
        }

        let encoded_id = percent_encode_query_component(vm_id);
        let target = format!("/logs?id={encoded_id}&follow=false&ansi=false&lines={lines}");
        let body = self.http_request("GET", &target, None, &[], "GET /logs", None)?;
        Ok(String::from_utf8_lossy(&body).into_owned())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedHttpResponse {
    status_code: u16,
    body: Vec<u8>,
}

/// Parse an HTTP/1.x response supporting `Content-Length`, `Transfer-Encoding: chunked`,
/// and EOF-terminated `Connection: close` bodies.
fn parse_http_response<R: BufRead>(reader: &mut R) -> Result<ParsedHttpResponse> {
    let mut header_bytes = 0usize;
    let status_line = read_bounded_line(reader, &mut header_bytes, MAX_HTTP_HEADER_BYTES)?;
    let status_code = parse_http_status_line(&status_line)?;

    let mut content_length: Option<usize> = None;
    let mut is_chunked = false;

    loop {
        let line = read_bounded_line(reader, &mut header_bytes, MAX_HTTP_HEADER_BYTES)?;
        if line.is_empty() {
            break;
        }
        let Some((raw_name, raw_value)) = line.split_once(':') else {
            bail!("malformed HTTP header line: {line}");
        };
        let name = raw_name.trim().to_ascii_lowercase();
        let value = raw_value.trim();
        match name.as_str() {
            "content-length" => {
                let parsed: usize = value
                    .parse()
                    .wrap_err_with(|| format!("invalid Content-Length header: {value}"))?;
                if let Some(prev) = content_length {
                    ensure!(
                        prev == parsed,
                        "conflicting Content-Length headers ({prev} vs {parsed})"
                    );
                }
                content_length = Some(parsed);
            }
            "transfer-encoding"
                if value
                    .split(',')
                    .any(|token| token.trim().eq_ignore_ascii_case("chunked")) =>
            {
                is_chunked = true;
            }
            _ => {}
        }
    }

    let body = if is_chunked {
        read_chunked_body(reader)?
    } else if let Some(len) = content_length {
        ensure!(
            len <= MAX_HTTP_BODY_BYTES,
            "HTTP response Content-Length {len} exceeds maximum {MAX_HTTP_BODY_BYTES}"
        );
        // Read incrementally rather than pre-allocating `len` bytes up front.
        let mut buf = Vec::with_capacity(len.min(MAX_HTTP_HEADER_BYTES));
        let mut limited = reader.take(len as u64);
        limited
            .read_to_end(&mut buf)
            .wrap_err("read HTTP response body")?;
        ensure!(
            buf.len() == len,
            "truncated HTTP response body (expected {len} bytes, got {})",
            buf.len()
        );
        buf
    } else {
        let mut buf = Vec::new();
        let mut limited = reader.take((MAX_HTTP_BODY_BYTES + 1) as u64);
        limited
            .read_to_end(&mut buf)
            .wrap_err("read EOF-terminated HTTP response body")?;
        ensure!(
            buf.len() <= MAX_HTTP_BODY_BYTES,
            "HTTP response body exceeds maximum {MAX_HTTP_BODY_BYTES} bytes"
        );
        buf
    };

    Ok(ParsedHttpResponse { status_code, body })
}

/// Read one LF-terminated HTTP header/framing line without ever allocating past `max_total_bytes`.
fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    total_bytes: &mut usize,
    max_total_bytes: usize,
) -> Result<String> {
    let mut line = Vec::new();
    loop {
        let remaining = max_total_bytes.saturating_sub(*total_bytes);
        ensure!(
            remaining > 0,
            "HTTP response headers exceed maximum {max_total_bytes} bytes"
        );
        let available = reader.fill_buf().wrap_err("read HTTP header line")?;
        if available.is_empty() {
            bail!("unexpected EOF while reading HTTP response headers");
        }
        let scan_len = available.len().min(remaining);
        if let Some(pos) = available[..scan_len].iter().position(|&b| b == b'\n') {
            let take = pos + 1;
            line.extend_from_slice(&available[..take]);
            reader.consume(take);
            *total_bytes += take;
            break;
        }
        if available.len() >= remaining {
            bail!("HTTP response headers exceed maximum {max_total_bytes} bytes");
        }
        let take = available.len();
        line.extend_from_slice(available);
        reader.consume(take);
        *total_bytes += take;
    }
    while matches!(line.last(), Some(b'\n' | b'\r')) {
        line.pop();
    }
    String::from_utf8(line).wrap_err("non-UTF-8 bytes in HTTP response header")
}

fn parse_http_status_line(line: &str) -> Result<u16> {
    let mut parts = line.split_whitespace();
    let version = parts
        .next()
        .ok_or_else(|| eyre::eyre!("empty HTTP status line"))?;
    ensure!(
        version.starts_with("HTTP/1."),
        "unsupported HTTP version in status line: {line}"
    );
    let code_str = parts
        .next()
        .ok_or_else(|| eyre::eyre!("missing HTTP status code in status line: {line}"))?;
    ensure!(
        code_str.len() == 3 && code_str.bytes().all(|b| b.is_ascii_digit()),
        "invalid HTTP status code in status line: {line}"
    );
    code_str
        .parse::<u16>()
        .wrap_err_with(|| format!("parse HTTP status code from: {line}"))
}

fn read_chunked_body<R: BufRead>(reader: &mut R) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut framing_bytes = 0usize;
    loop {
        let size_line = read_bounded_line(reader, &mut framing_bytes, MAX_HTTP_HEADER_BYTES)?;
        let size_hex = size_line.split(';').next().unwrap_or("").trim();
        ensure!(!size_hex.is_empty(), "empty HTTP chunk size line");
        let chunk_size = usize::from_str_radix(size_hex, 16)
            .wrap_err_with(|| format!("invalid HTTP chunk size: {size_hex}"))?;
        if chunk_size == 0 {
            // Consume optional trailer headers until the terminating blank line.
            loop {
                let trailer = read_bounded_line(reader, &mut framing_bytes, MAX_HTTP_HEADER_BYTES)?;
                if trailer.is_empty() {
                    break;
                }
            }
            break;
        }
        let next_len = out
            .len()
            .checked_add(chunk_size)
            .ok_or_else(|| eyre::eyre!("HTTP chunked body size overflow"))?;
        ensure!(
            next_len <= MAX_HTTP_BODY_BYTES,
            "HTTP chunked body exceeds maximum {MAX_HTTP_BODY_BYTES} bytes"
        );
        let start = out.len();
        out.resize(next_len, 0);
        reader
            .read_exact(&mut out[start..next_len])
            .wrap_err("truncated HTTP chunk payload")?;
        let mut crlf = [0u8; 2];
        reader
            .read_exact(&mut crlf)
            .wrap_err("missing CRLF after HTTP chunk payload")?;
        ensure!(
            crlf == *b"\r\n",
            "expected CRLF after HTTP chunk payload, got {crlf:?}"
        );
    }
    Ok(out)
}

fn tail_lines(text: &str, lines: usize) -> String {
    if lines == 0 || text.is_empty() {
        return String::new();
    }
    let collected: Vec<&str> = text.lines().collect();
    let start = collected.len().saturating_sub(lines);
    let mut out = collected[start..].join("\n");
    if text.ends_with('\n') && !out.is_empty() {
        out.push('\n');
    }
    out
}

#[derive(Clone)]
pub struct VmmClient<T> {
    transport: T,
    label: String,
}

impl<T: VmmTransport> VmmClient<T> {
    pub fn new(transport: T, label: impl Into<String>) -> Self {
        Self {
            transport,
            label: label.into(),
        }
    }

    pub fn create_vm(&self, request: &CreateVmRequest) -> Result<String> {
        self.create_vm_with_deadline(request, None)
    }

    pub fn create_vm_with_deadline(
        &self,
        request: &CreateVmRequest,
        deadline: Option<Instant>,
    ) -> Result<String> {
        let response: IdMessage = self.prpc_call("CreateVm", request, deadline)?;
        validate_vm_id(&response.id)?;
        Ok(response.id)
    }

    pub fn start_vm(&self, vm_id: &str) -> Result<()> {
        validate_vm_id(vm_id)?;
        let _: serde_json::Value = self.prpc_call(
            "StartVm",
            &IdMessage {
                id: vm_id.to_owned(),
            },
            None,
        )?;
        Ok(())
    }

    /// Request a graceful guest OS poweroff via dstack VMM `ShutdownVm`.
    ///
    /// In dstack VMM, `ShutdownVm` forwards to the guest agent (`systemctl poweroff`) and never
    /// falls back to a forced kill; once QEMU exits, VMM reports `running = false`, `status = "exited"`.
    pub fn shutdown_vm(&self, vm_id: &str) -> Result<()> {
        self.shutdown_vm_with_deadline(vm_id, None)
    }

    pub fn shutdown_vm_with_deadline(&self, vm_id: &str, deadline: Option<Instant>) -> Result<()> {
        validate_vm_id(vm_id)?;
        let _: serde_json::Value = self.prpc_call(
            "ShutdownVm",
            &IdMessage {
                id: vm_id.to_owned(),
            },
            deadline,
        )?;
        Ok(())
    }

    /// Force-stop a VM via dstack VMM `StopVm` (terminates QEMU without waiting for guest poweroff,
    /// clears `.started`, and releases TAP interfaces).
    pub fn stop_vm(&self, vm_id: &str) -> Result<()> {
        self.stop_vm_with_deadline(vm_id, None)
    }

    pub fn stop_vm_with_deadline(&self, vm_id: &str, deadline: Option<Instant>) -> Result<()> {
        validate_vm_id(vm_id)?;
        let _: serde_json::Value = self.prpc_call(
            "StopVm",
            &IdMessage {
                id: vm_id.to_owned(),
            },
            deadline,
        )?;
        Ok(())
    }

    pub fn remove_vm(&self, vm_id: &str) -> Result<()> {
        self.remove_vm_with_deadline(vm_id, None)
    }

    pub fn remove_vm_with_deadline(&self, vm_id: &str, deadline: Option<Instant>) -> Result<()> {
        validate_vm_id(vm_id)?;
        let _: serde_json::Value = self.prpc_call(
            "RemoveVm",
            &IdMessage {
                id: vm_id.to_owned(),
            },
            deadline,
        )?;
        Ok(())
    }

    pub fn status(&self, request: &StatusRequest) -> Result<StatusResponse> {
        self.status_with_deadline(request, None)
    }

    pub fn status_with_deadline(
        &self,
        request: &StatusRequest,
        deadline: Option<Instant>,
    ) -> Result<StatusResponse> {
        let response: StatusResponse = self.prpc_call("Status", request, deadline)?;
        for vm in &response.vms {
            if !vm.id.is_empty() {
                validate_vm_id(&vm.id)?;
            }
        }
        Ok(response)
    }

    pub fn get_info(&self, vm_id: &str) -> Result<GetInfoResponse> {
        self.get_info_with_deadline(vm_id, None)
    }

    pub fn get_info_with_deadline(
        &self,
        vm_id: &str,
        deadline: Option<Instant>,
    ) -> Result<GetInfoResponse> {
        validate_vm_id(vm_id)?;
        let response: GetInfoResponse = self.prpc_call(
            "GetInfo",
            &IdMessage {
                id: vm_id.to_owned(),
            },
            deadline,
        )?;
        if let Some(info) = &response.info {
            if !info.id.is_empty() {
                validate_vm_id(&info.id)?;
            }
        }
        Ok(response)
    }

    pub fn get_meta(&self) -> Result<serde_json::Value> {
        self.prpc_call("GetMeta", &serde_json::json!({}), None)
    }

    /// Read the tail of the VM serial log from `<vmm_home>/vm/<vm_id>/serial.log` (or VMM `/logs`).
    pub fn serial_log_tail(&self, vm_id: &str, lines: usize) -> Result<String> {
        validate_vm_id(vm_id)?;
        self.transport.read_serial_log(vm_id, lines)
    }

    fn prpc_call<Req: Serialize, Resp: DeserializeOwned>(
        &self,
        method: &str,
        request: &Req,
        deadline: Option<Instant>,
    ) -> Result<Resp> {
        let payload = serde_json::to_vec(request)?;
        let body = self
            .transport
            .prpc_post_with_deadline(method, &payload, deadline)
            .wrap_err_with(|| format!("VMM pRPC {method} failed on {}", self.label))?;
        if body.trim().is_empty() {
            return serde_json::from_str("{}")
                .wrap_err_with(|| format!("VMM pRPC {method} returned empty body"));
        }
        serde_json::from_str(&body)
            .wrap_err_with(|| format!("invalid JSON from VMM pRPC {method}: {}", body.trim()))
    }
}

/// Validate that `vm_id` is a canonical 36-character UUID (`8-4-4-4-12` hex).
pub fn validate_vm_id(vm_id: &str) -> Result<()> {
    let bytes = vm_id.as_bytes();
    let valid = bytes.len() == 36
        && bytes.iter().enumerate().all(|(idx, byte)| {
            if matches!(idx, 8 | 13 | 18 | 23) {
                *byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        });
    ensure!(valid, "invalid VM ID (expected canonical UUID): {vm_id}");
    Ok(())
}

/// Build the remote shell `curl --unix-socket` command for one VMM pRPC call (legacy SSH transport).
pub fn build_prpc_curl_command(vmm_home: &str, method: &str) -> String {
    let sock_expr = remote_vmm_path(vmm_home, "run/vmm.sock");
    format!(
        "curl -sS --connect-timeout 5 --max-time 60 --unix-socket {sock_expr} -H 'Content-Type: application/json' -w '\\n%{{http_code}}' -X POST --data-binary @- {}",
        shell_quote(&format!("http://localhost/prpc/{method}?json"))
    )
}

/// Build the remote shell command to read the serial log tail safely (legacy SSH transport).
pub fn build_serial_log_command(vmm_home: &str, vm_id: &str, lines: usize) -> Result<String> {
    validate_vm_id(vm_id)?;
    let log_expr = remote_vmm_path(vmm_home, &format!("vm/{vm_id}/serial.log"));
    let sock_expr = remote_vmm_path(vmm_home, "run/vmm.sock");
    let encoded_id = percent_encode_query_component(vm_id);
    let url =
        format!("http://localhost/logs?id={encoded_id}&follow=false&ansi=false&lines={lines}");
    let url_quoted = shell_quote(&url);
    Ok(format!(
        "if [ -f {log_expr} ]; then tail -n {lines} {log_expr}; else curl -fsS --unix-socket {sock_expr} {url_quoted} 2>/dev/null || true; fi"
    ))
}

fn percent_encode_query_component(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// Build a shell expression for `<vmm_home>/<relative>`, expanding a leading `~/`.
pub fn remote_vmm_path(vmm_home: &str, relative: &str) -> String {
    let trimmed_home = vmm_home.trim_end_matches('/');
    let trimmed_rel = relative.trim_start_matches('/');
    let joined = if trimmed_rel.is_empty() {
        trimmed_home.to_owned()
    } else {
        format!("{trimmed_home}/{trimmed_rel}")
    };
    if let Some(rest) = joined.strip_prefix("~/") {
        format!("\"$HOME\"/{}", shell_quote(rest))
    } else if joined == "~" {
        "\"$HOME\"".to_owned()
    } else {
        shell_quote(&joined)
    }
}

/// Compute the dstack `app_id` (first 40 lowercase hex chars of SHA-256 of `compose_file`).
pub fn calc_app_id(compose_file: &str) -> String {
    let digest = Sha256::digest(compose_file.as_bytes());
    hex::encode(digest)[..40].to_owned()
}

/// Extract the primary NIC MAC address from VMM `VmInfo`.
pub fn extract_primary_mac(info: &VmInfo) -> Result<String> {
    let selected = info
        .interfaces
        .iter()
        .find(|iface| iface.mode == "bridge" && !iface.mac.is_empty())
        .or_else(|| info.interfaces.iter().find(|iface| !iface.mac.is_empty()))
        .ok_or_else(|| {
            eyre::eyre!(
                "VMM did not report a network interface MAC for VM {} ({})",
                info.name,
                info.id
            )
        })?;
    normalize_mac(&selected.mac)
}

/// Validate and lowercase a colon-separated 6-octet MAC address.
fn normalize_mac(raw: &str) -> Result<String> {
    let trimmed = raw.trim().to_ascii_lowercase();
    let octets: Vec<&str> = trimmed.split(':').collect();
    ensure!(
        octets.len() == 6
            && octets
                .iter()
                .all(|octet| octet.len() == 2 && octet.bytes().all(|b| b.is_ascii_hexdigit())),
        "invalid MAC address from VMM: {raw}"
    );
    Ok(trimmed)
}

pub fn parse_curl_http_response<'a>(method: &str, raw: &'a str) -> Result<&'a str> {
    let trimmed = raw.trim_end_matches(['\r', '\n']);
    let (body, status_str) = match trimmed.rsplit_once('\n') {
        Some((body, code)) => (body, code.trim()),
        None => ("", trimmed.trim()),
    };
    let status: u16 = status_str
        .parse()
        .wrap_err_with(|| format!("VMM pRPC {method} returned non-HTTP output: {trimmed}"))?;
    if !(200..300).contains(&status) {
        bail!("VMM pRPC {method} returned HTTP {status}: {}", body.trim());
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use std::{
        io::Cursor,
        os::unix::net::UnixListener,
        sync::{Arc, Mutex},
        thread,
    };

    use serde_json::{json, Value};

    use super::*;

    #[test]
    fn create_vm_request_matches_dstack_prpc_shape() {
        let app_id = calc_app_id(APP_COMPOSE_JSON);
        assert_eq!(app_id.len(), 40);
        assert!(app_id.bytes().all(|b| b.is_ascii_hexdigit()));

        let req = CreateVmRequest {
            name: "vm-devnet-validator-0".into(),
            image: "outbe-dev-0.1".into(),
            compose_file: APP_COMPOSE_JSON.into(),
            vcpu: 3,
            memory: 5 * 1024,
            disk_size: 40,
            ports: Vec::new(),
            app_id: Some(app_id.clone()),
            user_config: String::new(),
            hugepages: false,
            pin_numa: false,
            stopped: true,
            no_tee: false,
            networking: Some(NetworkingConfig {
                mode: "bridge".into(),
                bridge_name: "obr0".into(),
            }),
            annotations: BTreeMap::new(),
        };
        let value: Value = serde_json::to_value(&req).unwrap();
        assert_eq!(value["name"], json!("vm-devnet-validator-0"));
        assert_eq!(value["image"], json!("outbe-dev-0.1"));
        assert_eq!(value["vcpu"], json!(3));
        assert_eq!(value["memory"], json!(5120));
        assert_eq!(value["disk_size"], json!(40));
        assert_eq!(value["ports"], json!([]));
        assert_eq!(value["app_id"], json!(app_id));
        assert_eq!(value["user_config"], json!(""));
        assert_eq!(value["hugepages"], json!(false));
        assert_eq!(value["pin_numa"], json!(false));
        assert_eq!(value["stopped"], json!(true));
        assert_eq!(value["no_tee"], json!(false));
        assert_eq!(
            value["networking"],
            json!({"mode": "bridge", "bridge_name": "obr0"})
        );
        assert_eq!(value["annotations"], json!({}));
        // Ensure KMS/gateway/encrypted_env fields are absent for key_provider: none.
        assert!(value.get("kms_urls").is_none());
        assert!(value.get("gateway_urls").is_none());
        assert!(value.get("encrypted_env").is_none());
    }

    #[test]
    fn id_and_status_requests_match_dstack_prpc_shape() {
        let id_req = serde_json::to_value(&IdMessage {
            id: "abc-123".into(),
        })
        .unwrap();
        assert_eq!(id_req, json!({"id": "abc-123"}));

        let status_req = serde_json::to_value(&StatusRequest {
            ids: vec!["vm-1".into()],
            brief: true,
            ..StatusRequest::default()
        })
        .unwrap();
        assert_eq!(
            status_req,
            json!({
                "ids": ["vm-1"],
                "brief": true,
                "keyword": "",
                "page": 0,
                "page_size": 0,
                "status": ""
            })
        );
    }

    #[test]
    fn extract_primary_mac_prefers_bridge_and_rejects_invalid() {
        let response: GetInfoResponse = serde_json::from_value(json!({
            "found": true,
            "info": {
                "id": "vm-42",
                "name": "validator-0",
                "status": "stopped",
                "running": false,
                "interfaces": [
                    { "mode": "user", "backend": "slirp", "mac": "52:54:00:11:22:33" },
                    { "mode": "bridge", "backend": "tap_bridge", "mac": "52:54:00:AA:BB:CC", "bridge_name": "obr0" }
                ]
            }
        }))
        .unwrap();
        let info = response.info.unwrap();
        assert_eq!(extract_primary_mac(&info).unwrap(), "52:54:00:aa:bb:cc");

        let empty = VmInfo::default();
        assert!(extract_primary_mac(&empty).is_err());

        assert!(normalize_mac("not-a-mac").is_err());
        assert!(normalize_mac("52:54:00:11:22").is_err());
    }

    #[test]
    fn remote_vmm_path_expands_tilde_safely() {
        assert_eq!(
            remote_vmm_path("~/.dstack-vmm", "run/vmm.sock"),
            "\"$HOME\"/'.dstack-vmm/run/vmm.sock'"
        );
        assert_eq!(
            remote_vmm_path("/srv/dstack-vmm/", "vm/abc/hda.img"),
            "'/srv/dstack-vmm/vm/abc/hda.img'"
        );
    }

    #[test]
    fn parse_curl_http_response_handles_ok_empty_and_errors() {
        assert_eq!(
            parse_curl_http_response("CreateVm", "{\"id\":\"vm-1\"}\n200\n").unwrap(),
            "{\"id\":\"vm-1\"}"
        );
        assert_eq!(parse_curl_http_response("StartVm", "200\n").unwrap(), "");
        let err = parse_curl_http_response("CreateVm", "already exists\n400\n").unwrap_err();
        assert!(err.to_string().contains("HTTP 400"), "{err}");
        assert!(err.to_string().contains("already exists"), "{err}");
    }

    #[test]
    fn validate_vm_id_rejects_shell_injection_and_non_uuid() {
        let valid = "550e8400-e29b-41d4-a716-446655440000";
        validate_vm_id(valid).unwrap();
        let cmd = build_serial_log_command("~/.dstack-vmm", valid, 40).unwrap();
        assert!(cmd.contains(&format!(
            "'http://localhost/logs?id={valid}&follow=false&ansi=false&lines=40'"
        )));

        let injection = "x'; printf REVIEW_INJECTION_OK >&2; fi; #";
        assert!(validate_vm_id(injection).is_err());
        assert!(build_serial_log_command("~/.dstack-vmm", injection, 40).is_err());
        assert!(validate_vm_id("vm-0001").is_err());
        assert!(validate_vm_id("").is_err());
    }

    #[test]
    fn vm_info_is_stopped_matches_dstack_runtime_status_states() {
        let stopped = VmInfo {
            status: VmInfo::STATUS_STOPPED.into(),
            running: false,
            ..VmInfo::default()
        };
        assert!(stopped.is_stopped());
        assert!(stopped.is_administratively_stopped());
        stopped.ensure_never_started_and_stopped().unwrap();

        // Graceful ShutdownVm powers off guest while .started stays true -> status = "exited".
        let exited = VmInfo {
            status: VmInfo::STATUS_EXITED.into(),
            running: false,
            ..VmInfo::default()
        };
        assert!(exited.is_stopped());
        assert!(!exited.is_administratively_stopped());
        assert!(exited.ensure_never_started_and_stopped().is_err());

        for (status, running) in [
            (VmInfo::STATUS_RUNNING, true),
            (VmInfo::STATUS_STOPPING, true),
            (VmInfo::STATUS_REMOVING, false),
            (VmInfo::STATUS_STOPPED, true),
            ("unknown", false),
        ] {
            let info = VmInfo {
                status: status.into(),
                running,
                ..VmInfo::default()
            };
            assert!(
                !info.is_stopped(),
                "status={status} running={running} must not count as stopped"
            );
            assert!(info.ensure_never_started_and_stopped().is_err());
        }
    }

    #[test]
    fn stub_transport_exercises_all_vmm_client_prpc_requests() {
        struct StubTransport {
            calls: Arc<Mutex<Vec<(String, Value)>>>,
        }

        impl VmmTransport for StubTransport {
            fn prpc_post(&self, method: &str, payload: &[u8]) -> Result<String> {
                let parsed: Value = serde_json::from_slice(payload)?;
                self.calls.lock().unwrap().push((method.to_owned(), parsed));
                let uuid = "550e8400-e29b-41d4-a716-446655440000";
                match method {
                    "CreateVm" => Ok(format!("{{\"id\":\"{uuid}\"}}")),
                    "GetInfo" => Ok(json!({
                        "found": true,
                        "info": {
                            "id": uuid,
                            "name": "dev-vm",
                            "status": "exited",
                            "uptime": "0s",
                            "running": false,
                            "configuration": { "image": "outbe-vm-dev-0123456789abcdef" },
                            "interfaces": [{ "mode": "bridge", "mac": "52:54:00:11:22:33" }]
                        }
                    })
                    .to_string()),
                    "StartVm" | "ShutdownVm" | "StopVm" | "RemoveVm" => Ok("{}".into()),
                    _ => Err(eyre::eyre!("unexpected method {method}")),
                }
            }

            fn read_serial_log(&self, vm_id: &str, lines: usize) -> Result<String> {
                Ok(format!("log:{vm_id}:{lines}\n"))
            }
        }

        let calls = Arc::new(Mutex::new(Vec::new()));
        let client = VmmClient::new(
            StubTransport {
                calls: Arc::clone(&calls),
            },
            "stub",
        );

        let id = client
            .create_vm(&CreateVmRequest {
                name: "dev-vm".into(),
                image: "outbe-vm-dev-0123456789abcdef".into(),
                compose_file: APP_COMPOSE_JSON.into(),
                vcpu: 3,
                memory: 5120,
                disk_size: 10,
                ports: Vec::new(),
                app_id: Some(calc_app_id(APP_COMPOSE_JSON)),
                user_config: String::new(),
                hugepages: false,
                pin_numa: false,
                stopped: true,
                no_tee: false,
                networking: Some(NetworkingConfig {
                    mode: "bridge".into(),
                    bridge_name: "obr0".into(),
                }),
                annotations: BTreeMap::new(),
            })
            .unwrap();
        client.start_vm(&id).unwrap();
        client.shutdown_vm(&id).unwrap();
        client.stop_vm(&id).unwrap();
        let info = client.get_info(&id).unwrap();
        assert!(info.found);
        assert!(info.info.as_ref().unwrap().is_stopped());
        assert_eq!(
            info.info.as_ref().unwrap().image_name(),
            Some("outbe-vm-dev-0123456789abcdef")
        );
        assert_eq!(
            client.serial_log_tail(&id, 25).unwrap(),
            format!("log:{id}:25\n")
        );
        client.remove_vm(&id).unwrap();

        let recorded = calls.lock().unwrap();
        let methods: Vec<&str> = recorded.iter().map(|(m, _)| m.as_str()).collect();
        assert_eq!(
            methods,
            vec![
                "CreateVm",
                "StartVm",
                "ShutdownVm",
                "StopVm",
                "GetInfo",
                "RemoveVm"
            ]
        );
        assert_eq!(recorded[0].1["networking"]["mode"], json!("bridge"));
        assert_eq!(recorded[0].1["networking"]["bridge_name"], json!("obr0"));
        assert_eq!(recorded[2].1, json!({"id": id}));
    }

    #[test]
    fn http_parser_handles_content_length_chunked_eof_and_errors() {
        // Content-Length response.
        let raw_cl =
            b"HTTP/1.1 200 OK\r\nContent-Length: 13\r\nConnection: close\r\n\r\n{\"ok\":\"true\"}";
        let parsed = parse_http_response(&mut Cursor::new(raw_cl)).unwrap();
        assert_eq!(parsed.status_code, 200);
        assert_eq!(parsed.body, br#"{"ok":"true"}"#);

        // Chunked response with chunk extension and trailer header.
        let raw_chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n7;ext=1\r\n{\"a\":1,\r\n6\r\n\"b\":2}\r\n0\r\nX-Trailer: yes\r\n\r\n";
        let parsed = parse_http_response(&mut Cursor::new(raw_chunked)).unwrap();
        assert_eq!(parsed.status_code, 200);
        assert_eq!(parsed.body, br#"{"a":1,"b":2}"#);

        // Connection-close EOF-framed response without Content-Length.
        let raw_eof = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{\"eof\":true}";
        let parsed = parse_http_response(&mut Cursor::new(raw_eof)).unwrap();
        assert_eq!(parsed.status_code, 200);
        assert_eq!(parsed.body, br#"{"eof":true}"#);

        // Truncated Content-Length fails closed.
        let raw_trunc = b"HTTP/1.1 200 OK\r\nContent-Length: 50\r\n\r\nshort";
        assert!(parse_http_response(&mut Cursor::new(raw_trunc)).is_err());

        // Conflicting Content-Length fails closed.
        let raw_conflict = b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nContent-Length: 5\r\n\r\n1234";
        assert!(parse_http_response(&mut Cursor::new(raw_conflict)).is_err());
    }

    #[test]
    fn http_header_limit_rejects_newline_less_stream_before_allocating_past_64kib() {
        /// Endless newline-less byte source counting how many bytes were read.
        struct EndlessNoNewline {
            read_bytes: Arc<Mutex<usize>>,
        }
        impl Read for EndlessNoNewline {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                buf.fill(b'A');
                *self.read_bytes.lock().unwrap() += buf.len();
                Ok(buf.len())
            }
        }

        let read_bytes = Arc::new(Mutex::new(0usize));
        let mut reader = BufReader::with_capacity(
            HTTP_READ_BUF_CAPACITY,
            EndlessNoNewline {
                read_bytes: Arc::clone(&read_bytes),
            },
        );
        let err = parse_http_response(&mut reader).unwrap_err();
        assert!(
            err.to_string().contains("exceed maximum"),
            "expected header size cap error, got: {err}"
        );
        let total_read = *read_bytes.lock().unwrap();
        assert!(
            total_read <= MAX_HTTP_HEADER_BYTES + HTTP_READ_BUF_CAPACITY,
            "reader consumed {total_read} bytes past header cap {MAX_HTTP_HEADER_BYTES}"
        );
    }

    fn spawn_one_shot_unix_server(
        sock_path: &Path,
        raw_response: Vec<u8>,
        delay: Duration,
    ) -> thread::JoinHandle<String> {
        let listener = UnixListener::bind(sock_path).unwrap();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let mut req_buf = Vec::new();
            let mut chunk = [0u8; 1024];
            let mut header_end = None;
            let mut content_len = 0usize;
            while let Ok(n) = stream.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                req_buf.extend_from_slice(&chunk[..n]);
                if header_end.is_none() {
                    if let Some(pos) = req_buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        header_end = Some(pos + 4);
                        let headers = String::from_utf8_lossy(&req_buf[..pos]);
                        for line in headers.lines() {
                            if let Some((k, v)) = line.split_once(':') {
                                if k.trim().eq_ignore_ascii_case("content-length") {
                                    content_len = v.trim().parse().unwrap_or(0);
                                }
                            }
                        }
                    }
                }
                if let Some(end) = header_end {
                    if req_buf.len() >= end + content_len {
                        break;
                    }
                }
            }
            if !delay.is_zero() {
                thread::sleep(delay);
            }
            let _ = stream.write_all(&raw_response);
            let _ = stream.flush();
            String::from_utf8_lossy(&req_buf).into_owned()
        })
    }

    #[test]
    fn local_unix_transport_enforces_deadline_on_blocked_connect() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("full-backlog.sock");

        // Bind a Unix listener and NEVER call accept(), then saturate its accept backlog on a
        // single helper thread so the next `connect()` blocks in the kernel.
        let listener = UnixListener::bind(&sock).unwrap();
        let somaxconn: usize = fs::read_to_string("/proc/sys/net/core/somaxconn")
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(4096);
        let max_fillers = somaxconn.saturating_add(64).min(16_384);
        let (tx, rx) = mpsc::channel();
        let filler_sock = sock.clone();
        let filler_handle = thread::spawn(move || {
            for _ in 0..max_fillers {
                match UnixStream::connect(&filler_sock) {
                    Ok(stream) => {
                        if tx.send(stream).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let mut fillers = Vec::new();
        let mut saturated = false;
        loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(stream) => fillers.push(stream),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    saturated = true;
                    break;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        assert!(
            saturated && !fillers.is_empty(),
            "expected listener backlog to saturate (filled {} connections, max {max_fillers})",
            fillers.len()
        );

        let started = Instant::now();
        let transport =
            LocalUnixVmmTransport::new(&sock, dir.path()).with_timeout(Duration::from_millis(200));
        let client = VmmClient::new(transport, "blocked-connect");
        let err = client.get_meta().unwrap_err();
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_secs(1),
            "blocked connect exceeded 200ms deadline: took {elapsed:?} ({err:#})"
        );
        let msg = format!("{err:#}");
        assert!(
            msg.contains("timed out connecting"),
            "expected connect timeout error, got: {msg}"
        );

        // Close listener and drop backlog connections so the blocked helper threads exit cleanly.
        drop(rx);
        drop(listener);
        drop(fillers);
        let _ = filler_handle.join();
    }

    #[test]
    fn local_unix_transport_enforces_total_deadline_against_trickle_response() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("trickle.sock");
        let listener = UnixListener::bind(&sock).unwrap();

        // Trickle 1 byte every 50ms for 4 seconds (80 bytes): inactivity never exceeds 50ms,
        // yet a 250ms total deadline must abort promptly well under 1s.
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut req = [0u8; 1024];
            let _ = stream.read(&mut req);
            let header = b"HTTP/1.1 200 OK\r\nContent-Length: 80\r\nConnection: close\r\n\r\n";
            let _ = stream.write_all(header);
            let _ = stream.flush();
            for _ in 0..80 {
                thread::sleep(Duration::from_millis(50));
                if stream.write_all(b"x").is_err() {
                    break;
                }
                let _ = stream.flush();
            }
        });

        let started = Instant::now();
        let transport =
            LocalUnixVmmTransport::new(&sock, dir.path()).with_timeout(Duration::from_millis(250));
        let client = VmmClient::new(transport, "trickle-test");
        let err = client.get_meta().unwrap_err();
        let elapsed = started.elapsed();
        let _ = handle.join();

        assert!(
            elapsed < Duration::from_secs(1),
            "trickle bypassed total deadline: took {elapsed:?} (err: {err:#})"
        );
    }

    #[test]
    fn local_unix_transport_handles_chunked_non_2xx_and_read_timeout() {
        let dir = tempfile::tempdir().unwrap();

        // 1. Chunked 200 response over real Unix socket, verifying Connection: close header.
        let sock_chunked = dir.path().join("chunked.sock");
        let chunked_resp = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n15\r\n{\"found\":false,\"info\"\r\n6\r\n:null}\r\n0\r\n\r\n".to_vec();
        let handle = spawn_one_shot_unix_server(&sock_chunked, chunked_resp, Duration::ZERO);
        let transport = LocalUnixVmmTransport::new(&sock_chunked, dir.path());
        let client = VmmClient::new(transport, "local-test");
        let resp = client
            .get_info("550e8400-e29b-41d4-a716-446655440000")
            .unwrap();
        assert!(!resp.found);
        assert!(resp.is_gone());
        let req_text = handle.join().unwrap();
        assert!(
            req_text
                .to_ascii_lowercase()
                .contains("connection: close\r\n"),
            "request must send Connection: close:\n{req_text}"
        );

        // 2. Non-2xx response includes HTTP status and body in the error.
        let sock_err = dir.path().join("err.sock");
        let err_resp = b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 27\r\n\r\n{\"error\":\"disk full on host\"}".to_vec();
        let handle = spawn_one_shot_unix_server(&sock_err, err_resp, Duration::ZERO);
        let client = VmmClient::new(
            LocalUnixVmmTransport::new(&sock_err, dir.path()),
            "local-err",
        );
        let err = client
            .start_vm("550e8400-e29b-41d4-a716-446655440000")
            .unwrap_err();
        let _ = handle.join();
        let msg = format!("{err:#}");
        assert!(msg.contains("HTTP 500"), "{msg}");
        assert!(msg.contains("disk full on host"), "{msg}");

        // 3. Read timeout triggers when server stalls past io_timeout.
        let sock_timeout = dir.path().join("timeout.sock");
        let handle = spawn_one_shot_unix_server(
            &sock_timeout,
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}".to_vec(),
            Duration::from_millis(400),
        );
        let transport = LocalUnixVmmTransport::new(&sock_timeout, dir.path())
            .with_timeout(Duration::from_millis(80));
        let client = VmmClient::new(transport, "local-timeout");
        let err = client.get_meta().unwrap_err();
        let _ = handle.join();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("timed out")
                || msg.contains("deadline")
                || msg.contains("read HTTP response"),
            "expected timeout error, got: {msg}"
        );
    }

    #[test]
    fn local_unix_transport_parses_real_amd_vmm_captures() {
        let dir = tempfile::tempdir().unwrap();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vmm");

        // Real GetMeta capture.
        let raw_meta = fs::read(fixtures.join("GetMeta.http")).unwrap();
        let sock = dir.path().join("meta.sock");
        let h = spawn_one_shot_unix_server(&sock, raw_meta, Duration::ZERO);
        let client = VmmClient::new(LocalUnixVmmTransport::new(&sock, dir.path()), "amd-meta");
        let meta = client.get_meta().unwrap();
        let _ = h.join();
        assert_eq!(meta["networking"]["default_mode"], json!("user"));
        assert_eq!(meta["networking"]["default_bridge"], json!("obr0"));

        // Real Status capture.
        let raw_status = fs::read(fixtures.join("Status.http")).unwrap();
        let sock = dir.path().join("status.sock");
        let h = spawn_one_shot_unix_server(&sock, raw_status, Duration::ZERO);
        let client = VmmClient::new(LocalUnixVmmTransport::new(&sock, dir.path()), "amd-status");
        let status = client.status(&StatusRequest::default()).unwrap();
        let _ = h.join();
        assert_eq!(status.vms.len(), 4);
        assert_eq!(status.total, 4);
        assert_eq!(status.vms[0].status, "running");
        assert!(status.vms[0].running);
        assert!(!status.vms[0].uptime.is_empty());
        assert_eq!(
            status.vms[0].image_name(),
            Some("outbe-vm-dev-3485ca2592b7a0c3")
        );

        // Real GetInfo capture for existing VM.
        let raw_info = fs::read(fixtures.join("GetInfo.http")).unwrap();
        let sock = dir.path().join("info.sock");
        let h = spawn_one_shot_unix_server(&sock, raw_info, Duration::ZERO);
        let client = VmmClient::new(LocalUnixVmmTransport::new(&sock, dir.path()), "amd-info");
        let resp = client
            .get_info("cdbc5a01-481f-47f1-8474-cdac63ed9f32")
            .unwrap();
        let _ = h.join();
        assert!(resp.found);
        assert!(!resp.is_gone());
        let info = resp.info.unwrap();
        assert_eq!(info.name, "final-amd-f36afc309-validator-0");
        assert_eq!(extract_primary_mac(&info).unwrap(), "be:f7:7b:2c:d1:85");
        assert_eq!(info.image_name(), Some("outbe-vm-dev-3485ca2592b7a0c3"));
        assert_eq!(info.configuration.as_ref().unwrap().vcpu, 3);
        assert_eq!(info.configuration.as_ref().unwrap().memory, 5120);
        assert_eq!(
            info.effective_app_id(),
            Some("7c85e0bd1f1a9b203bdc20bceb46f77fd35e830b")
        );

        // Real GetInfo-unknown capture (HTTP 200 with found:false, info:null).
        let raw_unknown = fs::read(fixtures.join("GetInfo-unknown.http")).unwrap();
        let sock = dir.path().join("unknown.sock");
        let h = spawn_one_shot_unix_server(&sock, raw_unknown, Duration::ZERO);
        let client = VmmClient::new(LocalUnixVmmTransport::new(&sock, dir.path()), "amd-unknown");
        let resp = client
            .get_info("82551677-4885-4acd-9f1d-758512c7e13b")
            .unwrap();
        let _ = h.join();
        assert!(!resp.found);
        assert!(resp.info.is_none());
        assert!(resp.is_gone());

        // Real UnknownMethod capture (HTTP 404 with JSON error body).
        let raw_404 = fs::read(fixtures.join("UnknownMethod.http")).unwrap();
        let sock = dir.path().join("404.sock");
        let h = spawn_one_shot_unix_server(&sock, raw_404, Duration::ZERO);
        let transport = LocalUnixVmmTransport::new(&sock, dir.path());
        let err = transport
            .prpc_post("S1ReadOnlyUnknownMethod", b"{}")
            .unwrap_err();
        let _ = h.join();
        let msg = format!("{err:#}");
        assert!(msg.contains("HTTP 404"), "{msg}");
        assert!(msg.contains("Service not found"), "{msg}");
    }
}
