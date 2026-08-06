//! Salvo HTTP pull boundary between the OCOMP Supervisor and local workers.

use std::collections::{BTreeMap, VecDeque};
use std::net::SocketAddr;
use std::str::FromStr as _;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use alloy_primitives::{keccak256, B256};
use outbe_ocomp_protocol::{RunUnitV1, SchemaLimits, UnitFinishedV1};
use salvo::affix_state;
use salvo::conn::Listener as _;
use salvo::prelude::*;
use salvo::request_id::RequestId;
use salvo::server::ServerHandle;
use salvo::size_limiter;
use salvo::timeout::Timeout;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::control::EndpointIdentity;

pub const MAX_REGISTERED_WORKERS: usize = 4;
const MAX_HTTP_CONNECTIONS: usize = 64;
const HTTP_HANDLER_TIMEOUT: Duration = Duration::from_secs(5);
const WORKER_REGISTRATION_TIMEOUT: Duration = Duration::from_secs(10);
const WORK_LEASE_TIMEOUT: Duration = Duration::from_secs(30);
const WORKER_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const WORK_RESULT_TIMEOUT: Duration = Duration::from_secs(7_200);
const DISPATCH_CANCEL_POLL: Duration = Duration::from_millis(100);

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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerClaimV1 {
    pub worker_id: String,
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
#[serde(deny_unknown_fields)]
pub struct WorkerApiStatusV1 {
    pub registry_generation: u64,
    pub registered_workers: usize,
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

#[derive(Clone, Copy)]
struct WorkerApiDurations {
    registration: Duration,
    lease: Duration,
    idle: Duration,
    result: Duration,
}

impl Default for WorkerApiDurations {
    fn default() -> Self {
        Self {
            registration: WORKER_REGISTRATION_TIMEOUT,
            lease: WORK_LEASE_TIMEOUT,
            idle: WORKER_IDLE_TIMEOUT,
            result: WORK_RESULT_TIMEOUT,
        }
    }
}

pub struct SupervisorWorkerHttpServerV1 {
    state: Arc<WorkerApiState>,
    address: SocketAddr,
    handle: ServerHandle,
    thread: Option<JoinHandle<()>>,
}

#[derive(Clone)]
pub struct SupervisorWorkerDispatcherV1 {
    state: Arc<WorkerApiState>,
}

impl SupervisorWorkerHttpServerV1 {
    pub fn start(
        address: SocketAddr,
        identity: EndpointIdentity,
        registry_generation: u64,
        limits: SchemaLimits,
    ) -> Result<Self, WorkerApiErrorV1> {
        if !address.ip().is_loopback() || registry_generation == 0 {
            return Err(WorkerApiErrorV1::InvalidConfiguration);
        }
        let state = Arc::new(WorkerApiState::new(
            identity,
            registry_generation,
            limits,
            WorkerApiDurations::default(),
        ));
        let state_for_server = Arc::clone(&state);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("ocomp-worker-http".to_owned())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .thread_name("ocomp-worker-http-runtime")
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
                    let server = Server::new(acceptor).max_connections(MAX_HTTP_CONNECTIONS);
                    let handle = server.handle();
                    if ready_tx.send(Ok((bound_address, handle))).is_err() {
                        return;
                    }
                    server.serve(worker_router(state_for_server)).await;
                });
            })
            .map_err(|error| WorkerApiErrorV1::Start(error.to_string()))?;
        let (address, handle) = ready_rx
            .recv_timeout(Duration::from_secs(10))
            .map_err(|error| WorkerApiErrorV1::Start(error.to_string()))?
            .map_err(WorkerApiErrorV1::Start)?;
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

    pub fn registered_workers(&self) -> Result<usize, WorkerApiErrorV1> {
        Ok(self.state.status()?.registered_workers)
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

impl Drop for SupervisorWorkerHttpServerV1 {
    fn drop(&mut self) {
        self.handle.stop_forceful();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct WorkerApiState {
    identity: EndpointIdentity,
    registry_generation: u64,
    limits: SchemaLimits,
    durations: WorkerApiDurations,
    next_id: AtomicU64,
    inner: Mutex<WorkerRegistry>,
    worker_registered: Condvar,
}

#[derive(Default)]
struct WorkerRegistry {
    workers: BTreeMap<B256, WorkerRecord>,
    pending: VecDeque<QueuedWork>,
}

struct WorkerRecord {
    process_nonce: B256,
    last_seen: Instant,
    active: Option<ActiveLease>,
    last_completed: Option<CompletedLease>,
}

struct CompletedLease {
    lease_id: B256,
    unit_id: B256,
    body_digest: B256,
}

struct QueuedWork {
    queue_id: u64,
    unit_id: B256,
    body: Vec<u8>,
    delivery_attempt: u32,
    completion: mpsc::SyncSender<UnitFinishedV1>,
}

struct ActiveLease {
    lease_id: B256,
    deadline: Instant,
    accepted: bool,
    work: QueuedWork,
}

impl WorkerApiState {
    fn new(
        identity: EndpointIdentity,
        registry_generation: u64,
        limits: SchemaLimits,
        durations: WorkerApiDurations,
    ) -> Self {
        Self {
            identity,
            registry_generation,
            limits,
            durations,
            next_id: AtomicU64::new(1),
            inner: Mutex::new(WorkerRegistry::default()),
            worker_registered: Condvar::new(),
        }
    }

    fn lock(&self) -> Result<MutexGuard<'_, WorkerRegistry>, WorkerApiErrorV1> {
        self.inner.lock().map_err(|_| WorkerApiErrorV1::Poisoned)
    }

    fn register(
        &self,
        request: WorkerRegistrationV1,
    ) -> Result<WorkerRegistrationResponseV1, WorkerApiErrorV1> {
        let genesis_hash = parse_b256(&request.genesis_hash)?;
        let process_nonce = parse_b256(&request.process_nonce)?;
        let protocol_bundle_hash = parse_b256(&request.protocol_bundle_hash)?;
        if process_nonce.is_zero()
            || request.chain_id != self.identity.chain_id
            || genesis_hash != self.identity.genesis_hash
            || protocol_bundle_hash != self.identity.protocol_bundle_hash
            || request.max_control_body_bytes != self.limits.max_control_body_bytes
        {
            return Err(WorkerApiErrorV1::IdentityMismatch);
        }
        let now = Instant::now();
        let mut registry = self.lock()?;
        self.cleanup_expired(&mut registry, now);
        if let Some((worker_id, worker)) = registry
            .workers
            .iter_mut()
            .find(|(_, worker)| worker.process_nonce == process_nonce)
        {
            worker.last_seen = now;
            return Ok(self.registration_response(*worker_id));
        }
        if registry.workers.len() >= MAX_REGISTERED_WORKERS {
            return Err(WorkerApiErrorV1::RegistryFull);
        }
        let worker_id = self.derive_id(b"OCOMP_WORKER_ID_V1", process_nonce.as_slice());
        registry.workers.insert(
            worker_id,
            WorkerRecord {
                process_nonce,
                last_seen: now,
                active: None,
                last_completed: None,
            },
        );
        self.worker_registered.notify_all();
        Ok(self.registration_response(worker_id))
    }

    fn registration_response(&self, worker_id: B256) -> WorkerRegistrationResponseV1 {
        WorkerRegistrationResponseV1 {
            worker_id: format!("{worker_id:#x}"),
            registry_generation: self.registry_generation,
            lease_timeout_ms: duration_ms(self.durations.lease),
        }
    }

    fn claim(&self, request: WorkerClaimV1) -> Result<Option<WorkLeaseV1>, WorkerApiErrorV1> {
        let worker_id = parse_b256(&request.worker_id)?;
        let now = Instant::now();
        let mut registry = self.lock()?;
        self.cleanup_expired(&mut registry, now);
        let existing = registry
            .workers
            .get_mut(&worker_id)
            .ok_or(WorkerApiErrorV1::UnknownWorker)?;
        existing.last_seen = now;
        if let Some(active) = existing.active.as_ref() {
            return Ok(Some(self.lease_response(worker_id, active)));
        }
        let Some(work) = registry.pending.pop_front() else {
            return Ok(None);
        };
        let mut lease_material = Vec::with_capacity(72);
        lease_material.extend_from_slice(worker_id.as_slice());
        lease_material.extend_from_slice(work.unit_id.as_slice());
        lease_material.extend_from_slice(&work.queue_id.to_be_bytes());
        let lease_id = self.derive_id(b"OCOMP_WORK_LEASE_V1", &lease_material);
        let active = ActiveLease {
            lease_id,
            deadline: now + self.durations.lease,
            accepted: false,
            work,
        };
        let response = self.lease_response(worker_id, &active);
        registry
            .workers
            .get_mut(&worker_id)
            .ok_or(WorkerApiErrorV1::UnknownWorker)?
            .active = Some(active);
        Ok(Some(response))
    }

    fn accepted(&self, request: WorkerLeaseRefV1) -> Result<(), WorkerApiErrorV1> {
        self.touch_lease(request, true)
    }

    fn heartbeat(&self, request: WorkerLeaseRefV1) -> Result<(), WorkerApiErrorV1> {
        self.touch_lease(request, false)
    }

    fn touch_lease(
        &self,
        request: WorkerLeaseRefV1,
        mark_accepted: bool,
    ) -> Result<(), WorkerApiErrorV1> {
        let worker_id = parse_b256(&request.worker_id)?;
        let lease_id = parse_b256(&request.lease_id)?;
        let unit_id = parse_b256(&request.unit_id)?;
        let now = Instant::now();
        let mut registry = self.lock()?;
        self.cleanup_expired(&mut registry, now);
        let worker = registry
            .workers
            .get_mut(&worker_id)
            .ok_or(WorkerApiErrorV1::UnknownWorker)?;
        let active = worker.active.as_mut().ok_or(WorkerApiErrorV1::StaleLease)?;
        if active.lease_id != lease_id || active.work.unit_id != unit_id {
            return Err(WorkerApiErrorV1::StaleLease);
        }
        if !mark_accepted && !active.accepted {
            return Err(WorkerApiErrorV1::LeaseNotAccepted);
        }
        worker.last_seen = now;
        active.deadline = now + self.durations.lease;
        active.accepted |= mark_accepted;
        Ok(())
    }

    fn complete(&self, request: WorkerCompletionV1) -> Result<(), WorkerApiErrorV1> {
        let worker_id = parse_b256(&request.worker_id)?;
        let lease_id = parse_b256(&request.lease_id)?;
        let unit_id = parse_b256(&request.unit_id)?;
        let body = hex::decode(&request.finished_body_hex)
            .map_err(|_| WorkerApiErrorV1::MalformedRequest)?;
        if body.len() > self.limits.max_control_body_bytes {
            return Err(WorkerApiErrorV1::MalformedRequest);
        }
        let finished = UnitFinishedV1::decode_body(&body, &self.limits)
            .map_err(|_| WorkerApiErrorV1::MalformedRequest)?;
        if finished.unit_id != unit_id {
            return Err(WorkerApiErrorV1::StaleLease);
        }
        let body_digest = keccak256(&body);
        let now = Instant::now();
        let mut registry = self.lock()?;
        self.cleanup_expired(&mut registry, now);
        let worker = registry
            .workers
            .get_mut(&worker_id)
            .ok_or(WorkerApiErrorV1::UnknownWorker)?;
        if let Some(completed) = worker.last_completed.as_ref() {
            if completed.lease_id == lease_id && completed.unit_id == unit_id {
                if completed.body_digest != body_digest {
                    return Err(WorkerApiErrorV1::ConflictingCompletion);
                }
                worker.last_seen = now;
                return Ok(());
            }
        }
        let active = worker.active.as_ref().ok_or(WorkerApiErrorV1::StaleLease)?;
        if active.lease_id != lease_id || active.work.unit_id != unit_id || !active.accepted {
            return Err(WorkerApiErrorV1::StaleLease);
        }
        let active = worker.active.take().ok_or(WorkerApiErrorV1::StaleLease)?;
        worker.last_seen = now;
        worker.last_completed = Some(CompletedLease {
            lease_id,
            unit_id,
            body_digest,
        });
        drop(registry);
        active
            .work
            .completion
            .send(finished)
            .map_err(|_| WorkerApiErrorV1::DispatchCancelled)
    }

    fn dispatch(
        &self,
        request: &RunUnitV1,
        cancelled: Option<&AtomicBool>,
    ) -> Result<UnitFinishedV1, WorkerApiErrorV1> {
        if cancelled.is_some_and(|cancelled| cancelled.load(Ordering::Acquire)) {
            return Err(WorkerApiErrorV1::DispatchCancelled);
        }
        let body = request
            .encode_body(&self.limits)
            .map_err(|error| WorkerApiErrorV1::Protocol(error.to_string()))?;
        let spec = outbe_ocomp_protocol::unit::UnitSpecV1::decode_canonical(
            &request.canonical_unit_spec.0,
            &self.limits,
        )
        .map_err(|error| WorkerApiErrorV1::Protocol(error.to_string()))?;
        let unit_id = spec
            .unit_id(&self.limits)
            .map_err(|error| WorkerApiErrorV1::Protocol(error.to_string()))?;
        let deadline = Instant::now() + self.durations.registration;
        let mut registry = self.lock()?;
        loop {
            if cancelled.is_some_and(|cancelled| cancelled.load(Ordering::Acquire)) {
                return Err(WorkerApiErrorV1::DispatchCancelled);
            }
            self.cleanup_expired(&mut registry, Instant::now());
            if !registry.workers.is_empty() {
                break;
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(WorkerApiErrorV1::NoRegisteredWorkers);
            }
            let timeout = deadline
                .saturating_duration_since(now)
                .min(DISPATCH_CANCEL_POLL);
            let (next, _) = self
                .worker_registered
                .wait_timeout(registry, timeout)
                .map_err(|_| WorkerApiErrorV1::Poisoned)?;
            registry = next;
        }
        let queue_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (completion_tx, completion_rx) = mpsc::sync_channel(1);
        registry.pending.push_back(QueuedWork {
            queue_id,
            unit_id,
            body,
            delivery_attempt: 1,
            completion: completion_tx,
        });
        drop(registry);
        let result_deadline = Instant::now() + self.durations.result;
        loop {
            if cancelled.is_some_and(|cancelled| cancelled.load(Ordering::Acquire)) {
                self.cancel(queue_id)?;
                return Err(WorkerApiErrorV1::DispatchCancelled);
            }
            let remaining = result_deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                self.cancel(queue_id)?;
                return Err(WorkerApiErrorV1::ResultTimeout);
            }
            match completion_rx.recv_timeout(remaining.min(DISPATCH_CANCEL_POLL)) {
                Ok(finished) => return Ok(finished),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.cancel(queue_id)?;
                    return Err(WorkerApiErrorV1::DispatchCancelled);
                }
            }
        }
    }

    fn cancel(&self, queue_id: u64) -> Result<(), WorkerApiErrorV1> {
        let mut registry = self.lock()?;
        registry.pending.retain(|work| work.queue_id != queue_id);
        for worker in registry.workers.values_mut() {
            if worker
                .active
                .as_ref()
                .is_some_and(|lease| lease.work.queue_id == queue_id)
            {
                worker.active = None;
            }
        }
        Ok(())
    }

    fn status(&self) -> Result<WorkerApiStatusV1, WorkerApiErrorV1> {
        let mut registry = self.lock()?;
        self.cleanup_expired(&mut registry, Instant::now());
        Ok(WorkerApiStatusV1 {
            registry_generation: self.registry_generation,
            registered_workers: registry.workers.len(),
            busy_workers: registry
                .workers
                .values()
                .filter(|worker| worker.active.is_some())
                .count(),
            accepted_leases: registry
                .workers
                .values()
                .filter(|worker| worker.active.as_ref().is_some_and(|lease| lease.accepted))
                .count(),
            queued_units: registry.pending.len(),
            max_workers: MAX_REGISTERED_WORKERS,
        })
    }

    fn lease_response(&self, worker_id: B256, active: &ActiveLease) -> WorkLeaseV1 {
        WorkLeaseV1 {
            worker_id: format!("{worker_id:#x}"),
            lease_id: format!("{:#x}", active.lease_id),
            unit_id: format!("{:#x}", active.work.unit_id),
            delivery_attempt: active.work.delivery_attempt,
            lease_timeout_ms: duration_ms(self.durations.lease),
            run_unit_body_hex: hex::encode(&active.work.body),
        }
    }

    fn cleanup_expired(&self, registry: &mut WorkerRegistry, now: Instant) {
        let mut requeue = Vec::new();
        registry.workers.retain(|_, worker| {
            if worker
                .active
                .as_ref()
                .is_some_and(|active| active.deadline <= now)
            {
                if let Some(mut active) = worker.active.take() {
                    active.work.delivery_attempt = active.work.delivery_attempt.saturating_add(1);
                    requeue.push(active.work);
                }
            }
            worker.active.is_some() || now.duration_since(worker.last_seen) <= self.durations.idle
        });
        registry.pending.extend(requeue);
    }

    fn derive_id(&self, domain: &[u8], material: &[u8]) -> B256 {
        let counter = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut bytes = Vec::with_capacity(domain.len() + material.len() + 16);
        bytes.extend_from_slice(domain);
        bytes.extend_from_slice(&self.registry_generation.to_be_bytes());
        bytes.extend_from_slice(&counter.to_be_bytes());
        bytes.extend_from_slice(material);
        keccak256(bytes)
    }
}

fn worker_router(state: Arc<WorkerApiState>) -> Router {
    let max_body = state
        .limits
        .max_control_body_bytes
        .saturating_mul(2)
        .saturating_add(8_192) as u64;
    Router::new()
        .hoop(RequestId::new())
        .hoop(Timeout::new(HTTP_HANDLER_TIMEOUT))
        .hoop(size_limiter::max_size(max_body))
        .hoop(affix_state::inject(state))
        .push(Router::with_path("health").get(health))
        .push(Router::with_path("v1/status").get(status_handler))
        .push(Router::with_path("v1/workers/register").post(register))
        .push(Router::with_path("v1/workers/claim").post(claim))
        .push(Router::with_path("v1/workers/accepted").post(accepted))
        .push(Router::with_path("v1/workers/complete").post(complete))
        .push(Router::with_path("v1/workers/heartbeat").post(heartbeat))
}

fn state(depot: &Depot) -> Result<&Arc<WorkerApiState>, WorkerApiErrorV1> {
    depot
        .get_typed::<Arc<WorkerApiState>>()
        .map_err(|_| WorkerApiErrorV1::MissingState)
}

#[handler]
async fn health(res: &mut Response) {
    res.render(Json(HealthV1 { status: "ok" }));
}

#[handler]
async fn status_handler(depot: &Depot, res: &mut Response) {
    match state(depot).and_then(|state| state.status()) {
        Ok(snapshot) => res.render(Json(snapshot)),
        Err(error) => render_error(res, error),
    };
}

#[handler]
async fn register(req: &mut Request, depot: &Depot, res: &mut Response) {
    let result = match req.parse_json::<WorkerRegistrationV1>().await {
        Ok(request) => state(depot).and_then(|state| state.register(request)),
        Err(_) => Err(WorkerApiErrorV1::MalformedRequest),
    };
    match result {
        Ok(response) => res.render(Json(response)),
        Err(error) => render_error(res, error),
    };
}

#[handler]
async fn claim(req: &mut Request, depot: &Depot, res: &mut Response) {
    let result = match req.parse_json::<WorkerClaimV1>().await {
        Ok(request) => state(depot).and_then(|state| state.claim(request)),
        Err(_) => Err(WorkerApiErrorV1::MalformedRequest),
    };
    match result {
        Ok(Some(response)) => res.render(Json(response)),
        Ok(None) => {
            res.status_code(StatusCode::NO_CONTENT);
        }
        Err(error) => render_error(res, error),
    };
}

#[handler]
async fn accepted(req: &mut Request, depot: &Depot, res: &mut Response) {
    unit_action(req, depot, res, true).await;
}

#[handler]
async fn heartbeat(req: &mut Request, depot: &Depot, res: &mut Response) {
    unit_action(req, depot, res, false).await;
}

async fn unit_action(req: &mut Request, depot: &Depot, res: &mut Response, accept: bool) {
    let result = match req.parse_json::<WorkerLeaseRefV1>().await {
        Ok(request) => state(depot).and_then(|state| {
            if accept {
                state.accepted(request)
            } else {
                state.heartbeat(request)
            }
        }),
        Err(_) => Err(WorkerApiErrorV1::MalformedRequest),
    };
    match result {
        Ok(()) => {
            res.status_code(StatusCode::NO_CONTENT);
        }
        Err(error) => render_error(res, error),
    }
}

#[handler]
async fn complete(req: &mut Request, depot: &Depot, res: &mut Response) {
    let result = match req.parse_json::<WorkerCompletionV1>().await {
        Ok(request) => state(depot).and_then(|state| state.complete(request)),
        Err(_) => Err(WorkerApiErrorV1::MalformedRequest),
    };
    match result {
        Ok(()) => {
            res.status_code(StatusCode::NO_CONTENT);
        }
        Err(error) => render_error(res, error),
    }
}

fn render_error(res: &mut Response, error: WorkerApiErrorV1) {
    let (response_status, code) = match error {
        WorkerApiErrorV1::MalformedRequest | WorkerApiErrorV1::Protocol(_) => {
            (StatusCode::BAD_REQUEST, "malformed_request")
        }
        WorkerApiErrorV1::IdentityMismatch => (StatusCode::FORBIDDEN, "identity_mismatch"),
        WorkerApiErrorV1::UnknownWorker => (StatusCode::NOT_FOUND, "unknown_worker"),
        WorkerApiErrorV1::RegistryFull => (StatusCode::SERVICE_UNAVAILABLE, "registry_full"),
        WorkerApiErrorV1::StaleLease => (StatusCode::CONFLICT, "stale_lease"),
        WorkerApiErrorV1::LeaseNotAccepted => (StatusCode::CONFLICT, "lease_not_accepted"),
        WorkerApiErrorV1::ConflictingCompletion => (StatusCode::CONFLICT, "conflicting_completion"),
        _ => (StatusCode::INTERNAL_SERVER_ERROR, "internal_error"),
    };
    res.status_code(response_status);
    res.render(Json(ApiErrorV1 {
        code,
        message: error.to_string(),
    }));
}

fn parse_b256(value: &str) -> Result<B256, WorkerApiErrorV1> {
    B256::from_str(value).map_err(|_| WorkerApiErrorV1::MalformedRequest)
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[derive(Debug, Error)]
pub enum WorkerApiErrorV1 {
    #[error("invalid Supervisor worker HTTP configuration")]
    InvalidConfiguration,
    #[error("failed to start Supervisor worker HTTP server: {0}")]
    Start(String),
    #[error("worker HTTP request is malformed")]
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
    #[error("worker HTTP registry lock is poisoned")]
    Poisoned,
    #[error("worker HTTP handler state is unavailable")]
    MissingState,
    #[error("worker protocol body is invalid: {0}")]
    Protocol(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn identity() -> EndpointIdentity {
        EndpointIdentity {
            chain_id: 41,
            genesis_hash: B256::repeat_byte(0x41),
            boot_nonce: B256::repeat_byte(0x42),
            protocol_bundle_hash: B256::repeat_byte(0x43),
        }
    }

    #[test]
    fn registry_accepts_four_workers_and_rejects_fifth() {
        let limits = crate::control::poc_schema_limits();
        let state = WorkerApiState::new(identity(), 7, limits, WorkerApiDurations::default());
        for ordinal in 0_u8..4 {
            let mut worker = identity();
            worker.boot_nonce = B256::repeat_byte(0x50 + ordinal);
            state
                .register(WorkerRegistrationV1::from_identity(worker, limits))
                .expect("register worker");
        }
        let mut fifth = identity();
        fifth.boot_nonce = B256::repeat_byte(0x60);
        assert!(matches!(
            state.register(WorkerRegistrationV1::from_identity(fifth, limits)),
            Err(WorkerApiErrorV1::RegistryFull)
        ));
        assert_eq!(state.status().unwrap().registered_workers, 4);
    }

    #[test]
    fn cancelled_dispatch_does_not_wait_for_registration_timeout() {
        let limits = crate::control::poc_schema_limits();
        let state = WorkerApiState::new(identity(), 7, limits, WorkerApiDurations::default());
        let cancelled = AtomicBool::new(true);
        let request = RunUnitV1 {
            protocol_bundle_hash: identity().protocol_bundle_hash,
            job_id: B256::repeat_byte(0x71),
            attempt: 1,
            plan_hash: B256::repeat_byte(0x72),
            unit_index: 0,
            canonical_unit_spec: outbe_ocomp_protocol::common::BoundedBytes(Vec::new()),
            unit_membership_siblings: Vec::new(),
            plan_ref: outbe_ocomp_protocol::CasObjectRefV1 {
                transport_digest: B256::repeat_byte(0x73),
                encoded_bytes: 1,
                expected_ocb1_kind: None,
            },
            input_manifest_ref: outbe_ocomp_protocol::CasObjectRefV1 {
                transport_digest: B256::repeat_byte(0x74),
                encoded_bytes: 1,
                expected_ocb1_kind: None,
            },
            ordered_input_refs: Vec::new(),
        };
        assert!(matches!(
            state.dispatch(&request, Some(&cancelled)),
            Err(WorkerApiErrorV1::DispatchCancelled)
        ));
    }

    #[test]
    fn expired_lease_is_requeued_and_stale_completion_conflicts() {
        let limits = crate::control::poc_schema_limits();
        let durations = WorkerApiDurations {
            registration: Duration::from_millis(50),
            lease: Duration::from_millis(20),
            idle: Duration::from_secs(1),
            result: Duration::from_secs(1),
        };
        let state = WorkerApiState::new(identity(), 7, limits, durations);
        let mut first_identity = identity();
        first_identity.boot_nonce = B256::repeat_byte(0x51);
        let first = state
            .register(WorkerRegistrationV1::from_identity(first_identity, limits))
            .unwrap();
        let mut second_identity = identity();
        second_identity.boot_nonce = B256::repeat_byte(0x52);
        let second = state
            .register(WorkerRegistrationV1::from_identity(second_identity, limits))
            .unwrap();

        let (tx, _rx) = mpsc::sync_channel(1);
        state.lock().unwrap().pending.push_back(QueuedWork {
            queue_id: 1,
            unit_id: B256::repeat_byte(0x91),
            body: vec![1, 2, 3],
            delivery_attempt: 1,
            completion: tx,
        });
        let first_lease = state
            .claim(WorkerClaimV1 {
                worker_id: first.worker_id,
            })
            .unwrap()
            .unwrap();
        assert!(matches!(
            state.heartbeat(WorkerLeaseRefV1 {
                worker_id: first_lease.worker_id.clone(),
                lease_id: first_lease.lease_id.clone(),
                unit_id: first_lease.unit_id.clone(),
            }),
            Err(WorkerApiErrorV1::LeaseNotAccepted)
        ));
        state
            .accepted(WorkerLeaseRefV1 {
                worker_id: first_lease.worker_id.clone(),
                lease_id: first_lease.lease_id.clone(),
                unit_id: first_lease.unit_id.clone(),
            })
            .unwrap();
        std::thread::sleep(Duration::from_millis(30));
        let second_lease = state
            .claim(WorkerClaimV1 {
                worker_id: second.worker_id,
            })
            .unwrap()
            .unwrap();
        assert_eq!(second_lease.delivery_attempt, 2);
        assert_ne!(first_lease.lease_id, second_lease.lease_id);
        assert!(matches!(
            state.complete(WorkerCompletionV1 {
                worker_id: first_lease.worker_id,
                lease_id: first_lease.lease_id,
                unit_id: first_lease.unit_id,
                finished_body_hex: hex::encode(
                    UnitFinishedV1 {
                        unit_id: B256::repeat_byte(0x91),
                        status: outbe_ocomp_protocol::UnitFinishedStatus::Failed,
                        exact_staged_bytes: 0,
                        transport_digest: B256::ZERO,
                    }
                    .encode_body(&limits)
                    .unwrap(),
                ),
            }),
            Err(WorkerApiErrorV1::StaleLease)
        ));
        state
            .accepted(WorkerLeaseRefV1 {
                worker_id: second_lease.worker_id.clone(),
                lease_id: second_lease.lease_id.clone(),
                unit_id: second_lease.unit_id.clone(),
            })
            .unwrap();
        let completion = WorkerCompletionV1 {
            worker_id: second_lease.worker_id,
            lease_id: second_lease.lease_id,
            unit_id: second_lease.unit_id,
            finished_body_hex: hex::encode(
                UnitFinishedV1 {
                    unit_id: B256::repeat_byte(0x91),
                    status: outbe_ocomp_protocol::UnitFinishedStatus::Failed,
                    exact_staged_bytes: 0,
                    transport_digest: B256::ZERO,
                }
                .encode_body(&limits)
                .unwrap(),
            ),
        };
        state.complete(completion.clone()).unwrap();
        state.complete(completion.clone()).unwrap();
        let mut conflicting = completion;
        conflicting.finished_body_hex = hex::encode(
            UnitFinishedV1 {
                unit_id: B256::repeat_byte(0x91),
                status: outbe_ocomp_protocol::UnitFinishedStatus::Success,
                exact_staged_bytes: 1,
                transport_digest: B256::repeat_byte(0x92),
            }
            .encode_body(&limits)
            .unwrap(),
        );
        assert!(matches!(
            state.complete(conflicting),
            Err(WorkerApiErrorV1::ConflictingCompletion)
        ));
    }

    #[test]
    fn stalled_http_connection_does_not_block_health_or_status() {
        let limits = crate::control::poc_schema_limits();
        let server = SupervisorWorkerHttpServerV1::start(
            "127.0.0.1:0".parse().unwrap(),
            identity(),
            7,
            limits,
        )
        .expect("start Salvo worker server");
        let mut stalled = std::net::TcpStream::connect(server.address())
            .expect("open deliberately incomplete HTTP connection");
        stalled
            .write_all(b"POST /v1/workers/register HTTP/1.1\r\nHost: localhost\r\nContent-Length: 4096\r\n")
            .expect("write incomplete HTTP headers");

        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let health_response = client
            .get(format!("http://{}/health", server.address()))
            .send()
            .expect("health remains independently serviceable");
        assert_eq!(health_response.status(), reqwest::StatusCode::OK);
        assert!(health_response.headers().contains_key("x-request-id"));
        let status: WorkerApiStatusV1 = client
            .get(format!("http://{}/v1/status", server.address()))
            .send()
            .expect("status remains independently serviceable")
            .json()
            .expect("decode status");
        assert_eq!(status.registered_workers, 0);
        assert_eq!(status.max_workers, 4);
    }
}
