//! Harness-owned mock price source and production `outbe-feeder` lifecycle.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use eyre::{bail, eyre, Result, WrapErr};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::internal::config::Config;
use crate::internal::proc::ChildGuard;

const COEN_USD_SYMBOL: &str = "COEN840";
const FEEDER_START_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, PartialEq, Eq)]
struct PricePoint {
    price: String,
    volume: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PriceBook {
    generation: u64,
    entries: BTreeMap<String, PricePoint>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct PriceQuote<'a> {
    pub(crate) price: &'a str,
    pub(crate) volume: &'a str,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct FeederSource<'a> {
    pub(crate) base: &'a str,
    pub(crate) quote: &'a str,
    pub(crate) price: &'a str,
    pub(crate) volume: &'a str,
}

#[derive(Clone, Debug)]
pub(crate) struct FeederPair<'a> {
    pub(crate) base: &'a str,
    pub(crate) quote: &'a str,
    pub(crate) sources: Vec<FeederSource<'a>>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct PriceOracleEvidenceV1 {
    pub feeder_binary: String,
    pub feeder_binary_sha256: String,
    pub feeder_processes: Vec<FeederProcessEvidenceV1>,
    pub ticker_requests: u64,
    pub candle_requests: u64,
    pub canonical_publications: Vec<CanonicalPricePublicationV1>,
}

#[derive(Clone, Debug, Serialize)]
pub struct FeederProcessEvidenceV1 {
    pub validator_index: usize,
    pub pid: u32,
    pub log: String,
    pub oracle_pairs: Vec<String>,
    pub source_markets: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct CanonicalPricePublicationV1 {
    pub validator_count: usize,
    pub rate: String,
    pub oracle_block: u64,
    pub oracle_timestamp: u64,
    pub finalized_height: u64,
    pub finalized_timestamp: u64,
    pub age_seconds: u64,
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

fn mock_response(path: &str, symbols: &str, book: &PriceBook) -> Result<String> {
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
                    bail!("unsupported mock price symbol `{symbol}`")
                }
            }
            MockResponse { data: Vec::new() }
        }
        other => bail!("unsupported mock price path `{other}`"),
    };
    serde_json::to_string(&response).map_err(Into::into)
}

#[derive(Debug)]
struct MockServerState {
    book: RwLock<PriceBook>,
    ticker_requests: AtomicU64,
    candle_requests: AtomicU64,
    stop: AtomicBool,
}

struct MockPriceServer {
    addr: SocketAddr,
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
    fn bind(book: PriceBook) -> Result<Self> {
        let listener =
            TcpListener::bind(("127.0.0.1", 0)).wrap_err("bind harness mock price server")?;
        listener
            .set_nonblocking(true)
            .wrap_err("set mock price server nonblocking")?;
        let addr = listener.local_addr()?;
        let state = Arc::new(MockServerState {
            book: RwLock::new(book),
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

    fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn book(&self) -> PriceBook {
        self.state
            .book
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    fn publish(&self, symbol: &str, price: &str, volume: &str) -> Result<u64> {
        if price.is_empty() || volume.is_empty() {
            bail!("controlled price and volume must be non-empty")
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

    fn request_counts(&self) -> (u64, u64) {
        (
            self.state.ticker_requests.load(Ordering::Relaxed),
            self.state.candle_requests.load(Ordering::Relaxed),
        )
    }

    fn stop(&mut self) {
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
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut request = [0_u8; 8_192];
    let size = stream.read(&mut request)?;
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
    let book = state
        .book
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    let (status, body) = match mock_response(path, symbols, &book) {
        Ok(body) => {
            match path {
                "/api/tickers" => {
                    state.ticker_requests.fetch_add(1, Ordering::Relaxed);
                }
                "/api/candles" => {
                    state.candle_requests.fetch_add(1, Ordering::Relaxed);
                }
                _ => {}
            }
            ("200 OK", body)
        }
        Err(error) => (
            "400 Bad Request",
            serde_json::json!({ "error": error.to_string() }).to_string(),
        ),
    };
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()?;
    Ok(())
}

#[derive(Debug)]
pub struct PriceOracleTopology {
    cfg: Config,
    mocks: BTreeMap<usize, MockPriceServer>,
    feeders: BTreeMap<usize, ChildGuard>,
    evidence: PriceOracleEvidenceV1,
}

impl PriceOracleTopology {
    pub(crate) fn new(cfg: Config) -> Self {
        Self {
            cfg,
            mocks: BTreeMap::new(),
            feeders: BTreeMap::new(),
            evidence: PriceOracleEvidenceV1::default(),
        }
    }

    pub(crate) fn start(
        &mut self,
        validator_index: usize,
        rpc_url: &str,
        chain_id: u64,
        private_key: &str,
        validator_address: &str,
        quote: PriceQuote<'_>,
        vote_period: u64,
    ) -> Result<()> {
        self.start_with_pairs(
            validator_index,
            rpc_url,
            chain_id,
            private_key,
            validator_address,
            &[FeederPair {
                base: "COEN",
                quote: "840",
                sources: vec![FeederSource {
                    base: "COEN",
                    quote: "840",
                    price: quote.price,
                    volume: quote.volume,
                }],
            }],
            vote_period,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start_with_pairs(
        &mut self,
        validator_index: usize,
        rpc_url: &str,
        chain_id: u64,
        private_key: &str,
        validator_address: &str,
        pairs: &[FeederPair<'_>],
        vote_period: u64,
    ) -> Result<()> {
        if self.feeders.contains_key(&validator_index) {
            bail!("validator-{validator_index} price feeder is already running")
        }
        if pairs.is_empty() || pairs.iter().any(|pair| pair.sources.is_empty()) {
            bail!("a feeder must configure at least one source for one Oracle pair")
        }
        let entries = pairs
            .iter()
            .flat_map(|pair| &pair.sources)
            .map(|source| {
                (
                    format!("{}{}", source.base, source.quote).to_ascii_uppercase(),
                    PricePoint {
                        price: source.price.to_owned(),
                        volume: source.volume.to_owned(),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let expected_book = PriceBook {
            generation: 1,
            entries,
        };
        if !self.mocks.contains_key(&validator_index) {
            self.mocks.insert(
                validator_index,
                MockPriceServer::bind(expected_book.clone())?,
            );
        } else if self
            .mocks
            .get(&validator_index)
            .map(MockPriceServer::book)
            .is_some_and(|book| book.entries != expected_book.entries)
        {
            bail!("price mock generation changed during one feeder acceptance window")
        }

        let directory = self.cfg.dir.join("price-oracle");
        fs::create_dir_all(&directory)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
        }
        let config_path = directory.join(format!("validator-{validator_index}-feeder.toml"));
        let log_path = directory.join(format!("validator-{validator_index}-feeder.log"));
        let mock_url = self
            .mocks
            .get(&validator_index)
            .expect("mock exists before feeder config")
            .base_url();
        write_feeder_config(
            &config_path,
            &FeederConfigInput {
                rpc_url,
                chain_id,
                private_key,
                validator_address,
                mock_url: &mock_url,
                vote_period,
                pairs,
            },
        )?;

        let log_start = fs::metadata(&log_path).map_or(0, |metadata| metadata.len());
        let stdout = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;
        let stderr = stdout.try_clone()?;
        let mut command = Command::new(&self.cfg.bin_feeder);
        command
            .arg("--config")
            .arg(&config_path)
            .current_dir(&self.cfg.repo)
            .env("RUST_LOG", "outbe_feeder=info")
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        let mut feeder =
            ChildGuard::spawn(format!("validator-{validator_index} price feeder"), command)?;
        let deadline = Instant::now() + FEEDER_START_TIMEOUT;
        loop {
            if feeder.exited() {
                let _ = fs::remove_file(&config_path);
                bail!(
                    "price feeder exited during startup; see {}",
                    log_path.display()
                )
            }
            let started = log_suffix_contains(&log_path, log_start, "starting outbe-feeder");
            if started {
                break;
            }
            if Instant::now() >= deadline {
                let _ = fs::remove_file(&config_path);
                bail!("price feeder did not start within {FEEDER_START_TIMEOUT:?}")
            }
            thread::sleep(Duration::from_millis(50));
        }
        fs::remove_file(&config_path).wrap_err("remove plaintext feeder config after startup")?;

        self.evidence.feeder_binary = self.cfg.bin_feeder.display().to_string();
        self.evidence.feeder_binary_sha256 = sha256_file(&self.cfg.bin_feeder)?;
        let feeder_pid = feeder.pid();
        let feeder_log = log_path.display().to_string();
        self.evidence
            .feeder_processes
            .push(FeederProcessEvidenceV1 {
                validator_index,
                pid: feeder_pid,
                log: feeder_log,
                oracle_pairs: pairs
                    .iter()
                    .map(|pair| format!("{}/{}", pair.base, pair.quote))
                    .collect(),
                source_markets: pairs
                    .iter()
                    .flat_map(|pair| &pair.sources)
                    .map(|source| format!("{}:{}/{}", "mock_http", source.base, source.quote))
                    .collect(),
            });
        self.feeders.insert(validator_index, feeder);
        Ok(())
    }

    pub fn stop_feeder(&mut self) {
        self.feeders.clear();
    }

    pub fn stop_validator_feeder(&mut self, validator_index: usize) -> Result<()> {
        self.feeders
            .remove(&validator_index)
            .ok_or_else(|| eyre!("validator-{validator_index} price feeder is not running"))?;
        Ok(())
    }

    pub fn ensure_feeder_alive(&mut self) -> Result<()> {
        if self.feeders.is_empty() {
            bail!("price feeder is not running")
        }
        for (index, feeder) in &mut self.feeders {
            if feeder.exited() {
                bail!("owned price feeder {index} exited")
            }
        }
        Ok(())
    }

    pub fn is_feeder_running(&mut self) -> bool {
        !self.feeders.is_empty() && self.feeders.values_mut().all(|feeder| !feeder.exited())
    }

    pub fn last_oracle_block(&self) -> Option<u64> {
        self.evidence
            .canonical_publications
            .last()
            .map(|publication| publication.oracle_block)
    }

    pub(crate) fn read_controlled_quote(&self) -> Option<(String, String)> {
        self.mocks.values().find_map(|mock| {
            mock.book()
                .entries
                .get(COEN_USD_SYMBOL)
                .map(|point| (point.price.clone(), point.volume.clone()))
        })
    }

    pub fn publish_quote(&mut self, price: &str, volume: &str) -> Result<u64> {
        if self.feeders.is_empty() {
            bail!("price feeder must be running before changing the controlled quote")
        }
        let mut generation = None;
        for mock in self.mocks.values() {
            if mock.book().entries.contains_key(COEN_USD_SYMBOL) {
                generation = Some(mock.publish(COEN_USD_SYMBOL, price, volume)?);
            }
        }
        generation.ok_or_else(|| eyre!("controlled COEN/840 price source is not configured"))
    }

    pub fn evidence_snapshot(&self) -> PriceOracleEvidenceV1 {
        let mut evidence = self.evidence.clone();
        for mock in self.mocks.values() {
            let (ticker_requests, candle_requests) = mock.request_counts();
            evidence.ticker_requests = evidence.ticker_requests.saturating_add(ticker_requests);
            evidence.candle_requests = evidence.candle_requests.saturating_add(candle_requests);
        }
        evidence
    }

    pub fn record_canonical_publication(
        &mut self,
        validator_count: usize,
        rate: alloy_primitives::U256,
        oracle_block: u64,
        oracle_timestamp: u64,
        finalized_height: u64,
        finalized_timestamp: u64,
    ) {
        self.evidence
            .canonical_publications
            .push(CanonicalPricePublicationV1 {
                validator_count,
                rate: rate.to_string(),
                oracle_block,
                oracle_timestamp,
                finalized_height,
                finalized_timestamp,
                age_seconds: finalized_timestamp.saturating_sub(oracle_timestamp),
            });
    }

    pub fn teardown(&mut self) {
        self.stop_feeder();
        self.mocks.clear();
    }
}

fn log_suffix_contains(path: &Path, start: u64, needle: &str) -> bool {
    let Ok(bytes) = fs::read(path) else {
        return false;
    };
    let Ok(start) = usize::try_from(start) else {
        return false;
    };
    bytes
        .get(start..)
        .is_some_and(|suffix| String::from_utf8_lossy(suffix).contains(needle))
}

impl Drop for PriceOracleTopology {
    fn drop(&mut self) {
        self.teardown();
    }
}

struct FeederConfigInput<'a> {
    rpc_url: &'a str,
    chain_id: u64,
    private_key: &'a str,
    validator_address: &'a str,
    mock_url: &'a str,
    vote_period: u64,
    pairs: &'a [FeederPair<'a>],
}

fn write_feeder_config(path: &Path, input: &FeederConfigInput<'_>) -> Result<()> {
    let mut config = format!(
        "[chain]\nrpc_endpoint = \"{}\"\nchain_id = {}\ngasless_oracle_votes = true\n\n[account]\nprivate_key = \"{}\"\nvalidator_address = \"{}\"\n\n[oracle]\nvote_period = {}\npoll_interval_secs = 1\n\n[health]\nenabled = false\n\n[[provider_endpoints]]\nname = \"mock_http\"\nrest = \"{}\"\n",
        input.rpc_url,
        input.chain_id,
        input.private_key,
        input.validator_address,
        input.vote_period,
        input.mock_url,
    );
    for pair in input.pairs {
        config.push_str(&format!(
            "\n[[currency_pairs]]\nbase = \"{}\"\nquote = \"{}\"\n",
            pair.base, pair.quote
        ));
        for source in &pair.sources {
            config.push_str(&format!(
                "\n[[currency_pairs.sources]]\nprovider = \"mock_http\"\nbase = \"{}\"\nquote = \"{}\"\n",
                source.base, source.quote
            ));
        }
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .wrap_err_with(|| format!("create feeder config {}", path.display()))?;
    file.write_all(config.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file =
        fs::File::open(path).wrap_err_with(|| format!("open feeder binary {}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1_024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex::encode(digest.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coen_book(generation: u64, price: &str, volume: &str) -> PriceBook {
        PriceBook {
            generation,
            entries: BTreeMap::from([(
                COEN_USD_SYMBOL.to_owned(),
                PricePoint {
                    price: price.to_owned(),
                    volume: volume.to_owned(),
                },
            )]),
        }
    }

    #[test]
    fn mock_http_wire_is_exact_and_generation_owned() {
        let mut book = coen_book(7, "1.000000", "1000.000000");
        book.entries.insert(
            "COENUSDC".into(),
            PricePoint {
                price: "1.000001".into(),
                volume: "500.000000".into(),
            },
        );

        assert_eq!(
            mock_response("/api/tickers", COEN_USD_SYMBOL, &book).unwrap(),
            r#"{"data":[{"symbol":"COEN840","price":"1.000000","volume":"1000.000000"}]}"#
        );
        assert_eq!(
            mock_response("/api/candles", COEN_USD_SYMBOL, &book).unwrap(),
            r#"{"data":[]}"#
        );
        assert_eq!(
            mock_response("/api/tickers", "COEN840,COENUSDC", &book).unwrap(),
            r#"{"data":[{"symbol":"COEN840","price":"1.000000","volume":"1000.000000"},{"symbol":"COENUSDC","price":"1.000001","volume":"500.000000"}]}"#
        );
        assert!(mock_response("/api/tickers", "COEN978", &book).is_err());
        assert!(mock_response("/unknown", COEN_USD_SYMBOL, &book).is_err());
        assert_eq!(book.generation, 7);
    }

    #[test]
    fn feeder_config_is_private_and_contains_only_the_frozen_interface() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("feeder.toml");
        let pairs = [FeederPair {
            base: "COEN",
            quote: "840",
            sources: vec![
                FeederSource {
                    base: "COEN",
                    quote: "USDT",
                    price: "1",
                    volume: "2",
                },
                FeederSource {
                    base: "COEN",
                    quote: "USDC",
                    price: "1",
                    volume: "3",
                },
            ],
        }];
        write_feeder_config(
            &path,
            &FeederConfigInput {
                rpc_url: "http://127.0.0.1:18545",
                chain_id: 54_322_345,
                private_key: "0xsecret",
                validator_address: "0x0000000000000000000000000000000000000001",
                mock_url: "http://127.0.0.1:31415",
                vote_period: 8,
                pairs: &pairs,
            },
        )
        .unwrap();
        let contents = fs::read_to_string(&path).unwrap();
        assert!(contents.contains("vote_period = 8"));
        assert!(contents.contains("poll_interval_secs = 1"));
        assert!(contents.contains("gasless_oracle_votes = true"));
        assert!(contents.contains("enabled = false"));
        assert!(contents.contains("[[currency_pairs.sources]]"));
        assert!(contents.contains("quote = \"USDT\""));
        assert!(contents.contains("quote = \"USDC\""));
        assert!(!contents.contains("providers ="));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn feeder_restart_does_not_accept_the_previous_log_episodes_start_marker() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("feeder.log");
        fs::write(&path, "starting outbe-feeder\nold episode stopped\n").unwrap();
        let next_episode = fs::metadata(&path).unwrap().len();

        assert!(!log_suffix_contains(
            &path,
            next_episode,
            "starting outbe-feeder"
        ));
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"starting outbe-feeder\n")
            .unwrap();
        assert!(log_suffix_contains(
            &path,
            next_episode,
            "starting outbe-feeder"
        ));
    }

    #[test]
    fn bound_mock_server_serves_ticker_and_empty_candle_fallback() {
        let mut server = match MockPriceServer::bind(coen_book(3, "1.250000", "4.000000")) {
            Ok(server) => server,
            Err(error) if format!("{error:#}").contains("Operation not permitted") => return,
            Err(error) => panic!("bind mock server: {error:#}"),
        };
        let ticker = raw_get(server.addr, "/api/tickers?symbols=COEN840");
        assert!(ticker.contains("200 OK"));
        assert!(ticker
            .contains(r#"{"data":[{"symbol":"COEN840","price":"1.250000","volume":"4.000000"}]}"#));
        let candles = raw_get(server.addr, "/api/candles?symbols=COEN840");
        assert!(candles.contains(r#"{"data":[]}"#));
        assert_eq!(server.request_counts(), (1, 1));
        server.stop();
    }

    #[test]
    fn bound_mock_server_publishes_one_atomic_new_price_generation() {
        let mut server = match MockPriceServer::bind(coen_book(3, "1.000000", "4.000000")) {
            Ok(server) => server,
            Err(error) if format!("{error:#}").contains("Operation not permitted") => return,
            Err(error) => panic!("bind mock server: {error:#}"),
        };

        let generation = server
            .publish(COEN_USD_SYMBOL, "1.080001", "4.000000")
            .expect("publish controlled qualification quote");
        assert_eq!(generation, 4);
        assert_eq!(server.book(), coen_book(4, "1.080001", "4.000000"));
        let ticker = raw_get(server.addr, "/api/tickers?symbols=COEN840");
        assert!(ticker
            .contains(r#"{"data":[{"symbol":"COEN840","price":"1.080001","volume":"4.000000"}]}"#));
        server.stop();
    }

    fn raw_get(addr: SocketAddr, target: &str) -> String {
        let mut stream = TcpStream::connect(addr).unwrap();
        write!(
            stream,
            "GET {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut response = Vec::new();
        let mut buffer = [0_u8; 4_096];
        loop {
            match stream.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => response.extend_from_slice(&buffer[..read]),
                Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => break,
                Err(error) => panic!("read mock response: {error}"),
            }
        }
        String::from_utf8(response).unwrap()
    }
}
