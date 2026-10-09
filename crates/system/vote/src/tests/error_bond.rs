//! Target-execution `Error` is a terminal outcome:
//!
//! - Failed target effects revert.
//! - The proposer's bond is refunded exactly once.
//! - The liability closes.
//! - Nothing is burned.
//! - The proposal leaves the pending index.

use alloy_primitives::{Address, U256};
use alloy_sol_types::SolEvent;
use outbe_primitives::{
    addresses::VOTE_ADDRESS,
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
};

use crate::{
    precompile::IVote,
    schema::{BondSettlement, ProposalStatus, Vote},
};

use super::targets::{
    approved_bonded_fixture, approved_legacy_proposal,
    assert_every_finalization_mutation_rolls_back, legacy_proposal, target_marker, PendingBond,
    PUBLIC_BONDED_REGISTRY, REJECTING_REGISTRY,
};
use super::{
    assert_bond_closed, assert_event_counts, assert_finalized_error, count_events, proposal_status,
    tally_after_window_with, validator_vote, VoteTestExt, PROPOSER,
};

const OWNER: Address = Address::repeat_byte(0x99);
const BOND: u64 = 123;
const SURPLUS: u64 = 7;

/// A bonded public proposal whose target reports `Error` at approval, approved
/// by quorum and waiting for its deadline to pass.
fn bonded_error_fixture() -> (HashMapStorageProvider, U256, u64) {
    // Escrow plus an unrelated surplus the refund must not touch.
    approved_bonded_fixture(
        OWNER,
        "execution-error",
        &[(VOTE_ADDRESS, U256::from(BOND + SURPLUS))],
    )
}

#[test]
fn a_bonded_target_error_refunds_the_bond_once_and_closes_the_liability() {
    let (mut provider, proposal_id, deadline) = bonded_error_fixture();
    {
        let storage = StorageHandle::new(&mut provider);
        let mut vote = Vote::new(storage.clone());
        vote.begin_block_with(deadline + 1, &PUBLIC_BONDED_REGISTRY)
            .unwrap();

        assert_eq!(proposal_status(&vote, proposal_id), ProposalStatus::Error);
        // Failed target effects revert. Admission state stays untouched.
        assert_eq!(
            target_marker(&storage, 996),
            U256::ZERO,
            "partial target execution must roll back"
        );
        assert_eq!(
            target_marker(&storage, 997),
            proposal_id,
            "admission reservation is target-owned state, not an execution effect"
        );
        // The bond is refunded in full, once, and the liability is closed.
        let bond = vote.proposal_bond(proposal_id).unwrap();
        assert_eq!(bond.amount, U256::from(BOND));
        assert_eq!(bond.settlement, BondSettlement::Refunded);
        assert_eq!(vote.bond_liabilities().unwrap(), U256::ZERO);
        assert_eq!(storage.balance(OWNER).unwrap(), U256::from(BOND));
        assert_eq!(
            storage.balance(VOTE_ADDRESS).unwrap(),
            U256::from(SURPLUS),
            "only the escrowed bond leaves the Vote account"
        );
        // Error is terminal: the proposal no longer occupies the pending index.
        assert_eq!(
            vote.list_pending_proposal_ids().unwrap(),
            Vec::<U256>::new()
        );
    }
    assert_event_counts(
        &provider,
        &[
            (IVote::ProposalErrored::SIGNATURE_HASH, 1),
            (IVote::ProposalBondRefunded::SIGNATURE_HASH, 1),
            (IVote::ProposalBondBurned::SIGNATURE_HASH, 0),
        ],
    );

    // A later block neither re-finalizes nor refunds a second time.
    {
        let storage = StorageHandle::new(&mut provider);
        let mut vote = Vote::new(storage.clone());
        vote.begin_block_with(deadline + 2, &PUBLIC_BONDED_REGISTRY)
            .unwrap();
        assert_eq!(
            vote.proposal_bond(proposal_id).unwrap().settlement,
            BondSettlement::Refunded
        );
        assert_eq!(storage.balance(OWNER).unwrap(), U256::from(BOND));
        assert_eq!(storage.balance(VOTE_ADDRESS).unwrap(), U256::from(SURPLUS));
    }
    assert_event_counts(
        &provider,
        &[
            (IVote::ProposalErrored::SIGNATURE_HASH, 1),
            (IVote::ProposalBondRefunded::SIGNATURE_HASH, 1),
        ],
    );
}

#[test]
fn an_unbonded_target_error_settles_no_bond_and_leaves_the_pending_index() {
    let mut provider = super::test_provider();
    provider.set_balance(VOTE_ADDRESS, U256::from(SURPLUS));
    let proposal_id;
    {
        let (storage, mut vote) = validator_vote(&mut provider);
        proposal_id = approved_legacy_proposal(&mut vote, &REJECTING_REGISTRY).unwrap();
        tally_after_window_with(&mut vote, 10, &REJECTING_REGISTRY).unwrap();

        assert_eq!(proposal_status(&vote, proposal_id), ProposalStatus::Error);
        let bond = vote.proposal_bond(proposal_id).unwrap();
        assert_eq!(bond.settlement, BondSettlement::NoBond);
        assert_eq!(bond.amount, U256::ZERO);
        assert_eq!(vote.bond_liabilities().unwrap(), U256::ZERO);
        assert_eq!(storage.balance(VOTE_ADDRESS).unwrap(), U256::from(SURPLUS));
        assert_eq!(storage.balance(PROPOSER).unwrap(), U256::ZERO);
        assert_eq!(
            vote.list_pending_proposal_ids().unwrap(),
            Vec::<U256>::new()
        );
    }
    assert_event_counts(
        &provider,
        &[
            (IVote::ProposalErrored::SIGNATURE_HASH, 1),
            (IVote::ProposalBondRefunded::SIGNATURE_HASH, 0),
            (IVote::ProposalBondBurned::SIGNATURE_HASH, 0),
        ],
    );
}

#[test]
fn a_failure_after_every_error_finalization_mutation_rolls_back_everything() {
    assert_every_finalization_mutation_rolls_back(
        bonded_error_fixture,
        PendingBond {
            liabilities: U256::from(BOND),
            vote_balance: U256::from(BOND + SURPLUS),
        },
        |storage, _, failure_point| {
            assert_eq!(
                storage.balance(OWNER).unwrap(),
                U256::ZERO,
                "failure point {failure_point}"
            );
            assert_eq!(
                target_marker(storage, 996),
                U256::ZERO,
                "failure point {failure_point}"
            );
        },
        &[
            IVote::ProposalBondRefunded::SIGNATURE_HASH,
            IVote::ProposalErrored::SIGNATURE_HASH,
        ],
    );
}

/// Writes the shape the previous binary persisted for a target-execution
/// Error:
///
/// - status `Error`
/// - the id still in the pending vector
/// - the bond still `Unsettled`
///
/// This is the upgrade boundary, not a corrupted record.
fn persist_legacy_error(vote: &mut Vote<'_>, proposal_id: U256) {
    let mut record = vote.proposals.get(proposal_id).unwrap().unwrap();
    record.set_proposal_status(ProposalStatus::Error);
    vote.proposals.update(&record).unwrap();
    assert_eq!(vote.list_pending_proposal_ids().unwrap(), vec![proposal_id]);
}

#[test]
fn a_persisted_legacy_bonded_error_is_released_and_refunded_once_on_the_next_pass() {
    let (mut provider, proposal_id, deadline) = bonded_error_fixture();
    {
        let storage = StorageHandle::new(&mut provider);
        let mut vote = Vote::new(storage.clone());
        persist_legacy_error(&mut vote, proposal_id);
        assert_eq!(
            vote.proposal_bond(proposal_id).unwrap().settlement,
            BondSettlement::Unsettled
        );
        assert_eq!(vote.bond_liabilities().unwrap(), U256::from(BOND));

        vote.begin_block_with(deadline + 5, &PUBLIC_BONDED_REGISTRY)
            .unwrap();

        assert_finalized_error(&vote, proposal_id);
        assert_eq!(vote.pending_proposal_count_by_proposer(OWNER).unwrap(), 0);
        assert_bond_closed(&vote, proposal_id, BondSettlement::Refunded);
        assert_eq!(storage.balance(OWNER).unwrap(), U256::from(BOND));
        assert_eq!(storage.balance(VOTE_ADDRESS).unwrap(), U256::from(SURPLUS));
        // The target is not re-executed during cleanup.
        assert_eq!(target_marker(&storage, 996), U256::ZERO);
        assert_eq!(target_marker(&storage, 995), U256::ZERO);

        // A later pass finds nothing to settle.
        vote.begin_block_with(deadline + 6, &PUBLIC_BONDED_REGISTRY)
            .unwrap();
        assert_eq!(storage.balance(OWNER).unwrap(), U256::from(BOND));
        assert_eq!(storage.balance(VOTE_ADDRESS).unwrap(), U256::from(SURPLUS));
    }
    assert_event_counts(
        &provider,
        &[
            (IVote::ProposalBondRefunded::SIGNATURE_HASH, 1),
            (IVote::ProposalBondBurned::SIGNATURE_HASH, 0),
        ],
    );
    assert_eq!(
        count_events(&provider, IVote::ProposalErrored::SIGNATURE_HASH),
        0,
        "cleanup does not announce a second finalization"
    );
}

#[test]
fn a_persisted_legacy_unbonded_error_leaves_the_pending_index_without_settlement() {
    let mut provider = super::test_provider();
    provider.set_balance(VOTE_ADDRESS, U256::from(SURPLUS));
    let proposal_id;
    {
        let (storage, mut vote) = validator_vote(&mut provider);
        proposal_id = legacy_proposal(&mut vote, &REJECTING_REGISTRY).unwrap();
        persist_legacy_error(&mut vote, proposal_id);
        assert_eq!(
            vote.pending_proposal_count_by_proposer(PROPOSER).unwrap(),
            1
        );

        vote.begin_block_with(11, &REJECTING_REGISTRY).unwrap();

        assert_eq!(
            vote.list_pending_proposal_ids().unwrap(),
            Vec::<U256>::new()
        );
        assert_eq!(
            vote.pending_proposal_count_by_proposer(PROPOSER).unwrap(),
            0
        );
        assert_bond_closed(&vote, proposal_id, BondSettlement::NoBond);
        assert_eq!(storage.balance(VOTE_ADDRESS).unwrap(), U256::from(SURPLUS));
        assert_eq!(
            target_marker(&storage, 999),
            U256::ZERO,
            "the rejecting target is not re-executed"
        );
    }
    assert_event_counts(
        &provider,
        &[
            (IVote::ProposalBondRefunded::SIGNATURE_HASH, 0),
            (IVote::ProposalErrored::SIGNATURE_HASH, 0),
        ],
    );
}
