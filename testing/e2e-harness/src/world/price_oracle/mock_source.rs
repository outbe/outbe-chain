//! The harness-owned HTTP price source each feeder polls.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, RwLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use eyre::{bail, eyre, Result, WrapErr};
use serde::Serialize;

use super::MockSourceFailure;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PricePoint {
    pub(super) price: String,
    pub(super) volume: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PriceBook {
    pub(super) generation: u64,
    pub(super) entries: BTreeMap<String, PricePoint>,
}

#[derive(Debug, Serialize)]
struct MockEntry<'a> {
    symbol: &'a str,
    price: &'a str,
    volume: &'a str,
}

#[derive(Debug, Serialize)]
struct MockResponse<'a> {
    data: Vec<MockEntry<'a>>,
}

pub(super) fn mock_response(path: &str, symbols: &str, book: &PriceBook) -> Result<String> {
    let requested = symbols.split(',').filter(|symbol| !symbol.is_empty());
    let response = match path {
        "/api/tickers" => {
            let mut data = Vec::new();
            for symbol in requested {
                let point = book
                    .entries
                    .get(symbol)
                    .ok_or_else(|| eyre!("unsupported mock price symbol `{symbol}`"))?;
                data.push(MockEntry {
                    symbol,
                    price: &point.price,
                    volume: &point.volume,
                });
            }
            MockResponse { data }
        }
        // An empty candle set deliberately exercises the production provider's
        // documented ticker-VWAP fallback.
        "/api/candles" => {
            for symbol in requested {
                if !book.entries.contains_key(symbol) {
                    bail!("unsupported mock price symbol `{symbol}`");
                }
            }
            MockResponse { data: Vec::new() }
        }
        other => {
            bail!("unsupported mock price path `{other}`");
        }
    };
    serde_json::to_string(&response).map_err(Into::into)
}

#[derive(Debug)]
struct MockServerState {
    book: RwLock<PriceBook>,
    failure: AtomicU8,
    ticker_requests: AtomicU64,
    candle_requests: AtomicU64,
    stop: AtomicBool,
}

pub(super) struct MockPriceServer {
    pub(super) addr: SocketAddr,
    state: Arc<MockServerState>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for MockPriceServer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MockPriceServer")
            .field("addr", &self.addr)
            .finish_non_exhaustive()
    }
}

impl MockPriceServer {
    pub(super) fn bind(book: PriceBook) -> Result<Self> {
        let listener =
            TcpListener::bind(("127.0.0.1", 0)).wrap_err("bind harness mock price server")?;
        listener
            .set_nonblocking(true)
            .wrap_err("set mock price server nonblocking")?;
        let addr = listener.local_addr()?;
        let state = Arc::new(MockServerState {
            book: RwLock::new(book),
            failure: AtomicU8::new(MockSourceFailure::Healthy as u8),
            ticker_requests: AtomicU64::new(0),
            candle_requests: AtomicU64::new(0),
            stop: AtomicBool::new(false),
        });
        let thread_state = Arc::clone(&state);
        let thread = thread::Builder::new()
            .name("e2e-price-source".into())
            .spawn(move || serve_mock_prices(listener, &thread_state))
            .wrap_err("spawn harness mock price server")?;
        Ok(Self {
            addr,
            state,
            thread: Some(thread),
        })
    }

    pub(super) fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub(super) fn book(&self) -> PriceBook {
        self.state
            .book
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub(super) fn publish(&self, symbol: &str, price: &str, volume: &str) -> Result<u64> {
        if price.is_empty() || volume.is_empty() {
            bail!("controlled price and volume must be non-empty");
        }
        let mut book = self
            .state
            .book
            .write()
            .map_err(|_| eyre!("controlled price book lock is poisoned"))?;
        let generation = book
            .generation
            .checked_add(1)
            .ok_or_else(|| eyre!("controlled price generation overflow"))?;
        let point = book
            .entries
            .get_mut(symbol)
            .ok_or_else(|| eyre!("controlled price symbol `{symbol}` is not configured"))?;
        *point = PricePoint {
            price: price.to_owned(),
            volume: volume.to_owned(),
        };
        book.generation = generation;
        Ok(generation)
    }

    pub(super) fn request_counts(&self) -> (u64, u64) {
        (
            self.state.ticker_requests.load(Ordering::Relaxed),
            self.state.candle_requests.load(Ordering::Relaxed),
        )
    }

    pub(super) fn set_failure(&self, failure: MockSourceFailure) {
        self.state.failure.store(failure as u8, Ordering::Release);
    }

    pub(super) fn stop(&mut self) {
        self.state.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.addr);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for MockPriceServer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn serve_mock_prices(listener: TcpListener, state: &MockServerState) {
    while !state.stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let _ = respond_to_price_request(&mut stream, state);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(_) => break,
        }
    }
}

fn respond_to_price_request(stream: &mut TcpStream, state: &MockServerState) -> Result<()> {
    // Accepted sockets may inherit the listener's nonblocking mode.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut request = [0_u8; 8_192];
    let mut size = 0;
    loop {
        let read = stream.read(&mut request[size..])?;
        if read == 0 {
            break;
        }
        size += read;
        if request[..size]
            .windows(4)
            .any(|window| window == b"\r\n\r\n")
        {
            break;
        }
        eyre::ensure!(
            size < request.len(),
            "mock HTTP request headers are too large"
        );
    }
    eyre::ensure!(
        request[..size]
            .windows(4)
            .any(|window| window == b"\r\n\r\n"),
        "mock HTTP request ended before its headers"
    );
    let request =
        std::str::from_utf8(&request[..size]).wrap_err("mock HTTP request is not UTF-8")?;
    let target = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or_else(|| eyre!("mock HTTP request has no target"))?;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let symbols = query
        .split('&')
        .find_map(|field| field.strip_prefix("symbols="))
        .unwrap_or("");
    match path {
        "/api/tickers" => {
            state.ticker_requests.fetch_add(1, Ordering::Relaxed);
        }
        "/api/candles" => {
            state.candle_requests.fetch_add(1, Ordering::Relaxed);
        }
        _ => {}
    }
    let failure = state.failure.load(Ordering::Acquire);
    if failure == MockSourceFailure::Timeout as u8 {
        thread::sleep(Duration::from_secs(6));
        return Ok(());
    }
    let book = state
        .book
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    let (status, body) = match failure {
        value if value == MockSourceFailure::HttpError as u8 => (
            "500 Internal Server Error",
            r#"{"error":"controlled source outage"}"#.to_owned(),
        ),
        value if value == MockSourceFailure::InvalidPayload as u8 => (
            "200 OK",
            r#"{"data":"invalid controlled payload"}"#.to_owned(),
        ),
        _ => match mock_response(path, symbols, &book) {
            Ok(body) => ("200 OK", body),
            Err(error) => (
                "400 Bad Request",
                serde_json::json!({ "error": error.to_string() }).to_string(),
            ),
        },
    };
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()?;
    Ok(())
}
