//! End-to-end vote → governance flow: validator quorum materializes Approved OIP/GIP.
//!
//! Complements crate-local vote_dispatch tests by using the node-level
//! `handlers::vote::registry()` (Update + Governance targets).

use alloy_primitives::{address, Address, U256};
use alloy_sol_types::SolCall;

use outbe_evm::handlers;
use outbe_governance::precompile::{dispatch as gov_dispatch, IGovernance};
use outbe_governance::{GovernanceVotePayload, ProposalKind};
use outbe_primitives::addresses::GOVERNANCE_ADDRESS;
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
use outbe_primitives::chain::DEVNET_CHAIN_ID;
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
use outbe_validatorset::contract::ValidatorSet;
use outbe_vote::constants::VOTING_WINDOW_BLOCKS;
use outbe_vote::lifecycle::VoteLifecycle;
use outbe_vote::schema::ProposalStatus;
use outbe_vote::schema::Vote;

const CHAIN_ID: u64 = DEVNET_CHAIN_ID;
const PROPOSER: Address = address!("0x1111111111111111111111111111111111111111");
const VOTER_A: Address = address!("0x2222222222222222222222222222222222222222");
const VOTER_B: Address = address!("0x3333333333333333333333333333333333333333");
const VOTER_C: Address = address!("0x4444444444444444444444444444444444444444");
const VALIDATOR_OWNER: Address = address!("0xffffffffffffffffffffffffffffffffffffffff");

const APPROVED: u8 = 1;
const KINDS: [ProposalKind; 2] = [ProposalKind::Oip, ProposalKind::Gip];

fn dummy_pubkey(seed: u8) -> [u8; 48] {
    let mut pk = [0u8; 48];
    pk[0] = seed;
    pk
}

fn register_active_validator(storage: StorageHandle, addr: Address, seed: u8) {
    let mut vs = ValidatorSet::new(storage.clone());
    if vs.config_owner.read().unwrap().is_zero() {
        vs.config_owner.write(VALIDATOR_OWNER).unwrap();
        vs.config_max_validators.write(100).unwrap();
    }
    vs.register_validator(VALIDATOR_OWNER, addr, &dummy_pubkey(seed))
        .unwrap();
    vs.activate_validator(addr).unwrap();
}

fn setup_four_validators(storage: StorageHandle) {
    register_active_validator(storage.clone(), PROPOSER, 1);
    register_active_validator(storage.clone(), VOTER_A, 2);
    register_active_validator(storage.clone(), VOTER_B, 3);
    register_active_validator(storage.clone(), VOTER_C, 4);
}

fn tally_block(created: u64) -> u64 {
    created.saturating_add(VOTING_WINDOW_BLOCKS + 1)
}

fn with_vote_runtime_at<F: FnOnce(StorageHandle, u64)>(current: u64, f: F) {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.set_block_number(current);
    let storage = StorageHandle::new(&mut provider);
    setup_four_validators(storage.clone());
    f(storage, current);
}

fn block_ctx(storage: StorageHandle, block_number: u64) -> BlockRuntimeContext {
    BlockRuntimeContext::new(
        BlockContext::new(block_number, block_number, CHAIN_ID, PROPOSER, Vec::new()),
        storage,
    )
}

fn run_vote_begin_block(storage: StorageHandle, block_number: u64) {
    let ctx = block_ctx(storage, block_number);
    VoteLifecycle::begin_block_with_handlers(&ctx, handlers::vote::registry())
        .expect("vote begin block should succeed");
}

fn create_gov_proposal(vote: &mut Vote<'_>, kind: ProposalKind, text: &str, current: u64) -> U256 {
    vote.create_proposal(
        PROPOSER,
        GOVERNANCE_ADDRESS,
        &GovernanceVotePayload::encode(kind, text),
        current,
        handlers::vote::registry(),
    )
    .unwrap()
}

fn cast_quorum_yes(vote: &mut Vote<'_>, proposal_id: U256, current: u64) {
    for (voter, off) in [(VOTER_A, 1), (VOTER_B, 2), (VOTER_C, 3)] {
        vote.cast_vote_approve(proposal_id, voter, true, current + off)
            .unwrap();
    }
}

fn get_proposal(
    storage: StorageHandle,
    kind: ProposalKind,
) -> (u8, Address, String) {
    match kind {
        ProposalKind::Oip => {
            let out = gov_dispatch(
                storage,
                &IGovernance::getOipCall { id: U256::from(1) }.abi_encode(),
                PROPOSER,
                U256::ZERO,
            )
            .unwrap();
            let p = IGovernance::getOipCall::abi_decode_returns(&out).unwrap();
            (p.status, p.author, p.text)
        }
        ProposalKind::Gip => {
            let out = gov_dispatch(
                storage,
                &IGovernance::getGipCall { id: U256::from(1) }.abi_encode(),
                PROPOSER,
                U256::ZERO,
            )
            .unwrap();
            let p = IGovernance::getGipCall::abi_decode_returns(&out).unwrap();
            (p.status, p.author, p.text)
        }
    }
}

fn proposal_count(storage: StorageHandle, kind: ProposalKind) -> u64 {
    match kind {
        ProposalKind::Oip => {
            let out = gov_dispatch(
                storage,
                &IGovernance::oipCountCall {}.abi_encode(),
                PROPOSER,
                U256::ZERO,
            )
            .unwrap();
            IGovernance::oipCountCall::abi_decode_returns(&out).unwrap()
        }
        ProposalKind::Gip => {
            let out = gov_dispatch(
                storage,
                &IGovernance::gipCountCall {}.abi_encode(),
                PROPOSER,
                U256::ZERO,
            )
            .unwrap();
            IGovernance::gipCountCall::abi_decode_returns(&out).unwrap()
        }
    }
}

#[test]
fn full_vote_governance_flow_creates_approved_proposal() {
    for kind in KINDS {
        let text = match kind {
            ProposalKind::Oip => "committee oip",
            ProposalKind::Gip => "committee gip",
        };
        with_vote_runtime_at(100, |storage, current| {
            let mut vote = Vote::new(storage.clone());
            let proposal_id = create_gov_proposal(&mut vote, kind, text, current);
            cast_quorum_yes(&mut vote, proposal_id, current);
            run_vote_begin_block(storage.clone(), tally_block(current));

            assert_eq!(
                vote.proposals
                    .get(proposal_id)
                    .unwrap()
                    .unwrap()
                    .proposal_status()
                    .unwrap(),
                ProposalStatus::Approved
            );
            let (status, author, body) = get_proposal(storage, kind);
            assert_eq!(status, APPROVED);
            assert_eq!(author, PROPOSER);
            assert_eq!(body, text);
        });
    }
}

#[test]
fn vote_without_quorum_expires_without_record() {
    for kind in KINDS {
        with_vote_runtime_at(100, |storage, current| {
            let mut vote = Vote::new(storage.clone());
            let proposal_id = create_gov_proposal(&mut vote, kind, "no quorum", current);

            vote.cast_vote_approve(proposal_id, PROPOSER, true, current + 1)
                .unwrap();
            vote.cast_vote_approve(proposal_id, VOTER_A, true, current + 2)
                .unwrap();
            vote.cast_vote_approve(proposal_id, VOTER_B, false, current + 3)
                .unwrap();
            vote.cast_vote_approve(proposal_id, VOTER_C, false, current + 4)
                .unwrap();

            run_vote_begin_block(storage.clone(), tally_block(current));

            assert_eq!(
                vote.proposals
                    .get(proposal_id)
                    .unwrap()
                    .unwrap()
                    .proposal_status()
                    .unwrap(),
                ProposalStatus::Expired
            );
            assert_eq!(proposal_count(storage, kind), 0);
        });
    }
}
