//! Vote target of the Update precompile. Each action is in its own file, with
//! its payload, its checks, its execution and their tests:
//!
//! - `protocol_update`: schedules a protocol update (`scheduleUpdate`);
//! - `enclave`: revokes the enclave binding of one node;
//! - `contract_upgrade`: upgrades our UUPS contracts on the listed chains
//!   (`upgradeContracts`). It is not part of the protocol update.

pub(crate) mod contract_upgrade;
pub(crate) mod enclave;
pub(crate) mod protocol_update;

use alloy_primitives::{Address, U256};
use outbe_primitives::addresses::UPDATE_ADDRESS;
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_system_common::vote_target::{
    decode_proposal_payload, decode_stored_proposal_payload, TargetExecutionOutcome, VoteTarget,
    VoteTargetContext,
};

use serde_json::Value;

use crate::errors::UpdateError;
use crate::schema::Update;
use contract_upgrade::UpgradeContractsPayload;
use enclave::RevokeEnclaveBindingPayloadV1;
use protocol_update::ScheduleUpdatePayload;

/// Vote payload accepted by [`UpdateVoteTarget`]. The field that names the
/// action selects the variant: `operation`, `action`, or none for a protocol
/// update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateVotePayload {
    ScheduleUpdate(ScheduleUpdatePayload),
    RevokeEnclaveBindingV1(RevokeEnclaveBindingPayloadV1),
    UpgradeContracts(UpgradeContractsPayload),
}

impl UpdateVotePayload {
    pub fn from_value(payload: &Value) -> std::result::Result<Self, UpdateError> {
        let has = |key| payload.as_object().is_some_and(|obj| obj.contains_key(key));
        if has("operation") {
            UpgradeContractsPayload::from_value(payload).map(Self::UpgradeContracts)
        } else if has("action") {
            RevokeEnclaveBindingPayloadV1::from_value(payload).map(Self::RevokeEnclaveBindingV1)
        } else {
            ScheduleUpdatePayload::from_value(payload).map(Self::ScheduleUpdate)
        }
    }

    pub fn validate(&self, current_height: u64) -> std::result::Result<(), UpdateError> {
        match self {
            Self::ScheduleUpdate(payload) => payload.validate(current_height),
            Self::RevokeEnclaveBindingV1(payload) => payload.validate(),
            Self::UpgradeContracts(payload) => payload.validate(),
        }
    }
}

/// Vote target handler wired to the Update precompile address.
pub struct UpdateVoteTarget;

impl VoteTarget for UpdateVoteTarget {
    fn target_module(&self) -> Address {
        UPDATE_ADDRESS
    }

    fn validate(&self, payload: &[u8], context: VoteTargetContext) -> Result<()> {
        let payload = decode_proposal_payload(payload)?;
        UpdateVotePayload::from_value(&payload)?
            .validate(context.block_number)
            .map_err(Into::into)
    }

    fn handle_approved(
        &self,
        ctx: &BlockRuntimeContext,
        proposal_id: U256,
        raw_payload: &[u8],
        _context: VoteTargetContext,
    ) -> Result<TargetExecutionOutcome> {
        let payload = decode_stored_proposal_payload(raw_payload, "Update")?;
        let decoded = UpdateVotePayload::from_value(&payload)
            .map_err(UpdateError::stored_proposal_payload_invalid)?;
        match decoded {
            UpdateVotePayload::ScheduleUpdate(decoded) => {
                let outcome = ctx.with_checkpoint(|| {
                    protocol_update::stage_approved_update(ctx, proposal_id, &payload, &decoded)
                });
                UpdateError::into_execution_outcome(outcome)
            }
            UpdateVotePayload::RevokeEnclaveBindingV1(decoded) => {
                enclave::execute_approved_revocation(ctx, proposal_id, &decoded)
            }
            UpdateVotePayload::UpgradeContracts(_) => {
                // The tally has no sub-calls: store the payload hash for
                // `applyApproved`.
                Update::new(ctx.storage.clone()).approve_contract_upgrade(
                    proposal_id,
                    raw_payload,
                    ctx.block.block_number,
                )?;
                Ok(TargetExecutionOutcome::Applied)
            }
        }
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
        | UpdateError::TeePolicyActivationMismatch
        | UpdateError::ActiveVersionDoesNotFitU32
        | UpdateError::CannotActivateVersionAboveCeiling { .. }
        | UpdateError::UpgradeHandlerFailed { .. }
        | UpdateError::StagedTeePolicyMissingProposal { .. }
        | UpdateError::TeePolicyActivationFailed { .. }
        | UpdateError::StoredProposalPayloadInvalid(_)
        | UpdateError::EnclaveRolloutRequiresExclusiveScheduledUpgrade
        | UpdateError::InvalidActiveTeePolicy(_)
        | UpdateError::VoteTargetInvariantFailure(_)
        | UpdateError::UnexpectedAppliedClassification
        | UpdateError::UnknownApprovedUpgrade { .. }
        | UpdateError::ApprovedPayloadMismatch { .. } => {
            Err(UpdateError::VoteTargetInvariantFailure(err.to_string()).into())
        }
    }
}

pub(super) fn classify_domain_error_as_precompile(err: UpdateError) -> PrecompileError {
    UpdateError::outcome_into_precompile_error(classify_domain_error(err))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_vote_payload_keeps_schedule_updates_without_action() {
        let value = serde_json::json!({"version":"1.2","activationHeight":1000,"info":""});
        let decoded = UpdateVotePayload::from_value(&value).unwrap();
        assert!(matches!(decoded, UpdateVotePayload::ScheduleUpdate(_)));
        decoded.validate(0).unwrap();
    }

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
                Err(PrecompileError::RevertBytes(_))
            ));
        }
    }
}
