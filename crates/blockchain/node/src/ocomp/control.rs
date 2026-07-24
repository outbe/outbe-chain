//! Node-owned bounded OCOMP control endpoint.
//!
//! This local service exposes only the one finalized live PoC job retained by
//! [`OcompRetentionCoordinator`]. Connection or protocol failures remain local
//! to OCOMP and are never propagated into consensus/finality.

use std::io;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::Duration;

use metrics::{counter, gauge};
use outbe_ocomp_protocol::local_control::{
    ControlError, ControlRole, ControlServerSession, EndpointIdentity, ServerPolicy,
};
use outbe_ocomp_protocol::{
    common::BoundedBytes, FinalizedJobSpecV1, FinalizedJobSummaryV1, GetJobSpecV1,
    ListFinalizedJobsResponseV1, ListFinalizedJobsV1, LocalErrorCode, LocalErrorV1,
    NodeMessageKind, ProtocolError, SchemaLimits,
};
use thiserror::Error;

use super::retention::{FinalizedJobPinV1, OcompRetentionCoordinator, RetentionError};

#[derive(Default)]
struct ReadinessState {
    ready: AtomicBool,
    compatible_sessions: AtomicU64,
    incompatible_sessions: AtomicU64,
    failed_sessions: AtomicU64,
}

#[derive(Clone, Default)]
pub struct OcompControlReadiness {
    state: Arc<ReadinessState>,
}

impl OcompControlReadiness {
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.state.ready.load(Ordering::Acquire)
    }

    #[must_use]
    pub fn compatible_sessions(&self) -> u64 {
        self.state.compatible_sessions.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn incompatible_sessions(&self) -> u64 {
        self.state.incompatible_sessions.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn failed_sessions(&self) -> u64 {
        self.state.failed_sessions.load(Ordering::Relaxed)
    }

    fn compatible(&self) {
        self.state.ready.store(true, Ordering::Release);
        self.state
            .compatible_sessions
            .fetch_add(1, Ordering::Relaxed);
        gauge!("outbe_ocomp_ready").set(1.0);
        counter!("outbe_ocomp_control_sessions_total", "result" => "compatible").increment(1);
    }

    fn incompatible(&self) {
        self.state.ready.store(false, Ordering::Release);
        self.state
            .incompatible_sessions
            .fetch_add(1, Ordering::Relaxed);
        gauge!("outbe_ocomp_ready").set(0.0);
        counter!("outbe_ocomp_control_sessions_total", "result" => "incompatible_bundle")
            .increment(1);
    }

    fn failed(&self) {
        self.state.failed_sessions.fetch_add(1, Ordering::Relaxed);
        counter!("outbe_ocomp_control_sessions_total", "result" => "failed").increment(1);
    }
}

pub struct OcompControlServer {
    retention: Arc<OcompRetentionCoordinator>,
    expected_supervisor_uid: u32,
    identity: EndpointIdentity,
    session_generation: u64,
    limits: SchemaLimits,
    readiness: OcompControlReadiness,
}

impl OcompControlServer {
    pub fn new(
        retention: Arc<OcompRetentionCoordinator>,
        expected_supervisor_uid: u32,
        identity: EndpointIdentity,
        session_generation: u64,
        limits: SchemaLimits,
    ) -> Result<Self, NodeControlError> {
        if session_generation == 0 {
            return Err(NodeControlError::ZeroSessionGeneration);
        }
        Ok(Self {
            retention,
            expected_supervisor_uid,
            identity,
            session_generation,
            limits,
            readiness: OcompControlReadiness::default(),
        })
    }

    #[must_use]
    pub fn readiness(&self) -> OcompControlReadiness {
        self.readiness.clone()
    }

    /// Runs a node-local listener until the owner requests shutdown. Individual
    /// connection failures are contained and counted; only listener failure is
    /// returned to the owning background task.
    pub fn serve_until(&self, listener: UnixListener, shutdown: &AtomicBool) -> io::Result<()> {
        listener.set_nonblocking(true)?;
        while !shutdown.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((stream, _)) => {
                    if let Err(error) = self.serve_connection(stream) {
                        if !matches!(
                            error,
                            NodeControlError::Control(ControlError::NoCommonBundle)
                        ) {
                            self.readiness.failed();
                        }
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    pub fn serve_connection(&self, stream: UnixStream) -> Result<(), NodeControlError> {
        let mut session = ControlServerSession::accept(
            stream,
            ServerPolicy::node(
                ControlRole::Supervisor,
                self.expected_supervisor_uid,
                self.identity,
                self.session_generation,
                self.limits,
            ),
        )?;
        match session.handshake() {
            Ok(_) => self.readiness.compatible(),
            Err(ControlError::NoCommonBundle) => {
                self.readiness.incompatible();
                return Err(NodeControlError::Control(ControlError::NoCommonBundle));
            }
            Err(error) => return Err(error.into()),
        }

        loop {
            let frame = match session.receive_request() {
                Ok(frame) => frame,
                Err(ControlError::ConnectionClosed) => return Ok(()),
                Err(error) => return Err(error.into()),
            };
            let response = match frame.message_kind {
                kind if kind == NodeMessageKind::ListFinalizedJobs as u16 => {
                    let request = ListFinalizedJobsV1::decode_body(&frame.body, &self.limits)?;
                    self.list_finalized_jobs(&request)?
                        .encode_body(&self.limits)?
                }
                kind if kind == NodeMessageKind::GetJobSpec as u16 => {
                    let request = GetJobSpecV1::decode_body(&frame.body, &self.limits)?;
                    match self.get_job_spec(request.job_id) {
                        Ok(spec) => spec.encode_body(&self.limits)?,
                        Err(NodeControlError::JobNotFound) => {
                            let error = LocalErrorV1 {
                                rejected_kind: frame.message_kind,
                                error_code: LocalErrorCode::NotFound as u16,
                                retryable: false,
                            };
                            session.send_response(
                                frame.request_id,
                                NodeMessageKind::Error as u16,
                                error.encode_body(&self.limits)?,
                            )?;
                            continue;
                        }
                        Err(error) => return Err(error),
                    }
                }
                actual => return Err(NodeControlError::UnexpectedMethod(actual)),
            };
            session.send_response(frame.request_id, NodeMessageKind::Response as u16, response)?;
        }
    }

    fn list_finalized_jobs(
        &self,
        request: &ListFinalizedJobsV1,
    ) -> Result<ListFinalizedJobsResponseV1, NodeControlError> {
        let Some(pin) = self.retention.finalized_live_job()? else {
            return Ok(ListFinalizedJobsResponseV1 {
                next_cursor: request.after_cursor,
                jobs: Vec::new(),
            });
        };
        let summary = summary(pin);
        if summary.cursor <= request.after_cursor {
            return Ok(ListFinalizedJobsResponseV1 {
                next_cursor: request.after_cursor,
                jobs: Vec::new(),
            });
        }
        Ok(ListFinalizedJobsResponseV1 {
            next_cursor: summary.cursor,
            jobs: vec![summary],
        })
    }

    fn get_job_spec(
        &self,
        job_id: alloy_primitives::B256,
    ) -> Result<FinalizedJobSpecV1, NodeControlError> {
        let pin = self
            .retention
            .finalized_live_job()?
            .filter(|pin| pin.job_id == job_id)
            .ok_or(NodeControlError::JobNotFound)?;
        let proof = self.retention.build_finalized_intent_proof(job_id)?;
        let intent = proof.decoded_intent(&self.limits)?;
        let canonical_job_intent = BoundedBytes(intent.encode_canonical(&self.limits)?);
        Ok(FinalizedJobSpecV1 {
            summary: summary(pin),
            canonical_job_intent,
        })
    }
}

fn summary(pin: FinalizedJobPinV1) -> FinalizedJobSummaryV1 {
    FinalizedJobSummaryV1 {
        cursor: pin.candidate.block_number,
        job_id: pin.job_id,
        intent_id: pin.candidate.intent_id,
        finalized_block_hash: pin.candidate.block_hash,
        finalized_state_root: pin.candidate.state_root,
        protocol_bundle_hash: pin.candidate.protocol_bundle_hash,
    }
}

#[derive(Debug, Error)]
pub enum NodeControlError {
    #[error(transparent)]
    Control(#[from] ControlError),
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error(transparent)]
    Retention(#[from] RetentionError),
    #[error("node OCOMP control received unsupported method {0:#06x}")]
    UnexpectedMethod(u16),
    #[error("requested finalized OCOMP job is not live")]
    JobNotFound,
    #[error("node OCOMP control session generation cannot be zero")]
    ZeroSessionGeneration,
}
