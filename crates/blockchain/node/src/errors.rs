//! This module defines typed errors for `outbe-node`.

use alloy_primitives::Address;
use reth_ethereum::consensus::ConsensusError;
use reth_provider::ProviderError;
use std::sync::Arc;

use crate::tee_remote_session::{ExternalRegistryAdmissionError, LocalRegistryAdmissionError};

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

/// Typed consensus policy and system-transaction validation errors.
#[derive(Debug, thiserror::Error)]
pub enum ConsensusPolicyError {
    #[error("block {block_number} protocol gas limit mismatch: expected {expected}, got {actual}")]
    ProtocolGasLimitMismatch {
        block_number: u64,
        expected: u64,
        actual: u64,
    },
    #[error(
        "block RLP size {rlp_length} exceeds the {max_block_rlp_bytes}-byte P2P transport cap"
    )]
    BlockRlpSizeExceedsTransportCap {
        rlp_length: usize,
        max_block_rlp_bytes: usize,
    },
    #[error(
        "block timestamp_millis {timestamp} exceeds parent {parent} by {drift} ms, above the {max_drift} ms maximum drift"
    )]
    TimestampExceedsMaxDrift {
        timestamp: u64,
        parent: u64,
        drift: u64,
        max_drift: u64,
    },
    #[error(
        "block timestamp_millis {timestamp} advances parent {parent} by only {drift} ms, below the {min_advance} ms minimum advance"
    )]
    TimestampBelowMinAdvance {
        timestamp: u64,
        parent: u64,
        drift: u64,
        min_advance: u64,
    },
    #[error("timestamp_millis_part {part} must be less than {max_part}")]
    InvalidTimestampMillisPart { part: u64, max_part: u64 },
    #[error("non-genesis block beneficiary must be REWARDS_ADDRESS {expected}: got {actual}")]
    NonGenesisBeneficiaryMismatch { expected: Address, actual: Address },
    #[error("decode system tx input: {0}")]
    DecodeSystemTxInput(String),
}

impl ConsensusPolicyError {
    #[must_use]
    pub fn decode_system_tx_input(error: impl core::fmt::Display) -> Self {
        Self::DecodeSystemTxInput(error.to_string())
    }
}

impl From<ConsensusPolicyError> for ConsensusError {
    fn from(error: ConsensusPolicyError) -> Self {
        consensus_display_error(error)
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
    #[error("additional shutdown failure: {0}")]
    AdditionalShutdownFailure(String),
}

impl NodeShutdownError {
    #[must_use]
    pub fn additional_shutdown_failure(error: &eyre::Report) -> Self {
        Self::AdditionalShutdownFailure(format!("{error:#}"))
    }
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
