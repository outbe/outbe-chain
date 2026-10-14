//! Axum registration/status plus a TCP ZeroMQ command channel for OCOMP workers.
// OCOMP-TEST-ID: OCM-WTR-001

mod dispatch;
mod state;

use std::net::SocketAddr;
use std::str::FromStr as _;
use std::sync::atomic::AtomicBool;
#[cfg(test)]
use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::Duration;

use alloy_primitives::B256;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bytes::Bytes;
use outbe_ocomp_protocol::{RunUnitV1, SchemaLimits, UnitFinishedV1};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use zeromq::{Endpoint, RouterSocket, Socket, SocketRecv, SocketSend, ZmqMessage};

#[cfg(test)]
use self::state::QueuedWork;
use self::state::{WorkerApiDurations, WorkerApiState};
use crate::control::EndpointIdentity;

pub const MAX_REGISTERED_WORKERS: usize = 4;
const WORKER_REGISTRATION_TIMEOUT: Duration = Duration::from_secs(10);
const WORK_LEASE_TIMEOUT: Duration = Duration::from_secs(30);
const WORKER_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const WORK_RESULT_TIMEOUT: Duration = Duration::from_secs(7_200);
const DISPATCH_CANCEL_POLL: Duration = Duration::from_millis(100);
const DISPATCH_WAIT_REPORT_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerRegistrationV1 {
    pub chain_id: u64,
    pub genesis_hash: String,
    pub process_nonce: String,
    pub protocol_bundle_hash: String,
    pub max_control_body_bytes: usize,
}

impl WorkerRegistrationV1 {
    pub fn from_identity(identity: EndpointIdentity, limits: SchemaLimits) -> Self {
        Self {
            chain_id: identity.chain_id,
            genesis_hash: format!("{:#x}", identity.genesis_hash),
            process_nonce: format!("{:#x}", identity.boot_nonce),
            protocol_bundle_hash: format!("{:#x}", identity.protocol_bundle_hash),
            max_control_body_bytes: limits.max_control_body_bytes,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerRegistrationResponseV1 {
    pub worker_id: String,
    pub registry_generation: u64,
    pub lease_timeout_ms: u64,
    pub message_endpoint: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkLeaseV1 {
    pub worker_id: String,
    pub lease_id: String,
    pub unit_id: String,
    pub delivery_attempt: u32,
    pub lease_timeout_ms: u64,
    pub run_unit_body_hex: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerLeaseRefV1 {
    pub worker_id: String,
    pub lease_id: String,
    pub unit_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerCompletionV1 {
    pub worker_id: String,
    pub lease_id: String,
    pub unit_id: String,
    pub finished_body_hex: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SupervisorCommandV1 {
    Work {
        lease: WorkLeaseV1,
        registry_generation: u64,
    },
    Cancel {
        lease: WorkerLeaseRefV1,
        registry_generation: u64,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkerEventV1 {
    Ready {
        worker_id: String,
        registry_generation: u64,
    },
    Accepted {
        lease: WorkerLeaseRefV1,
        registry_generation: u64,
    },
    Heartbeat {
        lease: WorkerLeaseRefV1,
        registry_generation: u64,
    },
    Completed {
        completion: WorkerCompletionV1,
        registry_generation: u64,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerApiStatusV1 {
    pub registry_generation: u64,
    pub registered_workers: usize,
    pub connected_workers: usize,
    pub busy_workers: usize,
    pub accepted_leases: usize,
    pub queued_units: usize,
    pub max_workers: usize,
}

#[derive(Clone, Debug, Serialize)]
struct HealthV1 {
    status: &'static str,
}

#[derive(Clone, Debug, Serialize)]
struct ApiErrorV1 {
    code: &'static str,
    message: String,
}

pub struct SupervisorWorkerServerV1 {
    state: Arc<WorkerApiState>,
    address: SocketAddr,
    message_address: SocketAddr,
    shutdown: tokio::sync::watch::Sender<bool>,
    thread: Option<JoinHandle<()>>,
}

#[derive(Clone)]
pub struct SupervisorWorkerDispatcherV1 {
    state: Arc<WorkerApiState>,
}

type ReadyServerV1 = (Arc<WorkerApiState>, SocketAddr, SocketAddr);

impl SupervisorWorkerServerV1 {
    pub fn start(
        address: SocketAddr,
        identity: EndpointIdentity,
        registry_generation: u64,
        limits: SchemaLimits,
    ) -> Result<Self, WorkerApiErrorV1> {
        if !address.ip().is_loopback() || registry_generation == 0 {
            return Err(WorkerApiErrorV1::InvalidConfiguration);
        }
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (shutdown, shutdown_rx) = tokio::sync::watch::channel(false);
        let thread = std::thread::Builder::new()
            .name("ocomp-worker-transport".to_owned())
            .spawn(move || {
                serve_worker_transport(
                    address,
                    identity,
                    registry_generation,
                    limits,
                    (ready_tx, shutdown_rx),
                );
            })
            .map_err(|error| WorkerApiErrorV1::Start(error.to_string()))?;
        let (state, address, message_address) = ready_rx
            .recv_timeout(Duration::from_secs(10))
            .map_err(|error| WorkerApiErrorV1::Start(error.to_string()))?
            .map_err(WorkerApiErrorV1::Start)?;
        Ok(Self {
            state,
            address,
            message_address,
            shutdown,
            thread: Some(thread),
        })
    }

    pub const fn address(&self) -> SocketAddr {
        self.address
    }

    pub const fn message_address(&self) -> SocketAddr {
        self.message_address
    }

    pub fn registered_workers(&self) -> Result<usize, WorkerApiErrorV1> {
        Ok(self.state.status()?.connected_workers)
    }

    pub fn dispatch(&self, request: &RunUnitV1) -> Result<UnitFinishedV1, WorkerApiErrorV1> {
        self.state.dispatch(request, None)
    }

    pub fn dispatch_with_cancel(
        &self,
        request: &RunUnitV1,
        cancelled: &AtomicBool,
    ) -> Result<UnitFinishedV1, WorkerApiErrorV1> {
        self.state.dispatch(request, Some(cancelled))
    }

    pub fn dispatcher(&self) -> SupervisorWorkerDispatcherV1 {
        SupervisorWorkerDispatcherV1 {
            state: Arc::clone(&self.state),
        }
    }

    pub fn status(&self) -> Result<WorkerApiStatusV1, WorkerApiErrorV1> {
        self.state.status()
    }
}

impl SupervisorWorkerDispatcherV1 {
    pub fn dispatch(&self, request: &RunUnitV1) -> Result<UnitFinishedV1, WorkerApiErrorV1> {
        self.state.dispatch(request, None)
    }

    pub fn dispatch_with_cancel(
        &self,
        request: &RunUnitV1,
        cancelled: &AtomicBool,
    ) -> Result<UnitFinishedV1, WorkerApiErrorV1> {
        self.state.dispatch(request, Some(cancelled))
    }

    pub fn status(&self) -> Result<WorkerApiStatusV1, WorkerApiErrorV1> {
        self.state.status()
    }
}

impl Drop for SupervisorWorkerServerV1 {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve_worker_transport(
    address: SocketAddr,
    identity: EndpointIdentity,
    registry_generation: u64,
    limits: SchemaLimits,
    (ready_tx, shutdown_rx): (
        mpsc::SyncSender<Result<ReadyServerV1, String>>,
        tokio::sync::watch::Receiver<bool>,
    ),
) {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("ocomp-worker-transport-runtime")
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = ready_tx.send(Err(error.to_string()));
            return;
        }
    };
    runtime.block_on(async move {
        let (listener, router, state_for_server, bound_address, message_address) =
            match bind_worker_transport(address, identity, registry_generation, limits).await {
                Ok(bound) => bound,
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                    return;
                }
            };
        let mut http_shutdown = shutdown_rx.clone();
        let http_state = Arc::clone(&state_for_server);
        let http = tokio::spawn(async move {
            let _ = axum::serve(listener, worker_router(http_state))
                .with_graceful_shutdown(async move {
                    while !*http_shutdown.borrow() {
                        if http_shutdown.changed().await.is_err() {
                            break;
                        }
                    }
                })
                .await;
        });
        if ready_tx
            .send(Ok((
                Arc::clone(&state_for_server),
                bound_address,
                message_address,
            )))
            .is_err()
        {
            return;
        }
        run_message_router(router, state_for_server, shutdown_rx).await;
        http.abort();
        let _ = http.await;
    });
}

async fn bind_worker_transport(
    address: SocketAddr,
    identity: EndpointIdentity,
    registry_generation: u64,
    limits: SchemaLimits,
) -> Result<
    (
        tokio::net::TcpListener,
        RouterSocket,
        Arc<WorkerApiState>,
        SocketAddr,
        SocketAddr,
    ),
    String,
> {
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .map_err(|error| error.to_string())?;
    let bound_address = listener.local_addr().map_err(|error| error.to_string())?;
    let mut router = RouterSocket::new();
    let requested_message_address =
        derive_message_address(address).map_err(|error| error.to_string())?;
    let bound_message_endpoint = router
        .bind(&format!("tcp://{requested_message_address}"))
        .await
        .map_err(|error| error.to_string())?;
    let Endpoint::Tcp(_, port) = bound_message_endpoint else {
        return Err("ZeroMQ did not bind a TCP endpoint".into());
    };
    let message_address = SocketAddr::new(address.ip(), port);
    let state_for_server = Arc::new(WorkerApiState::new(
        identity,
        registry_generation,
        limits,
        WorkerApiDurations::default(),
        format!("tcp://{message_address}"),
    ));
    Ok((
        listener,
        router,
        state_for_server,
        bound_address,
        message_address,
    ))
}

fn worker_router(state: Arc<WorkerApiState>) -> Router {
    let max_body = state
        .limits
        .max_control_body_bytes
        .saturating_mul(2)
        .saturating_add(8_192);
    Router::new()
        .route("/health", get(health))
        .route("/v1/status", get(status_handler))
        .route("/v1/workers/register", post(register))
        .layer(DefaultBodyLimit::max(max_body))
        .with_state(state)
}

async fn health() -> Json<HealthV1> {
    Json(HealthV1 { status: "ok" })
}

async fn status_handler(
    State(state): State<Arc<WorkerApiState>>,
) -> Result<Json<WorkerApiStatusV1>, WorkerApiErrorV1> {
    state.status().map(Json)
}

async fn register(
    State(state): State<Arc<WorkerApiState>>,
    Json(request): Json<WorkerRegistrationV1>,
) -> Result<Json<WorkerRegistrationResponseV1>, WorkerApiErrorV1> {
    state.register(request).map(Json).map_err(|error| {
        eprintln!("OCOMP worker registration rejected: {error}");
        error
    })
}

async fn run_message_router(
    router: RouterSocket,
    state: Arc<WorkerApiState>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let (mut sender, mut receiver) = router.split();
    let mut cleanup = tokio::time::interval(Duration::from_secs(1));
    cleanup.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            message = receiver.recv() => {
                match message
                    .map_err(|error| WorkerApiErrorV1::MessageTransport(error.to_string()))
                    .and_then(|message| decode_worker_event(message, state.limits.max_control_body_bytes))
                    .and_then(|(route, event)| state.handle_event(route, event))
                {
                    Ok(()) => {}
                    Err(error) => eprintln!("OCOMP worker message rejected: {error}"),
                }
            }
            _ = state.wake.notified() => {}
            _ = cleanup.tick() => {}
        }
        let commands = match state.drain_commands() {
            Ok(commands) => commands,
            Err(error) => {
                eprintln!("OCOMP worker command drain failed: {error}");
                continue;
            }
        };
        for (route, command) in commands {
            match encode_supervisor_command(route, &command) {
                Ok(message) => {
                    if let Err(error) = sender.send(message).await {
                        eprintln!("OCOMP worker command send failed: {error}");
                    }
                }
                Err(error) => eprintln!("OCOMP worker command encode failed: {error}"),
            }
        }
    }
}

fn decode_worker_event(
    message: ZmqMessage,
    max_body_bytes: usize,
) -> Result<(B256, WorkerEventV1), WorkerApiErrorV1> {
    let frames = message.into_vec();
    if frames.len() != 2 || frames[1].len() > max_body_bytes.saturating_mul(2).saturating_add(8_192)
    {
        return Err(WorkerApiErrorV1::MalformedRequest);
    }
    let route = std::str::from_utf8(&frames[0])
        .map_err(|_| WorkerApiErrorV1::MalformedRequest)
        .and_then(parse_b256)?;
    let event =
        serde_json::from_slice(&frames[1]).map_err(|_| WorkerApiErrorV1::MalformedRequest)?;
    Ok((route, event))
}

fn encode_supervisor_command(
    route: B256,
    command: &SupervisorCommandV1,
) -> Result<ZmqMessage, WorkerApiErrorV1> {
    let body = serde_json::to_vec(command).map_err(|_| WorkerApiErrorV1::MalformedRequest)?;
    ZmqMessage::try_from(vec![Bytes::from(format!("{route:#x}")), Bytes::from(body)])
        .map_err(|_| WorkerApiErrorV1::MalformedRequest)
}

fn derive_message_address(http: SocketAddr) -> Result<SocketAddr, WorkerApiErrorV1> {
    if http.port() != 0 {
        let port = http
            .port()
            .checked_add(1)
            .ok_or(WorkerApiErrorV1::InvalidConfiguration)?;
        return Ok(SocketAddr::new(http.ip(), port));
    }
    Ok(SocketAddr::new(http.ip(), 0))
}

impl IntoResponse for WorkerApiErrorV1 {
    fn into_response(self) -> Response {
        let message = self.to_string();
        let (response_status, code) = match self {
            WorkerApiErrorV1::MalformedRequest | WorkerApiErrorV1::Protocol(_) => {
                (StatusCode::BAD_REQUEST, "malformed_request")
            }
            WorkerApiErrorV1::IdentityMismatch => (StatusCode::FORBIDDEN, "identity_mismatch"),
            WorkerApiErrorV1::UnknownWorker => (StatusCode::NOT_FOUND, "unknown_worker"),
            WorkerApiErrorV1::RegistryFull => (StatusCode::SERVICE_UNAVAILABLE, "registry_full"),
            WorkerApiErrorV1::StaleLease => (StatusCode::CONFLICT, "stale_lease"),
            WorkerApiErrorV1::LeaseNotAccepted => (StatusCode::CONFLICT, "lease_not_accepted"),
            WorkerApiErrorV1::ConflictingCompletion => {
                (StatusCode::CONFLICT, "conflicting_completion")
            }
            WorkerApiErrorV1::StaleGeneration => (StatusCode::CONFLICT, "stale_generation"),
            _ => (StatusCode::INTERNAL_SERVER_ERROR, "internal_error"),
        };
        (response_status, Json(ApiErrorV1 { code, message })).into_response()
    }
}

fn parse_b256(value: &str) -> Result<B256, WorkerApiErrorV1> {
    B256::from_str(value).map_err(|_| WorkerApiErrorV1::MalformedRequest)
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[derive(Debug, Error)]
pub enum WorkerApiErrorV1 {
    #[error("invalid Supervisor worker transport configuration")]
    InvalidConfiguration,
    #[error("failed to start Supervisor worker transport: {0}")]
    Start(String),
    #[error("worker registration or message is malformed")]
    MalformedRequest,
    #[error("worker identity does not match the Supervisor domain")]
    IdentityMismatch,
    #[error("Supervisor worker registry is full")]
    RegistryFull,
    #[error("worker is not registered")]
    UnknownWorker,
    #[error("worker lease is stale or belongs to another unit")]
    StaleLease,
    #[error("worker lease must be accepted before heartbeat")]
    LeaseNotAccepted,
    #[error("completion retry does not match the already accepted result")]
    ConflictingCompletion,
    #[error("no OCOMP worker registered before the deadline")]
    NoRegisteredWorkers,
    #[error("worker result did not arrive before the bounded deadline")]
    ResultTimeout,
    #[error("worker dispatch was cancelled")]
    DispatchCancelled,
    #[error("worker registry generation is stale")]
    StaleGeneration,
    #[error("worker TCP ZeroMQ transport failed: {0}")]
    MessageTransport(String),
    #[error("worker registry lock is poisoned")]
    Poisoned,
    #[error("worker protocol body is invalid: {0}")]
    Protocol(String),
}

#[cfg(test)]
mod tests;
