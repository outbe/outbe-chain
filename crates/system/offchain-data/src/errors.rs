use alloy_primitives::B256;
use outbe_compressed_entities::WwdEntityId;
use outbe_gem::GemRepositoryError;
use outbe_nod::NodRepositoryError;
use outbe_offchain_storage::StorageError;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::time::WorldwideDay;
use outbe_tribute::TributeRepositoryError;
use thiserror::Error;

use crate::state::{ProjectionConfig, ProjectionSource};

/// Stable projector failures. No backend-specific type crosses this boundary.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ProjectionError {
    #[error("node-local cryptographic backend unavailable: {0}")]
    CryptoBackendUnavailable(#[source] outbe_compressed_entities::CommitmentError),
    #[error("off-chain storage failure: {0}")]
    Storage(#[from] StorageError),
    #[error("Tribute repository failure: {0}")]
    Tribute(#[from] TributeRepositoryError),
    #[error("Gem repository failure: {0}")]
    Gem(#[from] GemRepositoryError),
    #[error("Nod repository failure: {0}")]
    Nod(#[from] NodRepositoryError),
    #[error("failed to encode projection state")]
    StateEncode(#[source] postcard::Error),
    #[error("failed to decode projection state")]
    StateDecode(#[source] postcard::Error),
    #[error("corrupt projection state: {0}")]
    CorruptProjectionState(String),
    #[error("projection schema mismatch: expected {expected}, found {actual}")]
    ProjectionSchemaMismatch { expected: u32, actual: u32 },
    #[error("projection identity does not match configured chain")]
    ProjectionIdentityMismatch {
        expected: ProjectionConfig,
        actual_chain_id: u64,
        actual_genesis_hash: B256,
        actual_start_block: u64,
    },
    #[error("body/index records exist without projection state")]
    UnmanagedProjectionData,
    #[error(
        "checkpoint hash mismatch at block {block_number}: expected {expected}, found {actual}"
    )]
    CheckpointMismatch {
        block_number: u64,
        expected: B256,
        actual: B256,
    },
    #[error("non-sequential block: expected {expected}, found {actual}")]
    NonSequentialBlock { expected: u64, actual: u64 },
    #[error("invalid transaction order: expected {expected}, found {actual}")]
    InvalidTransactionOrder { expected: u64, actual: u64 },
    #[error("transaction index does not fit u64")]
    TransactionIndexOverflow,
    #[error("invalid block-global log order: expected {expected}, found {actual}")]
    InvalidLogOrder { expected: u64, actual: u64 },
    #[error("block-global log index overflow")]
    LogIndexOverflow,
    #[error("recognized projection log appears in a failed receipt at {0:?}")]
    ProjectionLogInFailedReceipt(Box<ProjectionSource>),
    #[error("malformed recognized projection event at {event_source:?}: {reason}")]
    MalformedProjectionEvent {
        event_source: Box<ProjectionSource>,
        reason: String,
    },
    #[error("malformed projection metadata: {0}")]
    MalformedProjectionMetadata(String),
    #[error("managed {entity} primary record has no projection metadata")]
    MissingProjectionMetadata { entity: &'static str },
    #[error("OCOMP retention selector failed for day {worldwide_day}: {reason}")]
    RetentionSelector {
        worldwide_day: WorldwideDay,
        reason: String,
    },
    #[error(
        "OCOMP retention selector returned day {selected} for requested partition {requested}"
    )]
    RetentionPinDayMismatch {
        requested: WorldwideDay,
        selected: WorldwideDay,
    },
    #[error("corrupt projected body: {0}")]
    CorruptProjectedBody(String),
    #[error(
        "{entity} {identity} commitment transition expected previous {expected_previous}, found {actual}"
    )]
    CommitmentTransitionMismatch {
        entity: &'static str,
        identity: WwdEntityId,
        expected_previous: B256,
        actual: B256,
    },
    #[error("tribute {tribute_id} changes after its worldwide day was retired in the same block")]
    TributeStoredAfterDayRetirement { tribute_id: WwdEntityId },
}

impl From<outbe_compressed_entities::CanonicalBodyError> for ProjectionError {
    fn from(error: outbe_compressed_entities::CanonicalBodyError) -> Self {
        Self::CorruptProjectedBody(error.to_string())
    }
}

impl From<outbe_compressed_entities::CommitmentError> for ProjectionError {
    fn from(error: outbe_compressed_entities::CommitmentError) -> Self {
        match error {
            outbe_compressed_entities::CommitmentError::CryptoBackend(_) => {
                Self::CryptoBackendUnavailable(error)
            }
            _ => Self::CorruptProjectedBody(error.to_string()),
        }
    }
}

pub enum PrecompileBodyReadClassification<'a> {
    Unavailable,
    Corruption(&'a str),
    Other,
}

pub fn classify_precompile_body_read_error(
    error: &PrecompileError,
) -> PrecompileBodyReadClassification<'_> {
    match error {
        PrecompileError::BodyReadUnavailable(_) => PrecompileBodyReadClassification::Unavailable,
        PrecompileError::BodyReadCorruption(message) => {
            PrecompileBodyReadClassification::Corruption(message)
        }
        _ => PrecompileBodyReadClassification::Other,
    }
}
