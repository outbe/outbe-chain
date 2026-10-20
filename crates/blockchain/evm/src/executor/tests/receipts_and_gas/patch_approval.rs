use super::*;
use alloy_evm::Evm;
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
    run_patch_vote_case(false);
    run_patch_vote_case(true);
}

fn run_patch_vote_case(approve_second: bool) {
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

    let block_context = BlockContext::new(
        10,
        1_700_000_010,
        CHAIN_ID,
        validators[0].0,
        validators.iter().map(|v| v.0).collect(),
    );
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
        if approve_second {
            IVote::ProposalStatus::Approved
        } else {
            IVote::ProposalStatus::Expired
        }
    );
    assert_eq!(proposal.state.yes, if approve_second { 2 } else { 1 });
    assert_eq!(proposal.state.no, if approve_second { 0 } else { 1 });
    assert_eq!(approval.approved, approve_second);
    if approve_second {
        assert_eq!(approval.payload, payload);
        assert_eq!(approval.approvedHeight, 10);
        assert_eq!(approval.approvedTimestamp, 1_700_000_010);
    } else {
        assert!(approval.payload.is_empty());
        assert_eq!(approval.approvedHeight, 0);
    }
    println!("EVM patch Vote proposal=1 yes={} electorate=3 deadline=9 terminal_height=10 approved={} subject={payload}", if approve_second { 2 } else { 1 }, approval.approved);
}
