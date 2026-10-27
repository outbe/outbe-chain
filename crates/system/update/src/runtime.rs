use alloy_primitives::{B256, U256};
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_teeregistry::TeeRegistry;
use serde_json::Value;

use crate::constants::{
    max_activatable_version, MAX_WAITING_FOR_ACTIVATION_UPDATES, MIN_ACTIVATION_BUFFER,
};
use crate::errors::UpdateError;
use crate::handlers::UpgradeHandlerRegistry;
use crate::payload::decode_schedule_update_json;
use crate::precompile::IUpdate;
use crate::schema::{ScheduledUpdateStatus, Update};
use crate::ProtocolVersion;

struct ScheduledUpdateRequest {
    version: ProtocolVersion,
    activation_height: u64,
    info: String,
}

impl Update<'_> {
    /// Schedules an update from an approved vote proposal payload.
    pub fn schedule_update_from_propose(
        &mut self,
        proposal_id: U256,
        payload: &Value,
        current_height: u64,
    ) -> Result<()> {
        self.schedule_update_from_propose_classified(proposal_id, payload, current_height)?
            .map_err(Into::into)
    }

    /// Schedules an approved Vote payload while preserving the boundary between
    /// deterministic domain rejection and storage/provider failure.
    pub(crate) fn schedule_update_from_propose_classified(
        &mut self,
        proposal_id: U256,
        payload: &Value,
        current_height: u64,
    ) -> Result<std::result::Result<(), UpdateError>> {
        let (version, activation_height, info) = match decode_schedule_update_json(payload) {
            Ok(decoded) => decoded,
            Err(err) => return Ok(Err(err)),
        };
        self.schedule_update_from_propose_fields_classified(
            proposal_id,
            ScheduledUpdateRequest {
                version,
                activation_height,
                info,
            },
            current_height,
        )
    }

    fn schedule_update_from_propose_fields_classified(
        &mut self,
        proposal_id: U256,
        request: ScheduledUpdateRequest,
        current_height: u64,
    ) -> Result<std::result::Result<(), UpdateError>> {
        let ScheduledUpdateRequest {
            version,
            activation_height,
            info,
        } = request;
        if let Err(error) = self.validate_new_upgrade(proposal_id, version)? {
            return Ok(Err(error));
        }
        if let Err(error) = self.validate_activation_slot(activation_height, current_height)? {
            return Ok(Err(error));
        }

        self.write_plan_schedule(proposal_id, version, activation_height, &info)?;
        Ok(Ok(()))
    }

    pub(crate) fn validate_plan_schedule(
        &self,
        proposal_id: U256,
        version: ProtocolVersion,
        activation_height: u64,
        current_height: u64,
    ) -> Result<std::result::Result<(), UpdateError>> {
        if let Err(error) = self.validate_new_upgrade(proposal_id, version)? {
            return Ok(Err(error));
        }
        self.validate_activation_slot(activation_height, current_height)
    }

    pub(crate) fn write_plan_schedule(
        &mut self,
        proposal_id: U256,
        version: ProtocolVersion,
        activation_height: u64,
        info: &str,
    ) -> Result<()> {
        self.write_scheduled_update(proposal_id, version, activation_height, info)?;
        self.emit(IUpdate::ScheduledUpdateCreated {
            proposalId: proposal_id,
            version: version.raw(),
            activationHeight: activation_height,
            info: info.as_bytes().to_vec().into(),
        })?;
        Ok(())
    }

    fn validate_new_upgrade(
        &self,
        proposal_id: U256,
        version: ProtocolVersion,
    ) -> Result<std::result::Result<(), UpdateError>> {
        if self.read_scheduled_update(proposal_id)?.is_some() {
            return Ok(Err(UpdateError::ScheduledUpdateAlreadyExists));
        }

        if version.is_zero() {
            return Ok(Err(UpdateError::InvalidVersion));
        }

        let active = self.get_active_version()?;
        if version <= active {
            return Ok(Err(UpdateError::DowngradeNotAllowed));
        }

        // Every scheduling path, including surviving legacy ballots, must honor
        // an enclave plan's reservation before its TEE successor has been staged.
        for pending in self.list_waiting_for_activation_proposal_ids()? {
            let scheduled = self
                .read_scheduled_update(pending)?
                .ok_or(UpdateError::ScheduledUpdateNotFound)?;
            if scheduled.status != ScheduledUpdateStatus::Scheduled {
                return Err(PrecompileError::Fatal(
                    "non-scheduled Update queue entry".into(),
                ));
            }
            if let Some(plan) = self.live_plans.get(pending)? {
                if !plan.mrenclave.is_zero() {
                    return Ok(Err(
                        UpdateError::EnclaveRolloutRequiresExclusiveScheduledUpgrade,
                    ));
                }
            } else {
                let vote = outbe_vote::Vote::new(self.storage.clone());
                if vote.proposals.get(pending)?.is_some()
                    && matches!(
                        vote.proposal_lifecycle(pending)?.kind,
                        outbe_vote::ProposalKind::Primary | outbe_vote::ProposalKind::Replacement
                    )
                {
                    return Err(PrecompileError::Fatal(
                        "live Update reservation is missing metadata".into(),
                    ));
                }
            }
        }
        let tee = TeeRegistry::new(self.storage.clone());
        if tee.staged_successor_policy_v1()?.is_some() || tee.strict_upgrade_pending_v1()? {
            return Ok(Err(
                UpdateError::EnclaveRolloutRequiresExclusiveScheduledUpgrade,
            ));
        }

        Ok(Ok(()))
    }

    fn validate_activation_slot(
        &self,
        activation_height: u64,
        current_height: u64,
    ) -> Result<std::result::Result<(), UpdateError>> {
        let min_activation = current_height.saturating_add(MIN_ACTIVATION_BUFFER);
        if activation_height < min_activation {
            return Ok(Err(UpdateError::HeightInPast));
        }

        if self.scheduled_activation_conflict(activation_height)? {
            return Ok(Err(UpdateError::ActivationConflict));
        }

        let waiting_len = self.waiting_for_activation_proposal_ids.len()?;
        if waiting_len >= MAX_WAITING_FOR_ACTIVATION_UPDATES {
            return Ok(Err(UpdateError::TooManyWaitingForActivation));
        }

        Ok(Ok(()))
    }

    /// Activates scheduled updates at the current block height using `registry`.
    /// Existing schedules activate at their recorded height; admission notice is not reapplied.
    pub fn process_begin_block_with_handlers(
        &mut self,
        ctx: &BlockRuntimeContext,
        registry: &UpgradeHandlerRegistry,
    ) -> Result<()> {
        self.assert_no_legacy_pending_enclave_rollout()?;
        let block_number = ctx.block.block_number;
        let waiting_ids = self.list_waiting_for_activation_proposal_ids()?;
        for proposal_id in waiting_ids {
            let Some(scheduled) = self.read_scheduled_update(proposal_id)? else {
                return Err(UpdateError::ScheduledUpdateNotFound.into());
            };
            if scheduled.status == ScheduledUpdateStatus::Scheduled
                && block_number >= scheduled.activation_height
            {
                self.activate_scheduled_update(ctx, registry, proposal_id)?;
            }
        }
        Ok(())
    }

    fn activate_scheduled_update(
        &mut self,
        ctx: &BlockRuntimeContext,
        registry: &UpgradeHandlerRegistry,
        proposal_id: U256,
    ) -> Result<()> {
        let scheduled = self
            .read_scheduled_update(proposal_id)?
            .ok_or(UpdateError::ScheduledUpdateNotFound)?;
        if scheduled.status != ScheduledUpdateStatus::Scheduled {
            return Ok(());
        }
        let active = self.get_active_version()?;
        if scheduled.version <= active {
            return ctx.with_checkpoint(|| {
                self.cancel_plan_effects(proposal_id)?;
                outbe_vote::schema::Vote::new(self.storage.clone()).complete_plan(
                    proposal_id,
                    outbe_primitives::addresses::UPDATE_ADDRESS,
                    true,
                )
            });
        }

        let ceiling = max_activatable_version(self.storage.chain_id()?);
        if scheduled.version > ceiling {
            return Err(UpdateError::CannotActivateVersionAboveCeiling {
                version: scheduled.version,
                ceiling,
            }
            .into());
        }

        let metadata = self.live_plans.get(proposal_id)?;
        if let Some(metadata) = &metadata {
            if !metadata.prepared || metadata.activation_height != scheduled.activation_height {
                return Err(outbe_primitives::error::PrecompileError::Fatal(
                    "Update activation requires preparation at replace end".into(),
                ));
            }
        } else {
            let vote = outbe_vote::schema::Vote::new(self.storage.clone());
            if vote.proposals.get(proposal_id)?.is_some()
                && matches!(
                    vote.proposal_lifecycle(proposal_id)?.kind,
                    outbe_vote::ProposalKind::Primary | outbe_vote::ProposalKind::Replacement
                )
            {
                return Err(outbe_primitives::error::PrecompileError::Fatal(
                    "live Update activation is missing plan metadata".into(),
                ));
            }
        }

        ctx.with_checkpoint(|| {
            for handler in registry.lookup(scheduled.version) {
                handler
                    .handle(ctx, &scheduled)
                    .map_err(|err| UpdateError::wrap_upgrade_handler_error(err, handler.label()))?;
            }

            self.reconcile_staged_tee_policy(
                ctx,
                &scheduled,
                metadata
                    .as_ref()
                    .and_then(|plan| (!plan.mrenclave.is_zero()).then_some(plan.mrenclave)),
            )?;

            self.set_active_version(scheduled.version, scheduled.activation_height)?;
            self.set_scheduled_update_status(proposal_id, ScheduledUpdateStatus::Activated)?;
            self.emit(IUpdate::UpgradeActivated {
                version: scheduled.version.raw(),
                activationHeight: scheduled.activation_height,
            })?;
            self.cancel_outdated_waiting_updates(scheduled.version)?;
            outbe_vote::schema::Vote::new(self.storage.clone()).complete_plan(
                proposal_id,
                outbe_primitives::addresses::UPDATE_ADDRESS,
                false,
            )
        })
    }

    fn reconcile_staged_tee_policy(
        &mut self,
        ctx: &BlockRuntimeContext,
        activating: &crate::state::ScheduledUpdateInfo,
        expected_measurement: Option<B256>,
    ) -> Result<()> {
        let mut tee_registry = TeeRegistry::new(self.storage.clone());
        let Some((staged_proposal_id, policy)) = tee_registry.staged_successor_policy_v1()? else {
            if expected_measurement.is_some() {
                return Err(PrecompileError::Fatal(
                    "prepared enclave Update lost its staged policy".into(),
                ));
            }
            return Ok(());
        };
        if let Some(measurement) = expected_measurement {
            if staged_proposal_id != activating.proposal_id
                || policy.activation_height != activating.activation_height
                || policy.measurement_rules.len() != 1
                || policy.measurement_rules[0].mrenclave != measurement
            {
                return Err(PrecompileError::Fatal(
                    "prepared enclave Update policy mismatch".into(),
                ));
            }
        }
        if staged_proposal_id == activating.proposal_id {
            return tee_registry
                .promote_staged_successor_policy_v1(staged_proposal_id, ctx.block.block_number)
                .map_err(|err| {
                    UpdateError::wrap_tee_policy_activation_error(err, staged_proposal_id)
                });
        }

        let staged_update = self.read_scheduled_update(staged_proposal_id)?.ok_or(
            UpdateError::StagedTeePolicyMissingProposal {
                proposal_id: staged_proposal_id,
            },
        )?;
        if staged_update.status != ScheduledUpdateStatus::Scheduled
            || staged_update.version <= activating.version
        {
            tee_registry
                .discard_staged_successor_policy_v1(staged_proposal_id)
                .map_err(|err| {
                    UpdateError::wrap_tee_policy_activation_error(err, staged_proposal_id)
                })?;
        }
        Ok(())
    }

    pub(crate) fn cancel_plan_effects(&mut self, proposal_id: U256) -> Result<()> {
        let scheduled = self
            .read_scheduled_update(proposal_id)?
            .ok_or(UpdateError::ScheduledUpdateNotFound)?;
        if scheduled.status != ScheduledUpdateStatus::Scheduled {
            return Ok(());
        }

        let mut tee = TeeRegistry::new(self.storage.clone());
        if tee
            .staged_successor_policy_v1()?
            .is_some_and(|(id, _)| id == proposal_id)
        {
            tee.discard_staged_successor_policy_v1(proposal_id)?;
        }

        self.set_scheduled_update_status(proposal_id, ScheduledUpdateStatus::Canceled)?;
        self.emit(IUpdate::UpgradeCanceled {
            proposalId: proposal_id,
            version: scheduled.version.raw(),
            activationHeight: scheduled.activation_height,
        })
    }

    fn cancel_outdated_waiting_updates(
        &mut self,
        active_version: crate::ProtocolVersion,
    ) -> Result<()> {
        let waiting_ids = self.list_waiting_for_activation_proposal_ids()?;
        for proposal_id in waiting_ids {
            let Some(scheduled) = self.read_scheduled_update(proposal_id)? else {
                continue;
            };
            if scheduled.status == ScheduledUpdateStatus::Scheduled
                && scheduled.version <= active_version
            {
                self.cancel_plan_effects(proposal_id)?;
                outbe_vote::schema::Vote::new(self.storage.clone()).complete_plan(
                    proposal_id,
                    outbe_primitives::addresses::UPDATE_ADDRESS,
                    true,
                )?;
            }
        }
        Ok(())
    }

    fn scheduled_activation_conflict(&self, activation_height: u64) -> Result<bool> {
        for proposal_id in self.list_waiting_for_activation_proposal_ids()? {
            let Some(scheduled) = self.read_scheduled_update(proposal_id)? else {
                continue;
            };
            if scheduled.status == ScheduledUpdateStatus::Scheduled
                && scheduled.activation_height == activation_height
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
