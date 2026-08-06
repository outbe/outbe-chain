//! Salvo-only observability surface for one OCOMP Worker process.

use std::net::SocketAddr;
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use salvo::affix_state;
use salvo::conn::Listener as _;
use salvo::prelude::*;
use salvo::request_id::RequestId;
use salvo::server::ServerHandle;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerPhaseV1 {
    Registering,
    Idle,
    Working,
    Cancelling,
    Disconnected,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkerStatusV1 {
    pub phase: WorkerPhaseV1,
    pub worker_id: Option<String>,
    pub registry_generation: Option<u64>,
    pub lease_id: Option<String>,
    pub unit_id: Option<String>,
    pub last_supervisor_activity_ms_ago: u64,
}

#[derive(Clone, Debug, Serialize)]
struct HealthV1 {
    status: &'static str,
}

struct WorkerStatusState {
    phase: WorkerPhaseV1,
    worker_id: Option<String>,
    registry_generation: Option<u64>,
    lease_id: Option<String>,
    unit_id: Option<String>,
    last_supervisor_activity: Instant,
}

pub struct WorkerObservabilityServerV1 {
    state: Arc<Mutex<WorkerStatusState>>,
    address: SocketAddr,
    handle: ServerHandle,
    thread: Option<JoinHandle<()>>,
}

impl WorkerObservabilityServerV1 {
    pub fn start(address: SocketAddr) -> Result<Self, WorkerObservabilityErrorV1> {
        if !address.ip().is_loopback() {
            return Err(WorkerObservabilityErrorV1::InvalidAddress(address));
        }
        let state = Arc::new(Mutex::new(WorkerStatusState {
            phase: WorkerPhaseV1::Registering,
            worker_id: None,
            registry_generation: None,
            lease_id: None,
            unit_id: None,
            last_supervisor_activity: Instant::now(),
        }));
        let server_state = Arc::clone(&state);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("ocomp-worker-observability".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = ready_tx.send(Err(error.to_string()));
                        return;
                    }
                };
                runtime.block_on(async move {
                    let acceptor = match salvo::conn::TcpListener::new(address).try_bind().await {
                        Ok(acceptor) => acceptor,
                        Err(error) => {
                            let _ = ready_tx.send(Err(error.to_string()));
                            return;
                        }
                    };
                    let bound_address = match acceptor.local_addr() {
                        Ok(address) => address,
                        Err(error) => {
                            let _ = ready_tx.send(Err(error.to_string()));
                            return;
                        }
                    };
                    let server = Server::new(acceptor).max_connections(32);
                    let handle = server.handle();
                    if ready_tx.send(Ok((bound_address, handle))).is_err() {
                        return;
                    }
                    server.serve(router(server_state)).await;
                });
            })
            .map_err(|error| WorkerObservabilityErrorV1::Start(error.to_string()))?;
        let (address, handle) = ready_rx
            .recv_timeout(Duration::from_secs(10))
            .map_err(|error| WorkerObservabilityErrorV1::Start(error.to_string()))?
            .map_err(WorkerObservabilityErrorV1::Start)?;
        Ok(Self {
            state,
            address,
            handle,
            thread: Some(thread),
        })
    }

    pub const fn address(&self) -> SocketAddr {
        self.address
    }

    pub fn registering(&self) {
        self.update(WorkerPhaseV1::Registering, None, None, None, None, false);
    }

    pub fn idle(&self, worker_id: String, registry_generation: u64) {
        self.update(
            WorkerPhaseV1::Idle,
            Some(worker_id),
            Some(registry_generation),
            None,
            None,
            true,
        );
    }

    pub fn working(
        &self,
        worker_id: String,
        registry_generation: u64,
        lease_id: String,
        unit_id: String,
    ) {
        self.update(
            WorkerPhaseV1::Working,
            Some(worker_id),
            Some(registry_generation),
            Some(lease_id),
            Some(unit_id),
            true,
        );
    }

    pub fn cancelling(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.phase = WorkerPhaseV1::Cancelling;
            state.last_supervisor_activity = Instant::now();
        }
    }

    pub fn disconnected(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.phase = WorkerPhaseV1::Disconnected;
        }
    }

    pub fn touch(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.last_supervisor_activity = Instant::now();
        }
    }

    pub fn status(&self) -> Result<WorkerStatusV1, WorkerObservabilityErrorV1> {
        snapshot(&self.state)
    }

    fn update(
        &self,
        phase: WorkerPhaseV1,
        worker_id: Option<String>,
        registry_generation: Option<u64>,
        lease_id: Option<String>,
        unit_id: Option<String>,
        supervisor_activity: bool,
    ) {
        if let Ok(mut state) = self.state.lock() {
            state.phase = phase;
            state.worker_id = worker_id;
            state.registry_generation = registry_generation;
            state.lease_id = lease_id;
            state.unit_id = unit_id;
            if supervisor_activity {
                state.last_supervisor_activity = Instant::now();
            }
        }
    }
}

impl Drop for WorkerObservabilityServerV1 {
    fn drop(&mut self) {
        self.handle.stop_forceful();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn router(state: Arc<Mutex<WorkerStatusState>>) -> Router {
    Router::new()
        .hoop(RequestId::new())
        .hoop(affix_state::inject(state))
        .push(Router::with_path("healthz").get(healthz))
        .push(Router::with_path("status").get(worker_status))
}

#[handler]
async fn healthz(res: &mut Response) {
    res.render(Json(HealthV1 { status: "ok" }));
}

#[handler]
async fn worker_status(depot: &Depot, res: &mut Response) {
    let result = depot
        .get_typed::<Arc<Mutex<WorkerStatusState>>>()
        .map_err(|_| WorkerObservabilityErrorV1::MissingState)
        .and_then(snapshot);
    match result {
        Ok(snapshot) => res.render(Json(snapshot)),
        Err(error) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            res.render(Text::Plain(error.to_string()));
        }
    };
}

fn snapshot(
    state: &Arc<Mutex<WorkerStatusState>>,
) -> Result<WorkerStatusV1, WorkerObservabilityErrorV1> {
    let state = state
        .lock()
        .map_err(|_| WorkerObservabilityErrorV1::Poisoned)?;
    Ok(WorkerStatusV1 {
        phase: state.phase,
        worker_id: state.worker_id.clone(),
        registry_generation: state.registry_generation,
        lease_id: state.lease_id.clone(),
        unit_id: state.unit_id.clone(),
        last_supervisor_activity_ms_ago: u64::try_from(
            state.last_supervisor_activity.elapsed().as_millis(),
        )
        .unwrap_or(u64::MAX),
    })
}

#[derive(Debug, Error)]
pub enum WorkerObservabilityErrorV1 {
    #[error("worker observability address {0} must be a loopback endpoint")]
    InvalidAddress(SocketAddr),
    #[error("failed to start Worker Salvo observability server: {0}")]
    Start(String),
    #[error("Worker observability state is unavailable")]
    MissingState,
    #[error("Worker observability state lock is poisoned")]
    Poisoned,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn healthz_and_status_remain_available_while_worker_is_busy() {
        let server = WorkerObservabilityServerV1::start("127.0.0.1:0".parse().unwrap())
            .expect("start Worker observability");
        server.working("worker-1".into(), 7, "lease-1".into(), "unit-1".into());
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        assert_eq!(
            client
                .get(format!("http://{}/healthz", server.address()))
                .send()
                .unwrap()
                .status(),
            reqwest::StatusCode::OK
        );
        let status: WorkerStatusV1 = client
            .get(format!("http://{}/status", server.address()))
            .send()
            .unwrap()
            .json()
            .unwrap();
        assert_eq!(status.phase, WorkerPhaseV1::Working);
        assert_eq!(status.lease_id.as_deref(), Some("lease-1"));
    }
}
