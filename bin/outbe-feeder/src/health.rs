//! Health and status HTTP server for the price-feeder daemon.
//!
//! Provides `/health` and `/status` endpoints for operator monitoring.
//! Runs as a background tokio task alongside the main feeder loop.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;

/// Shared feeder health state, updated by the main loop.
pub struct FeederHealth {
    /// Block number of the last successful vote submission.
    pub last_vote_block: AtomicU64,
    /// Unix timestamp of the last successful vote submission.
    pub last_vote_time: AtomicU64,
    /// Total number of votes successfully submitted.
    pub votes_submitted: AtomicU64,
    /// Total number of votes that failed submission.
    pub votes_failed: AtomicU64,
    /// Current vote period.
    pub current_period: AtomicU64,
    /// Configured vote period in blocks.
    pub vote_period: u64,
    observations: Mutex<Observations>,
}

struct Observations {
    started_at: Instant,
    head: Option<HeadObservation>,
    expected_pairs: BTreeSet<String>,
    oracle: BTreeMap<String, OracleObservation>,
    pending: Option<(String, u64)>,
    reason: String,
}

struct HeadObservation {
    height: u64,
    observed_at: Instant,
    progressed_at: Instant,
}

struct OracleObservation {
    block: u64,
    timestamp: u64,
    observed_at: Instant,
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl FeederHealth {
    pub fn new(vote_period: u64) -> Self {
        Self {
            last_vote_block: AtomicU64::new(0),
            last_vote_time: AtomicU64::new(0),
            votes_submitted: AtomicU64::new(0),
            votes_failed: AtomicU64::new(0),
            current_period: AtomicU64::new(0),
            vote_period,
            observations: Mutex::new(Observations {
                started_at: Instant::now(),
                head: None,
                expected_pairs: BTreeSet::new(),
                oracle: BTreeMap::new(),
                pending: None,
                reason: String::new(),
            }),
        }
    }

    pub fn record_success(&self, block: u64) {
        self.last_vote_block.store(block, Ordering::Relaxed);
        self.last_vote_time.store(unix_now(), Ordering::Relaxed);
        self.votes_submitted.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_failure(&self) {
        self.votes_failed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn set_period(&self, period: u64) {
        self.current_period.store(period, Ordering::Relaxed);
    }

    /// Configure every pair whose on-chain update is required for readiness.
    pub fn expect_pairs(&self, pairs: &[String]) {
        self.observations.lock().unwrap().expected_pairs = pairs.iter().cloned().collect();
    }

    /// Record a successfully observed committed head. Re-reading the same head
    /// refreshes RPC availability, but does not refresh chain progress.
    pub fn record_head(&self, height: u64) {
        let now = Instant::now();
        let mut observations = self.observations.lock().unwrap();
        match &mut observations.head {
            Some(head) => {
                head.observed_at = now;
                if height > head.height {
                    head.height = height;
                    head.progressed_at = now;
                }
            }
            None => {
                observations.head = Some(HeadObservation {
                    height,
                    observed_at: now,
                    progressed_at: now,
                });
            }
        }
    }

    /// Record getExchangeRateData metadata, even when the numerical rate did
    /// not change. Failed reads must not refresh an earlier observation.
    pub fn record_oracle(&self, pair: &str, block: u64, timestamp: u64) {
        self.observations.lock().unwrap().oracle.insert(
            pair.to_string(),
            OracleObservation {
                block,
                timestamp,
                observed_at: Instant::now(),
            },
        );
    }

    /// created_at is the Unix timestamp of the original broadcast attempt,
    /// retained across retries and restarts, not the last receipt poll.
    pub fn set_pending(&self, hash: Option<&str>, created_at: u64) {
        self.observations.lock().unwrap().pending = hash.map(|hash| (hash.to_string(), created_at));
    }

    /// Diagnostic scheduler context; readiness is determined from observations.
    pub fn set_reason(&self, reason: &str) {
        self.observations.lock().unwrap().reason = reason.to_string();
    }

    fn wall_timeout(&self) -> Duration {
        Duration::from_secs(self.vote_period.saturating_mul(60).max(60))
    }

    fn reasons_at(&self, observations: &Observations, now: Instant, unix: u64) -> Vec<String> {
        let timeout = self.wall_timeout();
        let grace_expired = now.saturating_duration_since(observations.started_at) >= timeout;
        let mut reasons = Vec::new();
        let last_vote = self.last_vote_time.load(Ordering::Relaxed);
        if last_vote == 0 {
            if grace_expired {
                reasons.push("no confirmed vote before startup grace expired".into());
            }
        } else if unix.saturating_sub(last_vote) >= timeout.as_secs() {
            reasons.push("last confirmed vote is stale".into());
        }

        match &observations.head {
            Some(head) => {
                if now.saturating_duration_since(head.observed_at) >= timeout {
                    reasons.push("committed head observation is stale".into());
                }
                if now.saturating_duration_since(head.progressed_at) >= timeout {
                    reasons.push("committed head has stopped advancing".into());
                }
            }
            None if grace_expired => reasons.push("no committed head observed".into()),
            None => {}
        }

        for pair in &observations.expected_pairs {
            match observations.oracle.get(pair) {
                Some(oracle) => {
                    if oracle.block == 0 || oracle.timestamp == 0 {
                        reasons.push(format!("oracle pair {pair} has no published observation"));
                    }
                    if now.saturating_duration_since(oracle.observed_at) >= timeout {
                        reasons.push(format!("oracle pair {pair} observation is stale"));
                    }
                    if let Some(head) = &observations.head {
                        if oracle.block > head.height {
                            reasons.push(format!(
                                "oracle pair {pair} update is ahead of observed head"
                            ));
                        } else if head.height - oracle.block > self.vote_period.saturating_mul(5) {
                            reasons.push(format!("oracle pair {pair} last update is stale"));
                        }
                    }
                }
                None if grace_expired => {
                    reasons.push(format!("oracle pair {pair} has not been observed"))
                }
                None => {}
            }
        }
        if let Some((hash, created_at)) = &observations.pending {
            if unix.saturating_sub(*created_at) >= timeout.as_secs() {
                reasons.push(format!("pending transaction {hash} is stale"));
            }
        }
        reasons
    }

    pub fn health_reasons(&self) -> Vec<String> {
        self.reasons_at(
            &self.observations.lock().unwrap(),
            Instant::now(),
            unix_now(),
        )
    }

    #[cfg(test)]
    fn is_healthy(&self) -> bool {
        self.health_reasons().is_empty()
    }

    fn to_json(&self) -> serde_json::Value {
        let now = Instant::now();
        let unix = unix_now();
        let observations = self.observations.lock().unwrap();
        let reasons = self.reasons_at(&observations, now, unix);
        let oracle: BTreeMap<_, _> = observations.expected_pairs.iter().map(|pair| {
            let value = observations.oracle.get(pair).map(|observation| json!({
                "last_update_block": observation.block,
                "last_update_timestamp": observation.timestamp,
                "observation_age_secs": now.saturating_duration_since(observation.observed_at).as_secs(),
                "blocks_since_update": observations.head.as_ref().map(|head| head.height.saturating_sub(observation.block)),
            }));
            (pair, value)
        }).collect();
        json!({
            "healthy": reasons.is_empty(),
            "health_reasons": reasons,
            "last_reason": observations.reason,
            "last_vote_block": self.last_vote_block.load(Ordering::Relaxed),
            "last_vote_time": self.last_vote_time.load(Ordering::Relaxed),
            "votes_submitted": self.votes_submitted.load(Ordering::Relaxed),
            "votes_failed": self.votes_failed.load(Ordering::Relaxed),
            "current_period": self.current_period.load(Ordering::Relaxed),
            "vote_period": self.vote_period,
            "startup_age_secs": now.saturating_duration_since(observations.started_at).as_secs(),
            "wall_timeout_secs": self.wall_timeout().as_secs(),
            "max_oracle_age_blocks": self.vote_period.saturating_mul(5),
            "head": observations.head.as_ref().map(|head| json!({
                "height": head.height,
                "observation_age_secs": now.saturating_duration_since(head.observed_at).as_secs(),
                "progress_age_secs": now.saturating_duration_since(head.progressed_at).as_secs(),
            })),
            "oracle_pairs": oracle,
            "pending": observations.pending.as_ref().map(|(hash, created_at)| json!({
                "hash": hash,
                "created_at": created_at,
                "age_secs": unix.saturating_sub(*created_at),
            })),
        })
    }
}

/// Starts the health HTTP server on the given bind address.
///
/// Serves:
/// - `GET /health` - 200 if healthy, 503 if not
/// - `GET /status` - JSON with full feeder state
///
/// Returns immediately; the server runs as a background task.
pub async fn start_health_server(bind_addr: &str, health: Arc<FeederHealth>) -> eyre::Result<()> {
    let listener = TcpListener::bind(bind_addr).await?;
    tracing::info!(addr = bind_addr, "health server listening");

    tokio::spawn(async move {
        loop {
            let (mut stream, _) = match listener.accept().await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(error = %e, "health server accept error");
                    continue;
                }
            };

            let health = health.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 1024];
                let n = match tokio::io::AsyncReadExt::read(&mut stream, &mut buf).await {
                    Ok(n) => n,
                    Err(_) => return,
                };

                let request = String::from_utf8_lossy(&buf[..n]);
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("/");

                let (status, body) = match path {
                    "/health" => {
                        let reasons = health.health_reasons();
                        let healthy = reasons.is_empty();
                        (
                            if healthy {
                                "200 OK"
                            } else {
                                "503 Service Unavailable"
                            },
                            json!({
                                "status": if healthy { "ok" } else { "unhealthy" },
                                "health_reasons": reasons,
                            })
                            .to_string(),
                        )
                    }
                    "/status" => {
                        let json = health.to_json();
                        (
                            "200 OK",
                            serde_json::to_string_pretty(&json).unwrap_or_default(),
                        )
                    }
                    _ => ("404 Not Found", "{\"error\":\"not found\"}".to_string()),
                };

                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready_health() -> FeederHealth {
        let health = FeederHealth::new(8);
        health.expect_pairs(&["COEN/USD".into()]);
        health.record_head(800);
        health.record_oracle("COEN/USD", 800, 1_000);
        health.record_success(799);
        health
    }

    fn expire_startup(health: &FeederHealth) {
        health.observations.lock().unwrap().started_at =
            Instant::now() - health.wall_timeout() - Duration::from_secs(1);
    }

    #[test]
    fn successful_votes_do_not_mask_stale_oracle() {
        let health = ready_health();
        health.record_head(840);
        assert!(health.is_healthy(), "exactly five periods is allowed");
        health.record_head(841);
        health.record_success(841);
        assert!(health
            .health_reasons()
            .iter()
            .any(|r| r.contains("last update is stale")));
        assert_eq!(health.to_json()["healthy"], false);
    }

    #[test]
    fn startup_without_a_vote_has_a_finite_grace() {
        let health = FeederHealth::new(8);
        assert!(health.is_healthy());
        expire_startup(&health);
        assert!(health
            .health_reasons()
            .iter()
            .any(|r| r.contains("no confirmed vote")));
    }

    #[test]
    fn polling_a_stalled_head_does_not_refresh_progress() {
        let health = ready_health();
        health
            .observations
            .lock()
            .unwrap()
            .head
            .as_mut()
            .unwrap()
            .progressed_at = Instant::now() - health.wall_timeout();
        health.record_head(800);
        assert!(health
            .health_reasons()
            .iter()
            .any(|r| r.contains("stopped advancing")));
        health.record_head(801);
        assert!(health.is_healthy());
    }

    #[test]
    fn rpc_outage_expires_the_head_observation() {
        let health = ready_health();
        health
            .observations
            .lock()
            .unwrap()
            .head
            .as_mut()
            .unwrap()
            .observed_at = Instant::now() - health.wall_timeout();
        assert!(health
            .health_reasons()
            .iter()
            .any(|r| r.contains("head observation is stale")));
    }

    #[test]
    fn every_expected_pair_must_be_observed() {
        let health = ready_health();
        health.expect_pairs(&["COEN/USD".into(), "COEN/EUR".into()]);
        expire_startup(&health);
        assert!(health
            .health_reasons()
            .iter()
            .any(|r| r.contains("COEN/EUR has not been observed")));
        assert_eq!(
            health.to_json()["oracle_pairs"]["COEN/EUR"],
            serde_json::Value::Null
        );
        health.record_oracle("COEN/EUR", 800, 1_000);
        assert!(health.is_healthy());
    }

    #[test]
    fn unchanged_rate_with_advancing_update_metadata_is_healthy() {
        let health = ready_health();
        // The health API deliberately accepts no numerical price: a constant
        // price is fresh if the chain keeps publishing observations of it.
        for height in [808, 816, 824, 832, 840, 848] {
            health.record_head(height);
            health.record_oracle("COEN/USD", height, 1_000 + height);
            assert!(health.is_healthy());
        }
        assert_eq!(
            health.to_json()["oracle_pairs"]["COEN/USD"]["last_update_timestamp"],
            1848
        );
    }

    #[test]
    fn failed_oracle_reads_cannot_keep_an_old_observation_healthy() {
        let health = ready_health();
        health
            .observations
            .lock()
            .unwrap()
            .oracle
            .get_mut("COEN/USD")
            .unwrap()
            .observed_at = Instant::now() - health.wall_timeout();
        health.record_head(801);
        assert!(health
            .health_reasons()
            .iter()
            .any(|r| r.contains("COEN/USD observation is stale")));
    }

    #[test]
    fn absent_publication_is_unhealthy_even_during_startup() {
        let health = ready_health();
        health.record_oracle("COEN/USD", 0, 0);
        assert!(health
            .health_reasons()
            .iter()
            .any(|r| r.contains("no published observation")));
    }

    #[test]
    fn pending_age_survives_reobservation_and_clears_on_resolution() {
        let health = ready_health();
        let created_at = unix_now() - health.wall_timeout().as_secs();
        health.set_pending(Some("0x1234"), created_at);
        health.set_reason("waiting for receipt");
        assert!(health
            .health_reasons()
            .iter()
            .any(|r| r.contains("pending transaction 0x1234 is stale")));
        let json = health.to_json();
        assert_eq!(json["pending"]["created_at"], created_at);
        assert_eq!(json["last_reason"], "waiting for receipt");
        health.set_pending(None, 0);
        assert!(health.is_healthy());
        assert_eq!(health.to_json()["pending"], serde_json::Value::Null);
    }

    #[test]
    fn regressed_head_does_not_hide_staleness_or_count_as_progress() {
        let health = ready_health();
        health.record_head(841);
        health.record_head(800);
        assert_eq!(health.to_json()["head"]["height"], 841);
        assert!(!health.is_healthy());
    }

    #[test]
    fn old_success_expires_even_with_fresh_chain_observations() {
        let health = ready_health();
        health.last_vote_time.store(
            unix_now() - health.wall_timeout().as_secs(),
            Ordering::Relaxed,
        );
        assert!(health
            .health_reasons()
            .iter()
            .any(|r| r.contains("confirmed vote is stale")));
    }

    #[test]
    fn test_health_new_is_healthy() {
        let h = FeederHealth::new(2);
        assert!(h.is_healthy());
    }

    #[test]
    fn test_health_after_success() {
        let h = FeederHealth::new(2);
        h.record_success(100);
        assert!(h.is_healthy());
        assert_eq!(h.votes_submitted.load(Ordering::Relaxed), 1);
        assert_eq!(h.last_vote_block.load(Ordering::Relaxed), 100);
    }

    #[test]
    fn test_health_failure_count() {
        let h = FeederHealth::new(2);
        h.record_failure();
        h.record_failure();
        assert_eq!(h.votes_failed.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn test_health_json() {
        let h = FeederHealth::new(2);
        h.record_success(42);
        h.set_period(21);
        let j = h.to_json();
        assert_eq!(j["last_vote_block"], 42);
        assert_eq!(j["current_period"], 21);
        assert_eq!(j["vote_period"], 2);
        assert_eq!(j["healthy"], true);
    }

    #[tokio::test]
    async fn test_health_server_starts() {
        let health = Arc::new(FeederHealth::new(2));
        // Use port 0 to let OS assign a free port
        let result = start_health_server("127.0.0.1:0", health).await;
        assert!(result.is_ok());
    }
}
