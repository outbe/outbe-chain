//! Synthetic vote targets and fixtures shared by the characterization tests.

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolError;
use outbe_primitives::{
    addresses::{UPDATE_ADDRESS, VOTE_ADDRESS},
    block::{BlockContext, BlockRuntimeContext},
    error::{PrecompileError, Result},
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
};

use crate::{
    constants::{MAX_PAYLOAD_BYTES, VOTING_WINDOW_BLOCKS},
    errors::VoteError,
    handlers::{
        TargetAdmission, TargetExecutionOutcome, VoteTarget, VoteTargetContext, VoteTargetRegistry,
    },
    precompile::IVote,
    schema::{BondSettlement, ProposalStatus, Vote},
    state::ProposalSubmission,
};

use super::{
    count_events, proposal_status, require_json_object, setup_default_validators, PROPOSER, VOTER_A,
};

pub(super) struct RejectingApprovedTarget;

impl VoteTarget for RejectingApprovedTarget {
    fn target_module(&self) -> Address {
        UPDATE_ADDRESS
    }

    fn validate(&self, payload: &[u8], _context: VoteTargetContext) -> Result<()> {
        require_json_object(payload, || {
            PrecompileError::Revert("expected object".into())
        })
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
        require_json_object(payload, || {
            PrecompileError::Revert("expected object".into())
        })
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

/// Target context of the raw-payload proposal at `block_number`.
fn raw_context(block_number: u64) -> VoteTargetContext {
    VoteTargetContext {
        proposer: PROPOSER,
        attached_value: U256::ZERO,
        block_number,
        chain_id: 1,
    }
}

pub(super) struct RawContextTarget;

impl VoteTarget for RawContextTarget {
    fn target_module(&self) -> Address {
        UPDATE_ADDRESS
    }

    fn validate(&self, payload: &[u8], context: VoteTargetContext) -> Result<()> {
        if payload != RAW_PAYLOAD.as_bytes() || context != raw_context(10) {
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
            || context != raw_context(expected_height)
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

/// The marker that a synthetic target wrote to Update storage slot `slot`.
pub(super) fn target_marker(storage: &StorageHandle<'_>, slot: u64) -> U256 {
    storage.sload(UPDATE_ADDRESS, U256::from(slot)).unwrap()
}

pub(super) fn block_context(
    storage: StorageHandle<'_>,
    block_number: u64,
) -> BlockRuntimeContext<'_> {
    BlockRuntimeContext::new(BlockContext::empty_for_tests(block_number, 0, 1), storage)
}

/// Payload of a validator-only legacy proposal.
pub(super) const LEGACY_PAYLOAD: &str = "{\"kind\":\"legacy\"}";

/// `PROPOSER` creates a validator-only proposal with [`LEGACY_PAYLOAD`] at
/// height 10.
pub(super) fn legacy_proposal(vote: &mut Vote<'_>, registry: &VoteTargetRegistry) -> Result<U256> {
    vote.create_proposal(PROPOSER, UPDATE_ADDRESS, LEGACY_PAYLOAD, 10, registry)
}

/// [`legacy_proposal`] approved by quorum.
pub(super) fn approved_legacy_proposal(
    vote: &mut Vote<'_>,
    registry: &VoteTargetRegistry,
) -> Result<U256> {
    let proposal_id = legacy_proposal(vote, registry)?;
    approve_by_quorum(vote, proposal_id)?;
    Ok(proposal_id)
}

/// Bond that [`PublicBondedTarget`] requires.
pub(super) const PUBLIC_BOND: u64 = 123;

/// Creates a bonded public proposal of `owner` with `payload` at height 10.
/// Then two validators approve it at height 11.
pub(super) fn approved_bonded_proposal(
    vote: &mut Vote<'_>,
    owner: Address,
    payload: &str,
) -> Result<U256> {
    let proposal_id = vote.create_proposal_with_value(
        ProposalSubmission {
            proposer: owner,
            target_module: UPDATE_ADDRESS,
            payload,
            created_height: 10,
            attached_value: U256::from(PUBLIC_BOND),
        },
        &PUBLIC_BONDED_REGISTRY,
    )?;
    approve_by_quorum(vote, proposal_id)?;
    Ok(proposal_id)
}

/// Two of the three default validators approve `proposal_id` at height 11.
pub(super) fn approve_by_quorum(vote: &mut Vote<'_>, proposal_id: U256) -> Result<()> {
    vote.cast_vote_approve(proposal_id, PROPOSER, true, 11)?;
    vote.cast_vote_approve(proposal_id, VOTER_A, true, 11)
}

/// Sets the `balances` in order, then creates an approved bonded proposal of
/// `owner`. Returns the provider with mutation counting reset, the proposal
/// id and its voting deadline.
pub(super) fn approved_bonded_fixture(
    owner: Address,
    payload: &str,
    balances: &[(Address, U256)],
) -> (HashMapStorageProvider, U256, u64) {
    let mut provider = super::test_provider();
    for (account, balance) in balances {
        provider.set_balance(*account, *balance);
    }
    let proposal_id;
    {
        let storage = StorageHandle::new(&mut provider);
        setup_default_validators(storage.clone());
        let mut vote = Vote::new(storage);
        proposal_id = approved_bonded_proposal(&mut vote, owner, payload).unwrap();
    }
    provider.clear_mutation_failure();
    (provider, proposal_id, 10 + VOTING_WINDOW_BLOCKS)
}

pub(super) fn public_bonded_finalization_fixture() -> (HashMapStorageProvider, U256, u64) {
    let owner = Address::repeat_byte(0x99);
    approved_bonded_fixture(
        owner,
        "applied-write",
        &[
            (VOTE_ADDRESS, U256::from(130u64)),
            (owner, U256::from(11u64)),
        ],
    )
}

/// Bond state that a rolled-back finalization leaves behind.
pub(super) struct PendingBond {
    pub(super) liabilities: U256,
    pub(super) vote_balance: U256,
}

/// Fails the finalization of a `fixture` proposal after each storage mutation
/// of a successful run in turn. Each failure must return a storage error and
/// leave the proposal pending with its bond unsettled, the bond `pending`
/// state, and no log whose first topic is in `absent_events`. `check` asserts
/// the scenario state for one failure point. Returns the baseline proposal id.
pub(super) fn assert_every_finalization_mutation_rolls_back(
    fixture: fn() -> (HashMapStorageProvider, U256, u64),
    pending: PendingBond,
    check: impl Fn(&StorageHandle<'_>, U256, usize),
    absent_events: &[B256],
) -> U256 {
    let (mut baseline, baseline_id, deadline) = fixture();
    finalize_after_deadline(&mut baseline, deadline).unwrap();
    let mutation_count = baseline.clear_mutation_failure();
    assert!(mutation_count > 0);

    for failure_point in 0..mutation_count {
        let (mut provider, proposal_id, deadline) = fixture();
        provider.fail_after_mutation_at(failure_point);
        let error = finalize_after_deadline(&mut provider, deadline).unwrap_err();
        assert!(
            matches!(error, PrecompileError::Storage(_)),
            "failure point {failure_point} returned {error:?}"
        );
        provider.clear_mutation_failure();

        {
            let storage = StorageHandle::new(&mut provider);
            assert_bond_still_pending(&storage, proposal_id, &pending, failure_point);
            check(&storage, proposal_id, failure_point);
        }
        for signature in absent_events {
            assert_eq!(
                count_events(&provider, *signature),
                0,
                "failure point {failure_point}"
            );
        }
    }
    baseline_id
}

/// Runs the begin-block pass at the block after `deadline` with the public
/// bonded registry.
fn finalize_after_deadline(provider: &mut HashMapStorageProvider, deadline: u64) -> Result<()> {
    let storage = StorageHandle::new(provider);
    Vote::new(storage.clone()).process_begin_block(
        &block_context(storage, deadline + 1),
        &PUBLIC_BONDED_REGISTRY,
    )
}

/// Asserts that `proposal_id` is still pending with its bond unsettled and
/// that the bond liabilities and the Vote balance equal `pending`.
fn assert_bond_still_pending(
    storage: &StorageHandle<'_>,
    proposal_id: U256,
    pending: &PendingBond,
    failure_point: usize,
) {
    let vote = Vote::new(storage.clone());
    assert_eq!(
        proposal_status(&vote, proposal_id),
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
        pending.liabilities,
        "failure point {failure_point}"
    );
    assert_eq!(
        storage.balance(VOTE_ADDRESS).unwrap(),
        pending.vote_balance,
        "failure point {failure_point}"
    );
}

struct PayloadBoundTarget {
    public_bonded: bool,
}

impl VoteTarget for PayloadBoundTarget {
    fn target_module(&self) -> Address {
        UPDATE_ADDRESS
    }

    fn admission(&self) -> TargetAdmission {
        if self.public_bonded {
            TargetAdmission::PublicBonded {
                amount: U256::from(PUBLIC_BOND),
            }
        } else {
            TargetAdmission::ActiveValidatorOnly
        }
    }

    fn validate(&self, payload: &[u8], _context: VoteTargetContext) -> Result<()> {
        assert!(
            payload.len() <= MAX_PAYLOAD_BYTES,
            "oversized raw payload reached target validation"
        );
        require_json_object(payload, || VoteError::InvalidPayload.into())
    }

    fn reserve(
        &self,
        storage: StorageHandle<'_>,
        proposal_id: U256,
        _payload: &[u8],
        _context: VoteTargetContext,
    ) -> Result<()> {
        storage.sstore(UPDATE_ADDRESS, U256::from(993u64), proposal_id)
    }

    fn handle_approved(
        &self,
        ctx: &BlockRuntimeContext,
        proposal_id: U256,
        payload: &[u8],
        _context: VoteTargetContext,
    ) -> Result<TargetExecutionOutcome> {
        require_json_object(payload, || VoteError::InvalidPayload.into())?;
        ctx.storage
            .sstore(UPDATE_ADDRESS, U256::from(992u64), proposal_id)?;
        Ok(TargetExecutionOutcome::Applied)
    }
}

static VALIDATOR_PAYLOAD_TARGET: PayloadBoundTarget = PayloadBoundTarget {
    public_bonded: false,
};
static BONDED_PAYLOAD_TARGET: PayloadBoundTarget = PayloadBoundTarget {
    public_bonded: true,
};
static VALIDATOR_PAYLOAD_REGISTRY: VoteTargetRegistry =
    VoteTargetRegistry::new(&[&VALIDATOR_PAYLOAD_TARGET]);
static BONDED_PAYLOAD_REGISTRY: VoteTargetRegistry =
    VoteTargetRegistry::new(&[&BONDED_PAYLOAD_TARGET]);

#[test]
fn raw_payload_byte_bound_accepts_exact_limit_for_both_creation_paths_and_admissions() {
    let mut payload =
        serde_json::json!({"text": "é".repeat(MAX_PAYLOAD_BYTES / 2 - 16)}).to_string();
    payload.push_str(&" ".repeat(MAX_PAYLOAD_BYTES - payload.len()));
    for custom_window in [false, true] {
        for (registry, attached_value) in [
            (&VALIDATOR_PAYLOAD_REGISTRY, U256::ZERO),
            (&BONDED_PAYLOAD_REGISTRY, U256::from(PUBLIC_BOND)),
        ] {
            let mut provider = super::test_provider();
            provider.set_balance(VOTE_ADDRESS, attached_value);
            let proposal_id;
            {
                let storage = StorageHandle::new(&mut provider);
                setup_default_validators(storage.clone());
                let mut vote = Vote::new(storage.clone());
                let submission = ProposalSubmission {
                    attached_value,
                    ..ProposalSubmission::new(PROPOSER, UPDATE_ADDRESS, &payload, 10)
                };
                proposal_id = if custom_window {
                    vote.create_proposal_with_voting_window(submission, 1, registry)
                } else {
                    vote.create_proposal_with_value(submission, registry)
                }
                .unwrap();
                assert_eq!(
                    vote.proposals.get(proposal_id).unwrap().unwrap().payload,
                    payload
                );
                assert_eq!(vote.list_pending_proposal_ids().unwrap(), vec![proposal_id]);
                assert_eq!(target_marker(&storage, 993), proposal_id);
                assert_eq!(vote.bond_liabilities().unwrap(), attached_value);
            }
            assert!(!provider.get_events(VOTE_ADDRESS).is_empty());
        }
    }
}

#[test]
fn oversized_raw_payloads_reject_before_validation_reservation_and_bond_effects() {
    let mut utf8_payload =
        serde_json::json!({"text": "é".repeat(MAX_PAYLOAD_BYTES / 2 - 16)}).to_string();
    utf8_payload.push_str(&" ".repeat(MAX_PAYLOAD_BYTES + 1 - utf8_payload.len()));
    for payload in [
        format!("{{}}{}", " ".repeat(MAX_PAYLOAD_BYTES - 1)),
        "x".repeat(MAX_PAYLOAD_BYTES + 1),
        " ".repeat(MAX_PAYLOAD_BYTES + 1),
        utf8_payload,
    ] {
        for custom_window in [false, true] {
            for (registry, attached_value) in [
                (&VALIDATOR_PAYLOAD_REGISTRY, U256::ZERO),
                (&BONDED_PAYLOAD_REGISTRY, U256::from(PUBLIC_BOND)),
            ] {
                let mut provider = super::test_provider();
                provider.set_balance(VOTE_ADDRESS, attached_value);
                provider.set_balance(PROPOSER, U256::from(17u64));
                {
                    let storage = StorageHandle::new(&mut provider);
                    setup_default_validators(storage);
                }
                provider.clear_mutation_failure();
                {
                    let storage = StorageHandle::new(&mut provider);
                    let mut vote = Vote::new(storage.clone());
                    let submission = ProposalSubmission {
                        attached_value,
                        ..ProposalSubmission::new(PROPOSER, UPDATE_ADDRESS, &payload, 10)
                    };
                    let error = if custom_window {
                        vote.create_proposal_with_voting_window(submission, 1, registry)
                    } else {
                        vote.create_proposal_with_value(submission, registry)
                    }
                    .unwrap_err();
                    assert!(matches!(
                        error,
                        PrecompileError::RevertBytes(bytes)
                            if IVote::InvalidPayload::abi_decode(&bytes).is_ok()
                    ));
                    assert_eq!(vote.proposal_count.read().unwrap(), U256::ZERO);
                    assert!(vote.proposals.get(U256::from(1u64)).unwrap().is_none());
                    assert!(vote.list_pending_proposal_ids().unwrap().is_empty());
                    assert_eq!(vote.bond_liabilities().unwrap(), U256::ZERO);
                    assert_eq!(target_marker(&storage, 993), U256::ZERO);
                    assert_eq!(storage.balance(VOTE_ADDRESS).unwrap(), attached_value);
                    assert_eq!(storage.balance(PROPOSER).unwrap(), U256::from(17u64));
                }
                assert_eq!(provider.clear_mutation_failure(), 0);
                assert!(provider.get_events(VOTE_ADDRESS).is_empty());
            }
        }
    }
}

#[test]
fn legacy_pending_payload_above_admission_bound_still_finalizes() {
    let payload = serde_json::json!({"text": "a".repeat(MAX_PAYLOAD_BYTES)}).to_string();
    super::with_vote(|storage| {
        let mut vote = Vote::new(storage.clone());
        // Persist the pending record as an earlier binary could have done,
        // bypassing today's admission guard without changing the stored payload.
        let proposal_id = vote
            .write_proposal(
                &ProposalSubmission::new(PROPOSER, UPDATE_ADDRESS, &payload, 10),
                10 + VOTING_WINDOW_BLOCKS,
                ProposalStatus::Pending,
            )
            .unwrap();
        approve_by_quorum(&mut vote, proposal_id).unwrap();
        vote.process_begin_block(
            &block_context(storage.clone(), 10 + VOTING_WINDOW_BLOCKS + 1),
            &VALIDATOR_PAYLOAD_REGISTRY,
        )
        .unwrap();
        assert_eq!(
            proposal_status(&vote, proposal_id),
            ProposalStatus::Approved
        );
        assert!(vote.list_pending_proposal_ids().unwrap().is_empty());
        assert_eq!(target_marker(&storage, 992), proposal_id);
        assert_eq!(
            vote.proposals.get(proposal_id).unwrap().unwrap().payload,
            payload
        );
    });
}
