//! This module defines typed errors for `outbe-node`.

use alloy_primitives::B256;
use outbe_metadosis::config::OcompForkInstallClassification;
use reth_ethereum::consensus::ConsensusError;
use reth_provider::ProviderError;
use std::sync::Arc;

use crate::{
    ocomp::{
        finality::{
            FinalizedIntentProofBuildError, FinalizedIntentVerifierError, HistoricalCommitteeError,
            PublicFinalizedIntentProofBuildError,
        },
        retention::RetentionError,
    },
    tee_remote_session::{ExternalRegistryAdmissionError, LocalRegistryAdmissionError},
};

/// This type alias represents the callback error for the enclave call context.
pub type CallContextError = String;

/// Build a [`ConsensusError::Other`] from a displayable error.
pub fn consensus_display_error(error: impl core::fmt::Display) -> ConsensusError {
    ConsensusError::Other(Arc::<dyn core::error::Error + Send + Sync>::from(Box::<
        dyn core::error::Error + Send + Sync,
    >::from(
        error.to_string(),
    )))
}

/// This enum defines errors that occur when the node reads a finalized frame.
#[derive(Debug, thiserror::Error)]
pub enum FinalizedFrameError {
    #[error("canonical finalized block {height} is unavailable")]
    CanonicalBlockUnavailable { height: u64 },
    #[error("canonical finalized block {height} ({hash}) is unavailable by hash")]
    BlockUnavailableByHash { height: u64, hash: B256 },
    #[error("provider returned finalized block {actual} while height {requested} was requested")]
    BlockHeightMismatch { actual: u64, requested: u64 },
    #[error("finalized block at height {height} recomputed to {actual}, expected {expected}")]
    RecomputedBlockHashMismatch {
        height: u64,
        actual: B256,
        expected: B256,
    },
    #[error(
        "canonical block hash {canonical} conflicts with finalized hash {finalized} at height {height}"
    )]
    FinalizedTargetHashConflict {
        canonical: B256,
        finalized: B256,
        height: u64,
    },
    #[error("receipts for finalized block {height} are unavailable")]
    ReceiptsUnavailable { height: u64 },
    #[error("finalized block {height} has {transactions} transactions but {receipts} receipts")]
    ReceiptCountMismatch {
        height: u64,
        transactions: usize,
        receipts: usize,
    },
}

impl FinalizedFrameError {
    /// Returns true when the error reflects transient provider lag rather than
    /// a canonical history or receipt conflict.
    #[must_use]
    pub const fn is_transient_unavailability(&self) -> bool {
        matches!(
            self,
            Self::CanonicalBlockUnavailable { .. }
                | Self::BlockUnavailableByHash { .. }
                | Self::ReceiptsUnavailable { .. }
        )
    }
}

/// This enum defines errors that [`crate::shutdown::NodeShutdown`] returns.
#[derive(Debug, thiserror::Error)]
pub enum NodeShutdownError {
    #[error("shutdown state poisoned")]
    StatePoisoned,
    #[error("payload service already registered")]
    PayloadServiceAlreadyRegistered,
    #[error("engine exit already observed")]
    EngineExitAlreadyObserved,
    #[error("execution exit observer panicked: {0}")]
    ExecutionExitObserverPanicked(String),
    #[error("consensus thread panicked: {0}")]
    ConsensusThreadPanicked(String),
    #[error("{name} panicked: {message}")]
    TaskPanicked { name: &'static str, message: String },
    #[error("execution engine completion missing after runtime teardown")]
    ExecutionEngineCompletionMissing,
}

/// This enum defines errors that occur when the node resolves the enclave call context.
#[derive(Debug, thiserror::Error)]
pub enum TeeCallContextError {
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error("enclave context header unavailable")]
    HeaderUnavailable,
    #[error("enclave context protocol version overflow")]
    ProtocolVersionOverflow,
}

impl TeeCallContextError {
    #[must_use]
    pub fn into_call_context_error(self) -> CallContextError {
        self.to_string()
    }
}

/// Errors returned by read-only [`outbe_primitives::storage::readonly::StorageReader`] adapters in `outbe-node`.
#[derive(Debug, thiserror::Error)]
pub enum NodeStorageReadError {
    #[error("pinned OCOMP snapshot state read failed: {0}")]
    PinnedOcompSnapshotStateReadFailed(#[source] ProviderError),
    #[error("anchored proof reader is restricted to TeeRegistry")]
    AnchoredProofReaderRestrictedToTeeRegistry,
    #[error("finalized state read failed: {0}")]
    FinalizedStateReadFailed(#[source] ProviderError),
}

impl From<NodeStorageReadError> for outbe_primitives::error::PrecompileError {
    fn from(error: NodeStorageReadError) -> Self {
        Self::Storage(error.to_string())
    }
}

/// This enum defines errors that occur when the node validates a canary batch.
#[derive(Debug, thiserror::Error)]
pub enum CanaryBatchValidationError {
    #[error("canary expected 1 result, got {0}")]
    UnexpectedResultCount(usize),
    #[error("canary offer rejected: {0}")]
    OfferRejected(String),
    #[error("canary result echoes a different owner")]
    OwnerMismatch,
    #[error("canary inputs_canonical_hash mismatch (non-determinism)")]
    InputsCanonicalHashMismatch,
    #[error("no pinned attestation key")]
    NoPinnedAttestationKey,
    #[error("canary attestation tag invalid: {0}")]
    InvalidAttestationTag(String),
}

impl CanaryBatchValidationError {
    #[must_use]
    pub fn invalid_attestation_tag(error: impl core::fmt::Display) -> Self {
        Self::InvalidAttestationTag(error.to_string())
    }
}

/// This enum defines errors that occur when the node validates an OCOMP fork install.
#[derive(Debug, thiserror::Error)]
pub enum OcompForkInstallError {
    #[error("invalid genesis config OCOMP fork install: {0}")]
    InvalidGenesisConfig(String),
    #[error("invalid canonical OCOMP fork install: {0}")]
    InvalidCanonicalInstall(String),
    #[error("OCOMP fork install does not bind selected chain: {0}")]
    ChainBindingMismatch(String),
    #[error("hash OCOMP fork install: {0}")]
    HashInstall(String),
    #[error("OCOMP fork install hash mismatch: manifest {manifest}, canonical {canonical}")]
    InstallHashMismatch { manifest: B256, canonical: B256 },
    #[error(
        "fresh-devnet OCOMP install must be Measurement at block {expected_height}, got {classification:?} at {actual_height}"
    )]
    InvalidFreshDevnetInstall {
        expected_height: u64,
        classification: OcompForkInstallClassification,
        actual_height: u64,
    },
    #[error("chain genesis config is missing required {key}")]
    MissingGenesisKey { key: &'static str },
    #[error(
        "fresh-devnet Measurement OCOMP install must activate at block {expected_height}, got {actual_height}"
    )]
    InvalidMeasurementActivationHeight {
        expected_height: u64,
        actual_height: u64,
    },
    #[error("invalid {key}: {reason}")]
    InvalidGenesisField { key: &'static str, reason: String },
    #[error("OCOMP-enabled chain genesis config is missing required {key}")]
    MissingOcompGenesisKey { key: &'static str },
    #[error("OCOMP snapshot retention horizon overflow")]
    SnapshotRetentionHorizonOverflow,
    #[error("OCOMP maximum job lifetime overflow")]
    MaximumJobLifetimeOverflow,
    #[error(
        "OCOMP snapshot retention horizon is too short: ({retain_epochs} - 1) * {epoch_length_blocks} = {retained_epoch_span} must be strictly greater than {awaiting_finality_deadline_blocks} + {min_finality_depth} + {result_deadline_blocks} = {required_job_lifetime}"
    )]
    SnapshotRetentionHorizonTooShort {
        retain_epochs: u64,
        epoch_length_blocks: u64,
        retained_epoch_span: u64,
        awaiting_finality_deadline_blocks: u64,
        min_finality_depth: u64,
        result_deadline_blocks: u64,
        required_job_lifetime: u64,
    },
    #[error("fresh-devnet genesis config is missing required {key}")]
    MissingFreshDevnetGenesisKey { key: &'static str },
    #[error("invalid genesis Metadosis storage layout: {0}")]
    InvalidMetadosisStorageLayout(String),
    #[error(
        "fresh-devnet Metadosis storage layout hash mismatch: manifest {manifest}, node {node}"
    )]
    MetadosisStorageLayoutHashMismatch { manifest: B256, node: B256 },
}

impl OcompForkInstallError {
    #[must_use]
    pub fn invalid_genesis_config(error: impl core::fmt::Display) -> Self {
        Self::InvalidGenesisConfig(error.to_string())
    }

    #[must_use]
    pub fn invalid_canonical_install(error: impl core::fmt::Display) -> Self {
        Self::InvalidCanonicalInstall(error.to_string())
    }

    #[must_use]
    pub fn chain_binding_mismatch(error: impl core::fmt::Display) -> Self {
        Self::ChainBindingMismatch(error.to_string())
    }

    #[must_use]
    pub fn hash_install(error: impl core::fmt::Display) -> Self {
        Self::HashInstall(error.to_string())
    }

    #[must_use]
    pub fn invalid_genesis_field(key: &'static str, error: impl core::fmt::Display) -> Self {
        Self::InvalidGenesisField {
            key,
            reason: error.to_string(),
        }
    }

    #[must_use]
    pub fn invalid_metadosis_storage_layout(error: impl core::fmt::Display) -> Self {
        Self::InvalidMetadosisStorageLayout(error.to_string())
    }
}

/// This enum defines errors that occur when the node projects finalized offchain data.
#[derive(Debug, thiserror::Error)]
pub enum NodeProjectionError {
    #[error("OCOMP projection containment has no local finalized block")]
    ContainmentNoLocalFinalizedBlock,
    #[error(
        "OCOMP Mongo checkpoint {checkpoint_number} ({checkpoint_hash}) is not finalized; local finality is {finalized_number} ({finalized_hash})"
    )]
    MongoCheckpointNotFinalized {
        checkpoint_number: u64,
        checkpoint_hash: B256,
        finalized_number: u64,
        finalized_hash: B256,
    },
    #[error(
        "OCOMP job checkpoint {required_number} ({required_hash}) is not finalized; local finality is {finalized_number} ({finalized_hash})"
    )]
    JobCheckpointNotFinalized {
        required_number: u64,
        required_hash: B256,
        finalized_number: u64,
        finalized_hash: B256,
    },
    #[error(
        "OCOMP Mongo checkpoint hash conflict at {block_number}: stored {stored}, canonical {canonical}"
    )]
    MongoCheckpointHashConflict {
        block_number: u64,
        stored: B256,
        canonical: B256,
    },
    #[error(
        "OCOMP Mongo checkpoint {block_number} ({block_hash}) is unavailable in local canonical history"
    )]
    MongoCheckpointUnavailable { block_number: u64, block_hash: B256 },
    #[error(
        "OCOMP finalized job hash conflict at {block_number}: requested {requested}, canonical {canonical}"
    )]
    JobHashConflict {
        block_number: u64,
        requested: B256,
        canonical: B256,
    },
    #[error(
        "OCOMP finalized job {block_number} ({block_hash}) is unavailable in local canonical history"
    )]
    JobUnavailable { block_number: u64, block_hash: B256 },
    #[error("ADR-005 requires projection start_block 1, found {0}")]
    InvalidStartBlock(u64),
    #[error("offchain storage startup recovery exceeded the eight-second total deadline")]
    StartupRecoveryDeadlineExceeded,
    #[error("offchain storage startup validation worker exited unexpectedly")]
    StartupValidationWorkerExited,
    #[error("offchain projection rejects unknown Outbe chain ID {0}")]
    UnknownChainId(u64),
    #[error("OCOMP projection/GC fence is poisoned")]
    RetentionFencePoisoned,
    #[error(
        "canonical block {block_number} has {transactions} transactions but {receipts} receipts"
    )]
    CanonicalBlockReceiptMismatch {
        block_number: u64,
        transactions: usize,
        receipts: usize,
    },
    #[error("transaction index does not fit u64")]
    TransactionIndexOverflow,
    #[error("block-global log index overflow")]
    LogIndexOverflow,
    #[error("projection body-read failure receiver is unavailable")]
    FailureReceiverUnavailable,
    #[error("projection body-read recovery sender is unavailable")]
    RecoverySenderUnavailable,
    #[error("durable projection checkpoint is ahead of or conflicts with finalized target")]
    DurableCheckpointConflict,
    #[error(
        "finalized frame hash {frame_hash} conflicts with durable projection hash {durable_hash} at height {height}"
    )]
    FinalizedFrameHashConflict {
        frame_hash: B256,
        durable_hash: B256,
        height: u64,
    },
    #[error("offchain projection overlay is required before a durable write")]
    OverlayRequired,
    #[error(
        "logical projection checkpoint {block_number} ({block_hash}) is ahead of durable frame-sink authority"
    )]
    LogicalCheckpointAhead { block_number: u64, block_hash: B256 },
    #[error(
        "projector returned checkpoint {projected_number} ({projected_hash}) after projecting shared frame {frame_number} ({frame_hash})"
    )]
    ProjectedCheckpointMismatch {
        projected_number: u64,
        projected_hash: B256,
        frame_number: u64,
        frame_hash: B256,
    },
    #[error(
        "offchain-data checkpoint identity mismatch at block {block_number}: stored {stored}, canonical {canonical}"
    )]
    CheckpointIdentityMismatch {
        block_number: u64,
        stored: B256,
        canonical: B256,
    },
    #[error(
        "canonical block {block_number} for offchain storage checkpoint {block_hash} is unavailable locally"
    )]
    CanonicalCheckpointBlockUnavailable { block_number: u64, block_hash: B256 },
    #[error(
        "offchain-data checkpoint {checkpoint_number} ({checkpoint_hash}) does not match local Reth finalized {finalized_number} ({finalized_hash})"
    )]
    CheckpointFinalizedMismatch {
        checkpoint_number: u64,
        checkpoint_hash: B256,
        finalized_number: u64,
        finalized_hash: B256,
    },
    #[error("offchain-data projector lock is poisoned")]
    ProjectorLockPoisoned,
    #[error("projection recovery acknowledgement receiver is closed")]
    RecoveryAckReceiverClosed,
    #[error("logical checkpoint receiver is closed")]
    LogicalCheckpointReceiverClosed,
    #[error("projection checkpoint height overflow")]
    CheckpointHeightOverflow,
    #[error("durable offchain storage writer queue is closed")]
    DurableWriterQueueClosed,
    #[error(
        "projection checkpoint {checkpoint_number} ({checkpoint_hash}) is ahead of finalized target {target_number} ({target_hash})"
    )]
    CheckpointAheadOfFinalizedTarget {
        checkpoint_number: u64,
        checkpoint_hash: B256,
        target_number: u64,
        target_hash: B256,
    },
    #[error(
        "projection checkpoint hash {checkpoint_hash} conflicts with finalized hash {target_hash} at height {height}"
    )]
    CheckpointFinalizedHashConflict {
        checkpoint_hash: B256,
        target_hash: B256,
        height: u64,
    },
    #[error("canonical block {block_number} for restored projection checkpoint is unavailable")]
    RestoredCheckpointCanonicalBlockUnavailable { block_number: u64 },
    #[error(
        "finalized target regressed from {current_number} ({current_hash}) to {incoming_number} ({incoming_hash})"
    )]
    FinalizedTargetRegressed {
        current_number: u64,
        current_hash: B256,
        incoming_number: u64,
        incoming_hash: B256,
    },
    #[error("finalized target hash changed at height {height}: {current_hash} -> {incoming_hash}")]
    FinalizedTargetHashChanged {
        height: u64,
        current_hash: B256,
        incoming_hash: B256,
    },
}

impl LocalRegistryAdmissionError {
    #[must_use]
    pub fn replacement_authorization(error: impl core::fmt::Display) -> Self {
        Self::ReplacementAuthorization(error.to_string())
    }
}

impl ExternalRegistryAdmissionError {
    fn map_reason(ctor: fn(String) -> Self, error: impl core::fmt::Display) -> Self {
        ctor(error.to_string())
    }

    #[must_use]
    pub fn slot_plan(error: impl core::fmt::Display) -> Self {
        Self::map_reason(Self::SlotPlan, error)
    }

    #[must_use]
    pub fn proof(error: impl core::fmt::Display) -> Self {
        Self::map_reason(Self::Proof, error)
    }

    #[must_use]
    pub fn registry(error: impl core::fmt::Display) -> Self {
        Self::map_reason(Self::Registry, error)
    }
}

impl HistoricalCommitteeError {
    #[must_use]
    pub fn from_display(error: impl core::fmt::Display) -> Self {
        Self::new(error.to_string())
    }
}

impl FinalizedIntentVerifierError {
    #[must_use]
    pub fn header_decode(error: impl core::fmt::Display) -> Self {
        Self::HeaderDecode(error.to_string())
    }

    #[must_use]
    pub fn consensus_finality(error: impl core::fmt::Display) -> Self {
        Self::ConsensusFinality(error.to_string())
    }

    #[must_use]
    pub fn account_proof(error: impl core::fmt::Display) -> Self {
        Self::AccountProof(error.to_string())
    }

    #[must_use]
    pub fn storage_proof(index: usize, error: impl core::fmt::Display) -> Self {
        Self::StorageProof {
            index,
            reason: error.to_string(),
        }
    }

    #[must_use]
    pub fn job_record_encoding(error: impl core::fmt::Display) -> Self {
        Self::JobRecordEncoding(error.to_string())
    }
}

impl FinalizedIntentProofBuildError {
    #[must_use]
    pub fn state(error: impl core::fmt::Display) -> Self {
        Self::State(error.to_string())
    }

    #[must_use]
    pub fn intent(error: impl core::fmt::Display) -> Self {
        Self::Intent(error.to_string())
    }
}

impl PublicFinalizedIntentProofBuildError {
    #[must_use]
    pub fn finalization(error: impl core::fmt::Display) -> Self {
        Self::Finalization(error.to_string())
    }

    #[must_use]
    pub fn account_proof(error: impl core::fmt::Display) -> Self {
        Self::AccountProof(error.to_string())
    }

    #[must_use]
    pub fn storage_proof(error: impl core::fmt::Display) -> Self {
        Self::StorageProof(error.to_string())
    }

    #[must_use]
    pub fn intent(error: impl core::fmt::Display) -> Self {
        Self::Intent(error.to_string())
    }

    #[must_use]
    pub fn witness(error: impl core::fmt::Display) -> Self {
        Self::Witness(error.to_string())
    }
}

impl RetentionError {
    #[must_use]
    pub fn source(error: impl core::fmt::Display) -> Self {
        Self::Source(error.to_string())
    }

    #[must_use]
    pub fn into_selector_error(self) -> outbe_offchain_data::RetentionSelectorError {
        match self {
            Self::RetentionCoordinatorNotInstalled => {
                outbe_offchain_data::RetentionSelectorError::RetentionCoordinatorNotInstalled
            }
            Self::RetainedTributeStorageUnavailable => {
                outbe_offchain_data::RetentionSelectorError::RetainedTributeStorageUnavailable
            }
            Self::MultipleInputRetentionIdentities => {
                outbe_offchain_data::RetentionSelectorError::MultipleInputRetentionIdentities
            }
            Self::Poisoned => outbe_offchain_data::RetentionSelectorError::Poisoned,
            Self::Quarantined(reason) => {
                outbe_offchain_data::RetentionSelectorError::Quarantined(reason)
            }
            Self::JournalUnavailable {
                operation,
                path,
                reason,
            } => outbe_offchain_data::RetentionSelectorError::JournalUnavailable {
                operation,
                path,
                reason,
            },
            other => outbe_offchain_data::RetentionSelectorError::Failed(other.to_string()),
        }
    }
}
