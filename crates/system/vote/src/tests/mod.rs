use alloy_primitives::{address, Address, B256, U256};

use outbe_primitives::addresses::{UPDATE_ADDRESS, VOTE_ADDRESS};
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_validatorset::contract::ValidatorSet;
use outbe_validatorset::ValidatorLifecycle;

use crate::api::{get_proposal, get_proposal_voters, list_proposals, list_proposals_by_status};
use crate::constants::VOTING_WINDOW_BLOCKS;
use crate::errors::VoteError;
use crate::handlers::{TargetExecutionOutcome, VoteTarget, VoteTargetContext, VoteTargetRegistry};
use crate::runtime::quorum_reached;
use crate::schema::Vote;
use crate::schema::{BondSettlement, ProposalStatus};
use crate::state::{calculate_vote_tally, vote_key, VoteKind, VoteTally};
use outbe_update::constants::MIN_ACTIVATION_BUFFER;
use outbe_update::encode_protocol_version;
use outbe_update::payload::encode_schedule_update_json;
use outbe_update::ProtocolVersion;
use serde_json::Value;

struct TestUpdateVoteTarget;

impl VoteTarget for TestUpdateVoteTarget {
    fn target_module(&self) -> Address {
        UPDATE_ADDRESS
    }

    fn validate(&self, payload: &[u8], _context: VoteTargetContext) -> Result<()> {
        require_json_object(payload, || VoteError::InvalidPayload.into())
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

static TEST_UPDATE_VOTE_TARGET: TestUpdateVoteTarget = TestUpdateVoteTarget;
static TEST_VOTE_HANDLERS: &[&dyn VoteTarget] = &[&TEST_UPDATE_VOTE_TARGET];
static TEST_VOTE_REGISTRY: VoteTargetRegistry = VoteTargetRegistry::new(TEST_VOTE_HANDLERS);

pub(super) fn test_vote_registry() -> &'static VoteTargetRegistry {
    &TEST_VOTE_REGISTRY
}

pub(super) fn create_proposal_test(
    vote: &mut Vote<'_>,
    proposer: Address,
    target_module: Address,
    payload: &str,
    current_height: u64,
) -> Result<U256> {
    vote.create_proposal(
        proposer,
        target_module,
        payload,
        current_height,
        test_vote_registry(),
    )
}

/// Creates an Update proposal with an empty, well-formed payload at `current_height`.
pub(super) fn create_update_proposal(
    vote: &mut Vote<'_>,
    proposer: Address,
    current_height: u64,
) -> Result<U256> {
    create_proposal_test(
        vote,
        proposer,
        UPDATE_ADDRESS,
        &empty_update_payload(current_height),
        current_height,
    )
}

/// Runs `f` with the storage and a Vote contract in a fresh Vote state.
pub(super) fn with_governance(f: impl FnOnce(StorageHandle<'_>, &mut Vote<'_>)) {
    with_vote(|storage| {
        let mut vote = Vote::new(storage.clone());
        f(storage, &mut vote);
    });
}

/// Runs `f` in a fresh Vote state after `PROPOSER` creates an Update
/// proposal at `current_height`. `f` receives the storage, the Vote, the
/// proposal id and `current_height`.
pub(super) fn with_update_proposal(
    current_height: u64,
    f: impl FnOnce(StorageHandle<'_>, &mut Vote<'_>, U256, u64),
) {
    with_governance(|storage, vote| {
        let proposal_id = create_update_proposal(vote, PROPOSER, current_height).unwrap();
        f(storage, vote, proposal_id, current_height);
    });
}

/// Asserts the tally and the voter count that `get_proposal` reports for
/// `proposal_id`.
pub(super) fn assert_proposal_view(
    storage: StorageHandle<'_>,
    proposal_id: U256,
    tally: VoteTally,
    voters_count: u64,
) {
    let info = get_proposal(storage, proposal_id).unwrap().unwrap();
    assert_eq!(info.state, tally);
    assert_eq!(info.voters_count, voters_count);
}

/// Asserts that `result` is a revert whose message contains `needle`.
pub(super) fn assert_reverts_with<T>(result: Result<T>, needle: &str) {
    assert!(matches!(
        &result,
        Err(PrecompileError::Revert(message)) if message.contains(needle)
    ));
}

/// Runs the begin-block tally one block after the voting window of a proposal
/// created at `created_height`.
pub(super) fn tally_after_window(vote: &mut Vote<'_>, created_height: u64) -> Result<()> {
    tally_after_window_with(vote, created_height, test_vote_registry())
}

/// [`tally_after_window`] with the targets of `registry`.
pub(super) fn tally_after_window_with(
    vote: &mut Vote<'_>,
    created_height: u64,
    registry: &VoteTargetRegistry,
) -> Result<()> {
    vote.begin_block_with(created_height + VOTING_WINDOW_BLOCKS + 1, registry)
}

/// Each voter in `voters` approves `proposal_id` in order, one block apart,
/// starting at `first_height`.
pub(super) fn approve_in_order(
    vote: &mut Vote<'_>,
    proposal_id: U256,
    voters: &[Address],
    first_height: u64,
) -> Result<()> {
    for (height, voter) in (first_height..).zip(voters) {
        vote.cast_vote_approve(proposal_id, *voter, true, height)?;
    }
    Ok(())
}

/// Status of a stored proposal.
pub(super) fn proposal_status(vote: &Vote<'_>, proposal_id: U256) -> ProposalStatus {
    vote.proposals
        .get(proposal_id)
        .unwrap()
        .unwrap()
        .proposal_status()
        .unwrap()
}

/// Accepts a payload that parses as a JSON object. Otherwise returns `invalid()`.
pub(super) fn require_json_object(
    payload: &[u8],
    invalid: impl FnOnce() -> PrecompileError,
) -> Result<()> {
    if serde_json::from_slice::<Value>(payload).is_ok_and(|value| value.is_object()) {
        Ok(())
    } else {
        Err(invalid())
    }
}

/// Number of Vote logs whose first topic is `signature`.
pub(super) fn count_events(provider: &HashMapStorageProvider, signature: B256) -> usize {
    provider
        .get_events(VOTE_ADDRESS)
        .iter()
        .filter(|log| log.topics().first() == Some(&signature))
        .count()
}

/// Asserts, in order, that each first topic in `expected` has its count of
/// Vote logs.
pub(super) fn assert_event_counts(provider: &HashMapStorageProvider, expected: &[(B256, usize)]) {
    for (signature, count) in expected {
        assert_eq!(count_events(provider, *signature), *count);
    }
}

/// Asserts that `proposal_id` finalized as `Error` and left the pending index.
pub(super) fn assert_finalized_error(vote: &Vote<'_>, proposal_id: U256) {
    assert_eq!(proposal_status(vote, proposal_id), ProposalStatus::Error);
    assert_eq!(
        vote.list_pending_proposal_ids().unwrap(),
        Vec::<U256>::new()
    );
}

/// Asserts that the bond of `proposal_id` has `settlement` and that no bond
/// liability remains.
pub(super) fn assert_bond_closed(vote: &Vote<'_>, proposal_id: U256, settlement: BondSettlement) {
    assert_eq!(
        vote.proposal_bond(proposal_id).unwrap().settlement,
        settlement
    );
    assert_eq!(vote.bond_liabilities().unwrap(), U256::ZERO);
}

pub(super) const PROPOSER: Address = address!("0x1111111111111111111111111111111111111111");
pub(super) const VOTER_A: Address = address!("0x2222222222222222222222222222222222222222");
pub(super) const VOTER_B: Address = address!("0x3333333333333333333333333333333333333333");
pub(super) const PENDING_VOTER: Address = address!("0x4444444444444444444444444444444444444444");
pub(super) const VALIDATOR_OWNER: Address = address!("0xffffffffffffffffffffffffffffffffffffffff");

mod bond;
mod characterization;
mod error_bond;
mod guards;
mod precompile;
mod targets;

fn dummy_pubkey(seed: u8) -> [u8; 48] {
    let mut pk = [0u8; 48];
    pk[0] = seed;
    pk
}

pub(super) fn register_active_validator(storage: StorageHandle, addr: Address, seed: u8) {
    let mut vs = ValidatorSet::new(storage.clone());
    vs.config_owner.write(VALIDATOR_OWNER).unwrap();
    vs.set_config_max_validators(100).unwrap();
    vs.register_validator(VALIDATOR_OWNER, addr, &dummy_pubkey(seed))
        .unwrap();
    vs.activate_validator_via_boundary_for_test(addr).unwrap();
}

pub(super) fn register_pending_validator(storage: StorageHandle, addr: Address, seed: u8) {
    let mut vs = ValidatorSet::new(storage.clone());
    vs.config_owner.write(VALIDATOR_OWNER).unwrap();
    vs.set_config_max_validators(100).unwrap();
    vs.test_register_validator_without_pop(addr, &dummy_pubkey(seed))
        .unwrap();
    vs.record_stake_increase(addr, U256::from(1), U256::from(1))
        .unwrap();
    assert!(
        matches!(
            vs.validator_lifecycle(addr).unwrap(),
            ValidatorLifecycle::WaitingForReadiness(_)
        ),
        "fixture must leave validator in PENDING status"
    );
}

pub(super) fn setup_default_validators(storage: StorageHandle) {
    register_active_validator(storage.clone(), PROPOSER, 1);
    register_active_validator(storage.clone(), VOTER_A, 2);
    register_active_validator(storage.clone(), VOTER_B, 3);
}

pub(super) fn min_activation_at(height: u64) -> u64 {
    height.saturating_add(MIN_ACTIVATION_BUFFER)
}

pub(super) fn update_json_payload(
    version: ProtocolVersion,
    activation_height: u64,
    info: &str,
) -> String {
    encode_schedule_update_json(version, activation_height, info)
}

pub(super) fn empty_update_payload(current_height: u64) -> String {
    update_json_payload(
        encode_protocol_version(1, 2),
        min_activation_at(current_height),
        "",
    )
}

pub(super) fn with_vote<F: FnOnce(StorageHandle)>(f: F) {
    let mut provider = test_provider();
    provider.set_block_number(1);
    let storage = StorageHandle::new(&mut provider);
    setup_default_validators(storage.clone());
    f(storage);
}

/// Opens `provider` with the three default validators. Returns the handle and
/// a Vote contract on it.
pub(super) fn validator_vote(
    provider: &mut HashMapStorageProvider,
) -> (StorageHandle<'_>, Vote<'_>) {
    let storage = StorageHandle::new(provider);
    setup_default_validators(storage.clone());
    let vote = Vote::new(storage.clone());
    (storage, vote)
}

pub(super) fn test_provider() -> HashMapStorageProvider {
    // Runtime voting reads immutable genesis parameters, including when this
    // test binary is built with test-protocol-overrides. Each process uses the
    // default profile. Isolated replay tests cover custom profiles.
    static INITIALIZE: std::sync::Once = std::sync::Once::new();
    INITIALIZE.call_once(|| {
        outbe_chain_constants::initialize(None).expect("initialize vote test protocol parameters");
    });
    HashMapStorageProvider::new(1)
}

fn block_ctx(storage: StorageHandle, block_number: u64) -> BlockRuntimeContext {
    BlockRuntimeContext::new(BlockContext::empty_for_tests(block_number, 0, 1), storage)
}

pub(super) trait VoteTestExt {
    fn process_begin_block_test(&mut self, block_number: u64) -> Result<()>;

    /// Runs the begin-block pass at `block_number` with `registry`.
    fn begin_block_with(&mut self, block_number: u64, registry: &VoteTargetRegistry) -> Result<()>;
}

impl VoteTestExt for Vote<'_> {
    fn process_begin_block_test(&mut self, block_number: u64) -> Result<()> {
        self.begin_block_with(block_number, test_vote_registry())
    }

    fn begin_block_with(&mut self, block_number: u64, registry: &VoteTargetRegistry) -> Result<()> {
        let ctx = block_ctx(self.storage.clone(), block_number);
        self.process_begin_block(&ctx, registry)
    }
}

#[test]
fn proposal_status_storage_roundtrip() {
    assert_eq!(ProposalStatus::Pending.to_u8(), 0);
    assert_eq!(ProposalStatus::Expired.to_u8(), 3);
    assert_eq!(ProposalStatus::Error.to_u8(), 4);
    assert_eq!(
        ProposalStatus::from_u8(ProposalStatus::Approved.to_u8()).unwrap(),
        ProposalStatus::Approved
    );
    assert!(ProposalStatus::Approved.is_terminal());
    assert!(!ProposalStatus::Pending.is_terminal());
    assert!(ProposalStatus::Error.is_terminal());
    assert_eq!(ProposalStatus::from_u8(4).unwrap(), ProposalStatus::Error);
    assert!(ProposalStatus::from_u8(5).is_err());
}

#[test]
fn vote_kind_bool_roundtrip() {
    assert_eq!(VoteKind::from_approve(true), VoteKind::Yes);
    assert_eq!(VoteKind::from_approve(false), VoteKind::No);
    assert!(VoteKind::Yes.to_approve());
    assert!(!VoteKind::No.to_approve());
}

#[test]
fn quorum_uses_two_thirds_threshold() {
    assert!(!quorum_reached(0, 0));
    assert!(!quorum_reached(2, 4));
    assert!(quorum_reached(3, 4));
    assert!(quorum_reached(2, 3));
}

#[test]
fn vote_key_depends_on_proposal_and_voter() {
    let voter_a = address!("0x1111111111111111111111111111111111111111");
    let voter_b = address!("0x2222222222222222222222222222222222222222");

    assert_ne!(
        vote_key(U256::from(1), voter_a),
        vote_key(U256::from(2), voter_a)
    );
    assert_ne!(
        vote_key(U256::from(1), voter_a),
        vote_key(U256::from(1), voter_b)
    );
}

#[test]
fn write_vote_appends_ordered_voters() {
    with_update_proposal(10, |_storage, governance, proposal_id, current| {
        governance
            .cast_vote_approve(proposal_id, VOTER_A, true, current + 1)
            .unwrap();
        governance
            .cast_vote_approve(proposal_id, VOTER_B, false, current + 2)
            .unwrap();

        assert_eq!(
            governance.read_proposal_voters(proposal_id).unwrap(),
            vec![VOTER_A, VOTER_B]
        );
        assert_eq!(
            governance.read_proposal_voters(proposal_id).unwrap().len(),
            2
        );
        assert_eq!(
            governance
                .votes_map
                .read(&vote_key(proposal_id, VOTER_A))
                .unwrap(),
            1
        );
        assert_eq!(
            governance
                .votes_map
                .read(&vote_key(proposal_id, VOTER_B))
                .unwrap(),
            2
        );
    });
}

#[test]
fn duplicate_vote_is_rejected() {
    with_update_proposal(20, |_storage, governance, proposal_id, current| {
        governance
            .cast_vote_approve(proposal_id, VOTER_A, true, current + 1)
            .unwrap();

        assert_reverts_with(
            governance.cast_vote_approve(proposal_id, VOTER_A, false, current + 2),
            "already voted",
        );

        assert_eq!(
            governance.read_proposal_voters(proposal_id).unwrap(),
            vec![VOTER_A]
        );
    });
}

#[test]
fn get_proposal_voters_pagination_is_deterministic() {
    with_update_proposal(30, |storage, governance, proposal_id, current| {
        governance
            .cast_vote_approve(proposal_id, VOTER_A, true, current + 1)
            .unwrap();
        governance
            .cast_vote_approve(proposal_id, VOTER_B, false, current + 2)
            .unwrap();

        assert_eq!(
            get_proposal_voters(storage.clone(), proposal_id, U256::ZERO, U256::from(1)).unwrap(),
            vec![VOTER_A]
        );
        assert_eq!(
            get_proposal_voters(storage.clone(), proposal_id, U256::from(1), U256::from(1))
                .unwrap(),
            vec![VOTER_B]
        );
        assert_eq!(
            get_proposal_voters(storage.clone(), proposal_id, U256::from(1), U256::from(10))
                .unwrap(),
            vec![VOTER_B]
        );
        assert!(
            get_proposal_voters(storage, proposal_id, U256::from(2), U256::from(1))
                .unwrap()
                .is_empty()
        );
    });
}

#[test]
fn get_proposal_uses_active_set_at_read_time() {
    with_update_proposal(40, |storage, governance, proposal_id, current| {
        approve_in_order(governance, proposal_id, &[VOTER_A, VOTER_B], current + 1).unwrap();

        assert_proposal_view(storage.clone(), proposal_id, VoteTally { yes: 2, no: 0 }, 2);

        ValidatorSet::new(storage.clone())
            .deactivate_validator(VALIDATOR_OWNER, VOTER_A)
            .unwrap();

        assert_proposal_view(storage, proposal_id, VoteTally { yes: 1, no: 0 }, 2);
    });
}

#[test]
fn inactive_voter_is_ignored_at_deadline_tally() {
    with_governance(|storage, governance| {
        let current = 100u64;
        let deadline = current + VOTING_WINDOW_BLOCKS + 1;
        let version = encode_protocol_version(1, 2);
        let activation = deadline.saturating_add(MIN_ACTIVATION_BUFFER);
        let payload = update_json_payload(version, activation, "");
        let proposal_id =
            create_proposal_test(governance, PROPOSER, UPDATE_ADDRESS, &payload, current).unwrap();

        governance
            .cast_vote_approve(proposal_id, VOTER_A, true, current + 1)
            .unwrap();

        ValidatorSet::new(storage.clone())
            .deactivate_validator(VALIDATOR_OWNER, VOTER_A)
            .unwrap();

        approve_in_order(governance, proposal_id, &[PROPOSER, VOTER_B], current + 2).unwrap();

        tally_after_window(governance, current).unwrap();

        let record = governance.proposals.get(proposal_id).unwrap().unwrap();
        assert_eq!(record.proposal_status().unwrap(), ProposalStatus::Approved);

        let active = crate::state::active_validator_addresses(storage.clone()).unwrap();
        let tally = calculate_vote_tally(governance, &record, &active).unwrap();
        assert_eq!(tally, VoteTally { yes: 2, no: 0 });
        assert_eq!(
            governance.read_proposal_voters(proposal_id).unwrap(),
            vec![VOTER_A, PROPOSER, VOTER_B]
        );
    });
}

#[test]
fn deadline_quorum_requires_two_thirds_of_active_set() {
    with_update_proposal(200, |_storage, governance, proposal_id, current| {
        governance
            .cast_vote_approve(proposal_id, VOTER_A, true, current + 1)
            .unwrap();

        tally_after_window(governance, current).unwrap();

        assert_eq!(
            proposal_status(governance, proposal_id),
            ProposalStatus::Expired
        );
    });
}

#[test]
fn list_proposals_and_by_status_are_paginated() {
    with_governance(|storage, governance| {
        let current = 300u64;
        let first = create_update_proposal(governance, PROPOSER, current).unwrap();
        let second = create_update_proposal(governance, VOTER_A, current + 1).unwrap();

        assert_eq!(
            list_proposals(storage.clone(), U256::ZERO, U256::from(10)).unwrap(),
            vec![first, second]
        );
        assert_eq!(
            list_proposals_by_status(
                storage.clone(),
                ProposalStatus::Pending,
                U256::ZERO,
                U256::from(1)
            )
            .unwrap(),
            vec![first]
        );
        assert_eq!(
            list_proposals_by_status(
                storage,
                ProposalStatus::Pending,
                U256::from(1),
                U256::from(1)
            )
            .unwrap(),
            vec![second]
        );
    });
}

#[test]
fn list_proposals_oversized_index_does_not_panic() {
    with_governance(|storage, governance| {
        let current = 300u64;
        let _ = create_update_proposal(governance, PROPOSER, current).unwrap();

        // U256::MAX used to panic in clamp_page via to::<u64>(). The value must saturate.
        assert_eq!(
            list_proposals(storage.clone(), U256::MAX, U256::from(1)).unwrap(),
            Vec::<U256>::new()
        );
        assert_eq!(
            list_proposals(storage, U256::ZERO, U256::MAX)
                .unwrap()
                .len(),
            1
        );
    });
}
