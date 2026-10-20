use super::*;
use outbe_primitives::{
    addresses::CODE_REGISTRY_ADDRESS,
    block::{BlockContext, BlockRuntimeContext},
    error::Result,
    storage::PrecompileStorageProvider,
};
use outbe_validatorset::contract::ValidatorSet;
use outbe_vote::{
    handlers::{TargetElectorate, VoteTarget, VoteTargetContext, VoteTargetRegistry},
    precompile::{dispatch_with_handlers, IVote},
    ProposalStatus, Vote,
};

use crate::{payload::PatchVotePayloadV1, vote_target::CodePatchVoteTarget};

static TARGET: CodePatchVoteTarget = CodePatchVoteTarget;
static HANDLERS: &[&dyn VoteTarget] = &[&TARGET];
static REGISTRY: VoteTargetRegistry = VoteTargetRegistry::new(HANDLERS);
const OWNER: Address = Address::repeat_byte(0xff);
const A: Address = Address::repeat_byte(1);
const B: Address = Address::repeat_byte(2);
const C: Address = Address::repeat_byte(3);
const D: Address = Address::repeat_byte(4);

fn setup() -> HashMapStorageProvider {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.set_block_number(10);
    provider.enter(|storage| {
        let mut validators = ValidatorSet::new(storage.clone());
        validators.config_owner.write(OWNER).unwrap();
        validators.set_config_max_validators(10).unwrap();
        for (i, address) in [A, B, C].into_iter().enumerate() {
            let mut key = [0; 48];
            key[0] = i as u8 + 1;
            validators.register_validator(OWNER, address, &key).unwrap();
            validators.activate_validator(address).unwrap();
        }
        let mut registry = CodeRegistry::new(storage);
        registry.config_max_repositories.write(2).unwrap();
        registry.register_repository(repo(0xaa), A).unwrap();
    });
    provider
}

fn subject() -> String {
    String::from_utf8(
        PatchVotePayloadV1::accept(repo(0xaa), repo(0xbb), repo(0xcc), repo(0xdd))
            .encode()
            .unwrap(),
    )
    .unwrap()
}

pub(super) fn create(provider: &mut HashMapStorageProvider, payload: String) -> Result<U256> {
    let call = IVote::createProposalWithVotingWindowCall {
        targetModule: CODE_REGISTRY_ADDRESS,
        payload,
        votingWindowBlocks: 2,
    };
    let output = provider.enter(|storage| {
        dispatch_with_handlers(storage, &call.abi_encode(), A, U256::ZERO, &REGISTRY)
    })?;
    Ok(IVote::createProposalWithVotingWindowCall::abi_decode_returns(&output).unwrap())
}

pub(super) fn cast(
    provider: &mut HashMapStorageProvider,
    id: U256,
    voter: Address,
    yes: bool,
) -> Result<()> {
    provider.set_block_number(11);
    let call = IVote::castVoteCall {
        proposalId: id,
        approve: yes,
    };
    provider
        .enter(|storage| {
            dispatch_with_handlers(storage, &call.abi_encode(), voter, U256::ZERO, &REGISTRY)
        })
        .map(|_| ())
}

pub(super) fn finalize(provider: &mut HashMapStorageProvider, height: u64) -> Result<()> {
    provider.set_block_number(height);
    let chain_id = provider.chain_id();
    provider.enter(|storage| {
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(height, 1_700_000_000 + height, chain_id),
            storage.clone(),
        );
        Vote::new(storage).process_begin_block(&ctx, &REGISTRY)
    })
}

fn approval(provider: &mut HashMapStorageProvider, id: U256) -> ICodeRegistry::PatchApprovalInfo {
    let call = ICodeRegistry::getPatchApprovalCall { proposalId: id };
    let output = provider
        .enter(|storage| dispatch(storage, &call.abi_encode(), Address::ZERO, U256::ZERO))
        .unwrap();
    ICodeRegistry::getPatchApprovalCall::abi_decode_returns(&output).unwrap()
}

#[test]
fn abi_vote_approves_exact_revision_once_only_after_deadline() {
    let mut provider = setup();
    let payload = subject();
    let id = create(&mut provider, payload.clone()).unwrap();
    assert_eq!(id, U256::ONE);
    assert_eq!(TARGET.electorate(), TargetElectorate::ActiveConsensus);
    assert!(!approval(&mut provider, id).approved);
    provider.enter(|storage| {
        assert_eq!(
            CodeRegistry::new(storage)
                .patch_execution_stage
                .read(&id)
                .unwrap(),
            0
        );
    });
    cast(&mut provider, id, A, true).unwrap();
    assert!(cast(&mut provider, id, A, true).is_err());
    cast(&mut provider, id, B, true).unwrap();
    finalize(&mut provider, 12).unwrap();
    assert!(!approval(&mut provider, id).approved);
    finalize(&mut provider, 13).unwrap();
    let approved = approval(&mut provider, id);
    assert!(approved.approved);
    assert_eq!(approved.payload, payload);
    assert_eq!(approved.approvedHeight, 13);
    assert_eq!(approved.approvedTimestamp, 1_700_000_013);
    let stage = ICodeRegistry::patchExecutionStageCall { proposalId: id }.abi_encode();
    let stage = provider
        .enter(|storage| dispatch(storage, &stage, Address::ZERO, U256::ZERO))
        .unwrap();
    assert_eq!(
        ICodeRegistry::patchExecutionStageCall::abi_decode_returns(&stage).unwrap(),
        crate::schema::PATCH_EXECUTION_PENDING
    );
    let decoded = PatchVotePayloadV1::decode(approved.payload.as_bytes()).unwrap();
    assert_eq!(decoded.patch_id, repo(0xbb));
    assert_eq!(decoded.revision_id, repo(0xcc));
    assert_eq!(decoded.expected_target_head, repo(0xdd));
    finalize(&mut provider, 14).unwrap();
    assert_eq!(approval(&mut provider, id), approved);
    assert!(!approval(&mut provider, id + U256::ONE).approved);

    provider.enter(|storage| {
        let registry = CodeRegistry::new(storage.clone());
        let before = registry.patch_approval(id).unwrap().unwrap();
        let ctx =
            BlockRuntimeContext::new(BlockContext::empty_for_tests(14, 99, CHAIN_ID), storage);
        let context = VoteTargetContext {
            proposer: A,
            attached_value: U256::ZERO,
            block_number: 14,
            chain_id: CHAIN_ID,
        };
        TARGET
            .handle_approved(&ctx, id, payload.as_bytes(), context)
            .unwrap();
        let substituted = payload.replace(&"cc".repeat(20), &"ee".repeat(20));
        assert!(TARGET
            .handle_approved(&ctx, id, substituted.as_bytes(), context)
            .is_err());
        assert_eq!(registry.patch_approval(id).unwrap().unwrap(), before);
    });
}

#[test]
fn outsiders_pending_and_retained_members_cannot_vote_or_inflate_denominator() {
    let mut provider = setup();
    provider.enter(|storage| {
        let mut validators = ValidatorSet::new(storage);
        validators.register_validator(OWNER, D, &[4; 48]).unwrap();
        validators.activate_validator(D).unwrap();
        validators.jail_validator(D).unwrap();
        assert!(validators.is_consensus_participant(D).unwrap());
        assert!(!validators
            .validator_lifecycle(D)
            .unwrap()
            .is_active_status());
        validators
            .register_validator(OWNER, OWNER, &[5; 48])
            .unwrap();
        validators.mark_pending(OWNER).unwrap();
    });
    let id = create(&mut provider, subject()).unwrap();
    for denied in [D, OWNER, Address::ZERO] {
        assert!(cast(&mut provider, id, denied, true).is_err());
    }
    cast(&mut provider, id, A, true).unwrap();
    cast(&mut provider, id, B, true).unwrap();
    provider.enter(|storage| {
        let vote = Vote::new(storage);
        assert_eq!(vote.eligible_validator_addresses(id).unwrap(), [A, B, C]);
        assert_eq!(vote.get_proposal(id).unwrap().unwrap().state.yes, 2);
    });
    finalize(&mut provider, 13).unwrap();
    assert!(
        approval(&mut provider, id).approved,
        "2/3 active voters suffice despite 4 effective signers"
    );
}

#[test]
fn tally_rechecks_current_membership_and_expired_decision_grants_no_permission() {
    let mut provider = setup();
    let id = create(&mut provider, subject()).unwrap();
    cast(&mut provider, id, A, true).unwrap();
    cast(&mut provider, id, B, true).unwrap();
    provider.enter(|storage| {
        ValidatorSet::new(storage.clone())
            .jail_validator(B)
            .unwrap();
        let vote = Vote::new(storage);
        assert_eq!(vote.eligible_validator_addresses(id).unwrap(), [A, C]);
        assert_eq!(vote.get_proposal(id).unwrap().unwrap().state.yes, 1);
    });
    finalize(&mut provider, 13).unwrap();
    assert!(!approval(&mut provider, id).approved);
    provider.enter(|storage| {
        assert_eq!(
            Vote::new(storage.clone())
                .get_proposal(id)
                .unwrap()
                .unwrap()
                .status,
            ProposalStatus::Expired
        );
        assert!(CodeRegistry::new(storage)
            .patch_approvals
            .get(id)
            .unwrap()
            .is_none());
    });
}

#[test]
fn pending_joiner_votes_only_after_activation_and_changes_current_quorum() {
    for joiner_votes in [false, true] {
        let mut provider = setup();
        provider.enter(|storage| {
            let mut validators = ValidatorSet::new(storage);
            validators.register_validator(OWNER, D, &[4; 48]).unwrap();
            validators.mark_pending(D).unwrap();
        });
        let id = create(&mut provider, subject()).unwrap();
        assert!(cast(&mut provider, id, D, true).is_err());
        cast(&mut provider, id, A, true).unwrap();
        cast(&mut provider, id, B, true).unwrap();
        provider.enter(|storage| ValidatorSet::new(storage).activate_validator(D).unwrap());
        if joiner_votes {
            cast(&mut provider, id, D, true).unwrap();
        }
        provider.enter(|storage| {
            let vote = Vote::new(storage);
            assert_eq!(vote.eligible_validator_addresses(id).unwrap(), [A, B, C, D]);
            assert_eq!(
                vote.get_proposal(id).unwrap().unwrap().state.yes,
                if joiner_votes { 3 } else { 2 }
            );
        });
        finalize(&mut provider, 13).unwrap();
        assert_eq!(approval(&mut provider, id).approved, joiner_votes);
    }
}

#[test]
fn unknown_repository_reservation_rolls_back_proposal_and_electorate() {
    let mut provider = setup();
    let unknown = subject().replace(&"aa".repeat(20), &"99".repeat(20));
    let before = provider.storage.clone();
    assert!(create(&mut provider, unknown).is_err());
    assert_eq!(provider.storage, before);
    let id = create(&mut provider, subject()).unwrap();
    assert_eq!(id, U256::ONE);
    provider.enter(|storage| {
        assert_eq!(Vote::new(storage).proposal_electorate.read(&id).unwrap(), 1);
    });
}

#[test]
fn approval_row_alone_does_not_authorize_pending_expired_or_error_proposals() {
    let mut provider = setup();
    let payload = subject();
    let id = create(&mut provider, payload.clone()).unwrap();
    provider.enter(|storage| {
        let registry = CodeRegistry::new(storage.clone());
        registry
            .patch_approvals
            .create(&crate::PatchApprovalRecord {
                proposal_id: id,
                payload,
                approved_height: 13,
                approved_timestamp: 1_700_000_013,
            })
            .unwrap();
        let mut vote = Vote::new(storage);
        for status in [
            ProposalStatus::Pending,
            ProposalStatus::Expired,
            ProposalStatus::Error,
        ] {
            vote.set_proposal_status(id, status).unwrap();
            assert!(registry.patch_approval(id).unwrap().is_none(), "{status:?}");
        }
    });
}

#[test]
fn every_terminal_mutation_failure_rolls_back_approval_status_and_events() {
    let mut succeeded = false;
    for operation in 0..128 {
        let mut provider = setup();
        let id = create(&mut provider, subject()).unwrap();
        cast(&mut provider, id, A, true).unwrap();
        cast(&mut provider, id, B, true).unwrap();
        let before = provider.storage.clone();
        let events = provider.events.clone();
        provider.fail_after_mutation_at(operation);
        let result = finalize(&mut provider, 13);
        provider.clear_mutation_failure();
        if result.is_ok() {
            assert!(operation > 0);
            assert!(approval(&mut provider, id).approved);
            succeeded = true;
            break;
        }
        assert_eq!(provider.storage, before, "mutation {operation}");
        assert_eq!(provider.events, events, "events at mutation {operation}");
        assert!(!approval(&mut provider, id).approved);
    }
    assert!(
        succeeded,
        "must exercise failures through the last mutation"
    );
}

#[test]
fn approval_execution_has_bounded_storage_gas() {
    let mut provider = setup();
    let id = create(&mut provider, subject()).unwrap();
    cast(&mut provider, id, A, true).unwrap();
    cast(&mut provider, id, B, true).unwrap();
    provider.enable_production_storage_gas_metering();
    provider.set_gas_limit(1_000_000);
    finalize(&mut provider, 13).unwrap();
    let used = provider.gas_used();
    assert!(used > 0 && used < 1_000_000, "gas={used}");
    assert!(approval(&mut provider, id).approved);
}

#[test]
fn maximum_electorate_has_bounded_cast_and_tally_storage_gas() {
    let mut provider = setup();
    let mut electorate = vec![A, B, C];
    provider.enter(|storage| {
        let mut validators = ValidatorSet::new(storage);
        validators.set_config_max_validators(256).unwrap();
        for i in 3u16..256 {
            let mut address = [0; 20];
            address[..2].copy_from_slice(&i.to_be_bytes());
            address[19] = 0x44;
            let address = Address::from(address);
            let mut key = [0; 48];
            key[..2].copy_from_slice(&i.to_be_bytes());
            key[47] = 0x55;
            validators.register_validator(OWNER, address, &key).unwrap();
            validators.activate_validator(address).unwrap();
            electorate.push(address);
        }
    });
    let id = create(&mut provider, subject()).unwrap();
    provider.enable_production_storage_gas_metering();
    let mut maximum_cast_gas = 0;
    for voter in electorate.into_iter().take(171) {
        provider.set_gas_limit(2_000_000);
        cast(&mut provider, id, voter, true).unwrap();
        maximum_cast_gas = maximum_cast_gas.max(provider.gas_used());
    }
    provider.set_gas_limit(5_000_000);
    finalize(&mut provider, 13).unwrap();
    let tally_gas = provider.gas_used();
    assert!(maximum_cast_gas > 0 && maximum_cast_gas < 2_000_000);
    assert!(tally_gas > 0 && tally_gas < 5_000_000);
    assert!(approval(&mut provider, id).approved);
    println!("Patch electorate n=256 yes=171 maximum_cast_storage_gas={maximum_cast_gas} terminal_storage_gas={tally_gas}");
}
