//! ABI surface and EVM dispatch for the Vote precompile.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{SolCall, SolInterface};

use outbe_primitives::dispatch::{
    dispatch_call, mutate, mutate_void, reject_value_unless_payable, view,
};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::api::{
    escrowed_bond_total, get_proposal, get_proposal_bond, get_proposal_voters, list_proposals,
    list_proposals_by_status,
};
use crate::errors::VoteError;
use crate::handlers::VoteTargetRegistry;
use crate::schema::{BondState, Vote};
use crate::state::{ProposalInfo, ProposalStatus, ProposalSubmission, VoteTally};

pub use crate::abi::IVote;

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[
    IVote::createProposalCall::SELECTOR,
    IVote::createProposalWithVotingWindowCall::SELECTOR,
];

/// Dispatches an ABI-encoded call to the Vote precompile.
pub fn dispatch_with_handlers(
    storage: StorageHandle<'_>,
    data: &[u8],
    caller: Address,
    value: U256,
    registry: &VoteTargetRegistry,
) -> Result<Bytes> {
    // Vote is a payable route, so the boundary credits value to this address.
    // Every selector that the module does not publish refuses the value here.
    reject_value_unless_payable(data, PAYABLE_SELECTORS, &value)?;
    dispatch_call(data, IVote::IVoteCalls::abi_decode, |call| {
        dispatch_vote_call(storage, call, caller, value, registry)
    })
}

fn dispatch_vote_call(
    storage: StorageHandle<'_>,
    call: IVote::IVoteCalls,
    caller: Address,
    value: U256,
    registry: &VoteTargetRegistry,
) -> Result<Bytes> {
    let mut governance = Vote::new(storage.clone());
    use IVote::IVoteCalls::*;
    match call {
        createProposal(c) => mutate(&storage, c, caller, |sender, c| {
            let block_number = storage.block_number()?;
            let submission =
                ProposalSubmission::new(sender, c.targetModule, &c.payload, block_number)
                    .with_attached_value(value);
            governance.create_proposal_with_value(submission, registry)
        }),
        createProposalWithVotingWindow(c) => mutate(&storage, c, caller, |sender, c| {
            let block_number = storage.block_number()?;
            let submission =
                ProposalSubmission::new(sender, c.targetModule, &c.payload, block_number)
                    .with_attached_value(value);
            governance.create_proposal_with_voting_window(
                submission,
                c.votingWindowBlocks,
                registry,
            )
        }),
        castVote(c) => mutate_void(&storage, c, caller, |sender, c| {
            let block_number = storage.block_number()?;
            governance.cast_vote_approve(c.proposalId, sender, c.approve, block_number)
        }),
        getProposal(c) => view(c, |c| {
            let info =
                get_proposal(storage.clone(), c.proposalId)?.ok_or(VoteError::ProposalNotFound)?;
            Ok(proposal_info_return(&info))
        }),
        getProposalVoters(c) => view(c, |c| {
            get_proposal_voters(storage.clone(), c.proposalId, c.index, c.count)
        }),
        listProposals(c) => view(c, |c| list_proposals(storage.clone(), c.index, c.count)),
        listProposalsByStatus(c) => view(c, |c| {
            list_proposals_by_status(
                storage.clone(),
                proposal_status_from_abi(c.status)?,
                c.index,
                c.count,
            )
        }),
        getProposalBond(c) => view(c, |c| {
            let bond = get_proposal_bond(storage.clone(), c.proposalId)?;
            Ok(IVote::getProposalBondReturn {
                amount: bond.amount,
                state: bond_state_to_abi(bond.state),
            })
        }),
        escrowedBondTotal(c) => view(c, |_| escrowed_bond_total(storage.clone())),
    }
}

fn proposal_info_return(info: &ProposalInfo) -> IVote::ProposalInfo {
    IVote::ProposalInfo {
        proposalId: info.id,
        proposer: info.proposer,
        targetModule: info.target_module,
        payload: info.payload.clone(),
        createdHeight: info.created_height,
        votingDeadlineHeight: info.voting_deadline_height,
        status: proposal_status_to_abi(info.status),
        state: vote_tally_return(&info.state),
        votersCount: U256::from(info.voters_count),
    }
}

fn vote_tally_return(tally: &VoteTally) -> IVote::VoteTally {
    IVote::VoteTally {
        yes: tally.yes,
        no: tally.no,
    }
}

fn proposal_status_to_abi(status: ProposalStatus) -> IVote::ProposalStatus {
    match status {
        ProposalStatus::Pending => IVote::ProposalStatus::Pending,
        ProposalStatus::Approved => IVote::ProposalStatus::Approved,
        ProposalStatus::Expired => IVote::ProposalStatus::Expired,
        ProposalStatus::Error => IVote::ProposalStatus::Error,
    }
}

fn proposal_status_from_abi(status: IVote::ProposalStatus) -> Result<ProposalStatus> {
    match status {
        IVote::ProposalStatus::Pending => Ok(ProposalStatus::Pending),
        IVote::ProposalStatus::Approved => Ok(ProposalStatus::Approved),
        IVote::ProposalStatus::Expired => Ok(ProposalStatus::Expired),
        IVote::ProposalStatus::Error => Ok(ProposalStatus::Error),
        _ => Err(VoteError::InvalidProposalStatus.into()),
    }
}

fn bond_state_to_abi(state: BondState) -> IVote::BondState {
    match state {
        BondState::NoBond => IVote::BondState::NoBond,
        BondState::Escrowed => IVote::BondState::Escrowed,
        BondState::Refunded => IVote::BondState::Refunded,
        BondState::Burned => IVote::BondState::Burned,
    }
}
