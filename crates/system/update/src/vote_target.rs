//! Vote target-module handler for scheduling protocol updates.

use alloy_primitives::{Address, U256};
use outbe_ocompregistry::OcompRegistry;
use outbe_primitives::addresses::UPDATE_ADDRESS;
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_teeregistry::TeeRegistry;
use outbe_vote::handlers::{
    decode_proposal_payload, decode_stored_proposal_payload, TargetExecutionOutcome, VoteTarget,
    VoteTargetContext,
};
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
        let payload = decode_proposal_payload(payload)?;
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
        let payload = decode_stored_proposal_payload(payload, "Update")?;
        let decoded = ScheduleUpdatePayload::from_value(&payload).map_err(|err| {
            PrecompileError::Fatal(format!("stored Update proposal payload is invalid: {err}"))
        })?;
        let outcome =
            ctx.with_checkpoint(|| stage_approved_update(ctx, proposal_id, &payload, &decoded));
        match outcome {
            Ok(()) => Ok(TargetExecutionOutcome::Applied),
            Err(PrecompileError::Revert(reason)) => Ok(TargetExecutionOutcome::Error { reason }),
            Err(err) => Err(err),
        }
    }
}

fn stage_approved_update(
    ctx: &BlockRuntimeContext,
    proposal_id: U256,
    payload: &Value,
    decoded: &ScheduleUpdatePayload,
) -> Result<()> {
    decoded
        .validate_measurement_upgrade()
        .map_err(PrecompileError::from)?;
    let mut tee_registry = TeeRegistry::new(ctx.storage.clone());
    if tee_registry.strict_upgrade_pending_v1()?
        || (decoded.mrenclave.is_some()
            && !Update::new(ctx.storage.clone())
                .list_waiting_for_activation_proposal_ids()?
                .is_empty())
    {
        return Err(PrecompileError::Revert(
            "enclave rollout requires an exclusive scheduled upgrade".into(),
        ));
    }
    match Update::new(ctx.storage.clone()).schedule_update_from_propose_classified(
        proposal_id,
        payload,
        ctx.block.block_number,
    )? {
        Ok(()) => {}
        Err(err) => return Err(classify_domain_error_as_precompile(err)),
    }
    stage_measurement_upgrade(&mut tee_registry, proposal_id, decoded)?;
    stage_ocomp_successor(ctx, proposal_id, decoded)?;
    Ok(())
}

fn stage_measurement_upgrade(
    tee_registry: &mut TeeRegistry<'_>,
    proposal_id: U256,
    decoded: &ScheduleUpdatePayload,
) -> Result<()> {
    if let Some(mrenclave) = decoded.mrenclave {
        // Bind the rollout to the policy active when the vote is approved.
        // Callers supply only the successor measurement.
        let predecessor = tee_registry
            .active_policy_v1()?
            .policy_hash()
            .map_err(|err| PrecompileError::Fatal(format!("invalid active TEE policy: {err}")))?;
        tee_registry.stage_measurement_upgrade_v1(
            proposal_id,
            mrenclave,
            predecessor,
            decoded.activation_height,
        )?;
    }
    Ok(())
}

fn stage_ocomp_successor(
    ctx: &BlockRuntimeContext,
    proposal_id: U256,
    decoded: &ScheduleUpdatePayload,
) -> Result<()> {
    if let Some(successor) = decoded.ocomp_successor().map_err(|err| {
        PrecompileError::Fatal(format!("stored Update OCOMP successor is invalid: {err}"))
    })? {
        OcompRegistry::new(ctx.storage.clone()).stage_successor(
            proposal_id,
            &successor,
            &outbe_ocompregistry::poc_schema_limits(),
        )?;
    }
    Ok(())
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
        | UpdateError::TeePolicyActivationMismatch
        | UpdateError::InvalidOcompSuccessor
        | UpdateError::OcompSuccessorChainIdentityMismatch
        | UpdateError::OcompSuccessorActivationMismatch => Err(PrecompileError::Fatal(format!(
            "Update Vote target invariant failure: {err}"
        ))),
    }
}

fn classify_domain_error_as_precompile(err: UpdateError) -> PrecompileError {
    match classify_domain_error(err) {
        Ok(TargetExecutionOutcome::Error { reason }) => PrecompileError::Revert(reason),
        Ok(TargetExecutionOutcome::Applied) => {
            PrecompileError::Fatal("unexpected applied Update classification".into())
        }
        Err(error) => error,
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
            UpdateError::InvalidOcompSuccessor,
            UpdateError::OcompSuccessorChainIdentityMismatch,
            UpdateError::OcompSuccessorActivationMismatch,
        ] {
            assert!(matches!(
                classify_domain_error(err),
                Err(PrecompileError::Fatal(_))
            ));
        }
    }
}
