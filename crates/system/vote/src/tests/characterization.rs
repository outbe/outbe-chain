use alloy_primitives::{Address, U256};
use alloy_sol_types::{SolError, SolEvent};
use outbe_primitives::{
    addresses::{UPDATE_ADDRESS, VOTE_ADDRESS},
    error::{PrecompileError, Result},
    stablecoin_fork::MAX_PENDING_PUBLIC_BONDED_PROPOSALS,
    storage::StorageHandle,
};

use crate::{
    constants::VOTING_WINDOW_BLOCKS,
    handlers::TargetAdmission,
    precompile::IVote,
    schema::{BondSettlement, ProposalStatus, Vote},
    state::ProposalSubmission,
};

use super::targets::*;
use super::{
    assert_bond_closed, assert_event_counts, assert_finalized_error, count_events,
    create_proposal_test, proposal_status, setup_default_validators, tally_after_window_with,
    test_vote_registry, validator_vote, VoteTestExt, PROPOSER,
};

#[test]
fn creation_preserves_original_payload_bytes_in_state_and_log() {
    let mut provider = super::test_provider();
    let proposal_id;
    {
        let storage = StorageHandle::new(&mut provider);
        setup_default_validators(storage.clone());
        let mut vote = Vote::new(storage);
        proposal_id = vote
            .create_proposal(
                PROPOSER,
                UPDATE_ADDRESS,
                RAW_PAYLOAD,
                10,
                &RAW_CONTEXT_REGISTRY,
            )
            .unwrap();
        let record = vote.proposals.get(proposal_id).unwrap().unwrap();
        assert_eq!(record.payload, RAW_PAYLOAD);
    }

    let created = provider
        .get_events(VOTE_ADDRESS)
        .iter()
        .find(|log| log.topics().first() == Some(&IVote::ProposalCreated::SIGNATURE_HASH))
        .expect("ProposalCreated log");
    let decoded = IVote::ProposalCreated::decode_log_data(created).unwrap();
    assert_eq!(decoded.proposalId, proposal_id);
    assert_eq!(decoded.payload, RAW_PAYLOAD);
}

#[test]
fn target_reservation_failure_rolls_back_proposal_target_state_and_log() {
    let mut provider = super::test_provider();
    {
        let (storage, mut vote) = validator_vote(&mut provider);
        assert!(vote
            .create_proposal(
                PROPOSER,
                UPDATE_ADDRESS,
                RAW_PAYLOAD,
                10,
                &FAILING_RESERVE_REGISTRY,
            )
            .is_err());
        assert_eq!(vote.proposal_count.read().unwrap(), U256::ZERO);
        assert_eq!(vote.pending_proposal_ids.len().unwrap(), 0);
        assert_eq!(target_marker(&storage, 999), U256::ZERO);
    }
    assert!(provider.get_events(VOTE_ADDRESS).is_empty());
}

#[test]
fn execution_receives_original_payload_and_exact_context() {
    let mut provider = super::test_provider();
    let (_, mut vote) = validator_vote(&mut provider);
    let proposal_id = vote
        .create_proposal(
            PROPOSER,
            UPDATE_ADDRESS,
            RAW_PAYLOAD,
            10,
            &RAW_CONTEXT_REGISTRY,
        )
        .unwrap();
    approve_by_quorum(&mut vote, proposal_id).unwrap();

    let finalize_height = 10 + VOTING_WINDOW_BLOCKS + 1;
    vote.begin_block_with(finalize_height, &RAW_CONTEXT_REGISTRY)
        .unwrap();

    assert_eq!(
        proposal_status(&vote, proposal_id),
        ProposalStatus::Approved
    );
}

#[test]
fn registry_rejects_duplicate_target_modules() {
    assert!(matches!(
        DUPLICATE_REGISTRY.lookup(UPDATE_ADDRESS),
        Err(PrecompileError::Revert(message)) if message.contains("duplicate")
    ));
}

#[test]
fn public_bonded_admission_records_only_its_exact_liability() {
    assert_eq!(
        PUBLIC_BONDED_REGISTRY
            .lookup(UPDATE_ADDRESS)
            .unwrap()
            .admission(),
        TargetAdmission::PublicBonded {
            amount: U256::from(123u64)
        }
    );

    let mut provider = super::test_provider();
    provider.set_balance(VOTE_ADDRESS, U256::from(130u64));
    let storage = StorageHandle::new(&mut provider);
    let proposal_id = {
        let mut vote = Vote::new(storage);
        vote.create_proposal_with_value(
            ProposalSubmission {
                proposer: Address::repeat_byte(0x99),
                target_module: UPDATE_ADDRESS,
                payload: RAW_PAYLOAD,
                created_height: 10,
                attached_value: U256::from(123u64),
            },
            &PUBLIC_BONDED_REGISTRY,
        )
        .unwrap()
    };
    let vote = Vote::new(StorageHandle::new(&mut provider));
    assert_eq!(
        vote.proposal_bond(proposal_id).unwrap().settlement,
        BondSettlement::Unsettled
    );
    assert_eq!(vote.bond_liabilities().unwrap(), U256::from(123u64));
    drop(vote);
    assert_eq!(provider.get_balance(VOTE_ADDRESS), U256::from(130u64));
}

/// A raw-payload public bonded submission of `proposer` at height 10.
fn raw_bonded_submission(proposer: Address, attached_value: U256) -> ProposalSubmission<'static> {
    ProposalSubmission {
        proposer,
        target_module: UPDATE_ADDRESS,
        payload: RAW_PAYLOAD,
        created_height: 10,
        attached_value,
    }
}

#[test]
fn public_bonded_wrong_value_fails_before_allocation() {
    let outsider = Address::repeat_byte(0x99);
    let mut invalid_provider = super::test_provider();
    {
        let storage = StorageHandle::new(&mut invalid_provider);
        let mut vote = Vote::new(storage);
        for actual in [U256::ZERO, U256::from(122u64), U256::from(124u64)] {
            match vote
                .create_proposal_with_value(
                    raw_bonded_submission(outsider, actual),
                    &PUBLIC_BONDED_REGISTRY,
                )
                .unwrap_err()
            {
                PrecompileError::RevertBytes(bytes) => {
                    assert_eq!(&bytes[..4], IVote::InvalidProposalBond::SELECTOR)
                }
                other => panic!("expected InvalidProposalBond, got {other:?}"),
            }
        }
        assert_eq!(vote.proposal_count.read().unwrap(), U256::ZERO);
        assert_eq!(vote.bond_liabilities().unwrap(), U256::ZERO);
    }
    assert!(invalid_provider.storage.is_empty());
    assert!(invalid_provider.get_events(VOTE_ADDRESS).is_empty());
}

#[test]
fn public_bonded_second_pending_proposal_of_a_proposer_fails() {
    let outsider = Address::repeat_byte(0x99);
    let mut identity_provider = super::test_provider();
    identity_provider.set_balance(VOTE_ADDRESS, U256::from(246u64));
    let storage = StorageHandle::new(&mut identity_provider);
    let mut vote = Vote::new(storage);
    let submission = raw_bonded_submission(outsider, U256::from(123u64));
    vote.create_proposal_with_value(submission, &PUBLIC_BONDED_REGISTRY)
        .unwrap();
    assert!(matches!(
        vote.create_proposal_with_value(submission, &PUBLIC_BONDED_REGISTRY),
        Err(PrecompileError::Revert(message))
            if message == "proposer already has a pending public bonded proposal"
    ));
    assert_eq!(vote.proposal_count.read().unwrap(), U256::from(1u64));
    assert_eq!(vote.bond_liabilities().unwrap(), U256::from(123u64));
}

#[test]
fn public_bonded_global_cap_fails_before_allocation() {
    let mut cap_provider = super::test_provider();
    let cap = MAX_PENDING_PUBLIC_BONDED_PROPOSALS;
    cap_provider.set_balance(VOTE_ADDRESS, U256::from(123u64) * U256::from(cap + 1));
    let storage = StorageHandle::new(&mut cap_provider);
    let mut vote = Vote::new(storage);
    for index in 0..cap {
        let proposer = Address::from_word(U256::from(index + 1).into());
        vote.create_proposal_with_value(
            raw_bonded_submission(proposer, U256::from(123u64)),
            &PUBLIC_BONDED_REGISTRY,
        )
        .unwrap();
    }
    assert!(matches!(
        vote.create_proposal_with_value(
            raw_bonded_submission(Address::repeat_byte(0xaa), U256::from(123u64)),
            &PUBLIC_BONDED_REGISTRY,
        ),
        Err(PrecompileError::Revert(message))
            if message == "too many pending public bonded proposals"
    ));
    assert_eq!(vote.proposal_count.read().unwrap(), U256::from(cap));
    assert_eq!(
        vote.bond_liabilities().unwrap(),
        U256::from(123u64) * U256::from(cap)
    );
}

#[test]
fn public_reservation_failure_rolls_back_proposal_liability_and_logs() {
    let mut provider = super::test_provider();
    provider.set_balance(VOTE_ADDRESS, U256::from(123u64));
    {
        let storage = StorageHandle::new(&mut provider);
        let mut vote = Vote::new(storage.clone());
        assert!(vote
            .create_proposal_with_value(
                ProposalSubmission {
                    proposer: Address::repeat_byte(0x99),
                    target_module: UPDATE_ADDRESS,
                    payload: "fail",
                    created_height: 10,
                    attached_value: U256::from(123u64),
                },
                &PUBLIC_BONDED_REGISTRY,
            )
            .is_err());
        assert_eq!(vote.proposal_count.read().unwrap(), U256::ZERO);
        assert_eq!(vote.bond_liabilities().unwrap(), U256::ZERO);
        assert_eq!(target_marker(&storage, 998), U256::ZERO);
    }
    assert!(provider.storage.is_empty());
    assert!(provider.get_events(VOTE_ADDRESS).is_empty());
}

#[test]
fn public_bonded_execution_error_rolls_back_target_refunds_bond_and_keeps_reservation() {
    let mut provider = super::test_provider();
    provider.set_balance(VOTE_ADDRESS, U256::from(123u64));
    let (storage, mut vote) = validator_vote(&mut provider);
    let proposal_id =
        approved_bonded_proposal(&mut vote, Address::repeat_byte(0x99), "execution-error").unwrap();

    tally_after_window_with(&mut vote, 10, &PUBLIC_BONDED_REGISTRY).unwrap();

    assert_finalized_error(&vote, proposal_id);
    assert_bond_closed(&vote, proposal_id, BondSettlement::Refunded);
    assert_eq!(storage.balance(VOTE_ADDRESS).unwrap(), U256::ZERO);
    assert_eq!(
        storage.balance(Address::repeat_byte(0x99)).unwrap(),
        U256::from(123u64)
    );
    assert_eq!(
        target_marker(&storage, 997),
        proposal_id,
        "admission reservation must survive target execution rollback"
    );
    assert_eq!(
        target_marker(&storage, 996),
        U256::ZERO,
        "partial target execution must roll back"
    );
}

#[test]
fn approved_public_bond_refunds_once_and_preserves_forced_surplus() {
    let owner = Address::repeat_byte(0x99);
    let forced_surplus = U256::from(7u64);
    let starting_owner_balance = U256::from(11u64);
    let mut provider = super::test_provider();
    provider.set_balance(VOTE_ADDRESS, U256::from(123u64) + forced_surplus);
    provider.set_balance(owner, starting_owner_balance);
    let proposal_id;
    {
        let (storage, mut vote) = validator_vote(&mut provider);
        proposal_id = approved_bonded_proposal(&mut vote, owner, RAW_PAYLOAD).unwrap();

        let deadline = 10 + VOTING_WINDOW_BLOCKS;
        vote.begin_block_with(deadline + 1, &PUBLIC_BONDED_REGISTRY)
            .unwrap();
        assert_eq!(
            proposal_status(&vote, proposal_id),
            ProposalStatus::Approved
        );
        assert_bond_closed(&vote, proposal_id, BondSettlement::Refunded);
        assert_eq!(storage.balance(VOTE_ADDRESS).unwrap(), forced_surplus);
        assert_eq!(
            storage.balance(owner).unwrap(),
            starting_owner_balance + U256::from(123u64)
        );

        vote.begin_block_with(deadline + 2, &PUBLIC_BONDED_REGISTRY)
            .unwrap();
        assert_eq!(storage.balance(VOTE_ADDRESS).unwrap(), forced_surplus);
        assert_eq!(
            storage.balance(owner).unwrap(),
            starting_owner_balance + U256::from(123u64)
        );
    }

    let refunds = provider
        .get_events(VOTE_ADDRESS)
        .iter()
        .filter(|log| log.topics().first() == Some(&IVote::ProposalBondRefunded::SIGNATURE_HASH))
        .collect::<Vec<_>>();
    assert_eq!(refunds.len(), 1);
    let refund = IVote::ProposalBondRefunded::decode_log_data(refunds[0]).unwrap();
    assert_eq!(refund.proposalId, proposal_id);
    assert_eq!(refund.owner, owner);
    assert_eq!(refund.amount, U256::from(123u64));
}

#[test]
fn expired_public_bond_burns_once_and_preserves_forced_surplus() {
    let owner = Address::repeat_byte(0x99);
    let forced_surplus = U256::from(7u64);
    let mut provider = super::test_provider();
    provider.set_balance(VOTE_ADDRESS, U256::from(123u64) + forced_surplus);
    let proposal_id;
    {
        let (storage, mut vote) = validator_vote(&mut provider);
        proposal_id = vote
            .create_proposal_with_value(
                ProposalSubmission {
                    proposer: owner,
                    target_module: UPDATE_ADDRESS,
                    payload: RAW_PAYLOAD,
                    created_height: 10,
                    attached_value: U256::from(123u64),
                },
                &PUBLIC_BONDED_REGISTRY,
            )
            .unwrap();

        let deadline = 10 + VOTING_WINDOW_BLOCKS;
        vote.begin_block_with(deadline + 1, &PUBLIC_BONDED_REGISTRY)
            .unwrap();
        assert_eq!(proposal_status(&vote, proposal_id), ProposalStatus::Expired);
        assert_bond_closed(&vote, proposal_id, BondSettlement::Burned);
        assert_eq!(storage.balance(VOTE_ADDRESS).unwrap(), forced_surplus);
        assert_eq!(storage.balance(owner).unwrap(), U256::ZERO);

        vote.begin_block_with(deadline + 2, &PUBLIC_BONDED_REGISTRY)
            .unwrap();
        assert_eq!(storage.balance(VOTE_ADDRESS).unwrap(), forced_surplus);
    }

    let burns = provider
        .get_events(VOTE_ADDRESS)
        .iter()
        .filter(|log| log.topics().first() == Some(&IVote::ProposalBondBurned::SIGNATURE_HASH))
        .collect::<Vec<_>>();
    assert_eq!(burns.len(), 1);
    let burn = IVote::ProposalBondBurned::decode_log_data(burns[0]).unwrap();
    assert_eq!(burn.proposalId, proposal_id);
    assert_eq!(burn.owner, owner);
    assert_eq!(burn.amount, U256::from(123u64));
}

#[test]
fn insufficient_escrow_rolls_back_target_status_index_accounting_and_events() {
    let owner = Address::repeat_byte(0x99);
    let mut provider = super::test_provider();
    provider.set_balance(VOTE_ADDRESS, U256::from(123u64));
    let proposal_id;
    {
        let (storage, mut vote) = validator_vote(&mut provider);
        proposal_id = approved_bonded_proposal(&mut vote, owner, "applied-write").unwrap();
        storage
            .decrease_balance(VOTE_ADDRESS, U256::from(1u64))
            .unwrap();

        assert!(matches!(
            tally_after_window_with(&mut vote, 10, &PUBLIC_BONDED_REGISTRY),
            Err(PrecompileError::Fatal(_))
        ));
        assert_eq!(proposal_status(&vote, proposal_id), ProposalStatus::Pending);
        assert_eq!(vote.list_pending_proposal_ids().unwrap(), vec![proposal_id]);
        assert_eq!(
            vote.proposal_bond(proposal_id).unwrap().settlement,
            BondSettlement::Unsettled
        );
        assert_eq!(vote.bond_liabilities().unwrap(), U256::from(123u64));
        assert_eq!(
            target_marker(&storage, 994),
            proposal_id,
            "admission reservation must survive failed settlement"
        );
        assert_eq!(
            target_marker(&storage, 995),
            U256::ZERO,
            "target execution must roll back with failed settlement"
        );
        assert_eq!(storage.balance(VOTE_ADDRESS).unwrap(), U256::from(122u64));
        assert_eq!(storage.balance(owner).unwrap(), U256::ZERO);
    }

    assert_event_counts(
        &provider,
        &[
            (IVote::ProposalBondRefunded::SIGNATURE_HASH, 0),
            (IVote::ProposalApproved::SIGNATURE_HASH, 0),
        ],
    );
}

#[test]
fn failure_after_every_approved_finalization_mutation_rolls_back_everything() {
    let baseline_id = assert_every_finalization_mutation_rolls_back(
        public_bonded_finalization_fixture,
        PendingBond {
            liabilities: U256::from(123u64),
            vote_balance: U256::from(130u64),
        },
        |storage, proposal_id, failure_point| {
            assert_eq!(
                storage.balance(Address::repeat_byte(0x99)).unwrap(),
                U256::from(11u64),
                "failure point {failure_point}"
            );
            assert_eq!(
                target_marker(storage, 994),
                proposal_id,
                "failure point {failure_point}"
            );
            assert_eq!(
                target_marker(storage, 995),
                U256::ZERO,
                "failure point {failure_point}"
            );
        },
        &[
            IVote::ProposalBondRefunded::SIGNATURE_HASH,
            IVote::ProposalApproved::SIGNATURE_HASH,
        ],
    );
    assert_eq!(baseline_id, U256::from(1u64));
}

#[test]
fn approved_handler_failure_rolls_back_target_and_records_error_without_replay() {
    let mut provider = super::test_provider();
    let proposal_id;
    {
        let (storage, mut vote) = validator_vote(&mut provider);
        proposal_id = approved_legacy_proposal(&mut vote, &REJECTING_REGISTRY).unwrap();

        let deadline = 10 + VOTING_WINDOW_BLOCKS;
        tally_after_window_with(&mut vote, 10, &REJECTING_REGISTRY).unwrap();
        assert_finalized_error(&vote, proposal_id);
        assert_eq!(target_marker(&storage, 999), U256::ZERO);

        vote.begin_block_with(deadline + 2, &REJECTING_REGISTRY)
            .unwrap();
    }

    let errored_count = count_events(&provider, IVote::ProposalErrored::SIGNATURE_HASH);
    assert_eq!(errored_count, 1, "error replay emitted a second log");
}

#[test]
fn infrastructure_failure_rolls_back_target_and_aborts_without_changing_proposal() {
    let mut provider = super::test_provider();
    let proposal_id;
    {
        let (storage, mut vote) = validator_vote(&mut provider);
        proposal_id = approved_legacy_proposal(&mut vote, &TECHNICALLY_FAILING_REGISTRY).unwrap();
        let err =
            tally_after_window_with(&mut vote, 10, &TECHNICALLY_FAILING_REGISTRY).unwrap_err();
        assert!(matches!(err, PrecompileError::Fatal(_)));
        assert_eq!(proposal_status(&vote, proposal_id), ProposalStatus::Pending);
        assert_eq!(vote.list_pending_proposal_ids().unwrap(), vec![proposal_id]);
        assert_eq!(target_marker(&storage, 999), U256::ZERO);
    }

    assert_event_counts(
        &provider,
        &[
            (IVote::ProposalErrored::SIGNATURE_HASH, 0),
            (IVote::ProposalApproved::SIGNATURE_HASH, 0),
        ],
    );
}

#[test]
fn outer_hook_checkpoint_revert_restores_pending_state_index_and_logs() {
    let mut provider = super::test_provider();
    let proposal_id;
    {
        let (storage, mut vote) = validator_vote(&mut provider);
        proposal_id = create_proposal_test(
            &mut vote,
            PROPOSER,
            UPDATE_ADDRESS,
            "{\"version\":\"1.2\"}",
            10,
        )
        .unwrap();
        approve_by_quorum(&mut vote, proposal_id).unwrap();
        let result: Result<()> = storage.with_checkpoint(|| {
            tally_after_window_with(&mut vote, 10, test_vote_registry())?;
            assert_eq!(
                vote.proposals
                    .get(proposal_id)?
                    .expect("proposal")
                    .proposal_status()?,
                ProposalStatus::Approved
            );
            Err(PrecompileError::Fatal("forced late hook failure".into()))
        });
        assert!(matches!(result, Err(PrecompileError::Fatal(_))));
        assert_eq!(proposal_status(&vote, proposal_id), ProposalStatus::Pending);
        assert_eq!(vote.list_pending_proposal_ids().unwrap(), vec![proposal_id]);
    }

    assert_event_counts(
        &provider,
        &[
            (IVote::ProposalApproved::SIGNATURE_HASH, 0),
            (IVote::ProposalCreated::SIGNATURE_HASH, 1),
        ],
    );
}

/// Vote is a payable route, so the boundary credits value to its address. Its
/// dispatch must refuse value for every selector outside `PAYABLE_SELECTORS`.
/// Otherwise, a funded call to any other selector would strand native value at an
/// address whose only outward path is the proposal-bond accounting.
///
/// Characterization: the negative-match block this replaced already covered
/// these selectors with the same message. Thus the test pins current behavior
/// and does not prove a fix. Its value is that it catches a future removal of
/// the guard.
#[test]
fn unpublished_selectors_refuse_native_value() {
    use alloy_sol_types::SolCall;

    use crate::precompile::{dispatch_with_handlers, IVote};

    let calls = [
        IVote::castVoteCall {
            proposalId: U256::ZERO,
            approve: true,
        }
        .abi_encode(),
        IVote::getProposalCall {
            proposalId: U256::ZERO,
        }
        .abi_encode(),
    ];

    let mut provider = super::test_provider();
    StorageHandle::enter(&mut provider, |storage| {
        for data in &calls {
            let funded = dispatch_with_handlers(
                storage.clone(),
                data,
                Address::ZERO,
                U256::from(1u64),
                &REJECTING_REGISTRY,
            );
            assert!(
                matches!(
                    funded,
                    Err(PrecompileError::Revert(ref message))
                        if message == "non-payable function called with value"
                ),
                "unpublished selector must refuse value, got {funded:?}"
            );
        }
    });
}
