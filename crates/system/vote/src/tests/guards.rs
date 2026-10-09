use alloy_primitives::{address, Address, U256};

use outbe_primitives::stablecoin_fork::{
    MAX_PENDING_PUBLIC_BONDED_PROPOSALS, STABLECOIN_V1_ABSOLUTE_VOTE_PENDING_CAP,
};

use crate::constants::{MAX_PENDING_PROPOSALS, MAX_PENDING_PROPOSALS_PER_VALIDATOR};
use crate::schema::ProposalStatus;
use crate::schema::Vote;
use crate::state::{active_validator_addresses, calculate_vote_tally, VoteTally};

use super::{
    approve_in_order, assert_proposal_view, assert_reverts_with, create_update_proposal,
    proposal_status, register_active_validator, register_pending_validator, with_governance,
    with_update_proposal, with_vote, VoteTestExt, PENDING_VOTER, PROPOSER, VOTER_A, VOTER_B,
};

const OUTSIDER: Address = address!("0xdeaddeaddeaddeaddeaddeaddeaddeaddeaddead");

fn extra_validator_addr(index: u32) -> Address {
    let mut bytes = [0u8; 20];
    bytes[0] = (index >> 8) as u8;
    bytes[1] = (index & 0xff) as u8;
    Address::from(bytes)
}

#[test]
fn stablecoin_public_cap_preserves_48_vote_slots() {
    const {
        assert!(MAX_PENDING_PROPOSALS == STABLECOIN_V1_ABSOLUTE_VOTE_PENDING_CAP);
        assert!(MAX_PENDING_PROPOSALS - MAX_PENDING_PUBLIC_BONDED_PROPOSALS == 48);
    }
}

#[test]
fn create_proposal_rejects_non_validator() {
    with_governance(|_storage, vote| {
        let current = 10u64;
        assert_reverts_with(
            create_update_proposal(vote, OUTSIDER, current),
            "not an active validator",
        );
    });
}

#[test]
fn cast_vote_rejects_non_validator() {
    with_update_proposal(10, |_storage, vote, proposal_id, _| {
        assert_reverts_with(
            vote.cast_vote_approve(proposal_id, OUTSIDER, true, 11),
            "not an active validator",
        );
    });
}

#[test]
fn pending_validator_cannot_cast_vote() {
    with_vote(|storage| {
        register_pending_validator(storage.clone(), PENDING_VOTER, 4);
        let mut vote = Vote::new(storage.clone());
        let current = 50u64;
        let proposal_id = create_update_proposal(&mut vote, PROPOSER, current).unwrap();

        assert_reverts_with(
            vote.cast_vote_approve(proposal_id, PENDING_VOTER, true, current + 1),
            "not an active validator",
        );
        assert!(vote.read_proposal_voters(proposal_id).unwrap().is_empty());
    });
}

#[test]
fn pending_validator_cannot_create_proposal() {
    with_vote(|storage| {
        register_pending_validator(storage.clone(), PENDING_VOTER, 4);
        let mut vote = Vote::new(storage.clone());
        let current = 10u64;
        assert_reverts_with(
            create_update_proposal(&mut vote, PENDING_VOTER, current),
            "not an active validator",
        );
    });
}

#[test]
fn pending_validator_cannot_add_a_ballot_to_the_tally() {
    with_vote(|storage| {
        register_pending_validator(storage.clone(), PENDING_VOTER, 4);
        let mut vote = Vote::new(storage.clone());
        let current = 60u64;
        let proposal_id = create_update_proposal(&mut vote, PROPOSER, current).unwrap();

        assert!(vote
            .cast_vote_approve(proposal_id, PENDING_VOTER, true, current + 1)
            .is_err());
        vote.cast_vote_approve(proposal_id, VOTER_A, true, current + 2)
            .unwrap();

        let record = vote.proposals.get(proposal_id).unwrap().unwrap();
        let active = active_validator_addresses(storage.clone()).unwrap();
        let tally = calculate_vote_tally(&vote, &record, &active).unwrap();
        assert_eq!(tally, VoteTally { yes: 1, no: 0 });

        assert_proposal_view(storage, proposal_id, VoteTally { yes: 1, no: 0 }, 1);
    });
}

#[test]
fn cast_vote_rejects_after_deadline() {
    with_update_proposal(100, |_storage, vote, proposal_id, _| {
        let record = vote.proposals.get(proposal_id).unwrap().unwrap();
        let after_deadline = record.voting_deadline_height + 1;
        assert_reverts_with(
            vote.cast_vote_approve(proposal_id, VOTER_A, true, after_deadline),
            "voting window is closed",
        );
    });
}

#[test]
fn cast_vote_rejects_when_not_pending() {
    with_update_proposal(200, |_storage, vote, proposal_id, current| {
        vote.cast_vote_approve(proposal_id, VOTER_A, true, current + 1)
            .unwrap();

        let deadline = current + crate::constants::VOTING_WINDOW_BLOCKS + 1;
        vote.process_begin_block_test(deadline).unwrap();

        assert_ne!(proposal_status(vote, proposal_id), ProposalStatus::Pending);

        assert_reverts_with(
            vote.cast_vote_approve(proposal_id, VOTER_B, true, deadline + 1),
            "not pending",
        );
    });
}

#[test]
fn begin_block_does_not_tally_at_exact_deadline() {
    with_update_proposal(300, |_storage, vote, proposal_id, current| {
        approve_in_order(vote, proposal_id, &[VOTER_A, VOTER_B], current + 1).unwrap();

        let record = vote.proposals.get(proposal_id).unwrap().unwrap();
        let deadline = record.voting_deadline_height;
        vote.process_begin_block_test(deadline).unwrap();
        assert_eq!(proposal_status(vote, proposal_id), ProposalStatus::Pending);

        vote.process_begin_block_test(deadline + 1).unwrap();
        assert_ne!(proposal_status(vote, proposal_id), ProposalStatus::Pending);
    });
}

#[test]
fn max_pending_proposals_per_validator_is_enforced() {
    with_governance(|_storage, vote| {
        let current = 350u64;
        create_update_proposal(vote, PROPOSER, current).unwrap();

        assert_reverts_with(
            create_update_proposal(vote, PROPOSER, current + 1),
            "validator has too many pending",
        );
        assert_eq!(
            vote.pending_proposal_count_by_proposer(PROPOSER).unwrap(),
            MAX_PENDING_PROPOSALS_PER_VALIDATOR
        );
    });
}

#[test]
fn other_validator_can_create_while_proposer_has_pending() {
    with_governance(|_storage, vote| {
        let current = 360u64;
        create_update_proposal(vote, PROPOSER, current).unwrap();

        create_update_proposal(vote, VOTER_A, current + 1).unwrap();
    });
}

#[test]
fn proposer_can_create_after_pending_proposal_is_tallied() {
    with_governance(|_storage, vote| {
        let current = 370u64;
        create_update_proposal(vote, PROPOSER, current).unwrap();

        let deadline = current + crate::constants::VOTING_WINDOW_BLOCKS + 1;
        vote.process_begin_block_test(deadline).unwrap();

        let record = vote.proposals.get(U256::from(1)).unwrap().unwrap();
        assert_ne!(record.proposal_status().unwrap(), ProposalStatus::Pending);

        create_update_proposal(vote, PROPOSER, deadline + 1).unwrap();
    });
}

#[test]
fn max_pending_proposals_is_enforced() {
    with_governance(|storage, vote| {
        let current = 400u64;
        for i in 0..MAX_PENDING_PROPOSALS {
            let proposer = match i {
                0 => PROPOSER,
                1 => VOTER_A,
                2 => VOTER_B,
                _ => {
                    let addr = extra_validator_addr(i);
                    register_active_validator(storage.clone(), addr, (i + 16) as u8);
                    addr
                }
            };
            create_update_proposal(vote, proposer, current + i as u64).unwrap();
        }
        let overflow_proposer = extra_validator_addr(MAX_PENDING_PROPOSALS);
        register_active_validator(
            storage.clone(),
            overflow_proposer,
            (MAX_PENDING_PROPOSALS + 16) as u8,
        );
        assert_reverts_with(
            create_update_proposal(
                vote,
                overflow_proposer,
                current + MAX_PENDING_PROPOSALS as u64,
            ),
            "too many pending",
        );
    });
}
