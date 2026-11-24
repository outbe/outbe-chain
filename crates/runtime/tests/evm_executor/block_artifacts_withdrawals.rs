//! Withdrawal handling at post-execution: empty withdrawals keep legacy behaviour. The executor
//! rejects a non-empty list before any write.

use super::*;

alloy_sol_types::sol! {
    event DepositEvent(
        bytes pubkey,
        bytes withdrawal_credentials,
        bytes amount,
        bytes signature,
        bytes index
    );
}

#[test]
fn duplicate_post_execution_returns_error_and_preserves_requests() {
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    );
    let mut state = state_with_active_proposer(OWNER);
    let evm = config.evm_with_env(&mut state, test_evm_env(0, REWARDS_ADDRESS));
    let mut executor = config.create_executor(evm, execution_ctx(None, Bytes::new()));
    executor.apply_outbe_ethereum_post_execution().unwrap();
    let requests = executor.ethereum_post_execution_requests.clone();
    assert!(requests.is_some());

    let error = executor.apply_outbe_ethereum_post_execution().unwrap_err();
    assert!(error
        .to_string()
        .contains("post-execution changes already applied"));
    assert_eq!(executor.ethereum_post_execution_requests, requests);
}

#[test]
fn outbe_post_execution_preserves_behavior_for_absent_or_empty_withdrawals() {
    use alloy_eips::eip6110::DEPOSIT_REQUEST_TYPE;

    const DAO_BALANCE: u128 = 37;
    const CUMULATIVE_TX_GAS: u64 = 11;
    const STATE_GAS: u64 = 23;

    use withdrawal_compatibility::{run, Case};
    let cases = [
        Case {
            name: "shanghai-withdrawals-and-dao",
            chain_spec: withdrawal_chain_spec(ChainSpecBuilder::shanghai_activated),
            spec_id: SpecId::SHANGHAI,
            include_deposit: false,
            expected_gas_used: CUMULATIVE_TX_GAS,
        },
        Case {
            name: "prague-deposit-and-system-requests",
            chain_spec: withdrawal_chain_spec(ChainSpecBuilder::prague_activated),
            spec_id: SpecId::PRAGUE,
            include_deposit: true,
            expected_gas_used: CUMULATIVE_TX_GAS,
        },
        Case {
            name: "amsterdam-state-gas",
            chain_spec: withdrawal_chain_spec(ChainSpecBuilder::amsterdam_activated),
            spec_id: SpecId::AMSTERDAM,
            include_deposit: false,
            expected_gas_used: STATE_GAS,
        },
    ];

    let withdrawal_cases = [("none", None), ("empty", Some(Vec::new()))];

    for case in cases {
        for (withdrawal_name, withdrawals) in withdrawal_cases.clone() {
            let observation =
                run(&case, withdrawals.clone()).expect("native withdrawal compatibility");
            assert_eq!(
                observation.0.gas_used, case.expected_gas_used,
                "{}",
                case.name
            );
            assert_eq!(
                observation.2,
                U256::ZERO,
                "{}: DAO source drains",
                case.name
            );
            assert_eq!(
                observation.3,
                U256::from(DAO_BALANCE),
                "{}: DAO beneficiary receives the drained balance",
                case.name
            );
            assert_eq!(
                observation.4,
                U256::ZERO,
                "{} / {withdrawal_name}: absent or empty withdrawals do not credit a balance",
                case.name,
            );
            assert_eq!(
                observation
                    .0
                    .requests
                    .iter()
                    .any(|request| request.first() == Some(&DEPOSIT_REQUEST_TYPE)),
                case.include_deposit,
                "{}: Prague deposit request branch is observable",
                case.name
            );
        }
    }
}

#[test]
fn non_empty_withdrawal_rejects_before_any_state_write() {
    use alloy_eips::eip4895::Withdrawal;

    const DAO_BALANCE: u128 = 37;
    const WITHDRAWAL_TARGET: Address = address!("0xBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB");

    fn account_balance(state: &mut CacheDB<EmptyDB>, address: Address) -> U256 {
        state
            .get_account(&address)
            .expect("post-execution balance is readable")
            .map_or(U256::ZERO, |account| account.balance)
    }

    {
        let (chain_spec, mut state) = unsupported_withdrawal_state();
        let config = OutbeEvmConfig::new(
            chain_spec.clone(),
            std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
        );
        let evm_env = EthEvmEnv::new(
            SpecId::SHANGHAI,
            BlockEnvExt {
                number: U256::ZERO,
                gas_limit: U256::from(30_000_000_u64),
                beneficiary: REWARDS_ADDRESS,
                timestamp: U256::ZERO,
                ..Default::default()
            },
            chain_spec.chain().id(),
        );
        let evm = config.evm_with_env(&mut state, evm_env);
        let mut ctx = execution_ctx(Some(0), Bytes::new());
        ctx.execute_outbe_block_hooks = false;
        ctx.inner.withdrawals = Some(std::borrow::Cow::Owned(vec![Withdrawal {
            index: 0,
            validator_index: 0,
            address: WITHDRAWAL_TARGET,
            amount: 1_000,
        }]));
        let mut executor = config.create_executor(evm, ctx);
        executor.validate_execution_summary = false;

        let error = executor
            .apply_pre_execution_changes()
            .expect_err("every non-empty withdrawals list must be rejected pre-state");
        drop(executor);
        assert!(
            error
                .to_string()
                .contains("non-empty EIP-4895 withdrawals are unsupported on Outbe"),
            "{error}"
        );

        assert_eq!(
            account_balance(
                &mut state,
                evm2::block::ethereum::dao_fork::DAO_HARDFORK_ACCOUNTS[0]
            ),
            U256::from(DAO_BALANCE),
            "validation must precede the DAO drain"
        );
        assert_eq!(
            account_balance(
                &mut state,
                evm2::block::ethereum::dao_fork::DAO_HARDFORK_BENEFICIARY
            ),
            U256::ZERO,
            "validation must precede any beneficiary credit"
        );
        assert_eq!(
            account_balance(&mut state, WITHDRAWAL_TARGET),
            U256::ZERO,
            "unsupported withdrawal must not credit its target"
        );
    }
}

fn withdrawal_chain_spec(
    activate: fn(ChainSpecBuilder) -> ChainSpecBuilder,
) -> Arc<ChainSpec<OutbeHeader>> {
    let mut spec = activate(ChainSpecBuilder::from(&*MAINNET)).build();
    spec.chain = CHAIN_ID.into();
    spec.genesis.config.chain_id = CHAIN_ID;
    Arc::new(spec.map_header(OutbeHeader::new))
}

mod withdrawal_compatibility {
    use super::*;
    use alloy_eips::eip6110::MAINNET_DEPOSIT_CONTRACT_ADDRESS;
    const DAO_BALANCE: u128 = 37;
    const CUMULATIVE_TX_GAS: u64 = 11;
    const REGULAR_GAS: u64 = 17;
    const STATE_GAS: u64 = 23;
    pub(super) struct Case {
        pub name: &'static str,
        pub chain_spec: Arc<ChainSpec<OutbeHeader>>,
        pub spec_id: SpecId,
        pub include_deposit: bool,
        pub expected_gas_used: u64,
    }

    fn fixture_receipt(include_deposit: bool) -> Receipt {
        let logs = if include_deposit {
            let event = DepositEvent {
                pubkey: Bytes::from(vec![0x11; 48]),
                withdrawal_credentials: Bytes::from(vec![0x22; 32]),
                amount: Bytes::from(vec![0x33; 8]),
                signature: Bytes::from(vec![0x44; 96]),
                index: Bytes::from(vec![0x55; 8]),
            };
            vec![Log {
                address: MAINNET_DEPOSIT_CONTRACT_ADDRESS,
                data: event.encode_log_data(),
            }]
        } else {
            Vec::new()
        };
        Receipt {
            tx_type: reth_ethereum::TxType::Legacy,
            success: true,
            cumulative_gas_used: CUMULATIVE_TX_GAS,
            logs,
        }
    }

    fn fixture_state() -> CacheDB<EmptyDB> {
        let mut database = CacheDB::new(EmptyDB::default());
        database.insert_account_info(
            &evm2::block::ethereum::dao_fork::DAO_HARDFORK_ACCOUNTS[0],
            AccountInfo {
                balance: U256::from(DAO_BALANCE),
                ..Default::default()
            },
        );
        database
    }

    fn balance(state: &mut CacheDB<EmptyDB>, address: Address) -> eyre::Result<U256> {
        Ok(state
            .get_account(&address)?
            .map_or(U256::ZERO, |account| account.balance))
    }

    pub(super) fn run(
        case: &Case,
        withdrawals: Option<Vec<alloy_eips::eip4895::Withdrawal>>,
    ) -> eyre::Result<(
        reth_ethereum::provider::BlockExecutionResult<Receipt>,
        B256,
        U256,
        U256,
        U256,
    )> {
        let mut state = fixture_state();
        let config = OutbeEvmConfig::new(
            case.chain_spec.clone(),
            std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
        );
        let evm_env = EthEvmEnv::new(
            case.spec_id,
            BlockEnvExt {
                number: U256::ZERO,
                gas_limit: U256::from(30_000_000_u64),
                beneficiary: REWARDS_ADDRESS,
                timestamp: U256::ZERO,
                ..Default::default()
            },
            case.chain_spec.chain().id(),
        );
        let output = {
            let evm = config.evm_with_env(&mut state, evm_env);
            let mut ctx = execution_ctx(Some(1), Bytes::new());
            ctx.inner.parent_beacon_block_root = Some(B256::ZERO);
            ctx.execute_outbe_block_hooks = false;
            ctx.inner.withdrawals = withdrawals.clone().map(std::borrow::Cow::Owned);

            let mut executor = config.create_executor(evm, ctx);
            executor.apply_pre_execution_changes()?;
            executor.inner.inner.receipts = vec![fixture_receipt(case.include_deposit)];
            executor.inner.inner.cumulative_gas_used = CUMULATIVE_TX_GAS;
            executor.inner.inner.block_regular_gas_used = REGULAR_GAS;
            executor.inner.inner.block_state_gas_used = STATE_GAS;
            executor.inner.inner.blob_gas_used = 5;
            executor.validate_execution_summary = false;
            executor.finish()?
        };
        state.commit_source(&BundleSource(&output.state));
        observe_finished_state(&mut state, &output.state, output.result)
    }

    fn observe_finished_state(
        state: &mut CacheDB<EmptyDB>,
        bundle: &BundleState,
        result: reth_ethereum::provider::BlockExecutionResult<Receipt>,
    ) -> eyre::Result<(
        reth_ethereum::provider::BlockExecutionResult<Receipt>,
        B256,
        U256,
        U256,
        U256,
    )> {
        let root = post_state_root(bundle);
        let dao_source_balance = balance(
            state,
            evm2::block::ethereum::dao_fork::DAO_HARDFORK_ACCOUNTS[0],
        )?;
        let dao_beneficiary_balance = balance(
            state,
            evm2::block::ethereum::dao_fork::DAO_HARDFORK_BENEFICIARY,
        )?;
        let withdrawal_balance = balance(
            state,
            address!("0xBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB"),
        )?;
        Ok((
            result,
            root,
            dao_source_balance,
            dao_beneficiary_balance,
            withdrawal_balance,
        ))
    }
}

fn unsupported_withdrawal_state() -> (Arc<ChainSpec<OutbeHeader>>, CacheDB<EmptyDB>) {
    const DAO_BALANCE: u128 = 37;
    let mut spec = ChainSpecBuilder::from(&*MAINNET)
        .shanghai_activated()
        .build();
    spec.chain = CHAIN_ID.into();
    spec.genesis.config.chain_id = CHAIN_ID;
    let chain_spec = Arc::new(spec.map_header(OutbeHeader::new));
    let mut database = CacheDB::new(EmptyDB::default());
    database.insert_account_info(
        &evm2::block::ethereum::dao_fork::DAO_HARDFORK_ACCOUNTS[0],
        AccountInfo {
            balance: U256::from(DAO_BALANCE),
            ..Default::default()
        },
    );
    (chain_spec, database)
}
