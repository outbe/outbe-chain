use super::receipts_and_gas_support::*;
use super::*;

const CREATION_BLOCK: u64 = 7;
const HOOK_EVENTS_GAS: u64 = 0;
struct ApprovedProposalFixture {
    state: State<CacheDB<EmptyDBTyped<ProviderError>>>,
    issuer: Address,
    validators: [(Address, [u8; 48]); 3],
    expected: ApprovedFactoryExpected,
}

fn prepare_approved_proposal() -> ApprovedProposalFixture {
    let issuer = Address::repeat_byte(0x11);
    let validators = [
        (Address::repeat_byte(0xa1), dummy_pubkey(0xa1)),
        (Address::repeat_byte(0xa2), dummy_pubkey(0xa2)),
        (Address::repeat_byte(0xa3), dummy_pubkey(0xa3)),
    ];
    let payload = encode_canonical_stablecoin_create(&StablecoinCreatePayload {
        issuer,
        name: "Example Dollar".into(),
        ticker: "EXUSD".into(),
        iso4217: 840,
        decimals: 6,
        supply_cap: U256::from(1_000_000u64),
        policy_id: U256::from(1u64),
    })
    .expect("canonical Factory payload");
    let payload = core::str::from_utf8(&payload).expect("canonical payload is UTF-8");
    let forced_surplus = U256::from(7u64);
    let mut expected_token_id = B256::ZERO;
    let mut expected_token = Address::ZERO;
    let state =
        state_with_active_validators_seeded_at_block(&validators, CREATION_BLOCK, |storage| {
            storage
                .set_balance(VOTE_ADDRESS, STABLECOIN_CREATE_BOND + forced_surplus)
                .unwrap();
            (expected_token_id, expected_token) = StablecoinFactoryContract::new(storage.clone())
                .predict_token_address(issuer, "EXUSD")
                .unwrap();
            let mut vote = Vote::new(storage);
            let proposal_id = vote
                .create_proposal_with_value(
                    outbe_vote::ProposalSubmission {
                        proposer: issuer,
                        target_module: STABLECOIN_FACTORY_ADDRESS,
                        payload,
                        created_height: CREATION_BLOCK,
                        attached_value: STABLECOIN_CREATE_BOND,
                    },
                    outbe_vote::handlers::registry(),
                )
                .unwrap();
            assert_eq!(proposal_id, U256::from(1u64));
            vote.cast_vote_approve(proposal_id, validators[0].0, true, CREATION_BLOCK + 1)
                .unwrap();
            vote.cast_vote_approve(proposal_id, validators[1].0, true, CREATION_BLOCK + 1)
                .unwrap();
        });

    ApprovedProposalFixture {
        state,
        issuer,
        validators,
        expected: ApprovedFactoryExpected {
            issuer,
            forced_surplus,
            token_id: expected_token_id,
            token: expected_token,
        },
    }
}

fn assert_committed_factory_order(receipt_logs: &[alloy_primitives::Log]) {
    let factory_logs: Vec<_> = receipt_logs
        .iter()
        .filter(|log| {
            log.address == STABLECOIN_FACTORY_ADDRESS
                && log.data.topics().first()
                    == Some(&IStablecoinFactory::StablecoinCreated::SIGNATURE_HASH)
        })
        .collect();
    assert_eq!(factory_logs.len(), 1);
    let factory_log_index = receipt_logs
        .iter()
        .position(|log| {
            log.address == STABLECOIN_FACTORY_ADDRESS
                && log.data.topics().first()
                    == Some(&IStablecoinFactory::StablecoinCreated::SIGNATURE_HASH)
        })
        .unwrap();
    let refund_log_index = receipt_logs
        .iter()
        .position(|log| {
            log.address == VOTE_ADDRESS
                && log.data.topics().first() == Some(&IVote::ProposalBondRefunded::SIGNATURE_HASH)
        })
        .expect("Approved proposal must emit one refund");
    assert!(
        factory_log_index < refund_log_index,
        "target event must precede settlement event in committed hook order"
    );
}

fn assert_token_marker(
    state: &mut State<CacheDB<EmptyDBTyped<ProviderError>>>,
    expected_token: Address,
) {
    let token_account = state
        .basic(expected_token)
        .expect("token account read")
        .expect("created token account");
    assert_eq!(
        token_account
            .code
            .as_ref()
            .expect("created token marker")
            .original_bytes()
            .as_ref(),
        outbe_primitives::addresses::STABLECOIN_MARKER_CODE
    );
}

fn assert_factory_receipt(receipt: &Receipt) {
    assert!(receipt.success);
    assert_eq!(receipt.cumulative_gas_used, HOOK_EVENTS_GAS);
    assert_eq!(
        receipt
            .logs
            .iter()
            .filter(|log| {
                log.address == STABLECOIN_FACTORY_ADDRESS
                    && log.data.topics().first()
                        == Some(&IStablecoinFactory::StablecoinCreated::SIGNATURE_HASH)
            })
            .count(),
        1
    );
    let with_factory_root =
        alloy_consensus::proofs::calculate_receipt_root(&[receipt.with_bloom_ref()]);
    let mut without_factory_log = receipt.clone();
    without_factory_log.logs.retain(|log| {
        log.address != STABLECOIN_FACTORY_ADDRESS
            || log.data.topics().first()
                != Some(&IStablecoinFactory::StablecoinCreated::SIGNATURE_HASH)
    });
    let without_factory_root =
        alloy_consensus::proofs::calculate_receipt_root(&[without_factory_log.with_bloom_ref()]);
    assert_ne!(
        with_factory_root, without_factory_root,
        "StablecoinCreated must contribute to the receipts root"
    );
    assert_ne!(
        logs_bloom(receipt.logs.iter()),
        logs_bloom(without_factory_log.logs.iter()),
        "StablecoinCreated must contribute to the logs bloom"
    );
}

pub(super) fn run() {
    let ApprovedProposalFixture {
        mut state,
        issuer,
        validators,
        expected,
    } = prepare_approved_proposal();
    let expected_token = expected.token;
    let finalization_block = CREATION_BLOCK + VOTING_WINDOW_BLOCKS + 1;
    let block_context = BlockContext {
        block_gas_limit: 0,
        user_gas_used: 0,
        block_number: finalization_block,
        timestamp: 1_700_000_000,
        chain_id: CHAIN_ID,
        genesis_hash: alloy_primitives::B256::ZERO,
        genesis_timestamp: 1_700_000_000,
        proposer: issuer,
        validators: validators.iter().map(|(address, _)| *address).collect(),
    };
    let receipt = execute_vote_system_tx(&mut state, finalization_block, issuer);
    assert_committed_factory_order(&receipt.logs);
    assert_approved_factory_state(&mut state, block_context.clone(), expected)
        .expect("assert approved factory state fixture succeeds");
    assert_token_marker(&mut state, expected_token);

    assert_factory_receipt(&receipt);
}

fn execute_vote_system_tx(
    state: &mut State<CacheDB<EmptyDBTyped<ProviderError>>>,
    block_number: u64,
    proposer: Address,
) -> Receipt {
    use alloy_evm::Evm as _;
    use revm::DatabaseCommit;

    let mut evm_env = test_evm_env(block_number, REWARDS_ADDRESS);
    evm_env.block_env.timestamp = U256::from(1_700_000_000u64);
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    );
    let result = {
        let mut evm = config.evm_with_env(&mut *state, evm_env);
        outbe_evm::executor::with_preloaded_system_tx_context(
            outbe_evm::executor::PreloadedSystemTxContext {
                proposer,
                finalized_summary: None,
                canonical_vrf_proof_hash: B256::ZERO,
                verified_late_credit_signers: None,
                block_gas_limit: 0,
                user_gas_used: 0,
            },
            || {
                evm.transact_system_call(
                    outbe_primitives::addresses::SYSTEM_ADDRESS,
                    OUTBE_SYSTEM_TX_ADDRESS,
                    SystemTx::BeginBlock { step_index: 0 }.encode().unwrap(),
                )
            },
        )
        .expect("Vote system call")
    };
    assert!(result.result.is_success(), "Vote system call must succeed");
    let logs = result.result.into_logs();
    state.commit(result.state);
    Receipt {
        tx_type: alloy_consensus::TxType::Legacy,
        success: true,
        cumulative_gas_used: 0,
        logs,
    }
}
