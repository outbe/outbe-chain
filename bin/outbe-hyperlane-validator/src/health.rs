//! Health and status HTTP server: `GET /health`, `GET /status`.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;

use crate::signer::DomainSigner;
use crate::store::Store;

/// The state of the HyperlaneController as the daemon saw it last.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Controller {
    /// `icaRouter()` is zero. The daemon polls again.
    WaitingForController,
    Ready,
}

/// One controller domain. `signer` is set once the domain started.
#[derive(Default)]
pub struct DomainHealth {
    pub signer: Option<Arc<DomainSigner>>,
    pub error: Option<String>,
}

#[derive(Default)]
struct State {
    /// `None` until the first successful read.
    controller: Option<Controller>,
    /// `None` until the first read of the active set.
    active: Option<bool>,
    domains: BTreeMap<u32, DomainHealth>,
}

pub struct ValidatorHealth {
    pub submitted: AtomicU64,
    pub failed: AtomicU64,
    pub last_submit_time: AtomicU64,
    /// Consecutive liveness misses recorded on-chain for this validator.
    pub miss_count: AtomicU64,
    store: Arc<Store>,
    state: Mutex<State>,
}

impl ValidatorHealth {
    pub fn new(store: Arc<Store>) -> Self {
        Self {
            submitted: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            last_submit_time: AtomicU64::new(0),
            miss_count: AtomicU64::new(0),
            store,
            state: Mutex::default(),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn record_success(&self) {
        self.last_submit_time.store(unix_now(), Ordering::Relaxed);
        self.submitted.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_failure(&self) {
        self.failed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn set_miss_count(&self, misses: u64) {
        self.miss_count.store(misses, Ordering::Relaxed);
    }

    pub fn set_active(&self, active: bool) {
        self.state().active = Some(active);
    }

    pub fn set_controller(&self, controller: Controller) {
        self.state().controller = Some(controller);
    }

    /// Marks a domain as started. It clears an earlier error.
    pub fn set_running(&self, signer: Arc<DomainSigner>) {
        let domain = signer.domain();
        self.state().domains.insert(
            domain,
            DomainHealth {
                signer: Some(signer),
                error: None,
            },
        );
    }

    /// Sets the error of a domain. A started domain keeps its signer, so its
    /// stored checkpoints stay readable.
    pub fn set_error(&self, domain: u32, error: String) {
        self.state().domains.entry(domain).or_default().error = Some(error);
    }

    /// Removes the domains that the controller does not list.
    pub fn retain_domains(&self, keep: impl Fn(u32) -> bool) {
        self.state().domains.retain(|d, _| keep(*d));
    }

    /// The signer of a started domain, also after a stop.
    pub fn signer(&self, domain: u32) -> Option<Arc<DomainSigner>> {
        self.state().domains.get(&domain)?.signer.clone()
    }

    /// Started domains without an error and without a halt: the liveness
    /// submits their checkpoints.
    pub fn running(&self) -> Vec<Arc<DomainSigner>> {
        self.state()
            .domains
            .values()
            .filter(|d| d.error.is_none())
            .filter_map(|d| d.signer.clone())
            .filter(|s| !s.halted())
            .collect()
    }

    /// Unhealthy once the controller records a liveness miss (the next misses
    /// lead to a jail), or when a domain halted or has an error. Waiting for
    /// the controller is healthy: there is nothing to sign yet.
    fn is_healthy(&self) -> bool {
        self.miss_count.load(Ordering::Relaxed) == 0
            && self
                .state()
                .domains
                .values()
                .all(|d| d.error.is_none() && !d.signer.as_ref().is_some_and(|s| s.halted()))
    }

    pub fn status_json(&self) -> serde_json::Value {
        let (controller, active, domains) = {
            let state = self.state();
            let domains: Vec<_> = state
                .domains
                .iter()
                .map(|(d, h)| (*d, h.signer.clone(), h.error.clone()))
                .collect();
            (state.controller, state.active, domains)
        };
        let domains: serde_json::Map<String, serde_json::Value> = domains
            .into_iter()
            .map(|(domain, signer, error)| {
                let store = &self.store;
                let value = json!({
                    "leaves": store.leaf_count(domain).ok(),
                    "latest_signed_index": store.latest_checkpoint_index(domain).ok().flatten(),
                    "cursor": store.cursor(domain).ok().flatten(),
                    "reorg": signer.is_some_and(|s| s.halted()),
                    "error": error,
                });
                (domain.to_string(), value)
            })
            .collect();
        json!({
            "healthy": self.is_healthy(),
            "controller": match controller {
                None => "unknown",
                Some(Controller::WaitingForController) => "waiting_for_controller",
                Some(Controller::Ready) => "ready",
            },
            "active": active,
            "miss_count": self.miss_count.load(Ordering::Relaxed),
            "submitted": self.submitted.load(Ordering::Relaxed),
            "failed": self.failed.load(Ordering::Relaxed),
            "last_submit_time": self.last_submit_time.load(Ordering::Relaxed),
            "domains": domains,
        })
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub async fn start_health_server(
    bind_addr: &str,
    health: Arc<ValidatorHealth>,
) -> eyre::Result<()> {
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
                let Ok(n) = tokio::io::AsyncReadExt::read(&mut stream, &mut buf).await else {
                    return;
                };
                let request = String::from_utf8_lossy(&buf[..n]);
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("/");
                let (status, body) = match path {
                    "/health" if health.is_healthy() => {
                        ("200 OK", "{\"status\":\"ok\"}".to_string())
                    }
                    "/health" => (
                        "503 Service Unavailable",
                        "{\"status\":\"unhealthy\"}".to_string(),
                    ),
                    "/status" => (
                        "200 OK",
                        serde_json::to_string_pretty(&health.status_json()).unwrap_or_default(),
                    ),
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

    #[tokio::test]
    async fn health_follows_misses_halts_and_errors() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());
        let health = ValidatorHealth::new(store.clone());
        assert!(health.is_healthy());
        assert_eq!(health.status_json()["controller"], "unknown");
        assert_eq!(health.status_json()["active"], serde_json::Value::Null);

        health.set_miss_count(1);
        assert!(!health.is_healthy());
        health.set_miss_count(0);
        health.record_success();
        health.set_active(false);
        health.set_controller(Controller::WaitingForController);
        assert!(
            health.is_healthy(),
            "an inactive or waiting validator is healthy"
        );
        let json = health.status_json();
        assert_eq!(json["submitted"], 1);
        assert_eq!(json["active"], false);
        assert_eq!(json["controller"], "waiting_for_controller");

        // A domain error is unhealthy until the domain starts.
        health.set_error(7, "no [[origin]]".into());
        assert!(!health.is_healthy());
        assert_eq!(
            health.status_json()["domains"]["7"]["error"],
            "no [[origin]]"
        );
        let origin = crate::sync::Origin {
            domain: 7,
            hook: alloy_primitives::Address::ZERO,
            validator_announce: alloy_primitives::Address::ZERO,
            mailbox: alloy_primitives::Address::ZERO,
            start_block: 1,
        };
        let key = alloy_signer_local::PrivateKeySigner::random();
        let signer = Arc::new(DomainSigner::new(origin, store.clone(), key).unwrap());
        health.set_running(signer);
        assert!(health.is_healthy());
        assert_eq!(health.running().len(), 1);
        let domain = &health.status_json()["domains"]["7"];
        assert_eq!(domain["leaves"], 0);
        assert_eq!(domain["latest_signed_index"], serde_json::Value::Null);
        assert_eq!(domain["reorg"], false);
        assert_eq!(domain["error"], serde_json::Value::Null);

        // A stopped domain keeps its signer but leaves the liveness.
        health.set_error(7, "hook changed".into());
        assert!(health.running().is_empty());
        assert!(health.signer(7).is_some());
        health.retain_domains(|_| false);
        assert!(health.signer(7).is_none());
        assert!(health.is_healthy());
    }
}
