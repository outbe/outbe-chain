use super::*;
use alloy_evm::Evm;
use alloy_sol_types::SolError;
use outbe_coderegistry::{
    payload::PatchVotePayloadV1, precompile::ICodeRegistry, CodeRegistry, RepoId,
};
use outbe_primitives::addresses::CODE_REGISTRY_ADDRESS;
use outbe_vote::precompile::IVote;
use revm::{context::TxEnv, DatabaseCommit};

fn call(
    state: &mut State<CacheDB<EmptyDBTyped<ProviderError>>>,
    height: u64,
    caller: Address,
    nonce: u64,
    target: Address,
    data: Vec<u8>,
) -> revm::context::result::ExecutionResult<revm::context::result::HaltReason> {
    let result = {
        let config = OutbeEvmConfig::new(test_chain_spec());
        let mut evm = config.evm_with_env(&mut *state, test_evm_env(height, REWARDS_ADDRESS));
        evm.transact_raw(
            TxEnv::builder()
                .chain_id(Some(CHAIN_ID))
                .caller(caller)
                .nonce(nonce)
                .kind(TxKind::Call(target))
                .data(Bytes::from(data))
                .gas_limit(2_000_000)
                .gas_price(u128::from(MIN_PROTOCOL_BASE_FEE))
                .build()
                .unwrap(),
        )
        .expect("execute actual EVM precompile transaction")
    };
    state.commit(result.state);
    result.result
}

#[test]
fn radicle_patch_approval_runs_through_evm_calls_and_real_begin_block_hooks() {
    run_patch_vote_case(true, false);
}

#[test]
fn changing_yes_to_no_at_deadline_prevents_patch_approval_without_removing_the_voter() {
    run_patch_vote_case(true, true);
}

#[test]
fn no_to_yes_is_rejected_before_deadline_without_changing_patch_outcome() {
    run_patch_vote_case(false, false);
}

fn run_patch_vote_case(approve_second: bool, change_to_no: bool) {
    let approved = approve_second && !change_to_no;
    let validators = [
        (Address::repeat_byte(0xa1), dummy_pubkey(0xa1)),
        (Address::repeat_byte(0xa2), dummy_pubkey(0xa2)),
        (Address::repeat_byte(0xa3), dummy_pubkey(0xa3)),
    ];
    let outsider = Address::repeat_byte(0xee);
    let mut state = state_with_active_validators_seeded_at_block(&validators, 7, |storage| {
        CodeRegistry::new(storage)
            .config_max_repositories
            .write(4)
            .unwrap();
    });
    let marker = Bytecode::new_legacy([0xef].into());
    state.database.insert_account_info(
        CODE_REGISTRY_ADDRESS,
        AccountInfo {
            code_hash: marker.hash_slow(),
            code: Some(marker),
            ..Default::default()
        },
    );
    for caller in validators.iter().map(|v| v.0).chain([outsider]) {
        state.database.insert_account_info(
            caller,
            AccountInfo {
                balance: U256::from(10u64).pow(U256::from(20u64)),
                ..Default::default()
            },
        );
    }
    let repo = RepoId::repeat_byte(0xaa);
    let register = ICodeRegistry::registerRepositoryCall { repoId: repo }.abi_encode();
    assert!(call(
        &mut state,
        7,
        validators[0].0,
        0,
        CODE_REGISTRY_ADDRESS,
        register
    )
    .is_success());
    let payload = String::from_utf8(
        PatchVotePayloadV1::accept(
            repo,
            RepoId::repeat_byte(0xbb),
            RepoId::repeat_byte(0xcc),
            RepoId::repeat_byte(0xdd),
        )
        .encode()
        .unwrap(),
    )
    .unwrap();
    let create = IVote::createProposalWithVotingWindowCall {
        targetModule: CODE_REGISTRY_ADDRESS,
        payload: payload.clone(),
        votingWindowBlocks: 2,
    }
    .abi_encode();
    let created = call(&mut state, 7, validators[0].0, 1, VOTE_ADDRESS, create);
    assert!(created.is_success(), "{created:?}");
    assert_eq!(
        IVote::createProposalWithVotingWindowCall::abi_decode_returns(created.output().unwrap())
            .unwrap(),
        U256::ONE
    );
    let ballot = IVote::castVoteCall {
        proposalId: U256::ONE,
        approve: true,
    }
    .abi_encode();
    assert!(!call(&mut state, 8, outsider, 0, VOTE_ADDRESS, ballot.clone()).is_success());
    assert!(call(
        &mut state,
        8,
        validators[0].0,
        2,
        VOTE_ADDRESS,
        ballot.clone()
    )
    .is_success());
    let second_ballot = IVote::castVoteCall {
        proposalId: U256::ONE,
        approve: approve_second,
    }
    .abi_encode();
    assert!(call(
        &mut state,
        8,
        validators[1].0,
        0,
        VOTE_ADDRESS,
        second_ballot
    )
    .is_success());
    let mut next_ballot_nonce = 1;
    if change_to_no {
        let changed = call(
            &mut state,
            9,
            validators[1].0,
            next_ballot_nonce,
            VOTE_ADDRESS,
            IVote::castVoteCall {
                proposalId: U256::ONE,
                approve: false,
            }
            .abi_encode(),
        );
        assert!(changed.is_success(), "{changed:?}");
        next_ballot_nonce += 1;
        assert!(changed.logs().iter().any(|log| {
            log.address == VOTE_ADDRESS
                && IVote::VoteCast::decode_log_data(&log.data).is_ok_and(|event| {
                    event.proposalId == U256::ONE
                        && event.validator == validators[1].0
                        && !event.approve
                })
        }));
    }
    if !approved {
        let reversal = call(
            &mut state,
            9,
            validators[1].0,
            next_ballot_nonce,
            VOTE_ADDRESS,
            IVote::castVoteCall {
                proposalId: U256::ONE,
                approve: true,
            }
            .abi_encode(),
        );
        next_ballot_nonce += 1;
        assert!(!reversal.is_success());
        assert_eq!(
            reversal.output().unwrap().as_ref(),
            IVote::AlreadyVoted {}.abi_encode()
        );
        assert!(reversal.logs().is_empty());
    }
    let get = ICodeRegistry::getPatchApprovalCall {
        proposalId: U256::ONE,
    }
    .abi_encode();
    let before = call(
        &mut state,
        9,
        outsider,
        1,
        CODE_REGISTRY_ADDRESS,
        get.clone(),
    );
    assert!(before.is_success());
    assert!(
        !ICodeRegistry::getPatchApprovalCall::abi_decode_returns(before.output().unwrap())
            .unwrap()
            .approved
    );

    let block_context = BlockContext {
        block_number: 10,
        timestamp: 1_700_000_010,
        chain_id: CHAIN_ID,
        genesis_hash: alloy_primitives::B256::ZERO,
        genesis_timestamp: 1_700_000_010,
        proposer: validators[0].0,
        validators: validators.iter().map(|v| v.0).collect(),
    };
    run_atomic_storage_hooks(&mut state, block_context, |ctx| {
        run_outbe_pre_execution_hooks(ctx, None)
    })
    .expect("actual begin-block hooks finalize patch Vote");
    let after = call(&mut state, 10, outsider, 2, CODE_REGISTRY_ADDRESS, get);
    assert!(after.is_success(), "{after:?}");
    let approval =
        ICodeRegistry::getPatchApprovalCall::abi_decode_returns(after.output().unwrap()).unwrap();
    let proposal = call(
        &mut state,
        10,
        outsider,
        3,
        VOTE_ADDRESS,
        IVote::getProposalCall {
            proposalId: U256::ONE,
        }
        .abi_encode(),
    );
    assert!(proposal.is_success());
    let proposal = IVote::getProposalCall::abi_decode_returns(proposal.output().unwrap()).unwrap();
    assert_eq!(
        proposal.status,
        if approved {
            IVote::ProposalStatus::Approved
        } else {
            IVote::ProposalStatus::Expired
        }
    );
    assert_eq!(proposal.state.yes, if approved { 2 } else { 1 });
    assert_eq!(proposal.state.no, if approved { 0 } else { 1 });
    assert_eq!(proposal.votersCount, U256::from(2));
    assert_eq!(approval.approved, approved);
    if approved {
        assert_eq!(approval.payload, payload);
        assert_eq!(approval.approvedHeight, 10);
        assert_eq!(approval.approvedTimestamp, 1_700_000_010);
    } else {
        assert!(approval.payload.is_empty());
        assert_eq!(approval.approvedHeight, 0);
    }
    let voters = call(
        &mut state,
        10,
        outsider,
        4,
        VOTE_ADDRESS,
        IVote::getProposalVotersCall {
            proposalId: U256::ONE,
            index: U256::ZERO,
            count: U256::from(10),
        }
        .abi_encode(),
    );
    assert!(voters.is_success(), "{voters:?}");
    assert_eq!(
        IVote::getProposalVotersCall::abi_decode_returns(voters.output().unwrap()).unwrap(),
        vec![validators[0].0, validators[1].0]
    );
    let late = call(
        &mut state,
        10,
        validators[1].0,
        next_ballot_nonce,
        VOTE_ADDRESS,
        IVote::castVoteCall {
            proposalId: U256::ONE,
            approve: false,
        }
        .abi_encode(),
    );
    assert!(!late.is_success());
    assert_eq!(
        late.output().unwrap().as_ref(),
        IVote::NotPending {}.abi_encode()
    );
    println!(
        "EVM patch Vote proposal=1 yes={} no={} voters=2 electorate=3 deadline=9 terminal_height=10 approved={} subject={payload}",
        proposal.state.yes, proposal.state.no, approval.approved
    );
}
