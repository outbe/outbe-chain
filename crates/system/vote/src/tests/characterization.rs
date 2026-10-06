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
};

use super::targets::*;
use super::{
    create_proposal_test, setup_default_validators, test_vote_registry, PROPOSER, VOTER_A,
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
        let storage = StorageHandle::new(&mut provider);
        setup_default_validators(storage.clone());
        let mut vote = Vote::new(storage.clone());
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
        assert_eq!(
            storage.sload(UPDATE_ADDRESS, U256::from(999u64)).unwrap(),
            U256::ZERO
        );
    }
    assert!(provider.get_events(VOTE_ADDRESS).is_empty());
}

#[test]
fn execution_receives_original_payload_and_exact_context() {
    let mut provider = super::test_provider();
    let storage = StorageHandle::new(&mut provider);
    setup_default_validators(storage.clone());
    let mut vote = Vote::new(storage.clone());
    let proposal_id = vote
        .create_proposal(
            PROPOSER,
            UPDATE_ADDRESS,
            RAW_PAYLOAD,
            10,
            &RAW_CONTEXT_REGISTRY,
        )
        .unwrap();
    vote.cast_vote_approve(proposal_id, PROPOSER, true, 11)
        .unwrap();
    vote.cast_vote_approve(proposal_id, VOTER_A, true, 11)
        .unwrap();

    let finalize_height = 10 + VOTING_WINDOW_BLOCKS + 1;
    vote.process_begin_block(
        &block_context(storage, finalize_height),
        &RAW_CONTEXT_REGISTRY,
    )
    .unwrap();

    assert_eq!(
        vote.proposals
            .get(proposal_id)
            .unwrap()
            .unwrap()
            .proposal_status()
            .unwrap(),
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
            Address::repeat_byte(0x99),
            UPDATE_ADDRESS,
            RAW_PAYLOAD,
            10,
            U256::from(123u64),
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

#[test]
fn public_bonded_value_identity_and_global_caps_fail_before_allocation() {
    let outsider = Address::repeat_byte(0x99);

    let mut invalid_provider = super::test_provider();
    {
        let storage = StorageHandle::new(&mut invalid_provider);
        let mut vote = Vote::new(storage);
        for actual in [U256::ZERO, U256::from(122u64), U256::from(124u64)] {
            match vote
                .create_proposal_with_value(
                    outsider,
                    UPDATE_ADDRESS,
                    RAW_PAYLOAD,
                    10,
                    actual,
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

    let mut identity_provider = super::test_provider();
    identity_provider.set_balance(VOTE_ADDRESS, U256::from(246u64));
    {
        let storage = StorageHandle::new(&mut identity_provider);
        let mut vote = Vote::new(storage);
        vote.create_proposal_with_value(
            outsider,
            UPDATE_ADDRESS,
            RAW_PAYLOAD,
            10,
            U256::from(123u64),
            &PUBLIC_BONDED_REGISTRY,
        )
        .unwrap();
        assert!(matches!(
            vote.create_proposal_with_value(
                outsider,
                UPDATE_ADDRESS,
                RAW_PAYLOAD,
                10,
                U256::from(123u64),
                &PUBLIC_BONDED_REGISTRY,
            ),
            Err(PrecompileError::Revert(message))
                if message == "proposer already has a pending public bonded proposal"
        ));
        assert_eq!(vote.proposal_count.read().unwrap(), U256::from(1u64));
        assert_eq!(vote.bond_liabilities().unwrap(), U256::from(123u64));
    }

    let mut cap_provider = super::test_provider();
    let cap = MAX_PENDING_PUBLIC_BONDED_PROPOSALS;
    cap_provider.set_balance(VOTE_ADDRESS, U256::from(123u64) * U256::from(cap + 1));
    {
        let storage = StorageHandle::new(&mut cap_provider);
        let mut vote = Vote::new(storage);
        for index in 0..cap {
            vote.create_proposal_with_value(
                Address::from_word(U256::from(index + 1).into()),
                UPDATE_ADDRESS,
                RAW_PAYLOAD,
                10,
                U256::from(123u64),
                &PUBLIC_BONDED_REGISTRY,
            )
            .unwrap();
        }
        assert!(matches!(
            vote.create_proposal_with_value(
                Address::repeat_byte(0xaa),
                UPDATE_ADDRESS,
                RAW_PAYLOAD,
                10,
                U256::from(123u64),
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
                Address::repeat_byte(0x99),
                UPDATE_ADDRESS,
                "fail",
                10,
                U256::from(123u64),
                &PUBLIC_BONDED_REGISTRY,
            )
            .is_err());
        assert_eq!(vote.proposal_count.read().unwrap(), U256::ZERO);
        assert_eq!(vote.bond_liabilities().unwrap(), U256::ZERO);
        assert_eq!(
            storage.sload(UPDATE_ADDRESS, U256::from(998u64)).unwrap(),
            U256::ZERO
        );
    }
    assert!(provider.storage.is_empty());
    assert!(provider.get_events(VOTE_ADDRESS).is_empty());
}

#[test]
fn public_bonded_execution_error_rolls_back_target_refunds_bond_and_keeps_reservation() {
    let mut provider = super::test_provider();
    provider.set_balance(VOTE_ADDRESS, U256::from(123u64));
    let storage = StorageHandle::new(&mut provider);
    setup_default_validators(storage.clone());
    let mut vote = Vote::new(storage.clone());
    let proposal_id = vote
        .create_proposal_with_value(
            Address::repeat_byte(0x99),
            UPDATE_ADDRESS,
            "execution-error",
            10,
            U256::from(123u64),
            &PUBLIC_BONDED_REGISTRY,
        )
        .unwrap();
    vote.cast_vote_approve(proposal_id, PROPOSER, true, 11)
        .unwrap();
    vote.cast_vote_approve(proposal_id, VOTER_A, true, 11)
        .unwrap();

    let deadline = 10 + VOTING_WINDOW_BLOCKS;
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
    assert_eq!(
        vote.list_pending_proposal_ids().unwrap(),
        Vec::<U256>::new()
    );
    assert_eq!(
        vote.proposal_bond(proposal_id).unwrap().settlement,
        BondSettlement::Refunded
    );
    assert_eq!(vote.bond_liabilities().unwrap(), U256::ZERO);
    assert_eq!(storage.balance(VOTE_ADDRESS).unwrap(), U256::ZERO);
    assert_eq!(
        storage.balance(Address::repeat_byte(0x99)).unwrap(),
        U256::from(123u64)
    );
    assert_eq!(
        storage.sload(UPDATE_ADDRESS, U256::from(997u64)).unwrap(),
        proposal_id,
        "admission reservation must survive target execution rollback"
    );
    assert_eq!(
        storage.sload(UPDATE_ADDRESS, U256::from(996u64)).unwrap(),
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
        let storage = StorageHandle::new(&mut provider);
        setup_default_validators(storage.clone());
        let mut vote = Vote::new(storage.clone());
        proposal_id = vote
            .create_proposal_with_value(
                owner,
                UPDATE_ADDRESS,
                RAW_PAYLOAD,
                10,
                U256::from(123u64),
                &PUBLIC_BONDED_REGISTRY,
            )
            .unwrap();
        vote.cast_vote_approve(proposal_id, PROPOSER, true, 11)
            .unwrap();
        vote.cast_vote_approve(proposal_id, VOTER_A, true, 11)
            .unwrap();

        let deadline = 10 + VOTING_WINDOW_BLOCKS;
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
            ProposalStatus::Approved
        );
        assert_eq!(
            vote.proposal_bond(proposal_id).unwrap().settlement,
            BondSettlement::Refunded
        );
        assert_eq!(vote.bond_liabilities().unwrap(), U256::ZERO);
        assert_eq!(storage.balance(VOTE_ADDRESS).unwrap(), forced_surplus);
        assert_eq!(
            storage.balance(owner).unwrap(),
            starting_owner_balance + U256::from(123u64)
        );

        vote.process_begin_block(
            &block_context(storage.clone(), deadline + 2),
            &PUBLIC_BONDED_REGISTRY,
        )
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
        let storage = StorageHandle::new(&mut provider);
        setup_default_validators(storage.clone());
        let mut vote = Vote::new(storage.clone());
        proposal_id = vote
            .create_proposal_with_value(
                owner,
                UPDATE_ADDRESS,
                RAW_PAYLOAD,
                10,
                U256::from(123u64),
                &PUBLIC_BONDED_REGISTRY,
            )
            .unwrap();

        let deadline = 10 + VOTING_WINDOW_BLOCKS;
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
            ProposalStatus::Expired
        );
        assert_eq!(
            vote.proposal_bond(proposal_id).unwrap().settlement,
            BondSettlement::Burned
        );
        assert_eq!(vote.bond_liabilities().unwrap(), U256::ZERO);
        assert_eq!(storage.balance(VOTE_ADDRESS).unwrap(), forced_surplus);
        assert_eq!(storage.balance(owner).unwrap(), U256::ZERO);

        vote.process_begin_block(
            &block_context(storage.clone(), deadline + 2),
            &PUBLIC_BONDED_REGISTRY,
        )
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
        let storage = StorageHandle::new(&mut provider);
        setup_default_validators(storage.clone());
        let mut vote = Vote::new(storage.clone());
        proposal_id = vote
            .create_proposal_with_value(
                owner,
                UPDATE_ADDRESS,
                "applied-write",
                10,
                U256::from(123u64),
                &PUBLIC_BONDED_REGISTRY,
            )
            .unwrap();
        vote.cast_vote_approve(proposal_id, PROPOSER, true, 11)
            .unwrap();
        vote.cast_vote_approve(proposal_id, VOTER_A, true, 11)
            .unwrap();
        storage
            .decrease_balance(VOTE_ADDRESS, U256::from(1u64))
            .unwrap();

        let deadline = 10 + VOTING_WINDOW_BLOCKS;
        assert!(matches!(
            vote.process_begin_block(
                &block_context(storage.clone(), deadline + 1),
                &PUBLIC_BONDED_REGISTRY,
            ),
            Err(PrecompileError::Fatal(_))
        ));
        assert_eq!(
            vote.proposals
                .get(proposal_id)
                .unwrap()
                .unwrap()
                .proposal_status()
                .unwrap(),
            ProposalStatus::Pending
        );
        assert_eq!(vote.list_pending_proposal_ids().unwrap(), vec![proposal_id]);
        assert_eq!(
            vote.proposal_bond(proposal_id).unwrap().settlement,
            BondSettlement::Unsettled
        );
        assert_eq!(vote.bond_liabilities().unwrap(), U256::from(123u64));
        assert_eq!(
            storage.sload(UPDATE_ADDRESS, U256::from(994u64)).unwrap(),
            proposal_id,
            "admission reservation must survive failed settlement"
        );
        assert_eq!(
            storage.sload(UPDATE_ADDRESS, U256::from(995u64)).unwrap(),
            U256::ZERO,
            "target execution must roll back with failed settlement"
        );
        assert_eq!(storage.balance(VOTE_ADDRESS).unwrap(), U256::from(122u64));
        assert_eq!(storage.balance(owner).unwrap(), U256::ZERO);
    }

    let logs = provider.get_events(VOTE_ADDRESS);
    assert_eq!(
        logs.iter()
            .filter(|log| {
                log.topics().first() == Some(&IVote::ProposalBondRefunded::SIGNATURE_HASH)
            })
            .count(),
        0
    );
    assert_eq!(
        logs.iter()
            .filter(|log| log.topics().first() == Some(&IVote::ProposalApproved::SIGNATURE_HASH))
            .count(),
        0
    );
}

#[test]
fn failure_after_every_approved_finalization_mutation_rolls_back_everything() {
    let (mut baseline, baseline_id, deadline) = public_bonded_finalization_fixture();
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
    assert_eq!(baseline_id, U256::from(1u64));

    for failure_point in 0..mutation_count {
        let (mut provider, proposal_id, deadline) = public_bonded_finalization_fixture();
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
            U256::from(123u64),
            "failure point {failure_point}"
        );
        assert_eq!(
            storage.balance(VOTE_ADDRESS).unwrap(),
            U256::from(130u64),
            "failure point {failure_point}"
        );
        assert_eq!(
            storage.balance(Address::repeat_byte(0x99)).unwrap(),
            U256::from(11u64),
            "failure point {failure_point}"
        );
        assert_eq!(
            storage.sload(UPDATE_ADDRESS, U256::from(994u64)).unwrap(),
            proposal_id,
            "failure point {failure_point}"
        );
        assert_eq!(
            storage.sload(UPDATE_ADDRESS, U256::from(995u64)).unwrap(),
            U256::ZERO,
            "failure point {failure_point}"
        );
        drop(vote);
        drop(storage);
        assert_eq!(
            provider
                .get_events(VOTE_ADDRESS)
                .iter()
                .filter(|log| {
                    log.topics().first() == Some(&IVote::ProposalBondRefunded::SIGNATURE_HASH)
                })
                .count(),
            0,
            "failure point {failure_point}"
        );
        assert_eq!(
            provider
                .get_events(VOTE_ADDRESS)
                .iter()
                .filter(|log| {
                    log.topics().first() == Some(&IVote::ProposalApproved::SIGNATURE_HASH)
                })
                .count(),
            0,
            "failure point {failure_point}"
        );
    }
}

#[test]
fn approved_handler_failure_rolls_back_target_and_records_error_without_replay() {
    let mut provider = super::test_provider();
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
        assert_eq!(
            vote.list_pending_proposal_ids().unwrap(),
            Vec::<U256>::new()
        );
        assert_eq!(
            storage.sload(UPDATE_ADDRESS, U256::from(999u64)).unwrap(),
            U256::ZERO
        );

        vote.process_begin_block(&block_context(storage, deadline + 2), &REJECTING_REGISTRY)
            .unwrap();
    }

    let errored_count = provider
        .get_events(VOTE_ADDRESS)
        .iter()
        .filter(|log| log.topics().first() == Some(&IVote::ProposalErrored::SIGNATURE_HASH))
        .count();
    assert_eq!(errored_count, 1, "error replay emitted a second log");
}

#[test]
fn infrastructure_failure_rolls_back_target_and_aborts_without_changing_proposal() {
    let mut provider = super::test_provider();
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
                &TECHNICALLY_FAILING_REGISTRY,
            )
            .unwrap();
        vote.cast_vote_approve(proposal_id, PROPOSER, true, 11)
            .unwrap();
        vote.cast_vote_approve(proposal_id, VOTER_A, true, 11)
            .unwrap();

        let deadline = 10 + VOTING_WINDOW_BLOCKS;
        let err = vote
            .process_begin_block(
                &block_context(storage.clone(), deadline + 1),
                &TECHNICALLY_FAILING_REGISTRY,
            )
            .unwrap_err();
        assert!(matches!(err, PrecompileError::Fatal(_)));
        assert_eq!(
            vote.proposals
                .get(proposal_id)
                .unwrap()
                .unwrap()
                .proposal_status()
                .unwrap(),
            ProposalStatus::Pending
        );
        assert_eq!(vote.list_pending_proposal_ids().unwrap(), vec![proposal_id]);
        assert_eq!(
            storage.sload(UPDATE_ADDRESS, U256::from(999u64)).unwrap(),
            U256::ZERO
        );
    }

    let logs = provider.get_events(VOTE_ADDRESS);
    assert_eq!(
        logs.iter()
            .filter(|log| log.topics().first() == Some(&IVote::ProposalErrored::SIGNATURE_HASH))
            .count(),
        0
    );
    assert_eq!(
        logs.iter()
            .filter(|log| log.topics().first() == Some(&IVote::ProposalApproved::SIGNATURE_HASH))
            .count(),
        0
    );
}

#[test]
fn outer_hook_checkpoint_revert_restores_pending_state_index_and_logs() {
    let mut provider = super::test_provider();
    let proposal_id;
    {
        let storage = StorageHandle::new(&mut provider);
        setup_default_validators(storage.clone());
        let mut vote = Vote::new(storage.clone());
        proposal_id = create_proposal_test(
            &mut vote,
            PROPOSER,
            UPDATE_ADDRESS,
            "{\"version\":\"1.2\"}",
            10,
        )
        .unwrap();
        vote.cast_vote_approve(proposal_id, PROPOSER, true, 11)
            .unwrap();
        vote.cast_vote_approve(proposal_id, VOTER_A, true, 11)
            .unwrap();

        let deadline = 10 + VOTING_WINDOW_BLOCKS;
        let result: Result<()> = storage.with_checkpoint(|| {
            vote.process_begin_block(
                &block_context(storage.clone(), deadline + 1),
                test_vote_registry(),
            )?;
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
        assert_eq!(
            vote.proposals
                .get(proposal_id)
                .unwrap()
                .unwrap()
                .proposal_status()
                .unwrap(),
            ProposalStatus::Pending
        );
        assert_eq!(vote.list_pending_proposal_ids().unwrap(), vec![proposal_id]);
    }

    let logs = provider.get_events(VOTE_ADDRESS);
    assert_eq!(
        logs.iter()
            .filter(|log| log.topics().first() == Some(&IVote::ProposalApproved::SIGNATURE_HASH))
            .count(),
        0
    );
    assert_eq!(
        logs.iter()
            .filter(|log| log.topics().first() == Some(&IVote::ProposalCreated::SIGNATURE_HASH))
            .count(),
        1
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
