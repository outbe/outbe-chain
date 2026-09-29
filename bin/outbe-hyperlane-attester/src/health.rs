//! Health and status HTTP server: `GET /health`, `GET /status`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;

pub struct AttesterHealth {
    pub submitted: AtomicU64,
    pub failed: AtomicU64,
    pub last_submit_time: AtomicU64,
    /// Consecutive liveness misses recorded on-chain for this validator.
    pub miss_count: AtomicU64,
}

impl AttesterHealth {
    pub fn new() -> Self {
        Self {
            submitted: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            last_submit_time: AtomicU64::new(0),
            miss_count: AtomicU64::new(0),
        }
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

    /// Unhealthy once the controller has recorded a liveness miss: the next
    /// misses lead to a jail, so the operator must look now.
    fn is_healthy(&self) -> bool {
        self.miss_count.load(Ordering::Relaxed) == 0
    }

    fn to_json(&self) -> serde_json::Value {
        json!({
            "healthy": self.is_healthy(),
            "submitted": self.submitted.load(Ordering::Relaxed),
            "failed": self.failed.load(Ordering::Relaxed),
            "last_submit_time": self.last_submit_time.load(Ordering::Relaxed),
            "miss_count": self.miss_count.load(Ordering::Relaxed),
        })
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub async fn start_health_server(bind_addr: &str, health: Arc<AttesterHealth>) -> eyre::Result<()> {
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
                        serde_json::to_string_pretty(&health.to_json()).unwrap_or_default(),
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

    #[test]
    fn miss_count_drives_health() {
        let health = AttesterHealth::new();
        assert!(health.is_healthy());
        health.set_miss_count(1);
        assert!(!health.is_healthy());
        health.set_miss_count(0);
        health.record_success();
        assert!(health.is_healthy());
        assert_eq!(health.to_json()["submitted"], 1);
    }
}
