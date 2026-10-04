//! Target-execution `Error` is a terminal outcome: failed target effects roll
//! back, the proposer's bond is refunded exactly once, the liability closes,
//! nothing is burned and the proposal leaves the pending index.

use alloy_primitives::{Address, U256};
use alloy_sol_types::SolEvent;
use outbe_primitives::{
    addresses::{UPDATE_ADDRESS, VOTE_ADDRESS},
    block::{BlockContext, BlockRuntimeContext},
    error::PrecompileError,
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
};

use crate::{
    constants::VOTING_WINDOW_BLOCKS,
    precompile::IVote,
    schema::{BondSettlement, ProposalStatus, Vote},
};

use super::targets::{PUBLIC_BONDED_REGISTRY, REJECTING_REGISTRY};
use super::{setup_default_validators, PROPOSER, VOTER_A};

const OWNER: Address = Address::repeat_byte(0x99);
const BOND: u64 = 123;
const SURPLUS: u64 = 7;

fn block_context(storage: StorageHandle<'_>, block_number: u64) -> BlockRuntimeContext<'_> {
    BlockRuntimeContext::new(BlockContext::empty_for_tests(block_number, 0, 1), storage)
}

/// A bonded public proposal whose target reports `Error` at approval, approved
/// by quorum and waiting for its deadline to pass.
fn bonded_error_fixture() -> (HashMapStorageProvider, U256, u64) {
    let mut provider = super::test_provider();
    // Escrow plus an unrelated surplus the refund must not touch.
    provider.set_balance(VOTE_ADDRESS, U256::from(BOND + SURPLUS));
    let proposal_id;
    {
        let storage = StorageHandle::new(&mut provider);
        setup_default_validators(storage.clone());
        let mut vote = Vote::new(storage);
        proposal_id = vote
            .create_proposal_with_value(
                OWNER,
                UPDATE_ADDRESS,
                "execution-error",
                10,
                U256::from(BOND),
                &PUBLIC_BONDED_REGISTRY,
            )
            .unwrap();
        vote.cast_vote_approve(proposal_id, PROPOSER, true, 11)
            .unwrap();
        vote.cast_vote_approve(proposal_id, VOTER_A, true, 11)
            .unwrap();
    }
    provider.clear_mutation_failure();
    (provider, proposal_id, 10 + VOTING_WINDOW_BLOCKS)
}

fn count_events(provider: &HashMapStorageProvider, signature: alloy_primitives::B256) -> usize {
    provider
        .get_events(VOTE_ADDRESS)
        .iter()
        .filter(|log| log.topics().first() == Some(&signature))
        .count()
}

#[test]
fn a_bonded_target_error_refunds_the_bond_once_and_closes_the_liability() {
    let (mut provider, proposal_id, deadline) = bonded_error_fixture();
    {
        let storage = StorageHandle::new(&mut provider);
        let mut vote = Vote::new(storage.clone());
        vote.process_begin_block(
            &block_context(storage.clone(), deadline + 1),
            &PUBLIC_BONDED_REGISTRY,
        )
        .unwrap();

        assert_eq!(
            vote.proposals
                .get(proposal_id)
                .unwrap()
                .unwrap()
                .proposal_status()
                .unwrap(),
            ProposalStatus::Error
        );
        // Failed target effects are rolled back; admission state is untouched.
        assert_eq!(
            storage.sload(UPDATE_ADDRESS, U256::from(996u64)).unwrap(),
            U256::ZERO,
            "partial target execution must roll back"
        );
        assert_eq!(
            storage.sload(UPDATE_ADDRESS, U256::from(997u64)).unwrap(),
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
    assert_eq!(
        count_events(&provider, IVote::ProposalErrored::SIGNATURE_HASH),
        1
    );
    assert_eq!(
        count_events(&provider, IVote::ProposalBondRefunded::SIGNATURE_HASH),
        1
    );
    assert_eq!(
        count_events(&provider, IVote::ProposalBondBurned::SIGNATURE_HASH),
        0
    );

    // A later block neither re-finalizes nor refunds a second time.
    {
        let storage = StorageHandle::new(&mut provider);
        let mut vote = Vote::new(storage.clone());
        vote.process_begin_block(
            &block_context(storage.clone(), deadline + 2),
            &PUBLIC_BONDED_REGISTRY,
        )
        .unwrap();
        assert_eq!(
            vote.proposal_bond(proposal_id).unwrap().settlement,
            BondSettlement::Refunded
        );
        assert_eq!(storage.balance(OWNER).unwrap(), U256::from(BOND));
        assert_eq!(storage.balance(VOTE_ADDRESS).unwrap(), U256::from(SURPLUS));
    }
    assert_eq!(
        count_events(&provider, IVote::ProposalErrored::SIGNATURE_HASH),
        1
    );
    assert_eq!(
        count_events(&provider, IVote::ProposalBondRefunded::SIGNATURE_HASH),
        1
    );
}

#[test]
fn an_unbonded_target_error_settles_no_bond_and_leaves_the_pending_index() {
    let mut provider = super::test_provider();
    provider.set_balance(VOTE_ADDRESS, U256::from(SURPLUS));
    let proposal_id;
    {
        let storage = StorageHandle::new(&mut provider);
        setup_default_validators(storage.clone());
        let mut vote = Vote::new(storage.clone());
        proposal_id = vote
            .create_proposal(
                PROPOSER,
                UPDATE_ADDRESS,
                "{\"kind\":\"legacy\"}",
                10,
                &REJECTING_REGISTRY,
            )
            .unwrap();
        vote.cast_vote_approve(proposal_id, PROPOSER, true, 11)
            .unwrap();
        vote.cast_vote_approve(proposal_id, VOTER_A, true, 11)
            .unwrap();
        let deadline = 10 + VOTING_WINDOW_BLOCKS;
        vote.process_begin_block(
            &block_context(storage.clone(), deadline + 1),
            &REJECTING_REGISTRY,
        )
        .unwrap();

        assert_eq!(
            vote.proposals
                .get(proposal_id)
                .unwrap()
                .unwrap()
                .proposal_status()
                .unwrap(),
            ProposalStatus::Error
        );
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
    assert_eq!(
        count_events(&provider, IVote::ProposalErrored::SIGNATURE_HASH),
        1
    );
    assert_eq!(
        count_events(&provider, IVote::ProposalBondRefunded::SIGNATURE_HASH),
        0
    );
    assert_eq!(
        count_events(&provider, IVote::ProposalBondBurned::SIGNATURE_HASH),
        0
    );
}

#[test]
fn a_failure_after_every_error_finalization_mutation_rolls_back_everything() {
    let (mut baseline, _, deadline) = bonded_error_fixture();
    {
        let storage = StorageHandle::new(&mut baseline);
        Vote::new(storage.clone())
            .process_begin_block(
                &block_context(storage, deadline + 1),
                &PUBLIC_BONDED_REGISTRY,
            )
            .unwrap();
    }
    let mutation_count = baseline.clear_mutation_failure();
    assert!(mutation_count > 0);

    for failure_point in 0..mutation_count {
        let (mut provider, proposal_id, deadline) = bonded_error_fixture();
        provider.fail_after_mutation_at(failure_point);
        {
            let storage = StorageHandle::new(&mut provider);
            let error = Vote::new(storage.clone())
                .process_begin_block(
                    &block_context(storage, deadline + 1),
                    &PUBLIC_BONDED_REGISTRY,
                )
                .unwrap_err();
            assert!(
                matches!(error, PrecompileError::Storage(_)),
                "failure point {failure_point} returned {error:?}"
            );
        }
        provider.clear_mutation_failure();

        let storage = StorageHandle::new(&mut provider);
        let vote = Vote::new(storage.clone());
        assert_eq!(
            vote.proposals
                .get(proposal_id)
                .unwrap()
                .unwrap()
                .proposal_status()
                .unwrap(),
            ProposalStatus::Pending,
            "failure point {failure_point}"
        );
        assert_eq!(
            vote.list_pending_proposal_ids().unwrap(),
            vec![proposal_id],
            "failure point {failure_point}"
        );
        assert_eq!(
            vote.proposal_bond(proposal_id).unwrap().settlement,
            BondSettlement::Unsettled,
            "failure point {failure_point}"
        );
        assert_eq!(
            vote.bond_liabilities().unwrap(),
            U256::from(BOND),
            "failure point {failure_point}"
        );
        assert_eq!(
            storage.balance(VOTE_ADDRESS).unwrap(),
            U256::from(BOND + SURPLUS),
            "failure point {failure_point}"
        );
        assert_eq!(
            storage.balance(OWNER).unwrap(),
            U256::ZERO,
            "failure point {failure_point}"
        );
        assert_eq!(
            storage.sload(UPDATE_ADDRESS, U256::from(996u64)).unwrap(),
            U256::ZERO,
            "failure point {failure_point}"
        );
        drop(vote);
        drop(storage);
        assert_eq!(
            count_events(&provider, IVote::ProposalBondRefunded::SIGNATURE_HASH),
            0,
            "failure point {failure_point}"
        );
        assert_eq!(
            count_events(&provider, IVote::ProposalErrored::SIGNATURE_HASH),
            0,
            "failure point {failure_point}"
        );
    }
}

/// Writes the shape the previous binary persisted for a target-execution
/// Error: status `Error`, the id still in the pending vector, the bond still
/// `Unsettled`. This is the upgrade boundary, not a corrupted record.
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

        vote.process_begin_block(
            &block_context(storage.clone(), deadline + 5),
            &PUBLIC_BONDED_REGISTRY,
        )
        .unwrap();

        assert_eq!(
            vote.proposals
                .get(proposal_id)
                .unwrap()
                .unwrap()
                .proposal_status()
                .unwrap(),
            ProposalStatus::Error
        );
        assert_eq!(
            vote.list_pending_proposal_ids().unwrap(),
            Vec::<U256>::new()
        );
        assert_eq!(vote.pending_proposal_count_by_proposer(OWNER).unwrap(), 0);
        assert_eq!(
            vote.proposal_bond(proposal_id).unwrap().settlement,
            BondSettlement::Refunded
        );
        assert_eq!(vote.bond_liabilities().unwrap(), U256::ZERO);
        assert_eq!(storage.balance(OWNER).unwrap(), U256::from(BOND));
        assert_eq!(storage.balance(VOTE_ADDRESS).unwrap(), U256::from(SURPLUS));
        // The target is not re-executed during cleanup.
        assert_eq!(
            storage.sload(UPDATE_ADDRESS, U256::from(996u64)).unwrap(),
            U256::ZERO
        );
        assert_eq!(
            storage.sload(UPDATE_ADDRESS, U256::from(995u64)).unwrap(),
            U256::ZERO
        );

        // A later pass finds nothing to settle.
        vote.process_begin_block(
            &block_context(storage.clone(), deadline + 6),
            &PUBLIC_BONDED_REGISTRY,
        )
        .unwrap();
        assert_eq!(storage.balance(OWNER).unwrap(), U256::from(BOND));
        assert_eq!(storage.balance(VOTE_ADDRESS).unwrap(), U256::from(SURPLUS));
    }
    assert_eq!(
        count_events(&provider, IVote::ProposalBondRefunded::SIGNATURE_HASH),
        1
    );
    assert_eq!(
        count_events(&provider, IVote::ProposalBondBurned::SIGNATURE_HASH),
        0
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
        let storage = StorageHandle::new(&mut provider);
        setup_default_validators(storage.clone());
        let mut vote = Vote::new(storage.clone());
        proposal_id = vote
            .create_proposal(
                PROPOSER,
                UPDATE_ADDRESS,
                "{\"kind\":\"legacy\"}",
                10,
                &REJECTING_REGISTRY,
            )
            .unwrap();
        persist_legacy_error(&mut vote, proposal_id);
        assert_eq!(
            vote.pending_proposal_count_by_proposer(PROPOSER).unwrap(),
            1
        );

        vote.process_begin_block(&block_context(storage.clone(), 11), &REJECTING_REGISTRY)
            .unwrap();

        assert_eq!(
            vote.list_pending_proposal_ids().unwrap(),
            Vec::<U256>::new()
        );
        assert_eq!(
            vote.pending_proposal_count_by_proposer(PROPOSER).unwrap(),
            0
        );
        assert_eq!(
            vote.proposal_bond(proposal_id).unwrap().settlement,
            BondSettlement::NoBond
        );
        assert_eq!(vote.bond_liabilities().unwrap(), U256::ZERO);
        assert_eq!(storage.balance(VOTE_ADDRESS).unwrap(), U256::from(SURPLUS));
        assert_eq!(
            storage.sload(UPDATE_ADDRESS, U256::from(999u64)).unwrap(),
            U256::ZERO,
            "the rejecting target is not re-executed"
        );
    }
    assert_eq!(
        count_events(&provider, IVote::ProposalBondRefunded::SIGNATURE_HASH),
        0
    );
    assert_eq!(
        count_events(&provider, IVote::ProposalErrored::SIGNATURE_HASH),
        0
    );
}
