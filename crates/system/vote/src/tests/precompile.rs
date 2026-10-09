use alloy_primitives::{Address, U256};
use alloy_sol_types::{SolCall, SolError, SolEvent};

use outbe_primitives::addresses::{UPDATE_ADDRESS, VOTE_ADDRESS};
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::precompile::{dispatch_with_handlers, IVote};
use crate::schema::{BondSettlement, Vote};

use super::{
    assert_reverts_with, create_proposal_test, create_update_proposal, empty_update_payload,
    proposal_status, setup_default_validators, targets::PUBLIC_BONDED_REGISTRY, test_vote_registry,
    PROPOSER, VOTER_A, VOTER_B,
};

fn dispatch(
    storage: StorageHandle<'_>,
    data: &[u8],
    caller: Address,
    value: U256,
) -> outbe_primitives::error::Result<alloy_primitives::Bytes> {
    dispatch_with_handlers(storage, data, caller, value, test_vote_registry())
}

fn with_vote_provider<F: FnOnce(StorageHandle)>(block_number: u64, f: F) -> HashMapStorageProvider {
    let mut provider = super::test_provider();
    provider.set_block_number(block_number);
    let storage = StorageHandle::new(&mut provider);
    setup_default_validators(storage.clone());
    f(storage);
    provider
}

#[test]
fn precompile_abi_compiles() {
    let _ = IVote::createProposalCall::SIGNATURE;
    let _ = IVote::castVoteCall::SIGNATURE;
    let _ = IVote::getProposalCall::SIGNATURE;
    let _ = IVote::getProposalBondCall::SIGNATURE;
    let _ = IVote::unsettledBondLiabilitiesCall::SIGNATURE;
}

#[test]
fn custom_window_persists_deadline_and_tallies_only_after_it() {
    for window in [1, 1_000, 30_000, 86_400] {
        let provider = with_vote_provider(100, |storage| {
            let payload = empty_update_payload(100);
            let data = IVote::createProposalWithVotingWindowCall {
                targetModule: UPDATE_ADDRESS,
                payload: payload.clone(),
                votingWindowBlocks: window,
            }
            .abi_encode();
            let ret = dispatch(storage.clone(), &data, PROPOSER, U256::ZERO).unwrap();
            let id = IVote::createProposalWithVotingWindowCall::abi_decode_returns(&ret).unwrap();
            let mut vote = Vote::new(storage.clone());
            let get = IVote::getProposalCall { proposalId: id }.abi_encode();
            let info = IVote::getProposalCall::abi_decode_returns(
                &dispatch(storage.clone(), &get, PROPOSER, U256::ZERO).unwrap(),
            )
            .unwrap();
            assert_eq!(info.createdHeight, 100);
            assert_eq!(info.votingDeadlineHeight, 100 + window);
            assert_eq!(info.payload, payload);
            vote.cast_vote_approve(id, VOTER_A, true, 100).unwrap();
            // A fresh runtime reads the immutable deadline from existing storage.
            drop(vote);
            let mut vote = Vote::new(storage.clone());
            vote.cast_vote_approve(id, VOTER_B, true, 100 + window)
                .unwrap();
            assert!(vote
                .cast_vote_approve(id, PROPOSER, true, 101 + window)
                .is_err());
            let ctx = super::block_ctx(storage.clone(), 100 + window);
            vote.process_begin_block(&ctx, test_vote_registry())
                .unwrap();
            assert_eq!(
                proposal_status(&vote, id),
                crate::state::ProposalStatus::Pending
            );
            let ctx = super::block_ctx(storage.clone(), 101 + window);
            vote.process_begin_block(&ctx, test_vote_registry())
                .unwrap();
            assert_eq!(
                proposal_status(&vote, id),
                crate::state::ProposalStatus::Approved
            );
        });
        let events = provider.get_events(VOTE_ADDRESS);
        let log = events
            .iter()
            .find(|log| log.topics().first() == Some(&IVote::ProposalCreated::SIGNATURE_HASH))
            .unwrap();
        let event = IVote::ProposalCreated::decode_log_data(log).unwrap();
        assert_eq!(event.votingDeadlineHeight, 100 + window);
    }
}

#[test]
fn custom_window_does_not_change_existing_proposal_or_quorum() {
    with_vote_provider(100, |storage| {
        let old = IVote::createProposalCall {
            targetModule: UPDATE_ADDRESS,
            payload: empty_update_payload(100),
        }
        .abi_encode();
        let old_id = IVote::createProposalCall::abi_decode_returns(
            &dispatch(storage.clone(), &old, PROPOSER, U256::ZERO).unwrap(),
        )
        .unwrap();
        let new = IVote::createProposalWithVotingWindowCall {
            targetModule: UPDATE_ADDRESS,
            payload: empty_update_payload(100),
            votingWindowBlocks: 1_000,
        }
        .abi_encode();
        let id = IVote::createProposalWithVotingWindowCall::abi_decode_returns(
            &dispatch(storage.clone(), &new, VOTER_A, U256::ZERO).unwrap(),
        )
        .unwrap();
        let mut vote = Vote::new(storage.clone());
        vote.cast_vote_approve(id, VOTER_A, true, 101).unwrap();
        vote.process_begin_block(&super::block_ctx(storage, 1101), test_vote_registry())
            .unwrap();
        assert_eq!(
            proposal_status(&vote, id),
            crate::state::ProposalStatus::Expired
        );
        let old = vote.proposals.get(old_id).unwrap().unwrap();
        assert_eq!(
            old.proposal_status().unwrap(),
            crate::state::ProposalStatus::Pending
        );
        assert_eq!(
            old.voting_deadline_height,
            100 + crate::constants::VOTING_WINDOW_BLOCKS
        );
    });
}

#[test]
fn custom_window_rejects_invalid_duration_overflow_and_unauthorized_creation() {
    for (height, window, caller, value, expected) in [
        (100, 0, PROPOSER, U256::ZERO, "voting window"),
        (100, 86_401, PROPOSER, U256::ZERO, "voting window"),
        (100, u64::MAX, PROPOSER, U256::ZERO, "voting window"),
        (u64::MAX, 1, PROPOSER, U256::ZERO, "overflows"),
        (100, 1_000, Address::ZERO, U256::ZERO, "active validator"),
    ] {
        let provider = with_vote_provider(height, |storage| {
            let data = IVote::createProposalWithVotingWindowCall {
                targetModule: UPDATE_ADDRESS,
                payload: empty_update_payload(100),
                votingWindowBlocks: window,
            }
            .abi_encode();
            let before = Vote::new(storage.clone()).proposal_count.read().unwrap();
            let err = dispatch(storage.clone(), &data, caller, value).unwrap_err();
            assert!(err.to_string().contains(expected), "{err}");
            assert_eq!(Vote::new(storage).proposal_count.read().unwrap(), before);
        });
        assert!(!has_event(
            &provider,
            IVote::ProposalCreated::SIGNATURE_HASH
        ));
    }
}

#[test]
fn custom_window_preserves_target_bond_rules() {
    with_vote_provider(100, |storage| {
        let data = IVote::createProposalWithVotingWindowCall {
            targetModule: UPDATE_ADDRESS,
            payload: empty_update_payload(100),
            votingWindowBlocks: 1_000,
        }
        .abi_encode();
        let err = dispatch(storage.clone(), &data, PROPOSER, U256::from(1)).unwrap_err();
        assert!(matches!(err, PrecompileError::RevertBytes(bytes)
            if bytes.starts_with(&IVote::InvalidProposalBond::SELECTOR)));
        assert_eq!(
            Vote::new(storage).proposal_count.read().unwrap(),
            U256::ZERO
        );
    });
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_block_number(100);
    provider.set_balance(VOTE_ADDRESS, U256::from(123));
    let storage = StorageHandle::new(&mut provider);
    let data = IVote::createProposalWithVotingWindowCall {
        targetModule: UPDATE_ADDRESS,
        payload: r#"{"kind":"public"}"#.into(),
        votingWindowBlocks: 1_000,
    }
    .abi_encode();
    let ret = dispatch_with_handlers(
        storage.clone(),
        &data,
        Address::repeat_byte(0x99),
        U256::from(123),
        &PUBLIC_BONDED_REGISTRY,
    )
    .unwrap();
    let id = IVote::createProposalWithVotingWindowCall::abi_decode_returns(&ret).unwrap();
    let vote = Vote::new(storage);
    assert_eq!(vote.bond_liabilities().unwrap(), U256::from(123));
    assert_eq!(
        vote.proposal_bond(id).unwrap().settlement,
        BondSettlement::Unsettled
    );
    assert_eq!(
        vote.proposals
            .get(id)
            .unwrap()
            .unwrap()
            .voting_deadline_height,
        1100
    );
}

#[test]
fn dispatch_create_proposal_emits_event() {
    let provider = with_vote_provider(100, |storage| {
        let payload = empty_update_payload(100);
        let data = IVote::createProposalCall {
            targetModule: UPDATE_ADDRESS,
            payload,
        }
        .abi_encode();

        let ret = dispatch(storage.clone(), &data, PROPOSER, U256::ZERO).unwrap();
        let proposal_id = IVote::createProposalCall::abi_decode_returns(&ret).unwrap();
        assert_eq!(proposal_id, U256::from(1));
    });

    assert!(has_event(&provider, IVote::ProposalCreated::SIGNATURE_HASH,));
}

#[test]
fn dispatch_create_proposal_accepts_exact_public_bond_only() {
    let mut provider = super::test_provider();
    provider.set_block_number(100);
    provider.set_balance(VOTE_ADDRESS, U256::from(130u64));
    {
        let storage = StorageHandle::new(&mut provider);
        let data = IVote::createProposalCall {
            targetModule: UPDATE_ADDRESS,
            payload: r#"{"kind":"public"}"#.into(),
        }
        .abi_encode();
        let output = dispatch_with_handlers(
            storage.clone(),
            &data,
            Address::repeat_byte(0x99),
            U256::from(123u64),
            &PUBLIC_BONDED_REGISTRY,
        )
        .unwrap();
        let proposal_id = IVote::createProposalCall::abi_decode_returns(&output).unwrap();
        let vote = Vote::new(storage);
        assert_eq!(
            vote.proposal_bond(proposal_id).unwrap().settlement,
            BondSettlement::Unsettled
        );
        assert_eq!(vote.bond_liabilities().unwrap(), U256::from(123u64));
    }
    assert!(has_event(
        &provider,
        IVote::ProposalBondEscrowed::SIGNATURE_HASH
    ));
    assert!(has_event(&provider, IVote::ProposalCreated::SIGNATURE_HASH));
}

#[test]
fn dispatch_cast_vote_emits_event() {
    let provider = with_vote_provider(100, |storage| {
        let mut governance = Vote::new(storage.clone());
        let proposal_id = create_update_proposal(&mut governance, PROPOSER, 100).unwrap();

        let data = IVote::castVoteCall {
            proposalId: proposal_id,
            approve: true,
        }
        .abi_encode();
        dispatch(storage.clone(), &data, VOTER_A, U256::ZERO).unwrap();
    });

    assert!(has_event(&provider, IVote::VoteCast::SIGNATURE_HASH));
}

#[test]
fn dispatch_rejects_non_zero_value() {
    with_vote_provider(100, |storage| {
        let data = IVote::getProposalCall {
            proposalId: U256::from(1),
        }
        .abi_encode();
        assert_reverts_with(
            dispatch(storage, &data, PROPOSER, U256::from(1)),
            "non-payable",
        );
    });
}

#[test]
fn bond_views_return_legacy_no_bond_and_recorded_liability() {
    with_vote_provider(100, |storage| {
        let mut vote = Vote::new(storage.clone());
        let proposal_id = create_update_proposal(&mut vote, PROPOSER, 100).unwrap();
        let bond_data = IVote::getProposalBondCall {
            proposalId: proposal_id,
        }
        .abi_encode();
        let bond = IVote::getProposalBondCall::abi_decode_returns(
            &dispatch(storage.clone(), &bond_data, PROPOSER, U256::ZERO).unwrap(),
        )
        .unwrap();
        assert_eq!(bond.amount, U256::ZERO);
        assert_eq!(bond.settlement, IVote::BondSettlement::NoBond);

        let liability_data = IVote::unsettledBondLiabilitiesCall {}.abi_encode();
        let liabilities = IVote::unsettledBondLiabilitiesCall::abi_decode_returns(
            &dispatch(storage.clone(), &liability_data, PROPOSER, U256::ZERO).unwrap(),
        )
        .unwrap();
        assert_eq!(liabilities, U256::ZERO);

        vote.record_proposal_bond(proposal_id, U256::from(123u64))
            .unwrap();
        let bond = IVote::getProposalBondCall::abi_decode_returns(
            &dispatch(storage.clone(), &bond_data, PROPOSER, U256::ZERO).unwrap(),
        )
        .unwrap();
        assert_eq!(bond.amount, U256::from(123u64));
        assert_eq!(bond.settlement, IVote::BondSettlement::Unsettled);
        let liabilities = IVote::unsettledBondLiabilitiesCall::abi_decode_returns(
            &dispatch(storage, &liability_data, PROPOSER, U256::ZERO).unwrap(),
        )
        .unwrap();
        assert_eq!(liabilities, U256::from(123u64));
    });
}

#[test]
fn dispatch_views_return_abi_shaped_data() {
    with_vote_provider(200, |storage| {
        let mut governance = Vote::new(storage.clone());
        let payload = update_json_payload_for_test(200);
        let proposal_id =
            create_proposal_test(&mut governance, PROPOSER, UPDATE_ADDRESS, &payload, 200).unwrap();
        governance
            .cast_vote_approve(proposal_id, VOTER_A, true, 201)
            .unwrap();
        governance
            .cast_vote_approve(proposal_id, VOTER_B, false, 202)
            .unwrap();

        let get_data = IVote::getProposalCall {
            proposalId: proposal_id,
        }
        .abi_encode();
        let ret = dispatch(storage.clone(), &get_data, PROPOSER, U256::ZERO).unwrap();
        let info = IVote::getProposalCall::abi_decode_returns(&ret).unwrap();
        assert_eq!(info.proposalId, proposal_id);
        assert_eq!(info.proposer, PROPOSER);
        assert_eq!(info.targetModule, UPDATE_ADDRESS);
        assert_eq!(info.payload, payload);
        assert_eq!(info.state.yes, 1);
        assert_eq!(info.state.no, 1);
        assert_eq!(info.votersCount, U256::from(2));

        let voters_data = IVote::getProposalVotersCall {
            proposalId: proposal_id,
            index: U256::ZERO,
            count: U256::from(10),
        }
        .abi_encode();
        let voters_ret = dispatch(storage.clone(), &voters_data, PROPOSER, U256::ZERO).unwrap();
        let voters = IVote::getProposalVotersCall::abi_decode_returns(&voters_ret).unwrap();
        assert_eq!(voters, vec![VOTER_A, VOTER_B]);

        let list_data = IVote::listProposalsCall {
            index: U256::ZERO,
            count: U256::from(10),
        }
        .abi_encode();
        let list_ret = dispatch(storage, &list_data, PROPOSER, U256::ZERO).unwrap();
        let ids = IVote::listProposalsCall::abi_decode_returns(&list_ret).unwrap();
        assert_eq!(ids, vec![proposal_id]);
    });
}

#[test]
fn dispatch_create_proposal_rejects_non_zero_value_before_state_change() {
    with_vote_provider(100, |storage| {
        let vote = Vote::new(storage.clone());
        let before = vote.proposal_count.read().unwrap();
        let payload = empty_update_payload(100);
        let data = IVote::createProposalCall {
            targetModule: UPDATE_ADDRESS,
            payload,
        }
        .abi_encode();
        let err = dispatch(storage.clone(), &data, PROPOSER, U256::from(1)).unwrap_err();
        match err {
            PrecompileError::RevertBytes(bytes) => {
                assert_eq!(&bytes[..4], IVote::InvalidProposalBond::SELECTOR)
            }
            other => panic!("expected InvalidProposalBond, got {other:?}"),
        }
        assert_eq!(vote.proposal_count.read().unwrap(), before);
    });
}

#[test]
fn dispatch_cast_vote_rejects_non_zero_value_before_state_change() {
    with_vote_provider(100, |storage| {
        let mut vote = Vote::new(storage.clone());
        let proposal_id = create_update_proposal(&mut vote, PROPOSER, 100).unwrap();
        let voters_before = vote.read_proposal_voters(proposal_id).unwrap().len();
        let data = IVote::castVoteCall {
            proposalId: proposal_id,
            approve: true,
        }
        .abi_encode();
        assert_reverts_with(
            dispatch(storage.clone(), &data, VOTER_A, U256::from(1)),
            "non-payable",
        );
        assert_eq!(
            vote.read_proposal_voters(proposal_id).unwrap().len(),
            voters_before
        );
    });
}

fn update_json_payload_for_test(current_height: u64) -> String {
    super::update_json_payload(
        outbe_update::encode_protocol_version(1, 2),
        super::min_activation_at(current_height),
        "notes",
    )
}

fn has_event(provider: &HashMapStorageProvider, topic0: alloy_primitives::B256) -> bool {
    provider
        .get_events(VOTE_ADDRESS)
        .iter()
        .any(|log| log.topics().first() == Some(&topic0))
}
