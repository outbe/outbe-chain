//! Scenario-owned HTTP proxy for transient follower transport faults.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use eyre::{ensure, eyre, Result};

const MAX_REQUEST: usize = 4 * 1024 * 1024;
const MAX_RESPONSE: u64 = 64 * 1024 * 1024;

struct State {
    stop: AtomicBool,
    rate_limit_tips: AtomicBool,
    tip_requests: AtomicU64,
    rejected_tips: AtomicU64,
    forge_finalizations: AtomicBool,
    forge_from_height: AtomicU64,
    finalization_requests: AtomicU64,
    forged_finalizations: AtomicU64,
    first_forged_height: AtomicU64,
}

pub(super) struct ControlledUpstream {
    address: SocketAddr,
    state: Arc<State>,
    task: Option<JoinHandle<()>>,
}

impl ControlledUpstream {
    pub(super) fn start(upstream_port: u16) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let state = Arc::new(State {
            stop: AtomicBool::new(false),
            rate_limit_tips: AtomicBool::new(false),
            tip_requests: AtomicU64::new(0),
            rejected_tips: AtomicU64::new(0),
            forge_finalizations: AtomicBool::new(false),
            forge_from_height: AtomicU64::new(0),
            finalization_requests: AtomicU64::new(0),
            forged_finalizations: AtomicU64::new(0),
            first_forged_height: AtomicU64::new(0),
        });
        let task_state = Arc::clone(&state);
        let task = thread::Builder::new()
            .name("e2e-follower-upstream".to_owned())
            .spawn(move || {
                while !task_state.stop.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((client, _)) => {
                            if let Err(error) = serve(client, upstream_port, &task_state) {
                                eprintln!("follower upstream proxy request failed: {error:#}");
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => {
                            eprintln!("follower upstream proxy accept failed: {error}");
                            break;
                        }
                    }
                }
            })?;
        Ok(Self {
            address,
            state,
            task: Some(task),
        })
    }

    pub(super) fn url(&self) -> String {
        format!("http://{}", self.address)
    }

    pub(super) fn rate_limit_tips(&self, active: bool) {
        self.state.rate_limit_tips.store(active, Ordering::Release);
    }

    pub(super) fn tip_requests(&self) -> u64 {
        self.state.tip_requests.load(Ordering::Acquire)
    }

    pub(super) fn rejected_tips(&self) -> u64 {
        self.state.rejected_tips.load(Ordering::Acquire)
    }

    pub(super) fn forge_finalizations_from(&self, height: u64) {
        self.state
            .forge_from_height
            .store(height, Ordering::Release);
        self.state
            .forge_finalizations
            .store(true, Ordering::Release);
    }

    pub(super) fn restore_finalizations(&self) {
        self.state
            .forge_finalizations
            .store(false, Ordering::Release);
    }

    pub(super) fn forged_finalizations(&self) -> u64 {
        self.state.forged_finalizations.load(Ordering::Acquire)
    }

    pub(super) fn finalization_requests(&self) -> u64 {
        self.state.finalization_requests.load(Ordering::Acquire)
    }

    pub(super) fn first_forged_height(&self) -> Option<u64> {
        match self.state.first_forged_height.load(Ordering::Acquire) {
            0 => None,
            height => Some(height),
        }
    }
}

impl Drop for ControlledUpstream {
    fn drop(&mut self) {
        self.state.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address);
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
    }
}

fn serve(mut client: TcpStream, upstream_port: u16, state: &State) -> Result<()> {
    client.set_read_timeout(Some(Duration::from_secs(15)))?;
    client.set_write_timeout(Some(Duration::from_secs(15)))?;
    let body = read_request(&mut client)?;
    let length = body.len();
    let request: serde_json::Value = serde_json::from_slice(&body)?;
    if request["method"] == "outbe_consensusStatus" {
        state.tip_requests.fetch_add(1, Ordering::AcqRel);
        if state.rate_limit_tips.load(Ordering::Acquire) {
            state.rejected_tips.fetch_add(1, Ordering::AcqRel);
            client.write_all(
                b"HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )?;
            return Ok(());
        }
    }
    let mut upstream = TcpStream::connect(("127.0.0.1", upstream_port))?;
    upstream.set_read_timeout(Some(Duration::from_secs(15)))?;
    upstream.set_write_timeout(Some(Duration::from_secs(15)))?;
    write!(upstream, "POST / HTTP/1.1\r\nHost: 127.0.0.1:{upstream_port}\r\nContent-Type: application/json\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n")?;
    upstream.write_all(&body)?;
    // Current followers prefer the ancestor-aware proof API and only fall back
    // to getFinalization when the upstream lacks it.
    let finalization_height = (request["method"] == "outbe_getFinalityProof"
        || request["method"] == "outbe_getFinalization")
        .then(|| request_height(&request["params"][0]))
        .flatten();
    if finalization_height.is_some() {
        state.finalization_requests.fetch_add(1, Ordering::AcqRel);
    }
    if finalization_height.is_some_and(|height| {
        state.forge_finalizations.load(Ordering::Acquire)
            && height >= state.forge_from_height.load(Ordering::Acquire)
    }) {
        let mut response = Vec::new();
        upstream.take(MAX_RESPONSE + 1).read_to_end(&mut response)?;
        ensure!(
            response.len() as u64 <= MAX_RESPONSE,
            "RPC response exceeds bound"
        );
        if let Some(forged) = forge_finalization_response(&response)? {
            let height = finalization_height.expect("checked finalization height");
            state
                .first_forged_height
                .compare_exchange(0, height, Ordering::AcqRel, Ordering::Acquire)
                .ok();
            state.forged_finalizations.fetch_add(1, Ordering::AcqRel);
            write!(client, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", forged.len())?;
            client.write_all(&forged)?;
            return Ok(());
        }
        client.write_all(&response)?;
        return Ok(());
    }
    let copied = std::io::copy(&mut upstream.take(MAX_RESPONSE + 1), &mut client)?;
    ensure!(copied <= MAX_RESPONSE, "RPC response exceeds bound");
    Ok(())
}

fn read_request(client: &mut TcpStream) -> Result<Vec<u8>> {
    let mut reader = BufReader::new(client);
    let mut content_length = None;
    let mut header_bytes = 0;
    loop {
        let mut line = String::new();
        ensure!(reader.read_line(&mut line)? > 0, "incomplete RPC request");
        header_bytes += line.len();
        ensure!(header_bytes <= 16 * 1024, "RPC headers exceed bound");
        if line == "\r\n" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            if key.eq_ignore_ascii_case("content-length") {
                content_length = Some(value.trim().parse::<usize>()?);
            }
        }
    }
    let length = content_length.ok_or_else(|| eyre!("RPC request missing Content-Length"))?;
    ensure!(length <= MAX_REQUEST, "RPC request body exceeds bound");
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    Ok(body)
}

fn request_height(value: &serde_json::Value) -> Option<u64> {
    value.as_u64().or_else(|| {
        let text = value.as_str()?;
        if let Some(hex) = text.strip_prefix("0x") {
            u64::from_str_radix(hex, 16).ok()
        } else {
            text.parse().ok()
        }
    })
}

fn forge_finalization_response(response: &[u8]) -> Result<Option<Vec<u8>>> {
    let separator = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| eyre!("upstream finalization response has no HTTP body"))?;
    let mut payload: serde_json::Value = serde_json::from_slice(&response[separator + 4..])?;
    let Some(hex) = payload["result"]["finalizationHex"].as_str() else {
        return Ok(None);
    };
    ensure!(
        hex.starts_with("0x") && hex.len() > 4 && hex.len().is_multiple_of(2),
        "upstream finalization proof is not canonical hex"
    );
    let mut forged = hex.as_bytes().to_vec();
    let last = forged.last_mut().expect("nonempty proof hex");
    *last = if *last == b'0' { b'1' } else { b'0' };
    payload["result"]["finalizationHex"] = serde_json::Value::String(String::from_utf8(forged)?);
    Ok(Some(serde_json::to_vec(&payload)?))
}
