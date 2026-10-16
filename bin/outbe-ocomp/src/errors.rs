//! Typed error definitions and constructors for `outbe-ocomp`.

use std::path::PathBuf;

use alloy_primitives::{B256, U256};
use outbe_lysis::program_v1::planner::PlannerErrorV1;
use outbe_ocomp_protocol::ProtocolError;
use outbe_primitives::entity_codec::CanonicalBodyError;
use thiserror::Error;

use crate::{
    admission_catalog::AdmissionCatalogError, cas::CasError,
    input_ref_catalog::InputRefCatalogError,
};
pub use crate::{
    embedded_runtime::EmbeddedOcompRuntimeErrorV1,
    opening_stage::OpeningStageError,
    result_attestation::LocalResultAttestationErrorV1,
    rpc_input_exporter::RpcInputExporterErrorV1,
    supervisor_export::SupervisorExportAdoptionError,
    vote_submitter::{LocalVotePreparationErrorV1, VoteSubmissionErrorV1},
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
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Control(#[from] crate::control::ControlError),
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error(transparent)]
    Worker(#[from] WorkerError),
    #[error(transparent)]
    Transport(#[from] outbe_tee::TransportError),
    #[error(transparent)]
    Signer(#[from] outbe_primitives::signer::SignerError),
    #[error(transparent)]
    WorkerObservability(#[from] WorkerObservabilityErrorV1),
    #[error(transparent)]
    ParseInt(#[from] std::num::ParseIntError),
    #[error(transparent)]
    FromHex(#[from] alloy_primitives::hex::FromHexError),
    #[error(transparent)]
    Storage(#[from] outbe_offchain_storage::StorageError),
    #[error(transparent)]
    RpcInputExporter(#[from] RpcInputExporterErrorV1),
    #[error(transparent)]
    DiscoverySpool(#[from] crate::discovery_spool::DiscoverySpoolError),
    #[error(transparent)]
    ConsensusChainId(#[from] outbe_consensus::proof::constants::ConsensusChainIdError),
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

#[derive(Debug, Error)]
pub enum InputArtifactError {
    #[error("private Tribute amount read failed: {0}")]
    PrivateTribute(#[from] outbe_tee::TransportError),
    #[error(transparent)]
    Cas(#[from] CasError),
    #[error("canonical OCOMP protocol object rejected: {0}")]
    Protocol(#[from] ProtocolError),
    #[error("canonical Tribute body rejected: {0}")]
    Tribute(#[from] CanonicalBodyError),
    #[error("input artifact invariant failed: {0}")]
    Invariant(&'static str),
    #[error("input artifact invariant failed: input chunk key allocation")]
    KeyAllocationFailed,
    #[error("input artifact invariant failed: input chunk first key")]
    MissingFirstKey,
    #[error("input artifact invariant failed: input chunk last key")]
    MissingLastKey,
    #[error("input artifact invariant failed: Fidelity subject count")]
    FidelitySubjectCount,
    #[error("input artifact invariant failed: Oracle subject WWD")]
    OracleSubjectWwd,
    #[error("input artifact invariant failed: Oracle subject ISO count")]
    OracleSubjectIsoCount,
    #[error("input artifact invariant failed: opening chunk kind")]
    OpeningChunkKind,
    #[error("input artifact invariant failed: input chunk reference allocation")]
    ReferenceAllocationFailed,
    #[error("input artifact count does not fit the protocol")]
    CountOverflow,
    #[error("input artifact byte count overflow")]
    ByteCountOverflow,
    #[error("Tribute nominal total overflow")]
    NominalTotalOverflow,
    #[error("streamed Tribute count mismatch: expected {expected}, got {actual}")]
    TributeCountMismatch { expected: u32, actual: u32 },
    #[error("streamed Tribute source failed: {0}")]
    TributeSource(String),
    #[error("streamed opening source failed: {0}")]
    OpeningSource(String),
    #[error(transparent)]
    InputRefCatalog(#[from] InputRefCatalogError),
}

#[derive(Debug, Error)]
pub enum TributeInventoryError {
    #[error("private Tribute amount read failed: {0}")]
    PrivateTribute(#[from] outbe_tee::TransportError),
    #[error("invalid Tribute inventory work configuration")]
    InvalidWorkConfig,
    #[error("Tribute inventory is already sealed")]
    AlreadySealed,
    #[error("Tribute inventory is locked by another writer")]
    Locked,
    #[error("unsafe Tribute inventory path: {0}")]
    UnsafePath(PathBuf),
    #[error("Tribute inventory authority mismatch: {0}")]
    Authority(&'static str),
    #[error("corrupt Tribute inventory: {0}")]
    Corrupt(&'static str),
    #[error("corrupt Tribute inventory: Tribute body spool header")]
    CorruptTributeBodySpoolHeader,
    #[error("corrupt Tribute inventory: owner run magic")]
    CorruptOwnerRunMagic,
    #[error("corrupt Tribute inventory: owner run length")]
    CorruptOwnerRunLength,
    #[error("corrupt Tribute inventory: Tribute body spool magic")]
    CorruptTributeBodySpoolMagic,
    #[error("corrupt Tribute inventory: empty Tribute body")]
    CorruptEmptyTributeBody,
    #[error("corrupt Tribute inventory: Tribute body spool closure")]
    CorruptTributeBodySpoolClosure,
    #[error("corrupt Tribute inventory: inventory file length")]
    CorruptInventoryFileLength,
    #[error("corrupt Tribute inventory: inventory header")]
    CorruptInventoryHeader,
    #[error("corrupt Tribute inventory: inventory worldwide day")]
    CorruptInventoryWorldwideDay,
    #[error("corrupt Tribute inventory: owner inventory digest")]
    CorruptOwnerInventoryDigest,
    #[error("corrupt Tribute inventory: Tribute body spool digest")]
    CorruptTributeBodySpoolDigest,
    #[error("corrupt Tribute inventory: reference ISO inventory")]
    CorruptReferenceIsoInventory,
    #[error("corrupt Tribute inventory: owner inventory count")]
    CorruptOwnerInventoryCount,
    #[error("corrupt Tribute inventory: owner inventory order")]
    CorruptOwnerInventoryOrder,
    #[error("Tribute count mismatch: expected {expected}, got {actual}")]
    CountMismatch { expected: u32, actual: u32 },
    #[error("Tribute nominal total mismatch: expected {expected}, got {actual}")]
    NominalTotalMismatch { expected: U256, actual: U256 },
    #[error("Tribute inventory nominal total overflow")]
    NominalTotalOverflow,
    #[error("reference ISO count {actual} exceeds existing OCOMP protocol bound {limit}")]
    ReferenceIsoCountOutsideProtocol { limit: usize, actual: usize },
    #[error("canonical Tribute body has {actual} bytes outside per-body bound {limit}")]
    BodyOutsideBound { limit: usize, actual: usize },
    #[error("Tribute inventory integer overflow")]
    IntegerOverflow,
    #[error("Tribute inventory I/O failed during {operation} at {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug, Error)]
pub enum ExactLysisPlanError {
    #[error("private Tribute amount read failed: {0}")]
    PrivateTribute(#[from] outbe_tee::TransportError),
    #[error(transparent)]
    Admission(#[from] AdmissionCatalogError),
    #[error(transparent)]
    InputRef(#[from] InputRefCatalogError),
    #[error(transparent)]
    InputArtifact(#[from] InputArtifactError),
    #[error(transparent)]
    TributeBody(#[from] CanonicalBodyError),
    #[error(transparent)]
    Cas(#[from] CasError),
    #[error(transparent)]
    Planner(#[from] PlannerErrorV1),
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error("Lysis plan authority mismatch: {0}")]
    AuthorityMismatch(&'static str),
    #[error("Lysis plan authority mismatch: fixed reduce league fractions")]
    FixedReduceLeagueFractions,
    #[error("Lysis plan authority mismatch: Tribute owner identity")]
    TributeOwnerIdentity,
    #[error("Lysis plan authority mismatch: Tribute WWD")]
    TributeWwd,
    #[error("Lysis plan authority mismatch: Fidelity owner batch bound")]
    FidelityOwnerBatchBound,
    #[error("Lysis plan authority mismatch: Oracle opening WWD")]
    OracleOpeningWwd,
    #[error("Lysis plan authority mismatch: Oracle root state")]
    OracleRootState,
    #[error("Lysis plan authority mismatch: shuffle producer")]
    ShuffleProducer,
    #[error("Lysis plan authority mismatch: producer admission")]
    ProducerAdmission,
    #[error("plan ordinal {plan_ordinal} admitted an unexpected UnitId")]
    UnexpectedUnitId { plan_ordinal: u32 },
}

#[derive(Debug, Error)]
pub enum PublicRpcError {
    #[error("public OCOMP RPC response limit must be non-zero")]
    InvalidResponseLimit,
    #[error("public OCOMP RPC transport failed: {0}")]
    Transport(String),
    #[error("public OCOMP RPC response exceeded the configured bound")]
    ResponseTooLarge,
    #[error("public OCOMP RPC response id mismatch")]
    ResponseId,
    #[error("public OCOMP RPC {method} returned an error: {detail}")]
    Remote {
        method: &'static str,
        detail: String,
    },
    #[error("public OCOMP RPC {0} response has no result")]
    MissingResult(&'static str),
    #[error("public OCOMP RPC returned no block")]
    MissingBlock,
    #[error("public OCOMP RPC {method} response is malformed: missing {field}")]
    MissingField { method: &'static str, field: String },
    #[error("public OCOMP RPC {method} response is malformed: {detail}")]
    Malformed {
        method: &'static str,
        detail: String,
    },
}

#[derive(Debug, Error)]
pub enum SignOnceError {
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error(
        "OCOMP sign-once equivocation for job {job_id}, attempt {attempt}: \
         recorded {recorded_digest}, requested {requested_digest}"
    )]
    Equivocation {
        job_id: B256,
        attempt: u32,
        recorded_digest: B256,
        requested_digest: B256,
    },
    #[error("OCOMP sign-once store is disabled: {0}")]
    Disabled(String),
    #[error("OCOMP sign-once store is disabled: sign-once state mutex is poisoned")]
    MutexPoisoned,
    #[error("OCOMP result-digest signing failed: {0}")]
    SigningFailed(String),
    #[error("OCOMP sign-once store is unsafe at {path}: {reason}")]
    UnsafeStore { path: PathBuf, reason: &'static str },
    #[error("OCOMP sign-once state is uncertain at {path}: {reason}")]
    UncertainState { path: PathBuf, reason: &'static str },
    #[error("OCOMP sign-once record is corrupt at {path}: {reason}")]
    CorruptRecord { path: PathBuf, reason: &'static str },
    #[error("OCOMP sign-once {operation} failed at {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl SignOnceError {
    pub(crate) fn disables_store(&self) -> bool {
        matches!(
            self,
            Self::Disabled(_)
                | Self::MutexPoisoned
                | Self::UnsafeStore { .. }
                | Self::UncertainState { .. }
                | Self::CorruptRecord { .. }
                | Self::SigningFailed(_)
                | Self::Io { .. }
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SupervisorJobFailureClassV1 {
    Retryable,
    Unrecoverable,
}

#[derive(Debug, Error)]
pub enum SupervisorJobRunnerErrorV1 {
    #[error("supervisor Lysis job stage `{name}` failed: {detail}")]
    Stage {
        class: SupervisorJobFailureClassV1,
        name: &'static str,
        detail: String,
    },
    #[error("supervisor Lysis job invariant failed: {0}")]
    Invariant(&'static str),
    #[error("supervisor Lysis job invariant failed: worker dispatch thread panicked")]
    WorkerDispatchThreadPanicked,
    #[error("worker failed plan ordinal {plan_ordinal} ({unit_id})")]
    WorkerFailed { plan_ordinal: u32, unit_id: B256 },
    #[error("supervisor Lysis job was superseded by a newer finalized job")]
    Cancelled,
}

impl SupervisorJobRunnerErrorV1 {
    #[must_use]
    pub const fn class(&self) -> SupervisorJobFailureClassV1 {
        match self {
            Self::Stage { class, .. } => *class,
            Self::Invariant(_) | Self::WorkerDispatchThreadPanicked => {
                SupervisorJobFailureClassV1::Unrecoverable
            }
            Self::WorkerFailed { .. } | Self::Cancelled => SupervisorJobFailureClassV1::Retryable,
        }
    }
}

#[derive(Debug, Error)]
pub enum PublicVoteRpcErrorV1 {
    #[error("invalid public vote RPC configuration")]
    InvalidConfiguration,
    #[error("public vote RPC transport failed: {0}")]
    Transport(String),
    #[error("public vote RPC response exceeds its byte cap")]
    ResponseTooLarge,
    #[error("public vote RPC response is malformed: {0}")]
    Malformed(String),
    #[error("public vote RPC response is malformed: missing result")]
    MissingResult,
    #[error("public vote RPC response is malformed: missing {0}")]
    MissingField(&'static str),
    #[error("public vote RPC response is malformed: {0} is not a string")]
    FieldNotString(&'static str),
    #[error("public vote RPC returned error {code}: {message}")]
    Remote { code: i64, message: String },
    #[error("public vote RPC returned no block")]
    MissingBlock,
    #[error("public vote RPC field {0} is not a canonical hex quantity")]
    InvalidQuantity(&'static str),
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
