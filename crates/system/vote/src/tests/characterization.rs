use alloy_primitives::{Address, U256};
use alloy_sol_types::SolEvent;
use outbe_primitives::{
    addresses::{UPDATE_ADDRESS, VOTE_ADDRESS},
    block::{BlockContext, BlockRuntimeContext},
    error::{PrecompileError, Result},
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
};
use serde_json::Value;

use crate::{
    constants::VOTING_WINDOW_BLOCKS,
    handlers::{VoteTarget, VoteTargetRegistry},
    precompile::IVote,
    schema::{ProposalStatus, Vote},
};

use super::{
    create_proposal_test, setup_default_validators, test_vote_registry, PROPOSER, VOTER_A,
};

struct RejectingApprovedTarget;

impl VoteTarget for RejectingApprovedTarget {
    fn target_module(&self) -> Address {
        UPDATE_ADDRESS
    }

    fn validate(&self, payload: &Value, _current_height: u64, _chain_id: u64) -> Result<()> {
        if payload.is_object() {
            Ok(())
        } else {
            Err(PrecompileError::Revert("expected object".into()))
        }
    }

    fn handle_approved(
        &self,
        _ctx: &BlockRuntimeContext,
        _proposal_id: U256,
        _payload: &Value,
    ) -> Result<()> {
        Err(PrecompileError::Revert(
            "characterized target failure".into(),
        ))
    }
}

static REJECTING_TARGET: RejectingApprovedTarget = RejectingApprovedTarget;
static REJECTING_HANDLERS: &[&dyn VoteTarget] = &[&REJECTING_TARGET];
static REJECTING_REGISTRY: VoteTargetRegistry = VoteTargetRegistry::new(REJECTING_HANDLERS);

fn block_context(storage: StorageHandle<'_>, block_number: u64) -> BlockRuntimeContext<'_> {
    BlockRuntimeContext::new(BlockContext::empty_for_tests(block_number, 0, 1), storage)
}

#[test]
fn creation_preserves_original_payload_bytes_in_state_and_log() {
    const RAW_PAYLOAD: &str = "{ \"z\":1, \"a\": [2, 3] }";
    let mut provider = HashMapStorageProvider::new(1);
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
                &REJECTING_REGISTRY,
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
fn approved_handler_failure_becomes_rejected_and_terminal_replay_is_noop() {
    let mut provider = HashMapStorageProvider::new(1);
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
            ProposalStatus::Rejected
        );
        assert!(vote.list_pending_proposal_ids().unwrap().is_empty());

        vote.process_begin_block(&block_context(storage, deadline + 2), &REJECTING_REGISTRY)
            .unwrap();
    }

    let rejected_count = provider
        .get_events(VOTE_ADDRESS)
        .iter()
        .filter(|log| log.topics().first() == Some(&IVote::ProposalRejected::SIGNATURE_HASH))
        .count();
    assert_eq!(rejected_count, 1, "terminal replay emitted a second log");
}

#[test]
fn outer_hook_checkpoint_revert_restores_pending_state_index_and_logs() {
    let mut provider = HashMapStorageProvider::new(1);
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
