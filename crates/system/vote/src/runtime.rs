use alloy_primitives::{Address, U256};
use outbe_primitives::addresses::VOTE_ADDRESS;
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::Result;
use outbe_primitives::stablecoin_fork::MAX_PENDING_PUBLIC_BONDED_PROPOSALS;
use outbe_primitives::storage::StorageHandle;
use outbe_validatorset::contract::ValidatorSet;

use crate::constants::{
    MAX_PENDING_PROPOSALS, MAX_PENDING_PROPOSALS_PER_VALIDATOR, QUORUM_DENOMINATOR,
    QUORUM_NUMERATOR,
};
use crate::errors::VoteError;
use crate::handlers::{
    self, TargetAdmission, TargetExecutionOutcome, VoteTargetContext, VoteTargetRegistry,
};
use crate::notify::ProposalFinalization;
use crate::schema::{BondSettlement, Vote};
use crate::state::{
    active_validator_addresses, calculate_vote_tally, ProposalBond, ProposalStatus, VoteKind,
};

/// Returns `Ok(())` when `caller` is a registered validator with `status == ACTIVE`.
pub fn ensure_active_validator(storage: StorageHandle<'_>, caller: Address) -> Result<()> {
    let vs = ValidatorSet::new(storage);
    if !vs.validator_lifecycle(caller)?.is_active_status() {
        return Err(VoteError::NotValidator.into());
    }
    Ok(())
}

/// Returns `Ok(())` when `caller` is a registered validator with `status in {PENDING, ACTIVE}`.
pub fn ensure_voting_validator(storage: StorageHandle<'_>, caller: Address) -> Result<()> {
    let vs = ValidatorSet::new(storage);
    let lifecycle = vs.validator_lifecycle(caller)?;
    if !lifecycle.is_active_status() && !lifecycle.is_pending() {
        return Err(VoteError::NotValidator.into());
    }
    Ok(())
}

/// Returns `true` when `yes_votes` reaches the configured 2/3 quorum.
pub const fn quorum_reached(yes_votes: u64, active_validator_count: u32) -> bool {
    if active_validator_count == 0 {
        return false;
    }
    let yes = yes_votes as u128;
    let active = active_validator_count as u128;
    yes * QUORUM_DENOMINATOR as u128 >= active * QUORUM_NUMERATOR as u128
}

impl Vote<'_> {
    /// Creates a pending generic proposal.
    pub fn create_proposal(
        &mut self,
        proposer: Address,
        target_module: Address,
        payload: &str,
        current_height: u64,
        registry: &VoteTargetRegistry,
    ) -> Result<U256> {
        self.create_proposal_with_value(
            proposer,
            target_module,
            payload,
            current_height,
            U256::ZERO,
            registry,
        )
    }

    /// Creates a proposal with the exact native value permitted by its
    /// compile-time target admission class.
    pub fn create_proposal_with_value(
        &mut self,
        proposer: Address,
        target_module: Address,
        payload: &str,
        current_height: u64,
        attached_value: U256,
        registry: &VoteTargetRegistry,
    ) -> Result<U256> {
        // Preserve the legacy calculation, including saturation, during replay.
        let deadline = current_height
            .saturating_add(outbe_chain_constants::get_governance_voting_window_blocks());
        self.create_proposal_with_deadline(
            proposer,
            target_module,
            payload,
            current_height,
            attached_value,
            deadline,
            registry,
        )
    }

    /// Creates a proposal with an immutable, author-selected voting duration.
    #[allow(clippy::too_many_arguments)]
    pub fn create_proposal_with_voting_window(
        &mut self,
        proposer: Address,
        target_module: Address,
        payload: &str,
        current_height: u64,
        attached_value: U256,
        voting_window_blocks: u64,
        registry: &VoteTargetRegistry,
    ) -> Result<U256> {
        let maximum = outbe_chain_constants::get_governance_voting_window_blocks();
        if voting_window_blocks == 0 || voting_window_blocks > maximum {
            return Err(VoteError::InvalidVotingWindow {
                actual: voting_window_blocks,
                maximum,
            }
            .into());
        }
        let deadline = current_height
            .checked_add(voting_window_blocks)
            .ok_or(VoteError::VotingDeadlineOverflow)?;
        self.create_proposal_with_deadline(
            proposer,
            target_module,
            payload,
            current_height,
            attached_value,
            deadline,
            registry,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn create_proposal_with_deadline(
        &mut self,
        proposer: Address,
        target_module: Address,
        payload: &str,
        current_height: u64,
        attached_value: U256,
        voting_deadline: u64,
        registry: &VoteTargetRegistry,
    ) -> Result<U256> {
        let chain_id = self.storage.chain_id()?;
        let target = registry.lookup(target_module)?;
        let admission = target.admission();
        match admission {
            TargetAdmission::ActiveValidatorOnly => {
                if !attached_value.is_zero() {
                    return Err(VoteError::InvalidProposalBond {
                        expected: U256::ZERO,
                        actual: attached_value,
                    }
                    .into());
                }
                ensure_active_validator(self.storage.clone(), proposer)?;
            }
            TargetAdmission::PublicBonded { amount } => {
                if attached_value != amount {
                    return Err(VoteError::InvalidProposalBond {
                        expected: amount,
                        actual: attached_value,
                    }
                    .into());
                }
            }
        }

        let pending_len = self.pending_proposal_ids.len()?;
        if pending_len >= MAX_PENDING_PROPOSALS {
            return Err(VoteError::TooManyPending.into());
        }

        match admission {
            TargetAdmission::ActiveValidatorOnly => {
                let proposer_pending = self.pending_proposal_count_by_proposer(proposer)?;
                if proposer_pending >= MAX_PENDING_PROPOSALS_PER_VALIDATOR {
                    return Err(VoteError::TooManyPendingByValidator.into());
                }
            }
            TargetAdmission::PublicBonded { .. } => {
                let (public_total, public_by_proposer) =
                    self.pending_public_bonded_counts(registry, proposer)?;
                if public_total >= MAX_PENDING_PUBLIC_BONDED_PROPOSALS {
                    return Err(VoteError::TooManyPendingPublicBonded.into());
                }
                if public_by_proposer > 0 {
                    return Err(VoteError::TooManyPendingPublicBondedByProposer.into());
                }
            }
        }

        let target_context = VoteTargetContext {
            proposer,
            attached_value,
            block_number: current_height,
            chain_id,
        };
        handlers::validate_target_payload(
            registry,
            target_module,
            payload.as_bytes(),
            target_context,
        )?;

        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            let proposal_id = self.write_proposal(
                proposer,
                target_module,
                payload,
                current_height,
                voting_deadline,
                ProposalStatus::Pending,
            )?;
            handlers::reserve_target_proposal(
                registry,
                storage.clone(),
                target_module,
                proposal_id,
                payload.as_bytes(),
                target_context,
            )?;
            if let TargetAdmission::PublicBonded { amount } = admission {
                self.record_proposal_bond(proposal_id, amount)?;
                let liabilities = self.bond_liabilities()?;
                let balance = storage.balance(VOTE_ADDRESS)?;
                if balance < liabilities {
                    return Err(VoteError::BondLiabilityInvariant {
                        balance,
                        liabilities,
                    }
                    .into());
                }
                self.notify_proposal_bond_escrowed(proposal_id, proposer, amount)?;
            }
            self.notify_proposal_created(
                proposal_id,
                proposer,
                target_module,
                payload,
                voting_deadline,
            )?;
            Ok(proposal_id)
        })
    }

    fn pending_public_bonded_counts(
        &self,
        registry: &VoteTargetRegistry,
        proposer: Address,
    ) -> Result<(u32, u32)> {
        let mut total = 0u32;
        let mut by_proposer = 0u32;
        for proposal_id in self.list_pending_proposal_ids()? {
            let proposal = self
                .proposals
                .get(proposal_id)?
                .ok_or(VoteError::ProposalNotFound)?;
            if matches!(
                registry.lookup(proposal.target_module)?.admission(),
                TargetAdmission::PublicBonded { .. }
            ) {
                total = total.saturating_add(1);
                if proposal.proposer == proposer {
                    by_proposer = by_proposer.saturating_add(1);
                }
            }
        }
        Ok((total, by_proposer))
    }

    /// ABI entry: `castVote(uint256 proposalId, bool approve)`.
    pub fn cast_vote_approve(
        &mut self,
        proposal_id: U256,
        voter: Address,
        approve: bool,
        block_number: u64,
    ) -> Result<()> {
        ensure_active_validator(self.storage.clone(), voter)?;

        let proposal = self
            .proposals
            .get(proposal_id)?
            .ok_or(VoteError::ProposalNotFound)?;
        if proposal.proposal_status()? != ProposalStatus::Pending {
            return Err(VoteError::NotPending.into());
        }
        if block_number > proposal.voting_deadline_height {
            return Err(VoteError::VotingClosed.into());
        }
        if self.read_vote(proposal_id, voter)?.is_some() {
            return Err(VoteError::AlreadyVoted.into());
        }

        self.write_vote(
            proposal_id,
            voter,
            VoteKind::from_approve(approve),
            block_number,
        )?;
        self.notify_vote_cast(proposal_id, voter, approve)?;
        Ok(())
    }

    /// Tallies proposals whose voting windows are closed.
    ///
    /// Transitions `Pending` -> `Approved` | `Expired` | `Error`, all terminal.
    /// Dispatches the tally outcome to the registered target-module handler in
    /// the same pass and settles the proposal bond with the status.
    pub fn process_begin_block(
        &mut self,
        ctx: &BlockRuntimeContext,
        registry: &VoteTargetRegistry,
    ) -> Result<()> {
        let block_number = ctx.block.block_number;
        let pending_ids = self.list_pending_proposal_ids()?;
        for proposal_id in pending_ids {
            let Some(proposal) = self.proposals.get(proposal_id)? else {
                return Err(VoteError::ProposalNotFound.into());
            };
            match proposal.proposal_status()? {
                ProposalStatus::Pending if block_number > proposal.voting_deadline_height => {
                    self.finalize_voting(ctx, proposal_id, registry)?;
                }
                // An earlier binary persisted this Error proposal in the pending
                // vector and left its bond unsettled. Release it once.
                ProposalStatus::Error => {
                    self.settle_legacy_error(proposal_id, proposal.proposer)?;
                }
                ProposalStatus::Pending
                | ProposalStatus::Approved
                | ProposalStatus::Rejected
                | ProposalStatus::Expired => {}
            }
        }
        Ok(())
    }

    /// Closes an Error proposal that an earlier binary left in the pending
    /// vector. The proposal leaves the bounded pending caps, and this function
    /// refunds its still-escrowed bond exactly once. This function does not
    /// execute the target again and does not announce a finalization. The
    /// admission caps already bound the pending-vector walk.
    fn settle_legacy_error(&mut self, proposal_id: U256, proposer: Address) -> Result<()> {
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.remove_pending_proposal_id(proposal_id)?;
            let bond = self.proposal_bond(proposal_id)?;
            if bond.settlement == BondSettlement::Unsettled {
                self.settle_terminal_bond(proposal_id, proposer, bond, ProposalStatus::Error)?;
            }
            Ok(())
        })
    }

    fn finalize_voting(
        &mut self,
        ctx: &BlockRuntimeContext,
        proposal_id: U256,
        registry: &VoteTargetRegistry,
    ) -> Result<()> {
        let proposal = self
            .proposals
            .get(proposal_id)?
            .ok_or(VoteError::ProposalNotFound)?;
        if proposal.proposal_status()? != ProposalStatus::Pending {
            return Ok(());
        }

        let active = active_validator_addresses(self.storage.clone())?;
        let tally = calculate_vote_tally(self, &proposal, &active)?;
        let vs = ValidatorSet::new(self.storage.clone());
        let active_count = vs.active_validator_count()?;
        let status = if quorum_reached(tally.yes, active_count) {
            ProposalStatus::Approved
        } else {
            ProposalStatus::Expired
        };
        let bond = self.proposal_bond(proposal_id)?;

        let finalization_checkpoint = self.storage.checkpoint_guard();
        let target_checkpoint = self.storage.checkpoint_guard();
        let target_outcome = handlers::handle_target_tally(
            registry,
            ctx,
            proposal_id,
            &proposal,
            bond.amount,
            status,
        )?;
        let (status, outcome) = match target_outcome {
            TargetExecutionOutcome::Applied => {
                target_checkpoint.commit();
                let outcome = match status {
                    ProposalStatus::Approved => ProposalFinalization::Approved,
                    ProposalStatus::Expired => ProposalFinalization::Expired,
                    ProposalStatus::Pending | ProposalStatus::Rejected | ProposalStatus::Error => {
                        return Err(VoteError::InvalidProposalStatus.into());
                    }
                };
                (status, outcome)
            }
            // A target-declared execution failure. Dropping the target checkpoint
            // drops the effects of the failure. The proposal is terminal in
            // `Error`. The code below refunds the proposer's bond in full
            // exactly once.
            TargetExecutionOutcome::Error { reason: _ } => {
                drop(target_checkpoint);
                (ProposalStatus::Error, ProposalFinalization::Error)
            }
        };
        self.set_proposal_status(proposal_id, status)?;
        self.settle_terminal_bond(proposal_id, proposal.proposer, bond, status)?;

        self.notify_proposal_finalized(&proposal, &tally, outcome)?;
        finalization_checkpoint.commit();
        Ok(())
    }

    fn settle_terminal_bond(
        &mut self,
        proposal_id: U256,
        owner: Address,
        bond: ProposalBond,
        status: ProposalStatus,
    ) -> Result<()> {
        match bond.settlement {
            BondSettlement::NoBond => return Ok(()),
            BondSettlement::Unsettled => {}
            BondSettlement::Refunded | BondSettlement::Burned => {
                return Err(outbe_primitives::error::PrecompileError::Fatal(format!(
                    "pending proposal {proposal_id} has an already settled bond"
                )));
            }
        }

        match status {
            // Approved and target-execution Error both return the escrow to the
            // proposer once. Only an expired vote burns it.
            ProposalStatus::Approved | ProposalStatus::Error => {
                self.storage
                    .transfer_balance(VOTE_ADDRESS, owner, bond.amount)?;
                self.settle_proposal_bond_accounting(proposal_id, BondSettlement::Refunded)?;
                self.notify_proposal_bond_refunded(proposal_id, owner, bond.amount)
            }
            ProposalStatus::Expired => {
                self.storage.decrease_balance(VOTE_ADDRESS, bond.amount)?;
                self.settle_proposal_bond_accounting(proposal_id, BondSettlement::Burned)?;
                self.notify_proposal_bond_burned(proposal_id, owner, bond.amount)
            }
            ProposalStatus::Pending | ProposalStatus::Rejected => {
                Err(VoteError::InvalidProposalStatus.into())
            }
        }
    }
}
