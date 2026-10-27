//! Update's three-window target: reserve at Q, prepare at T, activate at H.

use alloy_primitives::{Address, B256, U256};
use outbe_primitives::addresses::UPDATE_ADDRESS;
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;
use outbe_teeregistry::TeeRegistry;
use outbe_vote::handlers::{
    decode_stored_proposal_payload, ProposalAction, ProposalLifecycle, TargetExecutionOutcome,
    VotePlanContext, VoteTarget, VoteTargetContext,
};
use outbe_vote::schema::{ProposalRecord, ProposalStatus, Vote};

use crate::constants::MIN_ACTIVATION_BUFFER;
use crate::errors::UpdateError;
use crate::payload::{ScheduleUpdatePayload, UpdateVoteAction};
use crate::schema::{ScheduledUpdateStatus, Update, UpdatePlanRecord};

pub struct UpdateVoteTarget;

impl VoteTarget for UpdateVoteTarget {
    fn target_module(&self) -> Address {
        UPDATE_ADDRESS
    }

    fn lifecycle(&self) -> ProposalLifecycle {
        let total = outbe_chain_constants::get_governance_voting_window_blocks();
        ProposalLifecycle::ThreeWindow {
            voting_window: total / 2,
            replace_window: total - total / 2,
            activation_window: MIN_ACTIVATION_BUFFER,
        }
    }

    fn proposal_action(
        &self,
        payload: &[u8],
        _context: VoteTargetContext,
    ) -> Result<ProposalAction> {
        let action: UpdateVoteAction =
            serde_json::from_slice(payload).map_err(|_| UpdateError::InvalidPayload)?;
        if let Some(plan) = action.plan() {
            plan.validate()?;
        }
        Ok(match action {
            UpdateVoteAction::Primary(_) => ProposalAction::Primary,
            UpdateVoteAction::Cancel(cancel) => ProposalAction::Cancel(cancel.cancel_proposal_id),
            UpdateVoteAction::Replace(replacement) => {
                ProposalAction::Replace(replacement.replace_proposal_id)
            }
        })
    }

    fn validate(&self, payload: &[u8], context: VoteTargetContext) -> Result<()> {
        self.proposal_action(payload, context).map(|_| ())
    }

    fn reserve(
        &self,
        storage: StorageHandle<'_>,
        _id: U256,
        _payload: &[u8],
        _context: VoteTargetContext,
    ) -> Result<()> {
        Update::new(storage).assert_no_legacy_pending_enclave_rollout()
    }

    fn handle_approved(
        &self,
        ctx: &BlockRuntimeContext,
        id: U256,
        payload: &[u8],
        _context: VoteTargetContext,
    ) -> Result<TargetExecutionOutcome> {
        let payload = decode_stored_proposal_payload(payload, "Update")?;
        let decoded = ScheduleUpdatePayload::from_value(&payload)
            .map_err(UpdateError::stored_proposal_payload_invalid)?;
        if decoded.mrenclave.is_some() {
            return Err(legacy_enclave_cutover_error());
        }
        ctx.with_checkpoint(|| {
            let mut update = Update::new(ctx.storage.clone());
            update.assert_no_legacy_pending_enclave_rollout()?;
            match update.schedule_update_from_propose_classified(
                id,
                &payload,
                ctx.block.block_number,
            )? {
                Ok(()) => Ok(TargetExecutionOutcome::Applied),
                Err(error) => classify_domain_error(error),
            }
        })
    }

    fn register_plan(
        &self,
        ctx: &BlockRuntimeContext,
        id: U256,
        payload: &[u8],
        _context: VoteTargetContext,
        timing: VotePlanContext,
    ) -> Result<TargetExecutionOutcome> {
        let action =
            UpdateVoteAction::from_value(&decode_stored_proposal_payload(payload, "Update")?)
                .map_err(UpdateError::stored_proposal_payload_invalid)?;
        let plan = action
            .plan()
            .ok_or_else(|| invariant("cancel action cannot register a plan"))?;
        plan.validate()
            .map_err(UpdateError::stored_proposal_payload_invalid)?;
        let version = plan
            .protocol_version()
            .map_err(UpdateError::stored_proposal_payload_invalid)?;
        let mut update = Update::new(ctx.storage.clone());
        update.assert_no_legacy_pending_enclave_rollout()?;
        if timing.replace_end <= ctx.block.block_number
            || timing.activation_height <= timing.replace_end
        {
            return Err(invariant("invalid Update plan timing"));
        }
        let tee = TeeRegistry::new(ctx.storage.clone());
        if plan.mrenclave.is_some() && update.waiting_for_activation_proposal_ids.len()? != 0 {
            return Ok(TargetExecutionOutcome::Error {
                reason: UpdateError::EnclaveRolloutRequiresExclusiveScheduledUpgrade.to_string(),
            });
        }
        if let Some(measurement) = plan.mrenclave {
            if let Err(error) = tee.validate_measurement_upgrade_v1(measurement)? {
                return Ok(TargetExecutionOutcome::Error {
                    reason: error.to_string(),
                });
            }
        }
        // Validate all domain refusals before the first mutation. Provider failures
        // remain outer errors; never stringify an arbitrary storage result.
        if let Err(error) = update.validate_plan_schedule(
            id,
            version,
            timing.activation_height,
            ctx.block.block_number,
        )? {
            return classify_domain_error(error);
        }
        ctx.with_checkpoint(|| {
            update.write_plan_schedule(id, version, timing.activation_height, &plan.info)?;
            update.live_plans.create(&UpdatePlanRecord {
                proposal_id: id,
                activation_height: timing.activation_height,
                replace_end: timing.replace_end,
                mrenclave: plan.mrenclave.unwrap_or(B256::ZERO),
                prepared: false,
            })?;
            Ok(TargetExecutionOutcome::Applied)
        })
    }

    fn cancel_plan(
        &self,
        ctx: &BlockRuntimeContext,
        parent: U256,
        _by_proposal: U256,
    ) -> Result<TargetExecutionOutcome> {
        let mut update = Update::new(ctx.storage.clone());
        let Some(scheduled) = update.read_scheduled_update(parent)? else {
            return Err(invariant("missing parent Update plan"));
        };
        if scheduled.status != ScheduledUpdateStatus::Scheduled {
            return Ok(TargetExecutionOutcome::Error {
                reason: "Update parent is no longer scheduled".into(),
            });
        }
        let replace_end = if let Some(metadata) = update.live_plans.get(parent)? {
            let lifecycle = Vote::new(ctx.storage.clone()).proposal_lifecycle(parent)?;
            if metadata.activation_height != scheduled.activation_height
                || metadata.activation_height != lifecycle.activation_height
                || metadata.replace_end != lifecycle.replace_end
                || (metadata.prepared && lifecycle.phase != outbe_vote::ProposalPhase::Activation)
            {
                return Err(invariant(
                    "canceling Update metadata differs from its Vote phase",
                ));
            }
            if metadata.prepared {
                return Ok(TargetExecutionOutcome::Error {
                    reason: "prepared Update plan cannot be canceled".into(),
                });
            }
            metadata.replace_end
        } else {
            let proposal = Vote::new(ctx.storage.clone())
                .proposals
                .get(parent)?
                .ok_or_else(|| invariant("legacy cancellation has no Vote record"))?;
            self.existing_plan(ctx.storage.clone(), &proposal)?
                .ok_or_else(|| invariant("missing legacy cancellation plan"))?
                .replace_end
        };
        if replace_end == 0 || ctx.block.block_number > replace_end {
            return Ok(TargetExecutionOutcome::Error {
                reason: "Update replacement window is closed".into(),
            });
        }
        ctx.with_checkpoint(|| {
            // Vote owns its own phase/status mutation after this callback succeeds.
            update.cancel_plan_effects(parent)?;
            Ok(TargetExecutionOutcome::Applied)
        })
    }

    fn prepare_plan(
        &self,
        ctx: &BlockRuntimeContext,
        id: U256,
        activation_height: u64,
    ) -> Result<()> {
        let mut update = Update::new(ctx.storage.clone());
        update.assert_no_legacy_pending_enclave_rollout()?;
        let scheduled = update
            .read_scheduled_update(id)?
            .ok_or_else(|| invariant("missing preparing Update plan"))?;
        if scheduled.status != ScheduledUpdateStatus::Scheduled
            || scheduled.activation_height != activation_height
        {
            return Err(invariant("preparing Update plan does not match schedule"));
        }
        let Some(mut metadata) = update.live_plans.get(id)? else {
            // Missing metadata is valid only for a verified historical software plan.
            let proposal = Vote::new(ctx.storage.clone())
                .proposals
                .get(id)?
                .ok_or_else(|| invariant("preparing legacy plan has no Vote record"))?;
            let payload = ScheduleUpdatePayload::from_value(&decode_stored_proposal_payload(
                proposal.payload.as_bytes(),
                "Update",
            )?)
            .map_err(UpdateError::stored_proposal_payload_invalid)?;
            if payload.mrenclave.is_some() {
                return Err(legacy_enclave_cutover_error());
            }
            if payload.activation_height != activation_height
                || payload.protocol_version()? != scheduled.version
            {
                return Err(invariant(
                    "legacy preparing payload differs from scheduled plan",
                ));
            }
            return Ok(());
        };
        if metadata.activation_height != activation_height
            || ctx.block.block_number < metadata.replace_end
        {
            return Err(invariant(
                "Update preparation before replace end or at wrong H",
            ));
        }
        if metadata.prepared {
            if Vote::new(ctx.storage.clone()).proposal_lifecycle(id)?.phase
                != outbe_vote::ProposalPhase::Activation
            {
                return Err(invariant(
                    "prepared Update has not entered activation phase",
                ));
            }
            return Ok(());
        }
        if ctx.block.block_number != metadata.replace_end {
            return Err(invariant("missed Update preparation boundary"));
        }
        ctx.with_checkpoint(|| {
            if !metadata.mrenclave.is_zero() {
                let mut tee = TeeRegistry::new(ctx.storage.clone());
                let predecessor = tee
                    .active_policy_v1()?
                    .policy_hash()
                    .map_err(UpdateError::invalid_active_tee_policy)?;
                tee.stage_measurement_upgrade_v1(
                    id,
                    metadata.mrenclave,
                    predecessor,
                    activation_height,
                )?;
            }
            metadata.prepared = true;
            update.live_plans.update(&metadata)
        })
    }

    fn existing_plan(
        &self,
        storage: StorageHandle<'_>,
        proposal: &ProposalRecord,
    ) -> Result<Option<VotePlanContext>> {
        let mut update = Update::new(storage.clone());
        update.assert_no_legacy_pending_enclave_rollout()?;
        if proposal.target_module != UPDATE_ADDRESS
            || proposal.proposal_status()? != ProposalStatus::Approved
        {
            return Ok(None);
        }
        if update.live_plans.get(proposal.id)?.is_some() {
            return Ok(None);
        }
        let Some(scheduled) = update.read_scheduled_update(proposal.id)? else {
            return Ok(None);
        };
        if scheduled.status != ScheduledUpdateStatus::Scheduled {
            return Ok(None);
        }
        if matches!(
            Vote::new(storage).proposal_lifecycle(proposal.id)?.kind,
            outbe_vote::ProposalKind::Primary | outbe_vote::ProposalKind::Replacement
        ) {
            return Err(invariant(
                "live Update cannot be reinterpreted as a legacy plan",
            ));
        }
        let payload = ScheduleUpdatePayload::from_value(&decode_stored_proposal_payload(
            proposal.payload.as_bytes(),
            "Update",
        )?)
        .map_err(UpdateError::stored_proposal_payload_invalid)?;
        if payload.mrenclave.is_some() {
            return Err(legacy_enclave_cutover_error());
        }
        if payload.activation_height != scheduled.activation_height
            || payload.protocol_version()? != scheduled.version
        {
            return Err(invariant("legacy Update schedule differs from payload"));
        }
        // Older testnet plans could use less notice than the current buffer.
        // They keep H, but have no remaining replacement interval.
        let replace_end = scheduled
            .activation_height
            .saturating_sub(MIN_ACTIVATION_BUFFER);
        Ok(Some(VotePlanContext {
            replace_end,
            activation_height: scheduled.activation_height,
        }))
    }
}

impl Update<'_> {
    /// Coordinated cutover precondition. Successful admission is persisted once;
    /// callers must invoke this inside their outer storage checkpoint.
    pub fn assert_no_legacy_pending_enclave_rollout(&mut self) -> Result<()> {
        if self.three_window_cutover_checked.read()? {
            return Ok(());
        }
        let tee = TeeRegistry::new(self.storage.clone());
        if let Some((id, _)) = tee.staged_successor_policy_v1()? {
            if self.live_plans.get(id)?.is_none() {
                return Err(legacy_enclave_cutover_error());
            }
        }
        let vote = Vote::new(self.storage.clone());
        for id in self.list_waiting_for_activation_proposal_ids()? {
            if self.live_plans.get(id)?.is_some() {
                continue;
            }
            if let Some(record) = vote.proposals.get(id)? {
                if record.target_module != UPDATE_ADDRESS {
                    return Err(invariant("queued Update has wrong Vote target"));
                }
                let legacy = ScheduleUpdatePayload::from_value(&decode_stored_proposal_payload(
                    record.payload.as_bytes(),
                    "Update",
                )?)
                .map_err(UpdateError::stored_proposal_payload_invalid)?;
                if legacy.mrenclave.is_some() {
                    return Err(legacy_enclave_cutover_error());
                }
            }
        }
        for id in vote.list_pending_proposal_ids()? {
            let record = vote
                .proposals
                .get(id)?
                .ok_or_else(|| invariant("missing pending Vote record at cutover"))?;
            if record.target_module != UPDATE_ADDRESS {
                continue;
            }
            let payload = decode_stored_proposal_payload(record.payload.as_bytes(), "Update")?;
            if payload.get("activationHeight").is_some() {
                let legacy = ScheduleUpdatePayload::from_value(&payload)
                    .map_err(UpdateError::stored_proposal_payload_invalid)?;
                if legacy.mrenclave.is_some() {
                    return Err(legacy_enclave_cutover_error());
                }
            }
        }
        self.three_window_cutover_checked.write(true)
    }
}

fn legacy_enclave_cutover_error() -> PrecompileError {
    PrecompileError::Fatal("OIP-00331 cutover requires no pending legacy enclave rollout".into())
}

fn invariant(reason: &str) -> PrecompileError {
    PrecompileError::Fatal(format!("Update plan invariant: {reason}"))
}

fn classify_domain_error(error: UpdateError) -> Result<TargetExecutionOutcome> {
    match error {
        UpdateError::HeightInPast
        | UpdateError::DowngradeNotAllowed
        | UpdateError::ActivationConflict
        | UpdateError::TooManyWaitingForActivation
        | UpdateError::EnclaveRolloutRequiresExclusiveScheduledUpgrade => {
            Ok(TargetExecutionOutcome::Error {
                reason: error.to_string(),
            })
        }
        other => Err(PrecompileError::Fatal(format!(
            "Update target invariant: {other}"
        ))),
    }
}
