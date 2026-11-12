//! Health and status HTTP server: `GET /health`, `GET /status`.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::State as AxumState;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use serde_json::json;
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
    /// `H` is not the checkpoint signer that the controller accepts.
    signer_error: Option<String>,
    /// The error of the last failed `submitCheckpoint`. A success clears it.
    submit_error: Option<String>,
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
        self.state().submit_error = None;
    }

    pub fn record_failure(&self, error: String) {
        self.failed.fetch_add(1, Ordering::Relaxed);
        self.state().submit_error = Some(redact_urls(&error));
    }

    pub fn set_signer_error(&self, error: Option<String>) {
        self.state().signer_error = error;
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

    /// Sets the error of a domain. A started domain keeps its signer, so
    /// `/status` still shows its reorg flag.
    pub fn set_error(&self, domain: u32, error: String) {
        self.state().domains.entry(domain).or_default().error = Some(redact_urls(&error));
    }

    /// Removes the domains that the controller does not list.
    pub fn retain_domains(&self, keep: impl Fn(u32) -> bool) {
        self.state().domains.retain(|d, _| keep(*d));
    }

    /// The signer of a started domain, also after a stop.
    #[cfg(test)]
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
    /// lead to a jail), when `H` is not the accepted signer, or when a domain
    /// halted, stalled or has an error. Waiting for the controller is
    /// healthy: there is nothing to sign yet.
    fn is_healthy(&self) -> bool {
        let state = self.state();
        self.miss_count.load(Ordering::Relaxed) == 0
            && state.signer_error.is_none()
            && state.domains.values().all(|d| {
                d.error.is_none() && !d.signer.as_ref().is_some_and(|s| s.halted() || s.stalled())
            })
    }

    pub fn status_json(&self) -> serde_json::Value {
        let (controller, active, signer_error, submit_error, domains) = {
            let state = self.state();
            let domains: Vec<_> = state
                .domains
                .iter()
                .map(|(d, h)| (*d, h.signer.clone(), h.error.clone()))
                .collect();
            (
                state.controller,
                state.active,
                state.signer_error.clone(),
                state.submit_error.clone(),
                domains,
            )
        };
        let domains: serde_json::Map<String, serde_json::Value> = domains
            .into_iter()
            .map(|(domain, signer, error)| {
                let error = error.or_else(|| {
                    signer.as_ref().filter(|s| s.stalled()).map(|_| {
                        "the sync makes no progress: endpoints reject the read leaves again and again (possible reorg below the stored cursor)".to_string()
                    })
                });
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
            "signer_error": signer_error,
            "miss_count": self.miss_count.load(Ordering::Relaxed),
            "submitted": self.submitted.load(Ordering::Relaxed),
            "failed": self.failed.load(Ordering::Relaxed),
            "last_submit_time": self.last_submit_time.load(Ordering::Relaxed),
            "last_submit_error": submit_error,
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

/// Replaces each `http(s)://` or `ws(s)://` URL with `<url>`. An RPC URL can
/// hold a provider API key, and `/status` is public.
fn redact_urls(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = ["http://", "https://", "ws://", "wss://"]
        .iter()
        .filter_map(|scheme| rest.find(scheme))
        .min()
    {
        out.push_str(&rest[..start]);
        out.push_str("<url>");
        rest = &rest[start..];
        let end = rest
            .find(|c: char| c.is_whitespace() || matches!(c, ')' | '"' | '\'' | ',' | '>'))
            .unwrap_or(rest.len());
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

pub fn router(health: Arc<ValidatorHealth>) -> Router {
    Router::new()
        .route("/health", get(health_route))
        .route("/status", get(status_route))
        .fallback(|| async {
            json(
                StatusCode::NOT_FOUND,
                json!({"error": "not found"}).to_string(),
            )
        })
        .with_state(health)
}

fn json(status: StatusCode, body: String) -> Response {
    (status, [(header::CONTENT_TYPE, "application/json")], body).into_response()
}

async fn health_route(AxumState(health): AxumState<Arc<ValidatorHealth>>) -> Response {
    if health.is_healthy() {
        json(StatusCode::OK, json!({"status": "ok"}).to_string())
    } else {
        json(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"status": "unhealthy"}).to_string(),
        )
    }
}

async fn status_route(AxumState(health): AxumState<Arc<ValidatorHealth>>) -> Response {
    let status = serde_json::to_string_pretty(&health.status_json()).unwrap_or_default();
    json(StatusCode::OK, status)
}

pub async fn start_health_server(
    bind_addr: &str,
    health: Arc<ValidatorHealth>,
) -> eyre::Result<()> {
    let listener = TcpListener::bind(bind_addr).await?;
    tracing::info!(addr = bind_addr, "health server listening");
    let app = router(health);
    tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, app).await {
            tracing::warn!(%error, "health server stopped");
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

    #[test]
    fn redact_urls_hides_rpc_urls() {
        assert_eq!(
            redact_urls("error sending request for url (https://rpc.example/v2/KEY): timed out"),
            "error sending request for url (<url>): timed out"
        );
        assert_eq!(
            redact_urls("invalid RPC URL wss://a/KEY and http://b"),
            "invalid RPC URL <url> and <url>"
        );
        assert_eq!(redact_urls("no url"), "no url");
    }

    #[tokio::test]
    async fn routes_and_status_codes() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());
        let health = Arc::new(ValidatorHealth::new(store));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let app = router(health.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let get = |path: &str| {
            let url = format!("{url}{path}");
            async move {
                let r = reqwest::get(url).await.unwrap();
                (r.status().as_u16(), r.text().await.unwrap())
            }
        };

        assert_eq!(get("/health").await, (200, r#"{"status":"ok"}"#.into()));
        health.set_error(5, "eth_call failed: http://rpc/KEY".into());
        assert_eq!(
            get("/health").await,
            (503, r#"{"status":"unhealthy"}"#.into())
        );
        let (code, body) = get("/status").await;
        let status: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            (code, &status["domains"]["5"]["error"]),
            (200, &json!("eth_call failed: <url>"))
        );
        assert_eq!(
            get("/other").await,
            (404, r#"{"error":"not found"}"#.into())
        );
    }
}
