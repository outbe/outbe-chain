use super::*;

fn assert_timestamp_validation(error: BlockExecutionError) {
    assert!(
        matches!(error, BlockExecutionError::Validation(_)),
        "{error:?}"
    );
    assert_eq!(error.to_string(), "timestamp exceeds u64");
}

#[test]
fn wide_timestamp_rejects_pre_execution_before_any_mutation_with_or_without_hooks() {
    for timestamp in [U256::from(u64::MAX) + U256::from(1), U256::MAX] {
        for hooks in [false, true] {
            let config = OutbeEvmConfig::new(test_chain_spec());
            let mut state = State::builder()
                .with_database(CacheDB::<EmptyDBTyped<ProviderError>>::default())
                .with_bundle_update()
                .build();
            let mut env = test_evm_env(1, REWARDS_ADDRESS);
            env.block_env.timestamp = timestamp;
            let evm = config.evm_with_env(&mut state, env);
            let mut ctx = execution_ctx(Some(1), Bytes::new());
            ctx.execute_outbe_block_hooks = hooks;
            let mut executor = config.create_executor(evm, ctx);
            assert_eq!(
                executor.system_tx_phase_cursor,
                crate::system_tx::SystemTxPhase::UserTxs
            );

            assert_timestamp_validation(executor.apply_pre_execution_changes().unwrap_err());
            assert_eq!(
                executor.system_tx_phase_cursor,
                crate::system_tx::SystemTxPhase::UserTxs
            );
            assert!(executor.receipts().is_empty());
            assert!(executor.whitelisted_hook_event_logs.is_empty());
            assert!(executor.ethereum_post_execution_requests.is_none());
            drop(executor);
            assert!(
                state.cache.accounts.is_empty(),
                "no Ethereum or marker account writes"
            );
        }
    }
}

#[test]
fn wide_timestamp_rejects_standalone_transaction_routes_before_receipts_or_state() {
    let reserved: TransactionSigned = TxEip1559 {
        chain_id: CHAIN_ID,
        gas_limit: 100_000,
        to: TxKind::Call(OUTBE_SYSTEM_TX_ADDRESS),
        ..Default::default()
    }
    .into_signed(Signature::test_signature())
    .into();
    for tx in [test_regular_tx(), test_oracle_submit_vote_tx(), reserved] {
        for entry_point in 0..3 {
            let config = OutbeEvmConfig::new(test_chain_spec());
            let mut state = State::builder()
                .with_database(CacheDB::<EmptyDBTyped<ProviderError>>::default())
                .with_bundle_update()
                .build();
            let mut env = test_evm_env(1, REWARDS_ADDRESS);
            env.block_env.timestamp = U256::MAX;
            let evm = config.evm_with_env(&mut state, env);
            let mut executor = config.create_executor(evm, execution_ctx(Some(1), Bytes::new()));
            let recovered = tx.clone().try_into_recovered().unwrap();
            let error = match entry_point {
                0 => executor.execute_transaction(recovered).unwrap_err(),
                1 => executor
                    .execute_transaction_without_commit(recovered)
                    .unwrap_err(),
                _ if is_reserved_system_tx(&tx) => executor
                    .execute_reserved_system_tx(recovered, |_| CommitChanges::Yes)
                    .unwrap_err(),
                _ => executor
                    .route_user_transaction(TxEnv::default(), recovered, |_| CommitChanges::Yes)
                    .unwrap_err(),
            };
            assert_timestamp_validation(error);
            assert!(executor.receipts().is_empty());
            assert_eq!(executor.zero_fee_soft_failures, 0);
            assert_eq!(executor.current_block_validator_fees, U256::ZERO);
            assert_eq!(
                executor.system_tx_phase_cursor,
                crate::system_tx::SystemTxPhase::UserTxs
            );
            drop(executor);
            assert!(state.cache.accounts.is_empty());
        }
    }
}

#[test]
fn standalone_finish_and_post_execution_reject_wide_time_before_side_effects() {
    for finish in [false, true] {
        let config = OutbeEvmConfig::new(test_chain_spec());
        let mut state = State::builder()
            .with_database(CacheDB::<EmptyDBTyped<ProviderError>>::default())
            .with_bundle_update()
            .build();
        let mut env = test_evm_env(0, REWARDS_ADDRESS);
        env.block_env.timestamp = U256::from(u64::MAX) + U256::from(1);
        let evm = config.evm_with_env(&mut state, env);
        let mut executor = config.create_executor(evm, execution_ctx(Some(0), Bytes::new()));
        if finish {
            let error = match executor.finish() {
                Err(error) => error,
                Ok(_) => panic!("wide timestamp must fail finish"),
            };
            assert_timestamp_validation(error);
        } else {
            assert_timestamp_validation(
                executor.apply_outbe_ethereum_post_execution().unwrap_err(),
            );
            assert!(executor.ethereum_post_execution_requests.is_none());
            assert!(executor.receipts().is_empty());
            drop(executor);
        }
        assert!(state.cache.accounts.is_empty());
    }
}

#[test]
fn maximum_u64_time_passes_pending_pre_execution_conversion() {
    let config = OutbeEvmConfig::new(test_chain_spec());
    let mut state = State::builder()
        .with_database(CacheDB::<EmptyDBTyped<ProviderError>>::default())
        .with_bundle_update()
        .build();
    let mut env = test_evm_env(1, REWARDS_ADDRESS);
    env.block_env.timestamp = U256::from(u64::MAX);
    let evm = config.evm_with_env(&mut state, env);
    let mut ctx = execution_ctx(Some(1), Bytes::new());
    ctx.inner.parent_beacon_block_root = Some(B256::ZERO);
    ctx.execute_outbe_block_hooks = false;
    let mut executor = config.create_executor(evm, ctx);
    executor.apply_pre_execution_changes().unwrap();
    assert!(executor.receipts().is_empty());
}
