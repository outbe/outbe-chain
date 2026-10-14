use alloy_primitives::U256;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_vote::handlers::TargetExecutionOutcome;

use crate::version::format_protocol_version;
use crate::ProtocolVersion;

/// Module-local errors for upgrade storage and runtime.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum UpdateError {
    #[error("scheduled update not found")]
    ScheduledUpdateNotFound,
    #[error("scheduled update already exists for proposal id")]
    ScheduledUpdateAlreadyExists,
    #[error("activation height must be at least current height + MIN_ACTIVATION_BUFFER")]
    HeightInPast,
    #[error("invalid protocol version; expected non-zero u32 encoded as u8 major + u24 minor")]
    InvalidVersion,
    #[error("downgrade not allowed: new version must be greater than active version")]
    DowngradeNotAllowed,
    #[error("invalid vote payload")]
    InvalidPayload,
    #[error("another scheduled update already uses this activation height")]
    ActivationConflict,
    #[error("too many scheduled updates waiting for activation")]
    TooManyWaitingForActivation,
    #[error("invalid scheduled update status")]
    InvalidScheduledUpdateStatus,
    #[error("invalid canonical successor TEE policy")]
    InvalidTeePolicy,
    #[error("successor TEE policy chain identity does not match the update")]
    TeePolicyChainIdentityMismatch,
    #[error("successor TEE policy activation height does not match the update")]
    TeePolicyActivationMismatch,
    #[error("invalid canonical OCOMP successor authority")]
    InvalidOcompSuccessor,
    #[error("successor OCOMP authority chain identity does not match the update")]
    OcompSuccessorChainIdentityMismatch,
    #[error("successor OCOMP authority activation height does not match the update")]
    OcompSuccessorActivationMismatch,
    #[error("active Outbe protocol version does not fit the canonical u32 codec")]
    ActiveVersionDoesNotFitU32,
    #[error(
        "cannot activate protocol version {}: binary supports at most {}",
        format_protocol_version(*.version),
        format_protocol_version(*.ceiling)
    )]
    CannotActivateVersionAboveCeiling {
        version: ProtocolVersion,
        ceiling: ProtocolVersion,
    },
    #[error("upgrade handler '{label}' failed: {reason}")]
    UpgradeHandlerFailed { label: String, reason: String },
    #[error("staged TEE policy references missing Update proposal {proposal_id}")]
    StagedTeePolicyMissingProposal { proposal_id: U256 },
    #[error("staged OCOMP successor references missing Update proposal {proposal_id}")]
    StagedOcompSuccessorMissingProposal { proposal_id: U256 },
    #[error("TEE policy activation for Update proposal {proposal_id} failed: {reason}")]
    TeePolicyActivationFailed { proposal_id: U256, reason: String },
    #[error("stored Update proposal payload is invalid: {0}")]
    StoredProposalPayloadInvalid(String),
    #[error("enclave rollout requires an exclusive scheduled upgrade")]
    EnclaveRolloutRequiresExclusiveScheduledUpgrade,
    #[error("invalid active TEE policy: {0}")]
    InvalidActiveTeePolicy(String),
    #[error("stored Update OCOMP successor is invalid: {0}")]
    StoredOcompSuccessorInvalid(String),
    #[error("Update Vote target invariant failure: {0}")]
    VoteTargetInvariantFailure(String),
    #[error("unexpected applied Update classification")]
    UnexpectedAppliedClassification,
}

impl From<UpdateError> for PrecompileError {
    fn from(err: UpdateError) -> Self {
        match err {
            UpdateError::ActiveVersionDoesNotFitU32
            | UpdateError::CannotActivateVersionAboveCeiling { .. }
            | UpdateError::UpgradeHandlerFailed { .. }
            | UpdateError::StagedTeePolicyMissingProposal { .. }
            | UpdateError::StagedOcompSuccessorMissingProposal { .. }
            | UpdateError::TeePolicyActivationFailed { .. }
            | UpdateError::StoredProposalPayloadInvalid(_)
            | UpdateError::InvalidActiveTeePolicy(_)
            | UpdateError::StoredOcompSuccessorInvalid(_)
            | UpdateError::VoteTargetInvariantFailure(_)
            | UpdateError::UnexpectedAppliedClassification => {
                PrecompileError::Fatal(err.to_string())
            }
            _ => PrecompileError::Revert(err.to_string()),
        }
    }
}

impl UpdateError {
    /// Wraps a non-fatal upgrade handler failure as a fatal precompile error.
    pub fn wrap_upgrade_handler_error(err: PrecompileError, label: &str) -> PrecompileError {
        match err {
            PrecompileError::Fatal(message) => PrecompileError::Fatal(message),
            other => Self::UpgradeHandlerFailed {
                label: label.to_string(),
                reason: other.to_string(),
            }
            .into(),
        }
    }

    /// Wraps a non-fatal TEE policy activation error as a fatal precompile error.
    pub fn wrap_tee_policy_activation_error(
        err: PrecompileError,
        proposal_id: U256,
    ) -> PrecompileError {
        match err {
            PrecompileError::Fatal(message) => PrecompileError::Fatal(message),
            other => Self::TeePolicyActivationFailed {
                proposal_id,
                reason: other.to_string(),
            }
            .into(),
        }
    }

    /// Converts a stage result into a vote target execution outcome.
    pub fn into_execution_outcome(result: Result<()>) -> Result<TargetExecutionOutcome> {
        match result {
            Ok(()) => Ok(TargetExecutionOutcome::Applied),
            Err(PrecompileError::Revert(reason)) => Ok(TargetExecutionOutcome::Error { reason }),
            Err(err) => Err(err),
        }
    }

    /// Converts a classified target outcome into a precompile error.
    pub fn outcome_into_precompile_error(
        outcome: Result<TargetExecutionOutcome>,
    ) -> PrecompileError {
        match outcome {
            Ok(TargetExecutionOutcome::Error { reason }) => PrecompileError::Revert(reason),
            Ok(TargetExecutionOutcome::Applied) => Self::UnexpectedAppliedClassification.into(),
            Err(error) => error,
        }
    }

    /// Maps a stored proposal payload error into a fatal precompile error.
    pub fn stored_proposal_payload_invalid(error: impl std::fmt::Display) -> PrecompileError {
        Self::StoredProposalPayloadInvalid(error.to_string()).into()
    }

    /// Maps an active TEE policy error into a fatal precompile error.
    pub fn invalid_active_tee_policy(error: impl std::fmt::Display) -> PrecompileError {
        Self::InvalidActiveTeePolicy(error.to_string()).into()
    }

    /// Maps a stored OCOMP successor error into a fatal precompile error.
    pub fn stored_ocomp_successor_invalid(error: impl std::fmt::Display) -> PrecompileError {
        Self::StoredOcompSuccessorInvalid(error.to_string()).into()
    }
}

/// Startup protocol compatibility errors.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum StartupCompatibilityError {
    #[error(
        "binary protocol version {} is older than on-chain active protocol version {}; please upgrade the binary",
        format_protocol_version(*.binary),
        format_protocol_version(*.active)
    )]
    BinaryOlderThanActive {
        binary: ProtocolVersion,
        active: ProtocolVersion,
    },
}

impl StartupCompatibilityError {
    /// Checks whether the error message contains the given substring.
    pub fn contains(&self, pattern: &str) -> bool {
        self.to_string().contains(pattern)
    }
}
