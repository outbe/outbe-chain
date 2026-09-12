use crate::ocomp::retention::*;

/// Exact source identity retained before a local positive vote.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CandidatePinV1 {
    pub block_number: u64,
    pub block_hash: B256,
    pub state_root: B256,
    pub intent_id: B256,
    pub wwd: u32,
    pub ce_sealed_root: B256,
    pub protocol_bundle_hash: B256,
    pub input_lease_id: B256,
}

/// The single decoded `OffchainJobRequested` observation from one finalized
/// frame. It is a locator only; retention and discovery independently reopen
/// the exact block state before treating it as authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FinalizedRequestObservationV1 {
    pub intent_id: B256,
    pub wwd: u32,
    pub pending_nonce: u64,
    pub attempt: u32,
    pub activation_preconditions_hash: B256,
}

/// Exact finalized job derived from the candidate's typed state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FinalizedJobPinV1 {
    pub candidate: CandidatePinV1,
    pub job_id: B256,
    pub finality_recorded_height: u64,
    pub open_height: u64,
    pub deadline_height: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
// Both variants are protocol-bounded and this value crosses the finality seam
// by value. Retaining `Copy` avoids introducing fallible heap allocation into
// candidate classification.
#[allow(clippy::large_enum_variant)]
pub enum CandidateFinalityV1 {
    Finalized(FinalizedJobPinV1),
    Orphaned,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PinReleaseReason {
    Orphaned,
    RetentionSatisfied,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExportAuthorityV1 {
    pub source_generation: u64,
    pub lease_generation: u64,
    pub manifest_hash: B256,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReleasedJobAuthorityV1 {
    pub candidate: CandidatePinV1,
    pub job_id: B256,
    pub source_generation: u64,
    pub export: Option<ExportAuthorityV1>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PinStateV1 {
    Tentative {
        candidate: CandidatePinV1,
    },
    Finalized {
        candidate: CandidatePinV1,
        job_id: B256,
        finality_recorded_height: u64,
        open_height: u64,
        deadline_height: u64,
    },
    Exported {
        candidate: CandidatePinV1,
        job_id: B256,
        finality_recorded_height: u64,
        open_height: u64,
        deadline_height: u64,
        export: ExportAuthorityV1,
    },
    Terminal {
        candidate: CandidatePinV1,
        job_id: B256,
        finality_recorded_height: u64,
        open_height: u64,
        deadline_height: u64,
        source_generation: u64,
        export: Option<ExportAuthorityV1>,
        terminal_height: u64,
        release_height: u64,
    },
    GcPending {
        candidate: CandidatePinV1,
        job_id: B256,
        finality_recorded_height: u64,
        open_height: u64,
        deadline_height: u64,
        source_generation: u64,
        export: Option<ExportAuthorityV1>,
        terminal_height: u64,
        release_height: u64,
    },
    OrphanGcPending {
        candidate: CandidatePinV1,
        observed_height: u64,
    },
    Released {
        candidate: CandidatePinV1,
        job_id: Option<B256>,
        source_generation: Option<u64>,
        reason: PinReleaseReason,
        observed_height: u64,
        export: Option<ExportAuthorityV1>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PinRecordV1 {
    pub generation: u64,
    pub state: PinStateV1,
}

/// Read-only decoded view of one durable retention journal. This is used by
/// operational diagnostics and behavioral evidence; it shares the production
/// decoder and never creates, repairs or rewrites journal state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetentionJournalSnapshotV1 {
    pub generation: u64,
    pub last_updated: B256,
    pub records: Vec<(B256, PinRecordV1)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
// `Ready` deliberately returns the complete bounded durable record. Boxing it
// would make a read-only status observation depend on a heap allocation.
#[allow(clippy::large_enum_variant)]
pub enum RetentionStatus {
    Empty,
    Ready(PinRecordV1),
    Unavailable {
        operation: &'static str,
        path: PathBuf,
        reason: String,
    },
    Quarantined {
        reason: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DurablePinAck {
    pub generation: u64,
    pub record_hash: B256,
}

#[derive(Debug, thiserror::Error)]
pub enum RetentionError {
    #[error("OCOMP retention is quarantined: {0}")]
    Quarantined(String),
    #[error("OCOMP retention journal is unavailable after {operation} at {path}: {reason}")]
    JournalUnavailable {
        operation: &'static str,
        path: PathBuf,
        reason: String,
    },
    #[error("OCOMP pin journal mutex is poisoned")]
    Poisoned,
    #[error("pin journal {operation} failed at {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("pin journal is ambiguous: {0}")]
    AmbiguousJournal(&'static str),
    #[error("pin journal is malformed: {0}")]
    MalformedJournal(&'static str),
    #[error("pin journal version {actual} is unsupported")]
    UnsupportedJournalVersion { actual: u16 },
    #[error("pin generation overflow")]
    GenerationOverflow,
    #[error("OCOMP journal record count exceeds its u16 wire format")]
    RegistryCapacity,
    #[error("conflicting tentative candidate cannot replace the active pin")]
    ConflictingCandidate,
    #[error("fork-orphaned candidate cannot be pinned again")]
    OrphanedCandidate,
    #[error("stale pin generation: expected {expected}, actual {actual}")]
    StaleGeneration { expected: u64, actual: u64 },
    #[error("pin transition is invalid: {0}")]
    InvalidTransition(&'static str),
    #[error("finalized input source failed: {0}")]
    Source(String),
    #[error("retained Tribute storage is not configured")]
    RetainedTributeStorageUnavailable,
    #[error("retained Tribute garbage collection failed: {0}")]
    RetainedTributeGc(String),
    #[error("failed to spawn retained Tribute GC worker: {0}")]
    RetainedTributeGcWorkerSpawn(#[source] std::io::Error),
    #[error("OCOMP retention coordinator is not installed")]
    RetentionCoordinatorNotInstalled,
    #[error("OCOMP retention coordinator is already installed")]
    RetentionCoordinatorAlreadyInstalled,
}

pub(in crate::ocomp::retention) fn retention_status_error(
    status: &RetentionStatus,
) -> Option<RetentionError> {
    match status {
        RetentionStatus::Unavailable {
            operation,
            path,
            reason,
        } => Some(RetentionError::JournalUnavailable {
            operation,
            path: path.clone(),
            reason: reason.clone(),
        }),
        RetentionStatus::Quarantined { reason } => {
            Some(RetentionError::Quarantined(reason.clone()))
        }
        RetentionStatus::Empty | RetentionStatus::Ready(_) => None,
    }
}

pub(in crate::ocomp::retention) fn status_for_journal_error(
    error: &RetentionError,
) -> RetentionStatus {
    match error {
        RetentionError::Io {
            operation,
            path,
            source,
        } => RetentionStatus::Unavailable {
            operation,
            path: path.clone(),
            reason: source.to_string(),
        },
        _ => RetentionStatus::Quarantined {
            reason: error.to_string(),
        },
    }
}

pub(in crate::ocomp) fn retention_terminal_height_for_status(
    status: OcompJobStatus,
    observed_height: u64,
    deadline_height: u64,
    terminal_height: u64,
) -> Result<Option<u64>, RetentionError> {
    match status {
        OcompJobStatus::AwaitingFinality | OcompJobStatus::VotingOpen => Ok(None),
        OcompJobStatus::Completed => {
            if terminal_height >= deadline_height {
                return Err(RetentionError::Source(
                    "OCOMP quorum terminal height is outside its response window".to_owned(),
                ));
            }
            Ok((observed_height >= deadline_height).then_some(deadline_height))
        }
        OcompJobStatus::Expired | OcompJobStatus::Failed => Ok(Some(terminal_height)),
    }
}
