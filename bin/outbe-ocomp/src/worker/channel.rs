use super::execute_claimed_unit;
use super::WorkerConfig;
use super::WorkerError;

use crate::control::poc_schema_limits;

use crate::worker_observability::WorkerObservabilityServerV1;
use crate::worker_transport::SupervisorCommandV1;
use crate::worker_transport::WorkLeaseV1;
use crate::worker_transport::WorkerCompletionV1;
use crate::worker_transport::WorkerEventV1;
use crate::worker_transport::WorkerLeaseRefV1;
use crate::worker_transport::WorkerRegistrationResponseV1;
use crate::worker_transport::WorkerRegistrationV1;
use alloy_primitives::B256;

use outbe_ocomp_protocol::RunUnitV1;
use outbe_ocomp_protocol::SchemaLimits;
use outbe_ocomp_protocol::UnitFinishedStatus;
use outbe_ocomp_protocol::UnitFinishedV1;

use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use zeromq::util::PeerIdentity;
use zeromq::DealerSocket;
use zeromq::Socket;
use zeromq::SocketOptions;
use zeromq::SocketRecv;
use zeromq::SocketSend;
use zeromq::ZmqMessage;

pub(super) const SUPERVISOR_RECONNECT_DELAY: Duration = Duration::from_secs(1);

pub(super) const WORKER_HTTP_TIMEOUT: Duration = Duration::from_secs(10);

const WORKER_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);

pub(super) fn run_registered_message_loop(
    config: &WorkerConfig,
    client: &reqwest::blocking::Client,
    observability: &WorkerObservabilityServerV1,
) -> Result<(), WorkerError> {
    let limits = poc_schema_limits();
    let base_url = format!("http://{}", config.supervisor_address);
    let registration: WorkerRegistrationResponseV1 = decode_success(
        client
            .post(format!("{base_url}/v1/workers/register"))
            .json(&WorkerRegistrationV1::from_identity(
                config.identity,
                limits,
            ))
            .send()?,
    )?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| WorkerError::MessageTransport(error.to_string()))?;
    runtime.block_on(run_registered_message_channel(
        config.clone(),
        registration,
        limits,
        observability,
    ))
}

struct ActiveWorkerLease {
    reference: WorkerLeaseRefV1,
    cancelled: Arc<AtomicBool>,
    cancel_on_drop: bool,
}

impl Drop for ActiveWorkerLease {
    fn drop(&mut self) {
        if self.cancel_on_drop {
            self.cancelled.store(true, Ordering::Release);
        }
    }
}

struct WorkerExecutionResult {
    reference: WorkerLeaseRefV1,
    finished: UnitFinishedV1,
    cancelled: Arc<AtomicBool>,
}

async fn run_registered_message_channel(
    config: WorkerConfig,
    registration: WorkerRegistrationResponseV1,
    limits: SchemaLimits,
    observability: &WorkerObservabilityServerV1,
) -> Result<(), WorkerError> {
    let peer: PeerIdentity = registration
        .worker_id
        .parse()
        .map_err(|error: zeromq::ZmqError| WorkerError::MessageTransport(error.to_string()))?;
    let mut options = SocketOptions::default();
    options.peer_identity(peer);
    let mut socket = DealerSocket::with_options(options);
    socket
        .connect(&registration.message_endpoint)
        .await
        .map_err(|error| WorkerError::MessageTransport(error.to_string()))?;
    let (mut sender, mut receiver) = socket.split();
    send_worker_event(
        &mut sender,
        &WorkerEventV1::Ready {
            worker_id: registration.worker_id.clone(),
            registry_generation: registration.registry_generation,
        },
    )
    .await?;
    observability.idle(
        registration.worker_id.clone(),
        registration.registry_generation,
    );

    let (result_tx, mut result_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut active: Option<ActiveWorkerLease> = None;
    let mut heartbeat = tokio::time::interval(WORKER_HEARTBEAT_INTERVAL);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            command = receiver.recv() => {
                let command = command
                    .map_err(|error| WorkerError::MessageTransport(error.to_string()))
                    .and_then(decode_supervisor_command)?;
                observability.touch();
                match command {
                    SupervisorCommandV1::Work { lease, registry_generation } => {
                        require_registration_binding(&registration, &lease.worker_id, registry_generation)?;
                        let reference = lease_reference(&lease);
                        if let Some(current) = active.as_ref() {
                            if current.reference.lease_id == reference.lease_id
                                && current.reference.unit_id == reference.unit_id
                            {
                                send_worker_event(
                                    &mut sender,
                                    &WorkerEventV1::Accepted {
                                        lease: reference,
                                        registry_generation,
                                    },
                                ).await?;
                            }
                            continue;
                        }
                        let body = hex::decode(&lease.run_unit_body_hex)
                            .map_err(|_| WorkerError::UnitBindingMismatch)?;
                        let request = RunUnitV1::decode_body(&body, &limits)?;
                        let unit_id = lease.unit_id.parse::<B256>()
                            .map_err(|_| WorkerError::UnitBindingMismatch)?;
                        send_worker_event(
                            &mut sender,
                            &WorkerEventV1::Accepted {
                                lease: reference.clone(),
                                registry_generation,
                            },
                        ).await?;
                        observability.working(
                            registration.worker_id.clone(),
                            registry_generation,
                            reference.lease_id.clone(),
                            reference.unit_id.clone(),
                        );
                        let cancelled = Arc::new(AtomicBool::new(false));
                        active = Some(ActiveWorkerLease {
                            reference: reference.clone(),
                            cancelled: Arc::clone(&cancelled),
                            cancel_on_drop: true,
                        });
                        let execution_config = config.clone();
                        let execution_reference = reference;
                        let execution_cancelled = Arc::clone(&cancelled);
                        let execution_tx = result_tx.clone();
                        tokio::task::spawn_blocking(move || {
                            let result = execute_claimed_unit(
                                &execution_config,
                                &request,
                                Some(execution_cancelled.as_ref()),
                            );
                            let finished = terminal_completion(unit_id, &execution_reference.lease_id, result);
                            let _ = execution_tx.send(WorkerExecutionResult {
                                reference: execution_reference,
                                finished,
                                cancelled: execution_cancelled,
                            });
                        });
                    }
                    SupervisorCommandV1::Cancel { lease, registry_generation } => {
                        require_registration_binding(&registration, &lease.worker_id, registry_generation)?;
                        if let Some(current) = active.as_ref() {
                            if current.reference.lease_id == lease.lease_id
                                && current.reference.unit_id == lease.unit_id
                            {
                                current.cancelled.store(true, Ordering::Release);
                                observability.cancelling();
                            }
                        }
                    }
                }
            }
            Some(result) = result_rx.recv() => {
                let is_current = active.as_ref().is_some_and(|current| {
                    current.reference.lease_id == result.reference.lease_id
                        && current.reference.unit_id == result.reference.unit_id
                });
                if !is_current {
                    continue;
                }
                if let Some(mut current) = active.take() {
                    current.cancel_on_drop = false;
                }
                if result.cancelled.load(Ordering::Acquire) {
                    send_worker_event(
                        &mut sender,
                        &WorkerEventV1::Ready {
                            worker_id: registration.worker_id.clone(),
                            registry_generation: registration.registry_generation,
                        },
                    ).await?;
                    observability.cancelled();
                    observability.idle(
                        registration.worker_id.clone(),
                        registration.registry_generation,
                    );
                    continue;
                }
                let finished_status = result.finished.status;
                let finished_body = result.finished.encode_body(&limits)?;
                send_worker_event(
                    &mut sender,
                    &WorkerEventV1::Completed {
                        completion: WorkerCompletionV1 {
                            worker_id: result.reference.worker_id,
                            lease_id: result.reference.lease_id,
                            unit_id: result.reference.unit_id,
                            finished_body_hex: hex::encode(finished_body),
                        },
                        registry_generation: registration.registry_generation,
                    },
                ).await?;
                observability.completed(finished_status);
                observability.idle(
                    registration.worker_id.clone(),
                    registration.registry_generation,
                );
            }
            _ = heartbeat.tick() => {
                let event = match active.as_ref() {
                    Some(current) => WorkerEventV1::Heartbeat {
                        lease: current.reference.clone(),
                        registry_generation: registration.registry_generation,
                    },
                    None => WorkerEventV1::Ready {
                        worker_id: registration.worker_id.clone(),
                        registry_generation: registration.registry_generation,
                    },
                };
                send_worker_event(&mut sender, &event).await?;
                observability.touch();
            }
        }
    }
}

fn failed_completion(unit_id: B256) -> UnitFinishedV1 {
    UnitFinishedV1 {
        unit_id,
        status: UnitFinishedStatus::Failed,
        exact_staged_bytes: 0,
        transport_digest: B256::ZERO,
    }
}

pub(super) fn terminal_completion(
    unit_id: B256,
    lease_id: &str,
    result: Result<UnitFinishedV1, WorkerError>,
) -> UnitFinishedV1 {
    match result {
        Ok(finished) => finished,
        Err(error) => {
            eprintln!(
                "OCOMP worker reports terminal failure for accepted lease {lease_id}: {error}"
            );
            failed_completion(unit_id)
        }
    }
}

fn lease_reference(lease: &WorkLeaseV1) -> WorkerLeaseRefV1 {
    WorkerLeaseRefV1 {
        worker_id: lease.worker_id.clone(),
        lease_id: lease.lease_id.clone(),
        unit_id: lease.unit_id.clone(),
    }
}

fn require_registration_binding(
    registration: &WorkerRegistrationResponseV1,
    worker_id: &str,
    registry_generation: u64,
) -> Result<(), WorkerError> {
    if worker_id == registration.worker_id
        && registry_generation == registration.registry_generation
    {
        Ok(())
    } else {
        Err(WorkerError::UnitBindingMismatch)
    }
}

fn decode_supervisor_command(message: ZmqMessage) -> Result<SupervisorCommandV1, WorkerError> {
    let frames = message.into_vec();
    if frames.len() != 1 {
        return Err(WorkerError::MessageTransport(
            "Supervisor ZeroMQ command must contain exactly one body frame".into(),
        ));
    }
    serde_json::from_slice(&frames[0]).map_err(|_| WorkerError::UnitBindingMismatch)
}

async fn send_worker_event(
    sender: &mut zeromq::DealerSendHalf,
    event: &WorkerEventV1,
) -> Result<(), WorkerError> {
    let body = serde_json::to_vec(event).map_err(|_| WorkerError::UnitBindingMismatch)?;
    sender
        .send(body.into())
        .await
        .map_err(|error| WorkerError::MessageTransport(error.to_string()))
}

fn decode_success<T: serde::de::DeserializeOwned>(
    response: reqwest::blocking::Response,
) -> Result<T, WorkerError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response.json()?);
    }
    Err(WorkerError::SupervisorHttp {
        status: status.as_u16(),
        body: response.text().unwrap_or_default(),
    })
}
