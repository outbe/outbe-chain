//! Harness-owned mock price source and production `outbe-feeder` lifecycle.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
#[cfg(test)]
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use eyre::{bail, eyre, Result, WrapErr};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::internal::config::Config;
use crate::internal::proc::ChildGuard;

mod mock_source;
mod verify;

#[cfg(test)]
use mock_source::mock_response;
use mock_source::{MockPriceServer, PriceBook, PricePoint};
#[cfg(feature = "ocomp-integration")]
pub(crate) use verify::verify_price_oracle_evidence;
#[cfg(test)]
use verify::verify_publication_cohort;

const COEN_USD_SYMBOL: &str = "COEN840";
const FEEDER_START_TIMEOUT: Duration = Duration::from_secs(10);

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

#[derive(Clone, Copy, Debug)]
pub(crate) struct FeederLaunch<'a> {
    pub(crate) validator_index: usize,
    pub(crate) rpc_url: &'a str,
    pub(crate) chain_id: u64,
    pub(crate) private_key: &'a str,
    pub(crate) validator_address: &'a str,
    pub(crate) vote_period: u64,
    pub(crate) phase: OracleEvidencePhaseV1,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OracleEvidencePhaseV1 {
    Initial,
    ClockRestart,
    ControlledUpdate,
    QuorumRecovery,
    SourceRecovery,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum MockSourceFailure {
    Healthy = 0,
    HttpError = 1,
    InvalidPayload = 2,
    Timeout = 3,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PriceOracleEvidenceV1 {
    pub feeder_binary: String,
    pub feeder_binary_sha256: String,
    pub feeder_processes: Vec<FeederProcessEvidenceV1>,
    pub ticker_requests: u64,
    pub candle_requests: u64,
    pub controlled_sources: Vec<ControlledSourceEvidenceV1>,
    pub canonical_publications: Vec<CanonicalPricePublicationV1>,
    pub quorum_loss_windows: Vec<QuorumLossEvidenceV1>,
    pub penalty_snapshots: Vec<PenaltySnapshotEvidenceV1>,
    pub cohorts: Vec<OracleCohortEvidenceV1>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OracleCheckpointV1 {
    pub height: u64,
    pub block_hash: alloy_primitives::B256,
    pub state_root: alloy_primitives::B256,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OracleMemberV1 {
    pub index: usize,
    pub address: alloy_primitives::Address,
    pub port: u16,
    pub node_pid: u32,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OracleCohortV1 {
    pub checkpoint: OracleCheckpointV1,
    pub members: Vec<OracleMemberV1>,
    pub quorum: usize,
    pub feeder_indices: Vec<usize>,
    /// The fixed A/AB/AB/B negative fixture explicitly controls partial feeders.
    pub overlapping_pairs: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OracleCohortEvidenceV1 {
    /// First possible launch record for this cohort in append-only process history.
    pub first_process_record: usize,
    pub phase: OracleEvidencePhaseV1,
    pub cohort: OracleCohortV1,
    /// (owned validator index, launch attempt, PID), never signing material.
    pub current_attempts: Vec<(usize, u32, u32)>,
    /// Public observations, including unsuccessful polls before teardown.
    pub observations: Vec<serde_json::Value>,
}

fn validate_live_feeder_indices(cohort: &OracleCohortV1, live: &[usize]) -> Result<()> {
    let distinct = live
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    eyre::ensure!(
        distinct.len() == live.len() && !live.is_empty(),
        "empty or duplicate Oracle feeder owners"
    );
    if cohort.overlapping_pairs {
        eyre::ensure!(
            live.iter()
                .all(|index| cohort.feeder_indices.contains(index)),
            "unexpected overlapping-pair feeder"
        );
    } else {
        eyre::ensure!(
            live == cohort.feeder_indices,
            "Oracle feeder cohort changed; stopped feeders must not be replenished implicitly"
        );
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FeederProcessEvidenceV1 {
    pub validator_index: usize,
    pub validator_address: String,
    pub rpc_endpoint: String,
    pub vote_period: u64,
    pub attempt: u32,
    pub pid: u32,
    pub log: String,
    pub started_at_millis: u64,
    pub stopped_at_millis: Option<u64>,
    pub oracle_pairs: Vec<String>,
    pub source_markets: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ControlledSourceEvidenceV1 {
    pub phase: OracleEvidencePhaseV1,
    pub generation: u64,
    pub validator_index: usize,
    pub oracle_pair: String,
    pub source_market: String,
    pub price: String,
    pub volume: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalPricePublicationV1 {
    pub phase: OracleEvidencePhaseV1,
    pub validator_count: usize,
    pub base: String,
    pub quote: String,
    pub rate: String,
    pub volume: String,
    pub oracle_block: u64,
    pub oracle_timestamp: u64,
    pub finalized_height: u64,
    pub finalized_timestamp: u64,
    pub age_seconds: u64,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct CanonicalPublicationObservation {
    pub(crate) phase: OracleEvidencePhaseV1,
    pub(crate) validator_count: usize,
    pub(crate) base: alloy_primitives::Address,
    pub(crate) quote: alloy_primitives::Address,
    pub(crate) rate: alloy_primitives::U256,
    pub(crate) volume: alloy_primitives::U256,
    pub(crate) oracle_block: u64,
    pub(crate) oracle_timestamp: u64,
    pub(crate) finalized_height: u64,
    pub(crate) finalized_timestamp: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QuorumLossPairEvidenceV1 {
    pub base: String,
    pub quote: String,
    pub last_block_before: u64,
    pub last_block_after: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QuorumLossEvidenceV1 {
    pub stopped_validator_index: usize,
    pub finalized_height_before: u64,
    pub finalized_height_after: u64,
    pub pairs: Vec<QuorumLossPairEvidenceV1>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ValidatorPenaltyEvidenceV1 {
    pub validator_index: usize,
    pub validator_address: String,
    pub success: u64,
    pub miss: u64,
    pub abstain: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PenaltySnapshotEvidenceV1 {
    pub phase: OracleEvidencePhaseV1,
    pub validators: Vec<ValidatorPenaltyEvidenceV1>,
}

#[derive(Debug)]
pub struct PriceOracleTopology {
    cfg: Config,
    mocks: BTreeMap<usize, MockPriceServer>,
    feeders: BTreeMap<usize, ChildGuard>,
    evidence: PriceOracleEvidenceV1,
    fixed_pairs: Vec<FixedPair>,
}

/// A COEN pair every feeder quotes at one unchanging price beside the controlled COEN/840.
#[derive(Clone, Debug)]
pub(crate) struct FixedPair {
    pub(crate) iso_code: u16,
    pub(crate) rate: alloy_primitives::U256,
    quote: String,
    price: String,
    volume: String,
}

impl PriceOracleTopology {
    pub(crate) fn set_source_failure(&mut self, failure: MockSourceFailure) -> Result<()> {
        eyre::ensure!(
            !self.mocks.is_empty(),
            "no controlled price sources are running"
        );
        for mock in self.mocks.values() {
            mock.set_failure(failure);
        }
        Ok(())
    }
    pub(crate) fn install_cohort(&mut self, phase: OracleEvidencePhaseV1, cohort: OracleCohortV1) {
        self.evidence.cohorts.push(OracleCohortEvidenceV1 {
            first_process_record: self.evidence.feeder_processes.len(),
            phase,
            cohort,
            current_attempts: Vec::new(),
            observations: Vec::new(),
        });
    }

    pub(crate) fn cohort(&self) -> Result<OracleCohortV1> {
        self.evidence
            .cohorts
            .last()
            .map(|entry| entry.cohort.clone())
            .ok_or_else(|| eyre!("Oracle cohort has not been resolved"))
    }

    pub(crate) fn record_cohort_observation(&mut self, observation: serde_json::Value) {
        let process_history_len = self.evidence.feeder_processes.len();
        if let Some(entry) = self.evidence.cohorts.last_mut() {
            entry.observations.push(serde_json::json!({
                "process_history_len": process_history_len,
                "current_attempts": entry.current_attempts,
                "observation": observation,
            }));
        }
    }

    pub(crate) fn ensure_cohort_feeders_alive(&mut self) -> Result<()> {
        let cohort = self.cohort()?;
        validate_live_feeder_indices(&cohort, &self.feeders.keys().copied().collect::<Vec<_>>())?;
        self.ensure_feeder_alive()
    }

    fn refresh_cohort_attempts(&mut self) {
        let attempts = self
            .evidence
            .feeder_processes
            .iter()
            .filter(|process| process.stopped_at_millis.is_none())
            .map(|process| (process.validator_index, process.attempt, process.pid))
            .collect();
        if let Some(entry) = self.evidence.cohorts.last_mut() {
            entry.current_attempts = attempts;
        }
    }

    pub(crate) fn new(cfg: Config) -> Self {
        Self {
            cfg,
            mocks: BTreeMap::new(),
            feeders: BTreeMap::new(),
            evidence: PriceOracleEvidenceV1::default(),
            fixed_pairs: Vec::new(),
        }
    }

    /// Quote COEN in `iso_code` at the scale-6 `rate` on every feeder started from now on.
    #[cfg(feature = "ocomp-integration")]
    pub(crate) fn add_fixed_pair(
        &mut self,
        iso_code: u16,
        rate: alloy_primitives::U256,
        volume: &str,
    ) {
        let scale = alloy_primitives::U256::from(1_000_000_u64);
        self.fixed_pairs.push(FixedPair {
            iso_code,
            rate,
            quote: iso_code.to_string(),
            price: format!("{}.{:06}", rate / scale, rate % scale),
            volume: volume.to_owned(),
        });
    }

    pub(crate) fn fixed_pairs(&self) -> &[FixedPair] {
        &self.fixed_pairs
    }

    pub(crate) fn start(&mut self, launch: FeederLaunch<'_>, quote: PriceQuote<'_>) -> Result<()> {
        let fixed = self.fixed_pairs.clone();
        let pairs = std::iter::once(FeederPair {
            base: "COEN",
            quote: "840",
            sources: vec![FeederSource {
                base: "COEN",
                quote: "840",
                price: quote.price,
                volume: quote.volume,
            }],
        })
        .chain(fixed.iter().map(|pair| FeederPair {
            base: "COEN",
            quote: &pair.quote,
            sources: vec![FeederSource {
                base: "COEN",
                quote: &pair.quote,
                price: &pair.price,
                volume: &pair.volume,
            }],
        }))
        .collect::<Vec<_>>();
        self.start_with_pairs(launch, &pairs)
    }

    pub(crate) fn start_with_pairs(
        &mut self,
        launch: FeederLaunch<'_>,
        pairs: &[FeederPair<'_>],
    ) -> Result<()> {
        let FeederLaunch {
            validator_index,
            rpc_url,
            chain_id,
            private_key,
            validator_address,
            vote_period,
            phase,
        } = launch;
        if self.feeders.contains_key(&validator_index) {
            bail!("validator-{validator_index} price feeder is already running");
        }
        if pairs.is_empty() || pairs.iter().any(|pair| pair.sources.is_empty()) {
            bail!("a feeder must configure at least one source for one Oracle pair");
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
        match self.mocks.entry(validator_index) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(MockPriceServer::bind(expected_book.clone())?);
            }
            std::collections::btree_map::Entry::Occupied(entry)
                if entry.get().book().entries != expected_book.entries =>
            {
                bail!("price mock generation changed during one feeder acceptance window");
            }
            std::collections::btree_map::Entry::Occupied(_) => {}
        }

        let directory = self.cfg.dir.join("price-oracle");
        fs::create_dir_all(&directory)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
        }
        let attempt = u32::try_from(
            self.evidence
                .feeder_processes
                .iter()
                .filter(|process| process.validator_index == validator_index)
                .count()
                + 1,
        )
        .map_err(|_| eyre!("validator-{validator_index} feeder attempt overflow"))?;
        let config_path = directory.join(format!(
            "validator-{validator_index}-feeder-attempt-{attempt}.toml"
        ));
        let log_path = directory.join(format!(
            "validator-{validator_index}-feeder-attempt-{attempt}.log"
        ));
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
            // As under systemd: the vote journal cannot sit beside a config removed after startup.
            .env("STATE_DIRECTORY", &directory)
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
                );
            }
            let started = log_suffix_contains(&log_path, log_start, "starting outbe-feeder");
            if started {
                break;
            }
            if Instant::now() >= deadline {
                let _ = fs::remove_file(&config_path);
                bail!("price feeder did not start within {FEEDER_START_TIMEOUT:?}");
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
                validator_address: validator_address.to_owned(),
                rpc_endpoint: sanitize_rpc_endpoint(rpc_url),
                vote_period,
                attempt,
                pid: feeder_pid,
                log: feeder_log,
                started_at_millis: unix_millis(),
                stopped_at_millis: None,
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
        self.refresh_cohort_attempts();
        let generation = self
            .mocks
            .get(&validator_index)
            .expect("mock exists while recording its sources")
            .book()
            .generation;
        for pair in pairs {
            for source in &pair.sources {
                self.evidence
                    .controlled_sources
                    .push(ControlledSourceEvidenceV1 {
                        phase,
                        generation,
                        validator_index,
                        oracle_pair: format!("{}/{}", pair.base, pair.quote),
                        source_market: format!("mock_http:{}/{}", source.base, source.quote),
                        price: source.price.to_owned(),
                        volume: source.volume.to_owned(),
                    });
            }
        }
        self.feeders.insert(validator_index, feeder);
        Ok(())
    }

    pub fn stop_feeder(&mut self) {
        let stopped_at = unix_millis();
        for validator_index in self.feeders.keys().copied().collect::<Vec<_>>() {
            self.mark_latest_process_stopped(validator_index, stopped_at);
        }
        self.feeders.clear();
        self.refresh_cohort_attempts();
    }

    pub fn stop_validator_feeder(&mut self, validator_index: usize) -> Result<()> {
        self.feeders
            .remove(&validator_index)
            .ok_or_else(|| eyre!("validator-{validator_index} price feeder is not running"))?;
        self.mark_latest_process_stopped(validator_index, unix_millis());
        self.refresh_cohort_attempts();
        Ok(())
    }

    pub fn ensure_feeder_alive(&mut self) -> Result<()> {
        if self.feeders.is_empty() {
            bail!("price feeder is not running");
        }
        for (index, feeder) in &mut self.feeders {
            if feeder.exited() {
                bail!("owned price feeder {index} exited");
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

    pub fn publish_quote(
        &mut self,
        phase: OracleEvidencePhaseV1,
        price: &str,
        volume: &str,
    ) -> Result<u64> {
        if self.feeders.is_empty() {
            bail!("price feeder must be running before changing the controlled quote");
        }
        let mut generation = None;
        for (validator_index, mock) in &self.mocks {
            if mock.book().entries.contains_key(COEN_USD_SYMBOL) {
                let next_generation = mock.publish(COEN_USD_SYMBOL, price, volume)?;
                generation = Some(next_generation);
                self.evidence
                    .controlled_sources
                    .push(ControlledSourceEvidenceV1 {
                        phase,
                        generation: next_generation,
                        validator_index: *validator_index,
                        oracle_pair: "COEN/840".to_owned(),
                        source_market: "mock_http:COEN/840".to_owned(),
                        price: price.to_owned(),
                        volume: volume.to_owned(),
                    });
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

    pub(crate) fn record_canonical_publication(
        &mut self,
        observation: CanonicalPublicationObservation,
    ) {
        let CanonicalPublicationObservation {
            phase,
            validator_count,
            base,
            quote,
            rate,
            volume,
            oracle_block,
            oracle_timestamp,
            finalized_height,
            finalized_timestamp,
        } = observation;
        self.evidence
            .canonical_publications
            .push(CanonicalPricePublicationV1 {
                phase,
                validator_count,
                base: format!("{base:#x}"),
                quote: format!("{quote:#x}"),
                rate: rate.to_string(),
                volume: volume.to_string(),
                oracle_block,
                oracle_timestamp,
                finalized_height,
                finalized_timestamp,
                age_seconds: finalized_timestamp.saturating_sub(oracle_timestamp),
            });
    }

    pub fn record_quorum_loss(&mut self, evidence: QuorumLossEvidenceV1) {
        self.evidence.quorum_loss_windows.push(evidence);
    }

    pub fn record_penalty_snapshot(&mut self, evidence: PenaltySnapshotEvidenceV1) {
        self.evidence.penalty_snapshots.push(evidence);
    }

    fn mark_latest_process_stopped(&mut self, validator_index: usize, stopped_at_millis: u64) {
        if let Some(process) = self
            .evidence
            .feeder_processes
            .iter_mut()
            .rev()
            .find(|process| {
                process.validator_index == validator_index && process.stopped_at_millis.is_none()
            })
        {
            process.stopped_at_millis = Some(stopped_at_millis);
        }
    }

    pub fn teardown(&mut self) {
        self.stop_feeder();
        self.mocks.clear();
    }
}

fn unix_millis() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

fn sanitize_rpc_endpoint(endpoint: &str) -> String {
    let without_query = endpoint.split(['?', '#']).next().unwrap_or(endpoint);
    let Some((scheme, remainder)) = without_query.split_once("://") else {
        return "<invalid-rpc-endpoint>".to_owned();
    };
    let authority = remainder.split('/').next().unwrap_or(remainder);
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    format!("{scheme}://{authority}")
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
        "[chain]\nrpc_endpoint = \"{}\"\nchain_id = {}\ngasless_oracle_votes = true\n\n[account]\nprivate_key = \"{}\"\nvalidator_address = \"{}\"\n\n[oracle]\nvote_period = {}\npoll_interval_secs = 1\nsources = \"local\"\n\n[health]\nenabled = false\n",
        input.rpc_url,
        input.chain_id,
        input.private_key,
        input.validator_address,
        input.vote_period,
    );
    for pair in input.pairs {
        config.push_str(&format!(
            "\n[[currency_pairs]]\nbase = \"{}\"\nquote = \"{}\"\nmarkets = [\n",
            pair.base, pair.quote
        ));
        for source in &pair.sources {
            config.push_str(&format!(
                "  {{ provider = \"mock_http\", base = \"{}\", quote = \"{}\" }},\n",
                source.base, source.quote
            ));
        }
        config.push_str("]\n");
    }
    config.push_str(&format!(
        "\n[[api_providers]]\nname = \"mock_http\"\nrest = \"{}\"\n",
        input.mock_url
    ));
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
mod tests;
