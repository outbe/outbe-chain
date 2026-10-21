use crate::precompile::IUpdate;
use alloy_primitives::Bytes;
use alloy_primitives::U256;
use alloy_sol_types::SolError;
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
    #[error("TEE policy activation for Update proposal {proposal_id} failed: {reason}")]
    TeePolicyActivationFailed { proposal_id: U256, reason: String },
    #[error("stored Update proposal payload is invalid: {0}")]
    StoredProposalPayloadInvalid(String),
    #[error("enclave rollout requires an exclusive scheduled upgrade")]
    EnclaveRolloutRequiresExclusiveScheduledUpgrade,
    #[error("invalid active TEE policy: {0}")]
    InvalidActiveTeePolicy(String),
    #[error("Update Vote target invariant failure: {0}")]
    VoteTargetInvariantFailure(String),
    #[error("unexpected applied Update classification")]
    UnexpectedAppliedClassification,
}

impl From<UpdateError> for PrecompileError {
    fn from(error: UpdateError) -> Self {
        match error {
            UpdateError::ScheduledUpdateNotFound => PrecompileError::RevertBytes(Bytes::from(
                IUpdate::ScheduledUpdateNotFound {}.abi_encode(),
            )),
            UpdateError::ScheduledUpdateAlreadyExists => PrecompileError::RevertBytes(Bytes::from(
                IUpdate::ScheduledUpdateAlreadyExists {}.abi_encode(),
            )),
            UpdateError::HeightInPast => {
                PrecompileError::RevertBytes(Bytes::from(IUpdate::HeightInPast {}.abi_encode()))
            }
            UpdateError::InvalidVersion => {
                PrecompileError::RevertBytes(Bytes::from(IUpdate::InvalidVersion {}.abi_encode()))
            }
            UpdateError::DowngradeNotAllowed => PrecompileError::RevertBytes(Bytes::from(
                IUpdate::DowngradeNotAllowed {}.abi_encode(),
            )),
            UpdateError::InvalidPayload => {
                PrecompileError::RevertBytes(Bytes::from(IUpdate::InvalidPayload {}.abi_encode()))
            }
            UpdateError::ActivationConflict => PrecompileError::RevertBytes(Bytes::from(
                IUpdate::ActivationConflict {}.abi_encode(),
            )),
            UpdateError::TooManyWaitingForActivation => PrecompileError::RevertBytes(Bytes::from(
                IUpdate::TooManyWaitingForActivation {}.abi_encode(),
            )),
            UpdateError::InvalidScheduledUpdateStatus => PrecompileError::RevertBytes(Bytes::from(
                IUpdate::InvalidScheduledUpdateStatus {}.abi_encode(),
            )),
            UpdateError::InvalidTeePolicy => {
                PrecompileError::RevertBytes(Bytes::from(IUpdate::InvalidTeePolicy {}.abi_encode()))
            }
            UpdateError::TeePolicyChainIdentityMismatch => PrecompileError::RevertBytes(
                Bytes::from(IUpdate::TeePolicyChainIdentityMismatch {}.abi_encode()),
            ),
            UpdateError::TeePolicyActivationMismatch => PrecompileError::RevertBytes(Bytes::from(
                IUpdate::TeePolicyActivationMismatch {}.abi_encode(),
            )),
            UpdateError::ActiveVersionDoesNotFitU32 => PrecompileError::RevertBytes(Bytes::from(
                IUpdate::ActiveVersionDoesNotFitU32 {}.abi_encode(),
            )),
            error @ UpdateError::CannotActivateVersionAboveCeiling {
                version: _,
                ceiling: _,
            } => PrecompileError::Fatal(error.to_string()),
            UpdateError::UpgradeHandlerFailed { label, reason } => {
                PrecompileError::RevertBytes(Bytes::from(
                    IUpdate::UpgradeHandlerFailed {
                        label: label.to_string(),
                        reason: reason.to_string(),
                    }
                    .abi_encode(),
                ))
            }
            UpdateError::StagedTeePolicyMissingProposal { proposal_id } => {
                PrecompileError::RevertBytes(Bytes::from(
                    IUpdate::StagedTeePolicyMissingProposal { proposal_id }.abi_encode(),
                ))
            }
            UpdateError::TeePolicyActivationFailed {
                proposal_id,
                reason,
            } => PrecompileError::RevertBytes(Bytes::from(
                IUpdate::TeePolicyActivationFailed {
                    proposal_id,
                    reason: reason.to_string(),
                }
                .abi_encode(),
            )),
            UpdateError::StoredProposalPayloadInvalid(value) => {
                PrecompileError::RevertBytes(Bytes::from(
                    IUpdate::StoredProposalPayloadInvalid {
                        value: value.to_string(),
                    }
                    .abi_encode(),
                ))
            }
            UpdateError::EnclaveRolloutRequiresExclusiveScheduledUpgrade => {
                PrecompileError::RevertBytes(Bytes::from(
                    IUpdate::EnclaveRolloutRequiresExclusiveScheduledUpgrade {}.abi_encode(),
                ))
            }
            UpdateError::InvalidActiveTeePolicy(value) => {
                PrecompileError::RevertBytes(Bytes::from(
                    IUpdate::InvalidActiveTeePolicy {
                        value: value.to_string(),
                    }
                    .abi_encode(),
                ))
            }
            UpdateError::VoteTargetInvariantFailure(value) => {
                PrecompileError::RevertBytes(Bytes::from(
                    IUpdate::VoteTargetInvariantFailure {
                        value: value.to_string(),
                    }
                    .abi_encode(),
                ))
            }
            UpdateError::UnexpectedAppliedClassification => PrecompileError::RevertBytes(
                Bytes::from(IUpdate::UnexpectedAppliedClassification {}.abi_encode()),
            ),
        }
    }
}

impl UpdateError {
    /// Wraps a non-fatal upgrade handler failure as a precompile error.
    pub fn wrap_upgrade_handler_error(err: PrecompileError, label: &str) -> PrecompileError {
        match err {
            err if err.is_node_local() => err,
            PrecompileError::Fatal(message) => PrecompileError::Fatal(message),
            other => Self::UpgradeHandlerFailed {
                label: label.to_string(),
                reason: other.to_string(),
            }
            .into(),
        }
    }

    /// Wraps a non-fatal TEE policy activation error as a precompile error.
    pub fn wrap_tee_policy_activation_error(
        err: PrecompileError,
        proposal_id: U256,
    ) -> PrecompileError {
        match err {
            err if err.is_node_local() => err,
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
            Err(PrecompileError::RevertBytes(reason)) => Ok(TargetExecutionOutcome::Error {
                reason: alloy_primitives::hex::encode_prefixed(reason),
            }),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded(err: UpdateError) -> Bytes {
        match PrecompileError::from(err) {
            PrecompileError::RevertBytes(bytes) => bytes,
            other => panic!("expected typed revert bytes, got {other:?}"),
        }
    }

    #[test]
    fn caller_revert_keeps_string_fields_and_bytes_pass_into_the_target_outcome() {
        let bytes = encoded(UpdateError::UpgradeHandlerFailed {
            label: "hook".to_string(),
            reason: "rejected".to_string(),
        });
        let decoded = IUpdate::UpgradeHandlerFailed::abi_decode(&bytes).expect("handler payload");
        assert_eq!(decoded.label, "hook");
        assert_eq!(decoded.reason, "rejected");

        let bytes = encoded(UpdateError::StoredProposalPayloadInvalid(
            "invalid vote payload".to_string(),
        ));
        let decoded =
            IUpdate::StoredProposalPayloadInvalid::abi_decode(&bytes).expect("payload text");
        assert_eq!(decoded.value, "invalid vote payload");

        let outcome =
            UpdateError::into_execution_outcome(Err(PrecompileError::RevertBytes(bytes.clone())))
                .expect("typed revert becomes a target error");
        let TargetExecutionOutcome::Error { reason } = outcome else {
            panic!("expected target error");
        };
        assert_eq!(reason, alloy_primitives::hex::encode_prefixed(&bytes));

        let fatal = PrecompileError::from(UpdateError::CannotActivateVersionAboveCeiling {
            version: ProtocolVersion::from_raw(2),
            ceiling: ProtocolVersion::from_raw(1),
        });
        assert!(matches!(
            UpdateError::into_execution_outcome(Err(fatal)),
            Err(PrecompileError::Fatal(_))
        ));
    }
}
