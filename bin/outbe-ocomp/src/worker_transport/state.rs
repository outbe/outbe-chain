use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use alloy_primitives::{keccak256, B256};
use outbe_ocomp_protocol::{SchemaLimits, UnitFinishedV1};

use super::{
    duration_ms, parse_b256, SupervisorCommandV1, WorkLeaseV1, WorkerApiErrorV1, WorkerApiStatusV1,
    WorkerCompletionV1, WorkerEventV1, WorkerLeaseRefV1, WorkerRegistrationResponseV1,
    WorkerRegistrationV1, MAX_REGISTERED_WORKERS, WORKER_IDLE_TIMEOUT,
    WORKER_REGISTRATION_TIMEOUT, WORK_LEASE_TIMEOUT, WORK_RESULT_TIMEOUT,
};
use crate::control::EndpointIdentity;

#[derive(Clone, Copy)]
pub(super) struct WorkerApiDurations {
    pub(super) registration: Duration,
    pub(super) lease: Duration,
    pub(super) idle: Duration,
    pub(super) result: Duration,
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

pub(super) struct WorkerApiState {
    pub(super) identity: EndpointIdentity,
    pub(super) registry_generation: u64,
    pub(super) limits: SchemaLimits,
    pub(super) durations: WorkerApiDurations,
    pub(super) message_endpoint: String,
    pub(super) next_id: AtomicU64,
    pub(super) inner: Mutex<WorkerRegistry>,
    pub(super) worker_registered: Condvar,
    pub(super) wake: tokio::sync::Notify,
}

#[derive(Default)]
pub(super) struct WorkerRegistry {
    pub(super) workers: BTreeMap<B256, WorkerRecord>,
    pub(super) pending: VecDeque<QueuedWork>,
    pub(super) outbound: VecDeque<(B256, SupervisorCommandV1)>,
}

pub(super) struct WorkerRecord {
    pub(super) process_nonce: B256,
    pub(super) last_seen: Instant,
    pub(super) connected: bool,
    pub(super) active: Option<ActiveLease>,
    pub(super) last_completed: Option<CompletedLease>,
}

pub(super) struct CompletedLease {
    pub(super) lease_id: B256,
    pub(super) unit_id: B256,
    pub(super) body_digest: B256,
}

pub(super) struct QueuedWork {
    pub(super) queue_id: u64,
    pub(super) unit_id: B256,
    pub(super) body: Vec<u8>,
    pub(super) delivery_attempt: u32,
    pub(super) completion: mpsc::SyncSender<UnitFinishedV1>,
}

pub(super) struct ActiveLease {
    pub(super) lease_id: B256,
    pub(super) deadline: Instant,
    pub(super) accepted: bool,
    pub(super) work: QueuedWork,
}

impl WorkerApiState {
    pub(super) fn new(
        identity: EndpointIdentity,
        registry_generation: u64,
        limits: SchemaLimits,
        durations: WorkerApiDurations,
        message_endpoint: String,
    ) -> Self {
        Self {
            identity,
            registry_generation,
            limits,
            durations,
            message_endpoint,
            next_id: AtomicU64::new(1),
            inner: Mutex::new(WorkerRegistry::default()),
            worker_registered: Condvar::new(),
            wake: tokio::sync::Notify::new(),
        }
    }

    pub(super) fn lock(&self) -> Result<MutexGuard<'_, WorkerRegistry>, WorkerApiErrorV1> {
        self.inner.lock().map_err(|_| WorkerApiErrorV1::Poisoned)
    }

    pub(super) fn register(
        &self,
        request: WorkerRegistrationV1,
    ) -> Result<WorkerRegistrationResponseV1, WorkerApiErrorV1> {
        let genesis_hash = parse_b256(&request.genesis_hash)?;
        let process_nonce = parse_b256(&request.process_nonce)?;
        let protocol_bundle_hash = parse_b256(&request.protocol_bundle_hash)?;
        let actual_identity = (
            !process_nonce.is_zero(),
            request.chain_id,
            genesis_hash,
            protocol_bundle_hash,
            request.max_control_body_bytes,
        );
        let expected_identity = (
            true,
            self.identity.chain_id,
            self.identity.genesis_hash,
            self.identity.protocol_bundle_hash,
            self.limits.max_control_body_bytes,
        );
        if actual_identity != expected_identity {
            return Err(WorkerApiErrorV1::IdentityMismatch);
        }
        let now = Instant::now();
        let mut registry = self.lock()?;
        self.cleanup_expired(&mut registry, now);
        if let Some(worker_id) = registry.workers.iter().find_map(|(worker_id, worker)| {
            (worker.process_nonce == process_nonce).then_some(*worker_id)
        }) {
            let requeue = registry
                .workers
                .get_mut(&worker_id)
                .and_then(|worker| {
                    worker.last_seen = now;
                    worker.connected = false;
                    worker.active.take()
                })
                .map(|mut active| {
                    active.work.delivery_attempt = active.work.delivery_attempt.saturating_add(1);
                    active.work
                });
            if let Some(work) = requeue {
                registry.pending.push_back(work);
            }
            self.wake.notify_one();
            return Ok(self.registration_response(worker_id));
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
                connected: false,
                active: None,
                last_completed: None,
            },
        );
        Ok(self.registration_response(worker_id))
    }

    pub(super) fn registration_response(&self, worker_id: B256) -> WorkerRegistrationResponseV1 {
        WorkerRegistrationResponseV1 {
            worker_id: format!("{worker_id:#x}"),
            registry_generation: self.registry_generation,
            lease_timeout_ms: duration_ms(self.durations.lease),
            message_endpoint: self.message_endpoint.clone(),
        }
    }

    pub(super) fn ready(&self, route: B256, worker_id: &str) -> Result<(), WorkerApiErrorV1> {
        let worker_id = parse_b256(worker_id)?;
        if worker_id != route {
            return Err(WorkerApiErrorV1::IdentityMismatch);
        }
        let mut registry = self.lock()?;
        let worker = registry
            .workers
            .get_mut(&worker_id)
            .ok_or(WorkerApiErrorV1::UnknownWorker)?;
        worker.connected = true;
        worker.last_seen = Instant::now();
        drop(registry);
        self.worker_registered.notify_all();
        self.wake.notify_one();
        Ok(())
    }

    pub(super) fn handle_event(
        &self,
        route: B256,
        event: WorkerEventV1,
    ) -> Result<(), WorkerApiErrorV1> {
        let result = match event {
            WorkerEventV1::Ready {
                worker_id,
                registry_generation,
            } => {
                self.require_generation(registry_generation)?;
                self.ready(route, &worker_id)
            }
            WorkerEventV1::Accepted {
                lease,
                registry_generation,
            } => {
                self.require_generation(registry_generation)?;
                self.require_route(route, &lease.worker_id)?;
                self.accepted(lease)
            }
            WorkerEventV1::Heartbeat {
                lease,
                registry_generation,
            } => {
                self.require_generation(registry_generation)?;
                self.require_route(route, &lease.worker_id)?;
                self.heartbeat(lease)
            }
            WorkerEventV1::Completed {
                completion,
                registry_generation,
            } => {
                self.require_generation(registry_generation)?;
                self.require_route(route, &completion.worker_id)?;
                self.complete(completion)
            }
        };
        self.wake.notify_one();
        result
    }

    fn require_generation(&self, generation: u64) -> Result<(), WorkerApiErrorV1> {
        if generation == self.registry_generation {
            Ok(())
        } else {
            Err(WorkerApiErrorV1::StaleGeneration)
        }
    }

    fn require_route(&self, route: B256, worker_id: &str) -> Result<(), WorkerApiErrorV1> {
        if parse_b256(worker_id)? == route {
            Ok(())
        } else {
            Err(WorkerApiErrorV1::IdentityMismatch)
        }
    }

    pub(super) fn accepted(&self, request: WorkerLeaseRefV1) -> Result<(), WorkerApiErrorV1> {
        self.touch_lease(request, true)
    }

    pub(super) fn heartbeat(&self, request: WorkerLeaseRefV1) -> Result<(), WorkerApiErrorV1> {
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

    pub(super) fn complete(&self, request: WorkerCompletionV1) -> Result<(), WorkerApiErrorV1> {
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

    pub(super) fn status(&self) -> Result<WorkerApiStatusV1, WorkerApiErrorV1> {
        let mut registry = self.lock()?;
        self.cleanup_expired(&mut registry, Instant::now());
        Ok(WorkerApiStatusV1 {
            registry_generation: self.registry_generation,
            registered_workers: registry.workers.len(),
            connected_workers: registry
                .workers
                .values()
                .filter(|worker| worker.connected)
                .count(),
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

    pub(super) fn lease_response(&self, worker_id: B256, active: &ActiveLease) -> WorkLeaseV1 {
        WorkLeaseV1 {
            worker_id: format!("{worker_id:#x}"),
            lease_id: format!("{:#x}", active.lease_id),
            unit_id: format!("{:#x}", active.work.unit_id),
            delivery_attempt: active.work.delivery_attempt,
            lease_timeout_ms: duration_ms(self.durations.lease),
            run_unit_body_hex: hex::encode(&active.work.body),
        }
    }

    pub(super) fn derive_id(&self, domain: &[u8], material: &[u8]) -> B256 {
        let counter = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut bytes = Vec::with_capacity(domain.len() + material.len() + 16);
        bytes.extend_from_slice(domain);
        bytes.extend_from_slice(&self.registry_generation.to_be_bytes());
        bytes.extend_from_slice(&counter.to_be_bytes());
        bytes.extend_from_slice(material);
        keccak256(bytes)
    }
}
