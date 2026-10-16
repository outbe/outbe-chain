//! Typed error definitions and constructors for `outbe-ocomp`.

use alloy_primitives::B256;
use thiserror::Error;

pub use crate::{
    embedded_runtime::EmbeddedOcompRuntimeErrorV1,
    input_artifacts::InputArtifactError,
    opening_stage::OpeningStageError,
    public_rpc::PublicRpcError,
    result_attestation::LocalResultAttestationErrorV1,
    rpc_input_exporter::RpcInputExporterErrorV1,
    sign_once::SignOnceError,
    supervisor_export::SupervisorExportAdoptionError,
    supervisor_job::SupervisorJobRunnerErrorV1,
    vote_submitter::{LocalVotePreparationErrorV1, PublicVoteRpcErrorV1, VoteSubmissionErrorV1},
    worker::WorkerError,
    worker_observability::WorkerObservabilityErrorV1,
    worker_transport::WorkerApiErrorV1,
};

#[derive(Debug, Error)]
pub enum OcompCliError {
    #[error("OUTBE_OCOMP_BASE_PATH is required")]
    MissingBasePath,
    #[error("OCOMP_VALIDATOR_INDEX is required")]
    MissingValidatorIndex,
    #[error("OCOMP_VALIDATOR_INDEX must be valid UTF-8")]
    InvalidValidatorIndexUtf8,
    #[error("OCOMP_VALIDATOR_INDEX must be an unsigned 16-bit integer")]
    InvalidValidatorIndexInt,
    #[error("OCOMP base path must be a non-root absolute path")]
    InvalidBasePath,
    #[error("--supervisor-address must be a nonzero loopback registration endpoint")]
    InvalidSupervisorAddress,
    #[error("--development-root is unavailable in release outbe-ocomp builds")]
    DevelopmentRootUnavailableInRelease,
    #[error("--development-root must be a non-root absolute path")]
    InvalidDevelopmentRootPath,
    #[error("--development-root must name an existing real directory")]
    InvalidDevelopmentRootDir,
    #[error("OUTBE_OCOMP_TEE_ENDPOINT must be valid UTF-8")]
    InvalidTeeEndpointUtf8,
    #[error("OUTBE_OCOMP_TEE_ENDPOINT and OUTBE_OCOMP_NODE_DATA_DIR must be configured together")]
    IncompleteTeeReaderConfig,
    #[error("worker ordinal exceeds the supported per-Supervisor worker count")]
    WorkerOrdinalExceedsMax,
    #[error("derived worker process nonce is reserved zero")]
    ZeroWorkerProcessNonce,
    #[error("worker observability port offset overflow")]
    WorkerObservabilityPortOffsetOverflow,
    #[error("worker observability port overflow")]
    WorkerObservabilityPortOverflow,
    #[error("snapshot exporter observability port overflow")]
    SnapshotExporterObservabilityPortOverflow,
    #[error("wake snapshot exporter lane: {0}")]
    WakeSnapshotExporterLane(String),
    #[error("discovery offer references unconfigured protocol bundle {0}")]
    UnconfiguredProtocolBundle(B256),
    #[error("snapshot exporter lane {0} exited")]
    SnapshotExporterLaneExited(B256),
    #[error("all snapshot exporter lane workers exited")]
    AllSnapshotExporterLanesExited,
    #[error("discovery pending count overflow")]
    DiscoveryPendingCountOverflow,
    #[error("required environment variable {0} is missing")]
    MissingRequiredEnv(&'static str),
    #[error("OCOMP protocol bundle hash list is not canonical")]
    NonCanonicalBundleHashList,
    #[error("OCOMP protocol bundle hash list must contain one or two non-zero hashes")]
    InvalidBundleHashListCount,
    #[error("OCOMP protocol bundle {0} is not installed")]
    ProtocolBundleNotInstalled(B256),
}

impl OcompCliError {
    pub fn wake_snapshot_exporter_lane(error: impl std::fmt::Display) -> Self {
        Self::WakeSnapshotExporterLane(error.to_string())
    }
}

#[derive(Debug, Error)]
pub enum EmbeddedComputeAttemptError {
    #[error(transparent)]
    Adoption(#[from] SupervisorExportAdoptionError),
    #[error(transparent)]
    Runner(#[from] SupervisorJobRunnerErrorV1),
    #[error("{0}")]
    MismatchOverride(String),
}

impl EmbeddedComputeAttemptError {
    pub fn mismatch_override(error: impl std::fmt::Display) -> Self {
        Self::MismatchOverride(error.to_string())
    }
}

pub fn transport_enclave_error(error: impl std::fmt::Display) -> outbe_tee::TransportError {
    outbe_tee::TransportError::EnclaveError(error.to_string())
}

impl PublicRpcError {
    pub fn transport(error: impl std::fmt::Display) -> Self {
        Self::Transport(error.to_string())
    }

    pub fn malformed(method: &'static str, detail: impl std::fmt::Display) -> Self {
        Self::Malformed {
            method,
            detail: detail.to_string(),
        }
    }
}

impl OpeningStageError {
    pub fn verification(error: impl std::fmt::Display) -> Self {
        Self::Verification(error.to_string())
    }

    pub fn resolver(error: impl std::fmt::Display) -> Self {
        Self::Resolver(error.to_string())
    }
}

impl InputArtifactError {
    pub fn opening_source(error: impl std::fmt::Display) -> Self {
        Self::OpeningSource(error.to_string())
    }
}

impl SignOnceError {
    pub fn signing_failed(error: impl std::fmt::Display) -> Self {
        Self::SigningFailed(error.to_string())
    }
}

impl SupervisorExportAdoptionError {
    pub fn stage(stage: &'static str, error: impl std::fmt::Display) -> Self {
        Self::Stage {
            stage,
            detail: error.to_string(),
        }
    }
}

impl VoteSubmissionErrorV1 {
    pub fn invalid_prepared_transaction(error: impl std::fmt::Display) -> Self {
        Self::InvalidPreparedTransaction(error.to_string())
    }
}

impl PublicVoteRpcErrorV1 {
    pub fn transport(error: impl std::fmt::Display) -> Self {
        Self::Transport(error.to_string())
    }

    pub fn malformed(error: impl std::fmt::Display) -> Self {
        Self::Malformed(error.to_string())
    }
}

impl WorkerObservabilityErrorV1 {
    pub fn start(error: impl std::fmt::Display) -> Self {
        Self::Start(error.to_string())
    }

    pub fn recorder(error: impl std::fmt::Display) -> Self {
        Self::Recorder(error.to_string())
    }
}

impl WorkerApiErrorV1 {
    pub fn start(error: impl std::fmt::Display) -> Self {
        Self::Start(error.to_string())
    }

    pub fn message_transport(error: impl std::fmt::Display) -> Self {
        Self::MessageTransport(error.to_string())
    }

    pub fn protocol(error: impl std::fmt::Display) -> Self {
        Self::Protocol(error.to_string())
    }
}

impl WorkerError {
    pub fn message_transport(error: impl std::fmt::Display) -> Self {
        Self::MessageTransport(error.to_string())
    }
}
