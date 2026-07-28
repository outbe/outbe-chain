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
    handlers::{TargetExecutionOutcome, VoteTarget, VoteTargetContext, VoteTargetRegistry},
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

    fn validate(&self, payload: &[u8], _context: VoteTargetContext) -> Result<()> {
        if serde_json::from_slice::<Value>(payload).is_ok_and(|value| value.is_object()) {
            Ok(())
        } else {
            Err(PrecompileError::Revert("expected object".into()))
        }
    }

    fn handle_approved(
        &self,
        ctx: &BlockRuntimeContext,
        _proposal_id: U256,
        _payload: &[u8],
        _context: VoteTargetContext,
    ) -> Result<TargetExecutionOutcome> {
        ctx.storage
            .sstore(UPDATE_ADDRESS, U256::from(999u64), U256::from(1u64))?;
        Ok(TargetExecutionOutcome::Error {
            reason: "characterized target failure".into(),
        })
    }
}

static REJECTING_TARGET: RejectingApprovedTarget = RejectingApprovedTarget;
static REJECTING_HANDLERS: &[&dyn VoteTarget] = &[&REJECTING_TARGET];
static REJECTING_REGISTRY: VoteTargetRegistry = VoteTargetRegistry::new(REJECTING_HANDLERS);

struct TechnicallyFailingTarget;

impl VoteTarget for TechnicallyFailingTarget {
    fn target_module(&self) -> Address {
        UPDATE_ADDRESS
    }

    fn validate(&self, payload: &[u8], _context: VoteTargetContext) -> Result<()> {
        if serde_json::from_slice::<Value>(payload).is_ok_and(|value| value.is_object()) {
            Ok(())
        } else {
            Err(PrecompileError::Revert("expected object".into()))
        }
    }

    fn handle_approved(
        &self,
        ctx: &BlockRuntimeContext,
        _proposal_id: U256,
        _payload: &[u8],
        _context: VoteTargetContext,
    ) -> Result<TargetExecutionOutcome> {
        ctx.storage
            .sstore(UPDATE_ADDRESS, U256::from(999u64), U256::from(1u64))?;
        Err(PrecompileError::Fatal(
            "characterized infrastructure failure".into(),
        ))
    }
}

static TECHNICALLY_FAILING_TARGET: TechnicallyFailingTarget = TechnicallyFailingTarget;
static TECHNICALLY_FAILING_HANDLERS: &[&dyn VoteTarget] = &[&TECHNICALLY_FAILING_TARGET];
static TECHNICALLY_FAILING_REGISTRY: VoteTargetRegistry =
    VoteTargetRegistry::new(TECHNICALLY_FAILING_HANDLERS);

const RAW_PAYLOAD: &str = "{ \"z\":1, \"a\": [2, 3] }";

struct RawContextTarget;

impl VoteTarget for RawContextTarget {
    fn target_module(&self) -> Address {
        UPDATE_ADDRESS
    }

    fn validate(&self, payload: &[u8], context: VoteTargetContext) -> Result<()> {
        if payload != RAW_PAYLOAD.as_bytes()
            || context.proposer != PROPOSER
            || context.attached_value != U256::ZERO
            || context.block_number != 10
            || context.chain_id != 1
        {
            return Err(PrecompileError::Revert(
                "raw payload or target context changed".into(),
            ));
        }
        Ok(())
    }

    fn handle_approved(
        &self,
        _ctx: &BlockRuntimeContext,
        _proposal_id: U256,
        _payload: &[u8],
        _context: VoteTargetContext,
    ) -> Result<TargetExecutionOutcome> {
        Ok(TargetExecutionOutcome::Applied)
    }
}

static RAW_CONTEXT_TARGET: RawContextTarget = RawContextTarget;
static RAW_CONTEXT_HANDLERS: &[&dyn VoteTarget] = &[&RAW_CONTEXT_TARGET];
static RAW_CONTEXT_REGISTRY: VoteTargetRegistry = VoteTargetRegistry::new(RAW_CONTEXT_HANDLERS);

fn block_context(storage: StorageHandle<'_>, block_number: u64) -> BlockRuntimeContext<'_> {
    BlockRuntimeContext::new(BlockContext::empty_for_tests(block_number, 0, 1), storage)
}

#[test]
fn creation_preserves_original_payload_bytes_in_state_and_log() {
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
fn approved_handler_failure_rolls_back_target_and_records_error_without_replay() {
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
            ProposalStatus::Error
        );
        assert_eq!(vote.list_pending_proposal_ids().unwrap(), vec![proposal_id]);
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
