//! Error types for the Outbe JSON-RPC server.

use outbe_primitives::error::PrecompileError;
use thiserror::Error;

/// Errors returned by RPC state helpers and status builders.
#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub enum RpcError {
    /// A Radicle status counter does not fit in `u64`.
    #[error("Radicle status counter exceeds u64")]
    RadicleCounterOverflow,
}

/// Errors that occur when a registered validator record is incomplete.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum RpcValidatorStateError {
    /// A registered validator record has no consensus public key.
    #[error("registered validator is missing consensus pubkey")]
    MissingConsensusPubkey,
    /// A registered validator record has no history record.
    #[error("registered validator is missing history")]
    MissingHistory,
    /// A registered validator record has no persisted lifecycle status.
    #[error("registered validator has no persisted status")]
    MissingPersistedStatus,
}

impl From<RpcValidatorStateError> for PrecompileError {
    fn from(error: RpcValidatorStateError) -> Self {
        Self::Fatal(error.to_string())
    }
}

/// Errors that occur when bridging Reth state storage into precompile storage.
#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub enum RpcStateReadError {
    /// Reading a storage slot from the Reth state provider failed.
    #[error("state read failed: {0}")]
    StateReadFailed(String),
}

impl From<RpcStateReadError> for PrecompileError {
    fn from(error: RpcStateReadError) -> Self {
        Self::Storage(error.to_string())
    }
}

/// Typed internal errors returned by Outbe JSON-RPC methods.
#[derive(Debug, Error)]
pub enum RpcInternalError {
    /// Reading the latest state provider failed.
    #[error("failed to get latest state: {0}")]
    LatestStateReadFailed(String),
    /// A precompile state query failed.
    #[error("precompile error: {0}")]
    Precompile(#[from] PrecompileError),
    /// Reading the latest block number failed.
    #[error("failed to read latest block number: {0}")]
    LatestBlockNumberReadFailed(String),
    /// Reading the header for a target block failed.
    #[error("failed to read header for block {target}: {reason}")]
    BlockHeaderReadFailed {
        /// Target block number.
        target: u64,
        /// Underlying provider error message.
        reason: String,
    },
    /// This node is not serving consensus finalizations.
    #[error("node is not serving consensus finalizations")]
    NotServingConsensusFinalizations,
    /// No finalization is available for the requested height.
    #[error("no finalization available for height {0} (not finalized locally or pruned)")]
    NoFinalizationAvailable(u64),
    /// No direct finalization is available for the requested height.
    #[error("no direct finalization available for height {0}")]
    NoDirectFinalizationAvailable(u64),
    /// This node is not serving consensus blocks.
    #[error("node is not serving consensus blocks")]
    NotServingConsensusBlocks,
    /// No finalized consensus block is available for the requested height.
    #[error("no finalized consensus block available for height {0}")]
    NoFinalizedConsensusBlockAvailable(u64),
    /// Computing the ancestor proof height overflowed `u64`.
    #[error("proof height overflow")]
    ProofHeightOverflow,
    /// No finality evidence is available at the inspected height.
    #[error("no finality evidence available at height {0}")]
    NoFinalityEvidenceAvailable(u64),
    /// The ancestor finality proof exceeds the maximum response byte limit.
    #[error("ancestor finality proof exceeds byte limit")]
    AncestorFinalityProofExceedsByteLimit,
    /// No descendant finalization was found within the 64-block window.
    #[error("no descendant finalization within 64 blocks")]
    NoDescendantFinalizationWithin64Blocks,
    /// The blocking point-read worker task failed.
    #[error("point-read worker failed: {0}")]
    PointReadWorkerFailed(String),
    /// The TEE upgrade key source semaphore is busy.
    #[error("upgrade key source is busy; retry")]
    UpgradeKeySourceBusy,
    /// The supplied DCAP onboarding upgrade context is invalid.
    #[error("invalid upgrade context")]
    InvalidUpgradeContext,
    /// The blocking upgrade key export task failed.
    #[error("upgrade export task: {0}")]
    UpgradeExportTaskFailed(String),
    /// Finalized state is unavailable for upgrade candidate verification.
    #[error("finalized state unavailable")]
    FinalizedStateUnavailable,
    /// Finalized header is unavailable for upgrade candidate verification.
    #[error("finalized header unavailable")]
    FinalizedHeaderUnavailable,
    /// Finalized header hash does not match the finalized block marker.
    #[error("finalized header mismatch")]
    FinalizedHeaderMismatch,
    /// Immutable chain identity is not configured on the RPC server.
    #[error("immutable chain identity is not configured")]
    ImmutableChainIdentityNotConfigured,
    /// The upgrade key recipient is not a live finalized upgrade candidate.
    #[error("recipient is not a live finalized upgrade candidate")]
    RecipientNotLiveFinalizedUpgradeCandidate,
    /// The TEE renewal schedule is not configured on this node.
    #[error("TEE renewal schedule is not configured")]
    TeeRenewalScheduleNotConfigured,
    /// The TEE renewal schedule configuration has a zero minimum block time.
    #[error("TEE renewal schedule has zero minimum block time")]
    TeeRenewalScheduleZeroMinBlockTime,
    /// Reading the finalized block marker failed.
    #[error("failed to read finalized block: {0}")]
    FinalizedBlockReadFailed(String),
    /// The finalized block is unavailable.
    #[error("finalized block is unavailable")]
    FinalizedBlockIsUnavailable,
    /// Reading the finalized header at a height failed.
    #[error("failed to read finalized header {height}: {reason}")]
    FinalizedHeaderReadFailed {
        /// Finalized block height.
        height: u64,
        /// Underlying provider error message.
        reason: String,
    },
    /// The finalized header is unavailable.
    #[error("finalized header is unavailable")]
    FinalizedHeaderIsUnavailable,
    /// The finalized block marker and canonical header disagree.
    #[error("finalized block marker and canonical header disagree")]
    FinalizedBlockMarkerHeaderDisagree,
    /// Reading the finalized state failed.
    #[error("failed to read finalized state: {0}")]
    FinalizedStateReadFailed(String),
    /// The finalized epoch number does not fit in `u64`.
    #[error("finalized epoch number exceeds u64")]
    FinalizedEpochNumberExceedsU64,
    /// The finalized epoch length is zero.
    #[error("finalized epoch length is zero")]
    FinalizedEpochLengthZero,
    /// Computing the planned activation height overflowed `u64`.
    #[error("planned activation height overflow")]
    PlannedActivationHeightOverflow,
}

impl RpcInternalError {
    /// Creates [`RpcInternalError::LatestStateReadFailed`] from a displayable error.
    pub fn latest_state_read_failed(error: impl std::fmt::Display) -> Self {
        Self::LatestStateReadFailed(error.to_string())
    }

    /// Creates [`RpcInternalError::LatestBlockNumberReadFailed`] from a displayable error.
    pub fn latest_block_number_read_failed(error: impl std::fmt::Display) -> Self {
        Self::LatestBlockNumberReadFailed(error.to_string())
    }

    /// Creates [`RpcInternalError::BlockHeaderReadFailed`] from a target height and error.
    pub fn block_header_read_failed(target: u64, error: impl std::fmt::Display) -> Self {
        Self::BlockHeaderReadFailed {
            target,
            reason: error.to_string(),
        }
    }

    /// Creates [`RpcInternalError::PointReadWorkerFailed`] from a displayable error.
    pub fn point_read_worker_failed(error: impl std::fmt::Display) -> Self {
        Self::PointReadWorkerFailed(error.to_string())
    }

    /// Creates [`RpcInternalError::UpgradeExportTaskFailed`] from a displayable error.
    pub fn upgrade_export_task_failed(error: impl std::fmt::Display) -> Self {
        Self::UpgradeExportTaskFailed(error.to_string())
    }

    /// Creates [`RpcInternalError::FinalizedBlockReadFailed`] from a displayable error.
    pub fn finalized_block_read_failed(error: impl std::fmt::Display) -> Self {
        Self::FinalizedBlockReadFailed(error.to_string())
    }

    /// Creates [`RpcInternalError::FinalizedHeaderReadFailed`] from a height and error.
    pub fn finalized_header_read_failed(height: u64, error: impl std::fmt::Display) -> Self {
        Self::FinalizedHeaderReadFailed {
            height,
            reason: error.to_string(),
        }
    }

    /// Creates [`RpcInternalError::FinalizedStateReadFailed`] from a displayable error.
    pub fn finalized_state_read_failed(error: impl std::fmt::Display) -> Self {
        Self::FinalizedStateReadFailed(error.to_string())
    }
}

/// Typed invalid-parameter errors returned by Outbe JSON-RPC methods.
#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub enum RpcInvalidParamsError {
    /// The requested block height exceeds the node's advertised finalized height.
    #[error("height {height} is above the node's finalized height {finalized}")]
    HeightAboveFinalized {
        /// Caller-requested block height.
        height: u64,
        /// Node's latest advertised finalized block height.
        finalized: u64,
    },
}

/// JSON-RPC error code for retryable resource exhaustion / limit exceeded (EIP-1474).
pub const RESOURCE_EXHAUSTED_ERROR_CODE: i32 = -32005;

/// Typed resource-exhaustion errors returned with [`RESOURCE_EXHAUSTED_ERROR_CODE`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum RpcResourceExhaustedError {
    /// The bounded consensus finalization request queue is full.
    #[error("finalization service is busy; retry shortly")]
    FinalizationServiceBusy,
    /// Waiting for the consensus finalization drainer timed out.
    #[error("finalization request timed out")]
    FinalizationRequestTimedOut,
}
