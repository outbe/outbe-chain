use super::eip7702_support::*;
use super::*;
use outbe_primitives::addresses::ZEROFEE_ADDRESS;

#[test]
fn eip7702_sponsored_bootstrap_accepts_one_atomic_unit_without_fee_or_quota() {
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    );
    let recovered = bootstrap_test_tx();
    let replay = recovered.clone();
    let signer = Address::from(*recovered.signer());
    let initial_balance = U256::from(1);

    let mut state = CacheDB::new(EmptyDB::default());
    let marker = Bytecode::new_legacy([0xef].into());
    state.insert_account_info(
        &ZEROFEE_ADDRESS,
        AccountInfo {
            code_hash: marker.hash_slow(),
            code: Some(marker),
            ..Default::default()
        },
    );
    state.insert_account_info(
        &signer,
        AccountInfo {
            balance: initial_balance,
            nonce: 0,
            ..Default::default()
        },
    );

    let output = {
        let evm = config.evm_with_env(&mut state, pectra_evm_env(1));
        let mut ctx = execution_ctx(Some(1), Bytes::new());
        ctx.execute_outbe_block_hooks = false;
        let mut executor = config.create_executor(evm, ctx);

        assert_eq!(executor.sponsored_gas_used, 0);

        let gas_output = executor
            .execute_transaction(recovered)
            .expect("one-unit bootstrap must execute under the fee waiver");
        assert!(
            gas_output.tx_gas_used() > 0,
            "bootstrap gas must remain visible in block accounting"
        );
        assert!(
            gas_output.tx_gas_used() <= outbe_zerofee::FREE_TX_BOOTSTRAP_GAS_LIMIT,
            "bootstrap gas must fit its signed limit"
        );
        assert_eq!(
            executor.current_execution_summary().validator_fee_sum,
            U256::ZERO,
            "bootstrap must not credit a validator fee"
        );
        assert_eq!(executor.receipts().len(), 1);
        assert!(executor.receipts()[0].success);
        assert_eq!(
            executor.sponsored_gas_used,
            outbe_zerofee::FREE_TX_BOOTSTRAP_GAS_LIMIT,
            "bootstrap must charge its signed gas limit, not actual gas used"
        );

        executor
            .execute_transaction(replay)
            .expect_err("same-block bootstrap replay must fail against current state");
        assert_eq!(
            executor.receipts().len(),
            1,
            "replay must not append a receipt"
        );
        assert_eq!(
            executor.sponsored_gas_used,
            outbe_zerofee::FREE_TX_BOOTSTRAP_GAS_LIMIT,
            "rejected replay must not charge the budget"
        );
        executor.into_state()
    };
    state.commit_source(&BundleSource(&output));

    let account = state
        .get_account(&signer)
        .expect("bootstrap account read")
        .expect("bootstrap account exists");
    let code = account
        .code
        .or_else(|| state.get_code_by_hash(&account.code_hash).ok());
    assert_eq!(
        account.balance, initial_balance,
        "bootstrap must not charge COEN"
    );
    assert_eq!(
        account.nonce, 2,
        "outer tx plus self-authorization consume two nonces"
    );
    assert_eq!(
        code.and_then(|code| code.eip7702_address()),
        Some(ZEROFEE_ADDRESS),
        "bootstrap must install the canonical delegation"
    );
    assert_eq!(
        zerofee_counter_for(&mut state, signer),
        0,
        "bootstrap must leave all daily sponsored calls available"
    );
}
#[test]
fn eip7702_bootstrap_rejects_zero_balance_without_state_change() {
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    );
    let recovered = bootstrap_test_tx();
    let signer = Address::from(*recovered.signer());

    let mut state = CacheDB::new(EmptyDB::default());
    let marker = Bytecode::new_legacy([0xef].into());
    state.insert_account_info(
        &ZEROFEE_ADDRESS,
        AccountInfo {
            code_hash: marker.hash_slow(),
            code: Some(marker),
            ..Default::default()
        },
    );
    state.insert_account_info(&signer, AccountInfo::default());

    let output = {
        let evm = config.evm_with_env(&mut state, pectra_evm_env(1));
        let mut ctx = execution_ctx(Some(1), Bytes::new());
        ctx.execute_outbe_block_hooks = false;
        let mut executor = config.create_executor(evm, ctx);
        let _error = executor
            .execute_transaction(recovered)
            .expect_err("zero-balance bootstrap must not receive the waiver");
        assert!(executor.receipts().is_empty());
        executor.into_state()
    };
    state.commit_source(&BundleSource(&output));

    let account = state
        .get_account(&signer)
        .expect("bootstrap account read")
        .expect("bootstrap account exists");
    assert!(account.balance.is_zero());
    assert_eq!(account.nonce, 0);
    assert!(account.is_empty_code_hash());
    assert_eq!(zerofee_counter_for(&mut state, signer), 0);
}
#[test]
fn sponsored_bootstrap_execution_restores_existing_fee_flags_on_every_result_exit() {
    use evm2::EvmFeatures;
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    );
    for flags in 0..8u8 {
        for outcome in ["included", "declined", "error"] {
            let recovered = bootstrap_test_tx();
            let signer = Address::from(*recovered.signer());
            let mut state = CacheDB::new(EmptyDB::default());
            let marker = Bytecode::new_legacy([0xef].into());
            state.insert_account_info(
                &ZEROFEE_ADDRESS,
                AccountInfo {
                    code_hash: marker.hash_slow(),
                    code: Some(marker),
                    ..Default::default()
                },
            );
            state.insert_account_info(
                &signer,
                AccountInfo {
                    balance: U256::from(1),
                    ..Default::default()
                },
            );
            let expected = (flags & 1 != 0, flags & 2 != 0, flags & 4 != 0);
            let mut env = pectra_evm_env(1);
            env.version
                .features
                .set(EvmFeatures::BALANCE_CHECK, expected.0);
            env.version
                .features
                .set(EvmFeatures::BASE_FEE_CHECK, expected.1);
            env.version
                .features
                .set(EvmFeatures::FEE_CHARGE, expected.2);
            if outcome == "error" {
                // The waiver is authorized, but the inner executor rejects
                // the transaction against the available block gas.
                env.block.gas_limit = U256::ZERO;
            }
            let output = {
                let evm = config.evm_with_env(&mut state, env);
                let mut ctx = execution_ctx(Some(1), Bytes::new());
                ctx.execute_outbe_block_hooks = false;
                let mut executor = config.create_executor(evm, ctx);
                // Leave exactly enough room so only confirmed inclusion exhausts the budget.
                let initial_sponsored_gas = outbe_zerofee::SPONSORED_GAS_PER_BLOCK
                    - outbe_zerofee::FREE_TX_BOOTSTRAP_GAS_LIMIT;
                executor.sponsored_gas_used = initial_sponsored_gas;
                let result =
                    executor.execute_transaction_with_commit_condition(recovered, |_, _| {
                        if outcome == "declined" {
                            CommitChanges::No
                        } else {
                            CommitChanges::Yes
                        }
                    });
                match outcome {
                    "included" => {
                        assert!(result.unwrap().is_some());
                        assert_eq!(executor.receipts().len(), 1);
                        assert!(executor.receipts()[0].success);
                    }
                    "declined" => {
                        assert!(result.unwrap().is_none());
                        assert!(executor.receipts().is_empty());
                    }
                    "error" => {
                        assert!(result.is_err());
                        assert!(executor.receipts().is_empty());
                    }
                    _ => unreachable!(),
                }
                assert_eq!(
                    executor.sponsored_gas_used,
                    if outcome == "included" {
                        outbe_zerofee::SPONSORED_GAS_PER_BLOCK
                    } else {
                        initial_sponsored_gas
                    },
                    "flags={flags}, outcome={outcome}: only inclusion charges the budget"
                );
                let features = executor.evm().version().features;
                assert_eq!(
                    (
                        features.contains(EvmFeatures::BALANCE_CHECK),
                        features.contains(EvmFeatures::BASE_FEE_CHECK),
                        features.contains(EvmFeatures::FEE_CHARGE)
                    ),
                    expected,
                    "flags={flags}, outcome={outcome}"
                );
                assert_eq!(
                    executor.current_execution_summary().validator_fee_sum,
                    U256::ZERO
                );
                executor.into_state()
            };
            state.commit_source(&BundleSource(&output));
            let account = state.get_account(&signer).unwrap().unwrap();
            let code = account
                .code
                .or_else(|| state.get_code_by_hash(&account.code_hash).ok());
            assert_eq!(account.balance, U256::from(1));
            assert_eq!(account.nonce, if outcome == "included" { 2 } else { 0 });
            assert_eq!(
                code.and_then(|code| code.eip7702_address()),
                if outcome == "included" {
                    Some(ZEROFEE_ADDRESS)
                } else {
                    None
                }
            );
            assert_eq!(zerofee_counter_for(&mut state, signer), 0);
        }
    }
}
#[test]
fn eip7702_sponsored_bootstrap_over_budget_defers_without_state_changes() {
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    );
    let recovered = bootstrap_test_tx();
    let signer = Address::from(*recovered.signer());
    let mut state = CacheDB::new(EmptyDB::default());
    let marker = Bytecode::new_legacy([0xef].into());
    state.insert_account_info(
        &ZEROFEE_ADDRESS,
        AccountInfo {
            code_hash: marker.hash_slow(),
            code: Some(marker),
            ..Default::default()
        },
    );
    state.insert_account_info(
        &signer,
        AccountInfo {
            balance: U256::from(1),
            ..Default::default()
        },
    );

    let output = {
        let evm = config.evm_with_env(&mut state, pectra_evm_env(1));
        let mut ctx = execution_ctx(Some(1), Bytes::new());
        ctx.execute_outbe_block_hooks = false;
        let mut executor = config.create_executor(evm, ctx);
        let used =
            outbe_zerofee::SPONSORED_GAS_PER_BLOCK - outbe_zerofee::FREE_TX_BOOTSTRAP_GAS_LIMIT + 1;
        executor.sponsored_gas_used = used;
        let soft_failures = executor.zero_fee_soft_failures;
        let mut commit_called = false;
        let error = executor
            .execute_transaction_with_commit_condition(recovered, |_, _| {
                commit_called = true;
                CommitChanges::Yes
            })
            .expect_err("bootstrap exceeding the remaining budget must be deferred");
        match error {
            BlockExecutionError::Validation(BlockValidationError::InvalidTx { error, .. }) => {
                assert!(outbe_evm::is_sponsored_block_budget_deferral(
                    error.as_ref()
                ));
            }
            other => panic!("expected sponsored budget deferral, got {other:?}"),
        }
        assert!(!commit_called, "deferred bootstrap must not execute");
        assert!(executor.receipts().is_empty());
        assert_eq!(executor.sponsored_gas_used, used);
        assert_eq!(executor.zero_fee_soft_failures, soft_failures);
        assert_eq!(
            executor.current_execution_summary().validator_fee_sum,
            U256::ZERO
        );
        executor.into_state()
    };
    state.commit_source(&BundleSource(&output));

    let account = state.get_account(&signer).unwrap().unwrap();
    assert_eq!(account.balance, U256::from(1));
    assert_eq!(account.nonce, 0);
    assert!(
        account.is_empty_code_hash(),
        "delegation must not be installed"
    );
    assert_eq!(
        state
            .get_storage(&ZEROFEE_ADDRESS, &zerofee_counter_slot(signer))
            .unwrap(),
        U256::ZERO,
        "deferred bootstrap must not consume daily quota"
    );
}
