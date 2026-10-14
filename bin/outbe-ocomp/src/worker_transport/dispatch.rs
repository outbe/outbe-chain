use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, MutexGuard};
use std::time::Instant;

use alloy_primitives::B256;
use outbe_ocomp_protocol::{RunUnitV1, UnitFinishedV1};

use super::state::{ActiveLease, QueuedWork, WorkerApiState, WorkerRegistry};
use super::{
    SupervisorCommandV1, WorkerApiErrorV1, WorkerLeaseRefV1, DISPATCH_CANCEL_POLL,
    DISPATCH_WAIT_REPORT_INTERVAL,
};

impl WorkerApiState {
    pub(super) fn dispatch(
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
        let mut registry = self.wait_for_connected_worker(cancelled)?;
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
        self.wake.notify_one();
        self.wait_for_dispatch_completion((queue_id, unit_id), completion_rx, cancelled)
    }

    fn wait_for_connected_worker(
        &self,
        cancelled: Option<&AtomicBool>,
    ) -> Result<MutexGuard<'_, WorkerRegistry>, WorkerApiErrorV1> {
        let deadline = Instant::now() + self.durations.registration;
        let mut registry = self.lock()?;
        loop {
            if cancelled.is_some_and(|cancelled| cancelled.load(Ordering::Acquire)) {
                return Err(WorkerApiErrorV1::DispatchCancelled);
            }
            self.cleanup_expired(&mut registry, Instant::now());
            if registry.workers.values().any(|worker| worker.connected) {
                return Ok(registry);
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
    }

    fn wait_for_dispatch_completion(
        &self,
        (queue_id, unit_id): (u64, B256),
        completion_rx: mpsc::Receiver<UnitFinishedV1>,
        cancelled: Option<&AtomicBool>,
    ) -> Result<UnitFinishedV1, WorkerApiErrorV1> {
        let result_deadline = Instant::now() + self.durations.result;
        let waiting_since = Instant::now();
        let mut last_report = Instant::now();
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
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if last_report.elapsed() >= DISPATCH_WAIT_REPORT_INTERVAL {
                        last_report = Instant::now();
                        self.report_dispatch_wait(queue_id, unit_id, waiting_since);
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.cancel(queue_id)?;
                    return Err(WorkerApiErrorV1::DispatchCancelled);
                }
            }
        }
    }

    fn report_dispatch_wait(&self, queue_id: u64, unit_id: B256, waiting_since: Instant) {
        let Ok(registry) = self.lock() else {
            return;
        };
        let queued = registry
            .pending
            .iter()
            .any(|work| work.queue_id == queue_id);
        let connected = registry
            .workers
            .values()
            .filter(|worker| worker.connected)
            .count();
        let leased = registry.workers.values().any(|worker| {
            worker
                .active
                .as_ref()
                .is_some_and(|lease| lease.work.queue_id == queue_id)
        });
        drop(registry);
        eprintln!(
            "OCOMP supervisor still waiting for a worker result: unit={unit_id:#x} \
queue_id={queue_id} queued={queued} leased={leased} connected_workers={connected} \
waited_secs={}",
            waiting_since.elapsed().as_secs()
        );
    }

    fn cancel(&self, queue_id: u64) -> Result<(), WorkerApiErrorV1> {
        let mut registry = self.lock()?;
        registry.pending.retain(|work| work.queue_id != queue_id);
        let mut cancellation = None;
        for (worker_id, worker) in &mut registry.workers {
            if worker
                .active
                .as_ref()
                .is_some_and(|lease| lease.work.queue_id == queue_id)
            {
                if let Some(active) = worker.active.take() {
                    worker.connected = false;
                    cancellation = Some((
                        *worker_id,
                        SupervisorCommandV1::Cancel {
                            lease: WorkerLeaseRefV1 {
                                worker_id: format!("{worker_id:#x}"),
                                lease_id: format!("{:#x}", active.lease_id),
                                unit_id: format!("{:#x}", active.work.unit_id),
                            },
                            registry_generation: self.registry_generation,
                        },
                    ));
                }
                break;
            }
        }
        if let Some(cancellation) = cancellation {
            registry.outbound.push_back(cancellation);
        }
        drop(registry);
        self.wake.notify_one();
        Ok(())
    }

    pub(super) fn drain_commands(&self) -> Result<Vec<(B256, SupervisorCommandV1)>, WorkerApiErrorV1> {
        let now = Instant::now();
        let mut registry = self.lock()?;
        self.cleanup_expired(&mut registry, now);
        let mut commands = registry.outbound.drain(..).collect::<Vec<_>>();
        let available = registry
            .workers
            .iter()
            .filter_map(|(worker_id, worker)| {
                (worker.connected && worker.active.is_none()).then_some(*worker_id)
            })
            .collect::<Vec<_>>();
        for worker_id in available {
            let Some(work) = registry.pending.pop_front() else {
                break;
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
            let lease = self.lease_response(worker_id, &active);
            registry
                .workers
                .get_mut(&worker_id)
                .ok_or(WorkerApiErrorV1::UnknownWorker)?
                .active = Some(active);
            commands.push((
                worker_id,
                SupervisorCommandV1::Work {
                    lease,
                    registry_generation: self.registry_generation,
                },
            ));
        }
        Ok(commands)
    }

    pub(super) fn cleanup_expired(&self, registry: &mut WorkerRegistry, now: Instant) {
        let mut requeue = Vec::new();
        let mut cancellations = Vec::new();
        registry.workers.retain(|worker_id, worker| {
            if worker
                .active
                .as_ref()
                .is_some_and(|active| active.deadline <= now)
            {
                if let Some(mut active) = worker.active.take() {
                    cancellations.push((
                        *worker_id,
                        SupervisorCommandV1::Cancel {
                            lease: WorkerLeaseRefV1 {
                                worker_id: format!("{worker_id:#x}"),
                                lease_id: format!("{:#x}", active.lease_id),
                                unit_id: format!("{:#x}", active.work.unit_id),
                            },
                            registry_generation: self.registry_generation,
                        },
                    ));
                    active.work.delivery_attempt = active.work.delivery_attempt.saturating_add(1);
                    requeue.push(active.work);
                    worker.connected = false;
                }
            }
            worker.active.is_some() || now.duration_since(worker.last_seen) <= self.durations.idle
        });
        registry.pending.extend(requeue);
        registry.outbound.extend(cancellations);
    }
}
