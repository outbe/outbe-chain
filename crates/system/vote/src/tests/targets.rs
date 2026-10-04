//! Synthetic vote targets and fixtures shared by the characterization tests.

use alloy_primitives::{Address, U256};
use outbe_primitives::{
    addresses::{UPDATE_ADDRESS, VOTE_ADDRESS},
    block::{BlockContext, BlockRuntimeContext},
    error::{PrecompileError, Result},
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
};
use serde_json::Value;

use crate::{
    constants::VOTING_WINDOW_BLOCKS,
    handlers::{
        TargetAdmission, TargetExecutionOutcome, VoteTarget, VoteTargetContext, VoteTargetRegistry,
    },
    schema::Vote,
};

use super::{setup_default_validators, PROPOSER, VOTER_A};

pub(super) struct RejectingApprovedTarget;

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

pub(super) static REJECTING_TARGET: RejectingApprovedTarget = RejectingApprovedTarget;
pub(super) static REJECTING_HANDLERS: &[&dyn VoteTarget] = &[&REJECTING_TARGET];
pub(super) static REJECTING_REGISTRY: VoteTargetRegistry =
    VoteTargetRegistry::new(REJECTING_HANDLERS);

pub(super) struct TechnicallyFailingTarget;

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

pub(super) static TECHNICALLY_FAILING_TARGET: TechnicallyFailingTarget = TechnicallyFailingTarget;
pub(super) static TECHNICALLY_FAILING_HANDLERS: &[&dyn VoteTarget] = &[&TECHNICALLY_FAILING_TARGET];
pub(super) static TECHNICALLY_FAILING_REGISTRY: VoteTargetRegistry =
    VoteTargetRegistry::new(TECHNICALLY_FAILING_HANDLERS);

pub(super) const RAW_PAYLOAD: &str = "{ \"z\":1, \"a\": [2, 3] }";

pub(super) struct RawContextTarget;

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
        proposal_id: U256,
        payload: &[u8],
        context: VoteTargetContext,
    ) -> Result<TargetExecutionOutcome> {
        let expected_height = 10 + VOTING_WINDOW_BLOCKS + 1;
        if proposal_id != U256::from(1u64)
            || payload != RAW_PAYLOAD.as_bytes()
            || context.proposer != PROPOSER
            || context.attached_value != U256::ZERO
            || context.block_number != expected_height
            || context.chain_id != 1
        {
            return Err(PrecompileError::Fatal(
                "execution payload or target context changed".into(),
            ));
        }
        Ok(TargetExecutionOutcome::Applied)
    }
}

pub(super) static RAW_CONTEXT_TARGET: RawContextTarget = RawContextTarget;
pub(super) static RAW_CONTEXT_HANDLERS: &[&dyn VoteTarget] = &[&RAW_CONTEXT_TARGET];
pub(super) static RAW_CONTEXT_REGISTRY: VoteTargetRegistry =
    VoteTargetRegistry::new(RAW_CONTEXT_HANDLERS);
pub(super) static DUPLICATE_HANDLERS: &[&dyn VoteTarget] =
    &[&RAW_CONTEXT_TARGET, &RAW_CONTEXT_TARGET];
pub(super) static DUPLICATE_REGISTRY: VoteTargetRegistry =
    VoteTargetRegistry::new(DUPLICATE_HANDLERS);

pub(super) struct PublicBondedTarget;

impl VoteTarget for PublicBondedTarget {
    fn target_module(&self) -> Address {
        UPDATE_ADDRESS
    }

    fn admission(&self) -> TargetAdmission {
        TargetAdmission::PublicBonded {
            amount: U256::from(123u64),
        }
    }

    fn validate(&self, _payload: &[u8], context: VoteTargetContext) -> Result<()> {
        if context.attached_value != U256::from(123u64) {
            return Err(PrecompileError::Fatal(
                "public bonded target received the wrong attached value".into(),
            ));
        }
        Ok(())
    }

    fn reserve(
        &self,
        storage: StorageHandle<'_>,
        proposal_id: U256,
        payload: &[u8],
        _context: VoteTargetContext,
    ) -> Result<()> {
        if payload == b"fail" {
            storage.sstore(UPDATE_ADDRESS, U256::from(998u64), U256::from(1u64))?;
            return Err(PrecompileError::Revert(
                "characterized public reservation failure".into(),
            ));
        }
        if payload == b"execution-error" {
            storage.sstore(UPDATE_ADDRESS, U256::from(997u64), proposal_id)?;
        }
        if payload == b"applied-write" {
            storage.sstore(UPDATE_ADDRESS, U256::from(994u64), proposal_id)?;
        }
        Ok(())
    }

    fn handle_approved(
        &self,
        ctx: &BlockRuntimeContext,
        _proposal_id: U256,
        payload: &[u8],
        context: VoteTargetContext,
    ) -> Result<TargetExecutionOutcome> {
        if context.attached_value != U256::from(123u64) {
            return Err(PrecompileError::Fatal(
                "public bonded target lost its attached value at execution".into(),
            ));
        }
        if payload == b"execution-error" {
            ctx.storage
                .sstore(UPDATE_ADDRESS, U256::from(996u64), U256::from(1u64))?;
            return Ok(TargetExecutionOutcome::Error {
                reason: "characterized public target execution error".into(),
            });
        }
        if payload == b"applied-write" {
            ctx.storage
                .sstore(UPDATE_ADDRESS, U256::from(995u64), U256::from(1u64))?;
        }
        Ok(TargetExecutionOutcome::Applied)
    }
}

pub(super) static PUBLIC_BONDED_TARGET: PublicBondedTarget = PublicBondedTarget;
pub(super) static PUBLIC_BONDED_HANDLERS: &[&dyn VoteTarget] = &[&PUBLIC_BONDED_TARGET];
pub(super) static PUBLIC_BONDED_REGISTRY: VoteTargetRegistry =
    VoteTargetRegistry::new(PUBLIC_BONDED_HANDLERS);

pub(super) struct FailingReserveTarget;

impl VoteTarget for FailingReserveTarget {
    fn target_module(&self) -> Address {
        UPDATE_ADDRESS
    }

    fn validate(&self, _payload: &[u8], _context: VoteTargetContext) -> Result<()> {
        Ok(())
    }

    fn reserve(
        &self,
        storage: StorageHandle<'_>,
        _proposal_id: U256,
        _payload: &[u8],
        _context: VoteTargetContext,
    ) -> Result<()> {
        storage.sstore(UPDATE_ADDRESS, U256::from(999u64), U256::from(1u64))?;
        Err(PrecompileError::Revert(
            "characterized reservation failure".into(),
        ))
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

pub(super) static FAILING_RESERVE_TARGET: FailingReserveTarget = FailingReserveTarget;
pub(super) static FAILING_RESERVE_HANDLERS: &[&dyn VoteTarget] = &[&FAILING_RESERVE_TARGET];
pub(super) static FAILING_RESERVE_REGISTRY: VoteTargetRegistry =
    VoteTargetRegistry::new(FAILING_RESERVE_HANDLERS);

pub(super) fn block_context(
    storage: StorageHandle<'_>,
    block_number: u64,
) -> BlockRuntimeContext<'_> {
    BlockRuntimeContext::new(BlockContext::empty_for_tests(block_number, 0, 1), storage)
}

pub(super) fn public_bonded_finalization_fixture() -> (HashMapStorageProvider, U256, u64) {
    let owner = Address::repeat_byte(0x99);
    let mut provider = super::test_provider();
    provider.set_balance(VOTE_ADDRESS, U256::from(130u64));
    provider.set_balance(owner, U256::from(11u64));
    let proposal_id;
    {
        let storage = StorageHandle::new(&mut provider);
        setup_default_validators(storage.clone());
        let mut vote = Vote::new(storage);
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
    }
    provider.clear_mutation_failure();
    (provider, proposal_id, 10 + VOTING_WINDOW_BLOCKS)
}
