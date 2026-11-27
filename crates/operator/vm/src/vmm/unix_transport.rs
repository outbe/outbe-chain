//! Direct local Unix domain socket transport to dstack VMM with total deadlines and cancellation.

use std::{
    fs,
    io::{self, BufReader, Read, Write},
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

use super::{
    http::{parse_http_response, serial_logs_target, HttpRequest, HTTP_READ_BUF_CAPACITY},
    validate_vm_id, VmmTransport,
};

const DEFAULT_UNIX_IO_TIMEOUT: Duration = Duration::from_secs(30);
const CANCEL_POLL_SLICE: Duration = Duration::from_millis(25);

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
        request: &HttpRequest<'_>,
        context_label: &str,
        caller_deadline: Option<Instant>,
    ) -> Result<Vec<u8>> {
        let now = Instant::now();
        let default_deadline = now + self.io_timeout;
        let deadline = match caller_deadline {
            Some(d) => d.min(default_deadline),
            None => default_deadline,
        };
        if is_flag_set(self.cancel.as_deref()) {
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
        send_request(&mut deadline_stream, request, context_label)?;

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
}

/// Write the request head and body, then flush the stream.
fn send_request(
    stream: &mut DeadlineUnixStream,
    request: &HttpRequest<'_>,
    context_label: &str,
) -> Result<()> {
    stream
        .write_all(request.head().as_bytes())
        .wrap_err_with(|| format!("write HTTP headers for {context_label}"))?;
    if !request.body.is_empty() {
        stream
            .write_all(request.body)
            .wrap_err_with(|| format!("write HTTP body for {context_label}"))?;
    }
    stream
        .flush()
        .wrap_err_with(|| format!("flush HTTP request for {context_label}"))
}

fn is_flag_set(cancel: Option<&AtomicBool>) -> bool {
    cancel.is_some_and(|flag| flag.load(Ordering::Relaxed))
}

/// Return the next wait slice: the full `remaining` time, or a short poll slice when a
/// cancellation flag must be checked.
fn wait_slice(remaining: Duration, cancel_present: bool) -> Duration {
    if cancel_present {
        remaining.min(CANCEL_POLL_SLICE)
    } else {
        remaining
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
        if is_flag_set(cancel) {
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
        let wait = wait_slice(deadline.saturating_duration_since(now), cancel.is_some());
        match rx.recv_timeout(wait) {
            Ok(res) => {
                return res.wrap_err_with(|| {
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

/// Direction of one blocking socket operation, which selects its timeout and error texts.
#[derive(Clone, Copy)]
enum IoDirection {
    Read,
    Write,
}

impl IoDirection {
    fn set_timeout(self, stream: &UnixStream, slice: Duration) -> io::Result<()> {
        match self {
            Self::Read => stream.set_read_timeout(Some(slice)),
            Self::Write => stream.set_write_timeout(Some(slice)),
        }
    }

    fn canceled_message(self) -> &'static str {
        match self {
            Self::Read => "VMM HTTP request canceled by signal",
            Self::Write => "VMM HTTP write canceled by signal",
        }
    }

    fn timed_out_message(self) -> &'static str {
        match self {
            Self::Read => "VMM HTTP request timed out",
            Self::Write => "VMM HTTP write timed out",
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
        is_flag_set(self.cancel.as_deref())
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
        Ok(wait_slice(remaining, self.cancel.is_some()))
    }

    /// Decide whether a slice timeout ends the operation (cancel or deadline) or allows a retry.
    fn check_after_slice_timeout(&self, direction: IoDirection) -> io::Result<()> {
        if self.is_canceled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                direction.canceled_message(),
            ));
        }
        if Instant::now() >= self.deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                direction.timed_out_message(),
            ));
        }
        Ok(())
    }

    /// Run `op` in deadline-bounded slices, retrying on `EINTR` and on slice timeouts.
    fn retry_in_slices<T>(
        &mut self,
        direction: IoDirection,
        mut op: impl FnMut(&mut UnixStream) -> io::Result<T>,
    ) -> io::Result<T> {
        loop {
            let slice = self.next_slice()?;
            direction.set_timeout(&self.stream, slice)?;
            match op(&mut self.stream) {
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) if is_slice_timeout(&err) => self.check_after_slice_timeout(direction)?,
                other => return other,
            }
        }
    }
}

fn is_slice_timeout(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

impl Read for DeadlineUnixStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.retry_in_slices(IoDirection::Read, |stream| stream.read(buf))
    }
}

impl Write for DeadlineUnixStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.retry_in_slices(IoDirection::Write, |stream| stream.write(buf))
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
            is_valid_prpc_method(method),
            "invalid VMM pRPC method name: {method}"
        );
        let target = format!("/prpc/{method}?json");
        let request = HttpRequest {
            method: "POST",
            target: &target,
            content_type: Some("application/json"),
            body: payload,
        };
        let body = self.http_request(&request, &format!("pRPC {method}"), deadline)?;
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

        let target = serial_logs_target(vm_id, lines);
        let request = HttpRequest {
            method: "GET",
            target: &target,
            content_type: None,
            body: &[],
        };
        let body = self.http_request(&request, "GET /logs", None)?;
        Ok(String::from_utf8_lossy(&body).into_owned())
    }
}

fn is_valid_prpc_method(method: &str) -> bool {
    !method.is_empty()
        && method
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
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
