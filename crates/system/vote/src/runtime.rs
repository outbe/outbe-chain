use alloy_primitives::{Address, U256};
use outbe_primitives::addresses::VOTE_ADDRESS;
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::Result;
use outbe_primitives::stablecoin_fork::MAX_PENDING_PUBLIC_BONDED_PROPOSALS;
use outbe_primitives::storage::StorageHandle;
use outbe_validatorset::contract::ValidatorSet;
use outbe_validatorset::logic::status;

use crate::constants::{
    MAX_PENDING_PROPOSALS, MAX_PENDING_PROPOSALS_PER_VALIDATOR, QUORUM_DENOMINATOR,
    QUORUM_NUMERATOR, VOTING_WINDOW_BLOCKS,
};
use crate::errors::VoteError;
use crate::handlers::{
    self, TargetAdmission, TargetExecutionOutcome, VoteTargetContext, VoteTargetRegistry,
};
use crate::notify::ProposalFinalization;
use crate::schema::{BondSettlement, Vote};
use crate::state::{
    active_validator_addresses, calculate_vote_tally, ProposalBond, ProposalStatus, VoteKind,
    VoteTally,
};

const LOCALNET_CHAIN_ID: u64 = 54_322_345;

/// First block on chain `54_322_345` whose begin-zone finalizes a proposal as
/// soon as it holds a quorum.
///
/// Early finalization changes what `process_begin_block` writes, so a node
/// replaying history must apply it exactly where the producers did. Blocks
/// before this height were built under the deadline-only rule — proposal 1
/// reached quorum at 234_610 and was only approved at 235_507, the first block
/// the validators produced with this code. Without the gate a re-executing node
/// approves it at 234_611 and dies on a receipt-root mismatch.
const EARLY_QUORUM_FINALIZATION_HEIGHT: u64 = 235_507;

/// Whether the quorum-triggered finalization rule is in force at `block_number`.
fn early_quorum_finalization_active(chain_id: u64, block_number: u64) -> bool {
    chain_id != LOCALNET_CHAIN_ID || block_number >= EARLY_QUORUM_FINALIZATION_HEIGHT
}

fn voting_window_blocks(chain_id: u64) -> u64 {
    if chain_id != LOCALNET_CHAIN_ID {
        return VOTING_WINDOW_BLOCKS;
    }

    std::env::var("OUTBE_TEST_VOTING_WINDOW_BLOCKS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(VOTING_WINDOW_BLOCKS)
}

/// Returns `Ok(())` when `caller` is a registered validator with `status == ACTIVE`.
pub fn ensure_active_validator(storage: StorageHandle<'_>, caller: Address) -> Result<()> {
    let vs = ValidatorSet::new(storage);
    if !matches!(vs.get_validator(caller)?, Some(record) if record.status == status::ACTIVE) {
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

        let voting_deadline = current_height.saturating_add(voting_window_blocks(chain_id));
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

    /// Tally proposals that have reached quorum or whose voting windows have
    /// closed.
    ///
    /// Transitions `Pending` -> `Approved` | `Expired` | `Error`. Dispatches the
    /// tally outcome to the registered target-module handler in the same pass.
    ///
    /// A proposal that already holds a `yes` quorum is finalized immediately
    /// rather than sitting `Pending` for the rest of the voting window: the
    /// outcome cannot change, since only active validators vote and none may
    /// vote twice.
    ///
    /// Cost per block is one `calculate_vote_tally` per pending proposal, i.e.
    /// one `proposal_voters` read each. Both factors are bounded:
    /// `MAX_PENDING_PROPOSALS` proposals, and a voter list that cannot exceed
    /// the active validator set.
    pub fn process_begin_block(
        &mut self,
        ctx: &BlockRuntimeContext,
        registry: &VoteTargetRegistry,
    ) -> Result<()> {
        let block_number = ctx.block.block_number;
        let pending_ids = self.list_pending_proposal_ids()?;
        if pending_ids.is_empty() {
            return Ok(());
        }
        let early = early_quorum_finalization_active(self.storage.chain_id()?, block_number);
        // The validator set is the same for every proposal in this block, so it
        // is read once instead of per proposal.
        let active = active_validator_addresses(self.storage.clone())?;
        let active_count = ValidatorSet::new(self.storage.clone()).active_validator_count()?;
        for proposal_id in pending_ids {
            let Some(proposal) = self.proposals.get(proposal_id)? else {
                return Err(VoteError::ProposalNotFound.into());
            };
            if proposal.proposal_status()? != ProposalStatus::Pending {
                continue;
            }
            let tally = calculate_vote_tally(self, &proposal, &active)?;
            let deadline_passed = block_number > proposal.voting_deadline_height;
            // Finalizing early is only safe on a reached quorum: below it
            // `finalize_voting` records `Expired`, which would bury a proposal
            // that is still open for votes.
            if deadline_passed || (early && quorum_reached(tally.yes, active_count)) {
                self.finalize_voting(ctx, proposal_id, registry, &tally, active_count)?;
            }
        }
        Ok(())
    }

    fn finalize_voting(
        &mut self,
        ctx: &BlockRuntimeContext,
        proposal_id: U256,
        registry: &VoteTargetRegistry,
        tally: &VoteTally,
        active_count: u32,
    ) -> Result<()> {
        let proposal = self
            .proposals
            .get(proposal_id)?
            .ok_or(VoteError::ProposalNotFound)?;
        if proposal.proposal_status()? != ProposalStatus::Pending {
            return Ok(());
        }

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
        let outcome = match target_outcome {
            TargetExecutionOutcome::Applied => {
                target_checkpoint.commit();
                self.set_proposal_status(proposal_id, status)?;
                self.settle_terminal_bond(proposal_id, proposal.proposer, bond, status)?;
                match status {
                    ProposalStatus::Approved => ProposalFinalization::Approved,
                    ProposalStatus::Expired => ProposalFinalization::Expired,
                    ProposalStatus::Pending | ProposalStatus::Rejected | ProposalStatus::Error => {
                        unreachable!()
                    }
                }
            }
            TargetExecutionOutcome::Error { reason: _ } => {
                drop(target_checkpoint);
                self.set_proposal_status(proposal_id, ProposalStatus::Error)?;
                ProposalFinalization::Error
            }
        };

        self.notify_proposal_finalized(&proposal, tally, outcome)?;
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
            ProposalStatus::Approved => {
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
            ProposalStatus::Pending | ProposalStatus::Rejected | ProposalStatus::Error => {
                unreachable!("only Approved or Expired can settle a proposal bond")
            }
        }
    }
}
