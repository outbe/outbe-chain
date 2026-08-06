//! Vote target-module handler for scheduling protocol updates.

use alloy_primitives::{Address, U256};
use outbe_primitives::addresses::UPDATE_ADDRESS;
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_teeregistry::TeeRegistry;
use outbe_vote::handlers::{TargetExecutionOutcome, VoteTarget, VoteTargetContext};
use serde_json::Value;

use crate::errors::UpdateError;
use crate::payload::{validate_schedule_update_json, ScheduleUpdatePayload};
use crate::schema::Update;

/// Vote target handler wired to the Update precompile address.
pub struct UpdateVoteTarget;

impl VoteTarget for UpdateVoteTarget {
    fn target_module(&self) -> Address {
        UPDATE_ADDRESS
    }

    fn validate(&self, payload: &[u8], context: VoteTargetContext) -> Result<()> {
        let payload: Value = serde_json::from_slice(payload)
            .map_err(|_| PrecompileError::Revert("invalid proposal payload".into()))?;
        validate_schedule_update_json(&payload, context.block_number, context.chain_id)
            .map_err(Into::into)
    }

    fn handle_approved(
        &self,
        ctx: &BlockRuntimeContext,
        proposal_id: U256,
        payload: &[u8],
        _context: VoteTargetContext,
    ) -> Result<TargetExecutionOutcome> {
        let payload: Value = serde_json::from_slice(payload).map_err(|_| {
            PrecompileError::Fatal("stored Update proposal payload is invalid".into())
        })?;
        let decoded = ScheduleUpdatePayload::from_value(&payload).map_err(|err| {
            PrecompileError::Fatal(format!("stored Update proposal payload is invalid: {err}"))
        })?;
        match Update::new(ctx.storage.clone()).schedule_update_from_propose_classified(
            proposal_id,
            &payload,
            ctx.block.block_number,
        )? {
            Ok(()) => {
                if let Some(policy) = decoded.tee_policy().map_err(|err| {
                    PrecompileError::Fatal(format!(
                        "stored Update successor TEE policy is invalid: {err}"
                    ))
                })? {
                    if let Err(err) = TeeRegistry::new(ctx.storage.clone())
                        .stage_successor_policy_v1(proposal_id, &policy)
                    {
                        return classify_tee_policy_stage_error(err);
                    }
                }
                Ok(TargetExecutionOutcome::Applied)
            }
            Err(err) => classify_domain_error(err),
        }
    }
}

fn classify_tee_policy_stage_error(err: PrecompileError) -> Result<TargetExecutionOutcome> {
    match err {
        PrecompileError::Revert(reason) => Ok(TargetExecutionOutcome::Error { reason }),
        other => Err(other),
    }
}

fn classify_domain_error(err: UpdateError) -> Result<TargetExecutionOutcome> {
    match err {
        UpdateError::HeightInPast
        | UpdateError::DowngradeNotAllowed
        | UpdateError::ActivationConflict
        | UpdateError::TooManyWaitingForActivation => Ok(TargetExecutionOutcome::Error {
            reason: err.to_string(),
        }),
        UpdateError::ScheduledUpdateNotFound
        | UpdateError::ScheduledUpdateAlreadyExists
        | UpdateError::InvalidVersion
        | UpdateError::InvalidPayload
        | UpdateError::InvalidScheduledUpdateStatus
        | UpdateError::InvalidTeePolicy
        | UpdateError::TeePolicyChainIdentityMismatch
        | UpdateError::TeePolicyActivationMismatch => Err(PrecompileError::Fatal(format!(
            "Update Vote target invariant failure: {err}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_expected_execution_conflicts_become_proposal_error() {
        for err in [
            UpdateError::HeightInPast,
            UpdateError::DowngradeNotAllowed,
            UpdateError::ActivationConflict,
            UpdateError::TooManyWaitingForActivation,
        ] {
            assert!(matches!(
                classify_domain_error(err).unwrap(),
                TargetExecutionOutcome::Error { .. }
            ));
        }
    }

    #[test]
    fn invariant_and_persisted_state_errors_remain_fatal() {
        for err in [
            UpdateError::ScheduledUpdateNotFound,
            UpdateError::ScheduledUpdateAlreadyExists,
            UpdateError::InvalidVersion,
            UpdateError::InvalidPayload,
            UpdateError::InvalidScheduledUpdateStatus,
            UpdateError::InvalidTeePolicy,
            UpdateError::TeePolicyChainIdentityMismatch,
            UpdateError::TeePolicyActivationMismatch,
        ] {
            assert!(matches!(
                classify_domain_error(err),
                Err(PrecompileError::Fatal(_))
            ));
        }
    }
}
