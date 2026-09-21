//! Axum-registered worker driven by an asynchronous TCP ZeroMQ command channel.

use std::net::SocketAddr;
use std::path::PathBuf;

use alloy_primitives::B256;

use outbe_compressed_entities::CanonicalBodyError;

use outbe_lysis::program_v1::artifacts::LysisArtifactErrorV1;

use outbe_lysis::program_v1::planner::PlannerErrorV1;

use outbe_ocomp_protocol::unit::UnitPhase;

use outbe_oracle::OracleOcompError;

use thiserror::Error;

use crate::bundle::PinnedProtocolBundle;
use crate::cas::CasError;
use crate::cas::CasLimits;

use crate::control::ControlError;
use crate::control::EndpointIdentity;

use crate::inbox::WorkerInboxError;
use crate::inbox::WorkerInboxLimits;

use crate::input_artifacts::InputArtifactError;

use crate::lysis_phase_replay::LysisPhaseReplayError;

use crate::worker_observability::{WorkerObservabilityErrorV1, WorkerObservabilityServerV1};

#[derive(Clone, Debug)]
pub struct WorkerConfig {
    pub identity: EndpointIdentity,
    pub supervisor_address: SocketAddr,
    pub observability_address: SocketAddr,
    pub cas_root: PathBuf,
    pub cas_limits: CasLimits,
    pub inbox_root: PathBuf,
    pub inbox_limits: WorkerInboxLimits,
    pub protocol_bundle: PinnedProtocolBundle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkerOutcome {
    pub unit_id: B256,
}

#[derive(Debug, Error)]
pub enum WorkerError {
    #[error(transparent)]
    Control(#[from] ControlError),
    #[error(transparent)]
    Cas(#[from] CasError),
    #[error("worker request is not valid OCOMP protocol: {0}")]
    Protocol(#[from] outbe_ocomp_protocol::ProtocolError),
    #[error("worker Supervisor address {0} is not a nonzero loopback registration endpoint")]
    InvalidSupervisorAddress(SocketAddr),
    #[error("worker HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Supervisor worker HTTP endpoint returned {status}: {body}")]
    SupervisorHttp { status: u16, body: String },
    #[error("worker TCP ZeroMQ transport failed: {0}")]
    MessageTransport(String),
    #[error(transparent)]
    Observability(#[from] WorkerObservabilityErrorV1),
    #[error("Supervisor cancelled or expired the active worker lease")]
    LeaseCancelled,
    #[error("worker request binding does not match its canonical UnitSpecV1")]
    UnitBindingMismatch,
    #[error("worker request carries the reserved zero plan hash")]
    ZeroPlanHash,
    #[error("worker pinned protocol bundle does not match endpoint identity")]
    BundleIdentityMismatch,
    #[error(transparent)]
    InputArtifact(#[from] InputArtifactError),
    #[error(transparent)]
    Inbox(#[from] WorkerInboxError),
    #[error(transparent)]
    LysisArtifact(#[from] LysisArtifactErrorV1),
    #[error(transparent)]
    Planner(#[from] PlannerErrorV1),
    #[error(transparent)]
    CanonicalBody(#[from] CanonicalBodyError),
    #[error(transparent)]
    OracleOpening(#[from] OracleOcompError),
    #[error(transparent)]
    LysisPhaseReplay(#[from] LysisPhaseReplayError),
    #[error("worker does not yet implement Lysis phase {0:?}")]
    UnsupportedPhase(UnitPhase),
}

pub fn run_worker(config: WorkerConfig) -> Result<(), WorkerError> {
    if config.protocol_bundle.hash() != config.identity.protocol_bundle_hash {
        return Err(WorkerError::BundleIdentityMismatch);
    }
    if !config.supervisor_address.ip().is_loopback() || config.supervisor_address.port() == 0 {
        return Err(WorkerError::InvalidSupervisorAddress(
            config.supervisor_address,
        ));
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(WORKER_HTTP_TIMEOUT)
        .build()?;
    let observability = WorkerObservabilityServerV1::start(config.observability_address)?;
    loop {
        observability.registering();
        if let Err(error) = run_registered_message_loop(&config, &client, &observability) {
            observability.disconnected();
            eprintln!("OCOMP worker registration/message channel retry: {error}");
        }
        std::thread::sleep(SUPERVISOR_RECONNECT_DELAY);
    }
}

#[cfg(test)]
mod tests;

mod authority;
pub(crate) use authority::UnitExecutionAuthority;
use authority::{
    exact_unit_output_source, planner_from_authority, require_authenticated_input,
    require_plan_binding, resolve_scan_artifacts, scan_producer_inputs, unit_or_empty_id,
    validate_scan_artifact, ExpectedPlanBindingsV1,
};

mod phases;

use phases::execute_enumerate_unit;

use phases::{execute_fidelity_map_unit, execute_fixed_reduce_unit};

use phases::execute_amount_map_unit;

use phases::{execute_gratis_prefix_down_unit, execute_gratis_prefix_unit};

use phases::execute_output_finalize_unit;

use phases::execute_shuffle_unit;

use phases::execute_root_reduce_unit;
#[cfg(test)]
use phases::{
    require_complete_root_values, require_root_reduce_finalized_binding,
    require_root_reduce_shuffle_population,
};

mod execution;
pub(crate) use execution::execute_unit;
use execution::{execute_claimed_unit, require_lease_active};

mod channel;
use channel::{run_registered_message_loop, SUPERVISOR_RECONNECT_DELAY, WORKER_HTTP_TIMEOUT};

#[cfg(test)]
use channel::terminal_completion;
