//! Target-opted three-window governance. Ballots and scheduled plans have separate
//! bounded indexes; no block hook scans historical proposal ids.
use alloy_primitives::{Address, U256};
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::{PrecompileError, Result};

use crate::constants::MAX_PENDING_PROPOSALS;
use crate::handlers::{
    ProposalAction, ProposalLifecycle, TargetAdmission, TargetExecutionOutcome, VotePlanContext,
    VoteTarget, VoteTargetContext, VoteTargetRegistry,
};
use crate::notify::ProposalFinalization;
use crate::runtime::quorum_reached;
use crate::schema::{ProposalRecord, ProposalStatus, ProposalWindowRecord, Vote};
use crate::state::{calculate_vote_tally, ProposalSubmission, VoteTally};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ProposalKind {
    Standard = 0,
    Primary = 1,
    Cancel = 2,
    Replacement = 3,
    LegacyPlan = 4,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ProposalPhase {
    Voting = 0,
    Replace = 1,
    Activation = 2,
    Finished = 3,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProposalLifecycleInfo {
    pub kind: ProposalKind,
    pub phase: ProposalPhase,
    pub parent_id: U256,
    pub replace_end: u64,
    pub activation_height: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum WindowError {
    #[error("invalid three-window timing policy")]
    InvalidPolicy,
    #[error("three-window targets require validator-only zero-value admission")]
    UnsupportedAdmission,
    #[error("three-window timing cannot be overridden by the caller")]
    TimingOverride,
    #[error("linked proposal parent is not a current plan for this target")]
    InvalidParent,
    #[error("parent replacement window is closed")]
    ClosedWindow,
    #[error("too many active proposal plans")]
    TooManyPlans,
    #[error("plan completion owner or phase is invalid")]
    InvalidCompletion,
}
impl From<WindowError> for PrecompileError {
    fn from(error: WindowError) -> Self {
        Self::Revert(error.to_string())
    }
}
fn corrupt() -> PrecompileError {
    PrecompileError::Fatal("corrupt Vote lifecycle metadata".into())
}
fn add(a: u64, b: u64) -> Result<u64> {
    a.checked_add(b)
        .ok_or_else(|| WindowError::InvalidPolicy.into())
}

impl ProposalWindowRecord {
    fn info(&self) -> Result<ProposalLifecycleInfo> {
        let kind = match self.kind {
            1 => ProposalKind::Primary,
            2 => ProposalKind::Cancel,
            3 => ProposalKind::Replacement,
            4 => ProposalKind::LegacyPlan,
            _ => return Err(corrupt()),
        };
        let phase = match self.phase {
            0 => ProposalPhase::Voting,
            1 => ProposalPhase::Replace,
            2 => ProposalPhase::Activation,
            3 => ProposalPhase::Finished,
            _ => return Err(corrupt()),
        };
        if self.marker != 1
            || self.voting_window == 0
            || self.replace_window == 0
            || self.activation_window == 0
            || self.replace_end == 0
            || self.replace_end.checked_add(self.activation_window) != Some(self.activation_height)
            || (matches!(kind, ProposalKind::Cancel | ProposalKind::Replacement)
                == self.parent_id.is_zero())
            || (!self.parent_id.is_zero() && self.parent_id >= self.id)
            || (kind == ProposalKind::Cancel
                && matches!(phase, ProposalPhase::Replace | ProposalPhase::Activation))
            || (kind == ProposalKind::LegacyPlan && phase == ProposalPhase::Voting)
        {
            return Err(corrupt());
        }
        Ok(ProposalLifecycleInfo {
            kind,
            phase,
            parent_id: self.parent_id,
            replace_end: self.replace_end,
            activation_height: self.activation_height,
        })
    }
    fn plan(&self) -> VotePlanContext {
        VotePlanContext {
            replace_end: self.replace_end,
            activation_height: self.activation_height,
        }
    }
}

impl Vote<'_> {
    fn active_memberships(&self, id: U256) -> Result<u32> {
        let position = self.active_plan_positions.read(&id)?;
        if position == 0 {
            return Ok(0);
        }
        let len = self.active_plan_ids.len()?;
        if len > MAX_PENDING_PROPOSALS
            || position > len
            || self.active_plan_ids.get(position - 1)? != Some(id)
        {
            return Err(corrupt());
        }
        Ok(1)
    }

    /// Validate the bounded forward index once before any begin-block effects.
    /// Exact reverse positions also reject duplicate ids without another set.
    pub(crate) fn validate_active_plan_index(&self) -> Result<()> {
        let len = self.active_plan_ids.len()?;
        if len > MAX_PENDING_PROPOSALS {
            return Err(corrupt());
        }
        for index in 0..len {
            let id = self.active_plan_ids.get(index)?.ok_or_else(corrupt)?;
            if self.active_plan_positions.read(&id)? != index + 1
                || self.window_record(id)?.is_none()
            {
                return Err(corrupt());
            }
        }
        Ok(())
    }

    pub(crate) fn window_record(&self, id: U256) -> Result<Option<ProposalWindowRecord>> {
        let record = self.proposal_windows.get(id)?;
        let memberships = self.active_memberships(id)?;
        let Some(window) = record else {
            if memberships != 0 {
                return Err(corrupt());
            }
            return Ok(None);
        };
        let info = window.info()?;
        let proposal = self.proposals.get(id)?.ok_or_else(corrupt)?;
        let status = proposal.proposal_status().map_err(|_| corrupt())?;
        let scheduled = matches!(
            info.phase,
            ProposalPhase::Replace | ProposalPhase::Activation
        );
        if (scheduled && status != ProposalStatus::Approved)
            || (info.phase == ProposalPhase::Voting && status != ProposalStatus::Pending)
            || (info.phase == ProposalPhase::Finished && !status.is_terminal())
            || (memberships != u32::from(scheduled))
            || (info.kind == ProposalKind::Primary
                && proposal
                    .voting_deadline_height
                    .checked_add(1)
                    .and_then(|q| q.checked_add(window.replace_window))
                    != Some(window.replace_end))
            || (matches!(info.kind, ProposalKind::Cancel | ProposalKind::Replacement)
                && info.phase == ProposalPhase::Voting
                && proposal.voting_deadline_height.checked_add(1) != Some(window.replace_end))
            || (info.kind == ProposalKind::Replacement
                && scheduled
                && proposal
                    .voting_deadline_height
                    .checked_add(1)
                    .and_then(|t| t.checked_add(window.replace_window))
                    != Some(window.replace_end))
        {
            return Err(corrupt());
        }
        if info.phase == ProposalPhase::Voting && !window.parent_id.is_zero() {
            let parent = self.proposals.get(window.parent_id)?.ok_or_else(corrupt)?;
            let parent_window = self
                .proposal_windows
                .get(window.parent_id)?
                .ok_or_else(corrupt)?;
            if parent.proposal_status().map_err(|_| corrupt())? != ProposalStatus::Approved
                || parent.target_module != proposal.target_module
                || parent_window.info()?.phase != ProposalPhase::Replace
                || self.active_memberships(parent.id)? != 1
                || parent_window.replace_end != window.replace_end
                || parent_window.activation_height != window.activation_height
                || parent_window.voting_window != window.voting_window
                || parent_window.replace_window != window.replace_window
                || parent_window.activation_window != window.activation_window
            {
                return Err(corrupt());
            }
        }
        Ok(Some(window))
    }

    pub fn proposal_lifecycle(&self, id: U256) -> Result<ProposalLifecycleInfo> {
        let proposal = self
            .proposals
            .get(id)?
            .ok_or(crate::errors::VoteError::ProposalNotFound)?;
        match self.window_record(id)? {
            Some(record) => record.info(),
            None => Ok(ProposalLifecycleInfo {
                kind: ProposalKind::Standard,
                phase: if proposal.proposal_status()?.is_terminal() {
                    ProposalPhase::Finished
                } else {
                    ProposalPhase::Voting
                },
                parent_id: U256::ZERO,
                replace_end: 0,
                activation_height: 0,
            }),
        }
    }

    fn add_active_plan(&mut self, id: U256) -> Result<()> {
        if self.active_memberships(id)? != 0 {
            return Err(corrupt());
        }
        let len = self.active_plan_ids.len()?;
        if len >= MAX_PENDING_PROPOSALS {
            return Err(WindowError::TooManyPlans.into());
        }
        self.active_plan_ids.push(id)?;
        self.active_plan_positions.write(&id, len + 1)
    }

    fn remove_active_plan(&mut self, id: U256) -> Result<()> {
        if self.active_memberships(id)? != 1 {
            return Err(corrupt());
        }
        let position = self.active_plan_positions.read(&id)?;
        let len = self.active_plan_ids.len()?;
        if position != len {
            let moved = self.active_plan_ids.get(len - 1)?.ok_or_else(corrupt)?;
            if self.active_plan_positions.read(&moved)? != len {
                return Err(corrupt());
            }
            self.active_plan_ids.set(position - 1, moved)?;
            self.active_plan_positions.write(&moved, position)?;
        }
        self.active_plan_ids.pop()?;
        self.active_plan_positions.clear(&id)
    }

    fn write_window(&mut self, window: &ProposalWindowRecord, create: bool) -> Result<()> {
        window.info()?;
        if create {
            self.proposal_windows.create(window)?;
        } else {
            self.proposal_windows.update(window)?;
        }
        self.notify_proposal_phase_changed(
            window.id,
            window.phase,
            window.replace_end,
            window.activation_height,
        )
    }

    /// Called inside the admission checkpoint, so legacy adoption is rolled back
    /// with target reservations, proposal allocation, and receipt failures.
    pub(crate) fn admit_window(
        &mut self,
        submission: &ProposalSubmission<'_>,
        target: &dyn VoteTarget,
        action: ProposalAction,
    ) -> Result<Option<ProposalWindowRecord>> {
        let ProposalLifecycle::ThreeWindow {
            voting_window,
            replace_window,
            activation_window,
        } = target.lifecycle()
        else {
            if action != ProposalAction::Primary {
                return Err(WindowError::InvalidPolicy.into());
            }
            return Ok(None);
        };
        if target.admission() != TargetAdmission::ActiveValidatorOnly
            || !submission.attached_value.is_zero()
        {
            return Err(WindowError::UnsupportedAdmission.into());
        }
        if voting_window == 0 || replace_window == 0 || activation_window == 0 {
            return Err(WindowError::InvalidPolicy.into());
        }
        let id = self.peek_next_proposal_id()?;
        let mut window = ProposalWindowRecord {
            id,
            marker: 1,
            kind: ProposalKind::Primary as u8,
            phase: ProposalPhase::Voting as u8,
            parent_id: U256::ZERO,
            voting_window,
            replace_window,
            activation_window,
            replace_end: 0,
            activation_height: 0,
        };
        match action {
            ProposalAction::Primary => {
                window.replace_end = add(
                    add(add(submission.created_height, voting_window)?, 1)?,
                    replace_window,
                )?;
                window.activation_height = add(window.replace_end, activation_window)?;
            }
            ProposalAction::Cancel(parent) | ProposalAction::Replace(parent) => {
                let proposal = self
                    .proposals
                    .get(parent)?
                    .ok_or(WindowError::InvalidParent)?;
                if proposal.target_module != submission.target_module
                    || proposal.proposal_status()? != ProposalStatus::Approved
                {
                    return Err(WindowError::InvalidParent.into());
                }
                let parent_window = match self.window_record(parent)? {
                    Some(record) => record,
                    None => {
                        let plan = target
                            .existing_plan(self.storage.clone(), &proposal)?
                            .ok_or(WindowError::InvalidParent)?;
                        if submission.created_height >= plan.replace_end {
                            return Err(WindowError::ClosedWindow.into());
                        }
                        if plan.replace_end.checked_add(activation_window)
                            != Some(plan.activation_height)
                        {
                            return Err(corrupt());
                        }
                        let record = ProposalWindowRecord {
                            id: parent,
                            marker: 1,
                            kind: ProposalKind::LegacyPlan as u8,
                            phase: ProposalPhase::Replace as u8,
                            parent_id: U256::ZERO,
                            voting_window,
                            replace_window,
                            activation_window,
                            replace_end: plan.replace_end,
                            activation_height: plan.activation_height,
                        };
                        self.add_active_plan(parent)?;
                        self.write_window(&record, true)?;
                        record
                    }
                };
                if parent_window.info()?.phase != ProposalPhase::Replace {
                    return Err(WindowError::InvalidParent.into());
                }
                if submission.created_height >= parent_window.replace_end {
                    return Err(WindowError::ClosedWindow.into());
                }
                window.kind = if matches!(action, ProposalAction::Cancel(_)) {
                    ProposalKind::Cancel as u8
                } else {
                    ProposalKind::Replacement as u8
                };
                window.parent_id = parent;
                window.voting_window = parent_window.voting_window;
                window.replace_window = parent_window.replace_window;
                window.activation_window = parent_window.activation_window;
                window.replace_end = parent_window.replace_end;
                window.activation_height = parent_window.activation_height;
                if matches!(action, ProposalAction::Replace(_)) {
                    add(
                        add(window.replace_end, window.replace_window)?,
                        window.activation_window,
                    )?;
                }
            }
        }
        window.info()?;
        Ok(Some(window))
    }

    pub(crate) fn commit_window(&mut self, window: &ProposalWindowRecord) -> Result<()> {
        self.write_window(window, true)
    }

    fn finish_window(&mut self, mut window: ProposalWindowRecord) -> Result<()> {
        if matches!(
            window.info()?.phase,
            ProposalPhase::Replace | ProposalPhase::Activation
        ) {
            self.remove_active_plan(window.id)?;
        }
        window.phase = ProposalPhase::Finished as u8;
        self.write_window(&window, false)
    }

    fn cancel_window(&mut self, window: ProposalWindowRecord, by: U256) -> Result<()> {
        self.set_proposal_status(window.id, ProposalStatus::Canceled)?;
        self.notify_proposal_canceled(window.id, by)?;
        self.finish_window(window)
    }

    /// A target reports actual activation or automatic cancellation, never merely
    /// acceptance. Legacy ordinary records deliberately remain unchanged.
    pub fn complete_plan(&mut self, id: U256, target: Address, canceled: bool) -> Result<()> {
        let Some(window) = self.window_record(id)? else {
            return Ok(());
        };
        let proposal = self
            .proposals
            .get(id)?
            .ok_or(crate::errors::VoteError::ProposalNotFound)?;
        let phase = window.info()?.phase;
        if proposal.target_module != target
            || proposal.proposal_status()? != ProposalStatus::Approved
            || (!canceled && phase != ProposalPhase::Activation)
            || (canceled && !matches!(phase, ProposalPhase::Replace | ProposalPhase::Activation))
        {
            return Err(WindowError::InvalidCompletion.into());
        }
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            if canceled {
                for child in self.list_pending_proposal_ids()? {
                    if let Some(child_window) = self.window_record(child)? {
                        if child_window.parent_id == id {
                            self.cancel_window(child_window, id)?;
                        }
                    }
                }
                self.cancel_window(window, U256::ZERO)
            } else {
                self.finish_window(window)
            }
        })
    }

    fn target_context(
        &self,
        ctx: &BlockRuntimeContext,
        proposal: &ProposalRecord,
    ) -> Result<VoteTargetContext> {
        Ok(VoteTargetContext {
            proposer: proposal.proposer,
            attached_value: U256::ZERO,
            block_number: ctx.block.block_number,
            chain_id: ctx.storage.chain_id()?,
        })
    }

    fn verify_stored_action(
        &self,
        proposal: &ProposalRecord,
        window: &ProposalWindowRecord,
        target: &dyn VoteTarget,
    ) -> Result<()> {
        let expected = match window.info()?.kind {
            ProposalKind::Primary => ProposalAction::Primary,
            ProposalKind::Cancel => ProposalAction::Cancel(window.parent_id),
            ProposalKind::Replacement => ProposalAction::Replace(window.parent_id),
            ProposalKind::Standard | ProposalKind::LegacyPlan => return Err(corrupt()),
        };
        // Use immutable admission context, not the current height. A valid but
        // corrupted kind/parent must never change the action the voters endorsed.
        let action = target.proposal_action(
            proposal.payload.as_bytes(),
            VoteTargetContext {
                proposer: proposal.proposer,
                attached_value: U256::ZERO,
                block_number: proposal.created_height,
                chain_id: self.storage.chain_id()?,
            },
        )?;
        if action != expected {
            return Err(corrupt());
        }
        Ok(())
    }

    pub(crate) fn finalize_primary(
        &mut self,
        ctx: &BlockRuntimeContext,
        proposal: &ProposalRecord,
        mut window: ProposalWindowRecord,
        registry: &VoteTargetRegistry,
    ) -> Result<()> {
        self.verify_stored_action(proposal, &window, registry.lookup(proposal.target_module)?)?;
        let active = self.eligible_validator_addresses(proposal.id)?;
        let tally = calculate_vote_tally(self, proposal, &active)?;
        let count = u32::try_from(active.len()).map_err(|_| corrupt())?;
        let checkpoint = self.storage.checkpoint_guard();
        let (status, outcome) = if quorum_reached(tally.yes, count) {
            let target_checkpoint = self.storage.checkpoint_guard();
            let outcome = if self.active_plan_ids.len()? >= MAX_PENDING_PROPOSALS {
                TargetExecutionOutcome::Error {
                    reason: WindowError::TooManyPlans.to_string(),
                }
            } else {
                registry.lookup(proposal.target_module)?.register_plan(
                    ctx,
                    proposal.id,
                    proposal.payload.as_bytes(),
                    self.target_context(ctx, proposal)?,
                    window.plan(),
                )?
            };
            match outcome {
                TargetExecutionOutcome::Applied => {
                    target_checkpoint.commit();
                    (ProposalStatus::Approved, ProposalFinalization::Approved)
                }
                TargetExecutionOutcome::Error { .. } => {
                    drop(target_checkpoint);
                    (ProposalStatus::Error, ProposalFinalization::Error)
                }
            }
        } else {
            (ProposalStatus::Expired, ProposalFinalization::Expired)
        };
        self.set_proposal_status(proposal.id, status)?;
        if status == ProposalStatus::Approved {
            window.phase = ProposalPhase::Replace as u8;
            self.add_active_plan(proposal.id)?;
            self.write_window(&window, false)?;
        } else {
            self.finish_window(window)?;
        }
        self.notify_proposal_finalized(proposal, &tally, outcome)?;
        checkpoint.commit();
        Ok(())
    }

    pub(crate) fn process_plan_windows(
        &mut self,
        ctx: &BlockRuntimeContext,
        registry: &VoteTargetRegistry,
    ) -> Result<()> {
        for id in self.active_plan_ids.read_all()? {
            // Earlier windows may have shadowed this plan in the same block.
            let Some(window) = self.window_record(id)? else {
                return Err(corrupt());
            };
            if window.info()?.phase == ProposalPhase::Replace
                && ctx.block.block_number >= window.replace_end
            {
                self.close_replace_window(ctx, window, registry)?;
            }
        }
        Ok(())
    }

    fn close_replace_window(
        &mut self,
        ctx: &BlockRuntimeContext,
        parent_window: ProposalWindowRecord,
        registry: &VoteTargetRegistry,
    ) -> Result<()> {
        let parent = self.proposals.get(parent_window.id)?.ok_or_else(corrupt)?;
        let target = registry.lookup(parent.target_module)?;
        let checkpoint = self.storage.checkpoint_guard();
        let mut candidates: Vec<(ProposalRecord, ProposalWindowRecord, VoteTally, bool)> =
            Vec::new();
        for id in self.list_pending_proposal_ids()? {
            let Some(window) = self.window_record(id)? else {
                continue;
            };
            if window.parent_id != parent.id {
                continue;
            }
            let proposal = self.proposals.get(id)?.ok_or_else(corrupt)?;
            if proposal.target_module != parent.target_module
                || window.replace_end != parent_window.replace_end
                || window.activation_height != parent_window.activation_height
                || window.voting_window != parent_window.voting_window
                || window.replace_window != parent_window.replace_window
                || window.activation_window != parent_window.activation_window
                || window.info()?.phase != ProposalPhase::Voting
            {
                return Err(corrupt());
            }
            self.verify_stored_action(&proposal, &window, target)?;
            let active = self.eligible_validator_addresses(id)?;
            let tally = calculate_vote_tally(self, &proposal, &active)?;
            let quorum = quorum_reached(
                tally.yes,
                u32::try_from(active.len()).map_err(|_| corrupt())?,
            );
            candidates.push((proposal, window, tally, quorum));
        }
        candidates.sort_unstable_by_key(|candidate| candidate.0.id);
        let winner = candidates.iter().position(|candidate| candidate.3);
        let winner_proposal_id = winner.map(|index| candidates[index].0.id);
        let mut replaced = false;
        for (index, (proposal, mut window, tally, _)) in candidates.into_iter().enumerate() {
            if winner.is_some_and(|winner| index > winner) {
                self.cancel_window(window, winner_proposal_id.ok_or_else(corrupt)?)?;
                continue;
            }
            let (status, outcome) = if Some(index) == winner {
                let original_timing = (window.replace_end, window.activation_height);
                let target_checkpoint = self.storage.checkpoint_guard();
                let mut result = target.cancel_plan(ctx, parent.id, proposal.id)?;
                if result == TargetExecutionOutcome::Applied
                    && window.kind == ProposalKind::Replacement as u8
                {
                    window.replace_end = add(parent_window.replace_end, window.replace_window)?;
                    window.activation_height = add(window.replace_end, window.activation_window)?;
                    result = target.register_plan(
                        ctx,
                        proposal.id,
                        proposal.payload.as_bytes(),
                        self.target_context(ctx, &proposal)?,
                        window.plan(),
                    )?;
                }
                match result {
                    TargetExecutionOutcome::Applied => {
                        target_checkpoint.commit();
                        self.cancel_window(parent_window.clone(), proposal.id)?;
                        replaced = true;
                        (ProposalStatus::Approved, ProposalFinalization::Approved)
                    }
                    TargetExecutionOutcome::Error { .. } => {
                        drop(target_checkpoint);
                        (window.replace_end, window.activation_height) = original_timing;
                        (ProposalStatus::Error, ProposalFinalization::Error)
                    }
                }
            } else {
                (ProposalStatus::Expired, ProposalFinalization::Expired)
            };
            self.set_proposal_status(proposal.id, status)?;
            if status == ProposalStatus::Approved && window.kind == ProposalKind::Replacement as u8
            {
                window.phase = ProposalPhase::Replace as u8;
                self.add_active_plan(proposal.id)?;
                self.write_window(&window, false)?;
            } else {
                self.finish_window(window)?;
            }
            self.notify_proposal_finalized(&proposal, &tally, outcome)?;
        }
        if !replaced {
            target.prepare_plan(ctx, parent.id, parent_window.activation_height)?;
            let mut window = parent_window;
            window.phase = ProposalPhase::Activation as u8;
            self.write_window(&window, false)?;
        }
        checkpoint.commit();
        Ok(())
    }
}
