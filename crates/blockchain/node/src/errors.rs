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
    pub fn into_selector_error(self) -> CallContextError {
        self.to_string()
    }
}
