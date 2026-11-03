//! Unconditional reservation and terminal-phase regression tests.

use super::*;
use outbe_primitives::system_tx::{BlockGasBudget, SystemTxPhase};
use revm::context_interface::Cfg;

#[test]
fn genesis_with_hooks_completes_empty_begin_and_end_phases() {
    let config = OutbeEvmConfig::new(test_chain_spec());
    let mut state = state_with_active_proposer(OWNER);
    let evm = config.evm_with_env(&mut state, test_evm_env(0, REWARDS_ADDRESS));
    let mut ctx = execution_ctx(None, Bytes::new());
    ctx.execute_outbe_block_hooks = true;
    let mut executor = config.create_executor(evm, ctx);
    executor.apply_pre_execution_changes().unwrap();
    assert_eq!(executor.system_tx_phase_cursor, SystemTxPhase::UserTxs);
    executor.complete_end_block_phase().unwrap();
    assert_eq!(
        executor.system_tx_phase_cursor,
        SystemTxPhase::EndBlockComplete {
            gas_capacity: executor.inner.evm.block().gas_limit(),
        }
    );
    assert!(executor.receipts().is_empty());
    let (_, result) = executor.finish().unwrap();
    assert_eq!(result.gas_used, 0);
    assert!(result.receipts.is_empty());
}

#[test]
fn raw_commit_after_end_is_refused_and_poisoned_for_final_validation() {
    let tx = test_regular_tx().try_into_recovered().unwrap();
    let signer = tx.signer();
    let config = OutbeEvmConfig::new(test_chain_spec());
    let mut state = state_with_active_proposer_and_funded_account(OWNER, signer);
    let evm = config.evm_with_env(&mut state, test_evm_env(2, REWARDS_ADDRESS));
    let mut executor = config.create_executor(evm, execution_ctx(None, Bytes::new()));
    let output = executor.execute_transaction_without_commit(tx).unwrap();
    executor.complete_end_block_phase().unwrap();
    executor.commit_transaction(output);
    assert!(executor.receipts().is_empty());
    assert_eq!(executor.inner.cumulative_tx_gas_used, 0);
    assert_eq!(
        executor
            .inner
            .evm
            .db_mut()
            .basic(signer)
            .unwrap()
            .unwrap()
            .nonce,
        0
    );
    assert!(executor
        .finish()
        .err()
        .unwrap()
        .to_string()
        .contains("after the end-block"));
}

#[test]
fn reservation_admission_is_atomic_for_paid_and_synthetic_routes() {
    for block_number in [0, 1, 2] {
        for tx in [test_regular_tx(), test_oracle_submit_vote_tx()] {
            let tx = tx.try_into_recovered().unwrap();
            let config = OutbeEvmConfig::new(test_chain_spec());
            let mut state = state_with_active_proposer_and_funded_account(OWNER, tx.signer());
            let evm = config.evm_with_env(&mut state, test_evm_env(block_number, REWARDS_ADDRESS));
            let mut executor = config.create_executor(evm, execution_ctx(None, Bytes::new()));
            let budget = BlockGasBudget::new(executor.inner.evm.block().gas_limit()).unwrap();
            let used = budget.pre_end_capacity() - tx.gas_limit() + 1;
            executor.inner.cumulative_tx_gas_used = used;
            let err = executor.execute_transaction(tx.clone()).unwrap_err();
            assert!(matches!(
                err,
                BlockExecutionError::Validation(
                    BlockValidationError::TransactionGasLimitMoreThanAvailableBlockGas { .. }
                )
            ));
            assert!(executor.receipts().is_empty());
            assert_eq!(executor.inner.cumulative_tx_gas_used, used);
            assert_eq!(executor.inner.block_regular_gas_used, 0);
            assert_eq!(executor.inner.block_state_gas_used, 0);
            assert_eq!(executor.zero_fee_soft_failures, 0);
            assert_eq!(executor.sponsored_gas_used, 0);
            assert_eq!(executor.current_block_validator_fees, U256::ZERO);
            assert_eq!(executor.current_block_burned_fees, U256::ZERO);
            assert_eq!(executor.body_tx_index, 0);
            assert!(executor.witness_state.is_none());
            assert_eq!(
                executor
                    .inner
                    .evm
                    .db_mut()
                    .basic(tx.signer())
                    .unwrap()
                    .unwrap()
                    .nonce,
                0
            );
            // The noncommitting adapter must reject before running the EVM, too.
            assert!(executor.execute_transaction_without_commit(tx).is_err());
            assert!(executor.receipts().is_empty());
        }
    }
}

#[test]
fn exact_capacity_commits_actual_gas_then_empty_end_phase_is_terminal() {
    for gas_limit in [21_000, 100_000] {
        let tx: TransactionSigned = TxEip1559 {
            chain_id: CHAIN_ID,
            nonce: 0,
            gas_limit,
            max_fee_per_gas: u128::from(MIN_PROTOCOL_BASE_FEE),
            max_priority_fee_per_gas: 0,
            to: TxKind::Call(Address::ZERO),
            value: U256::ZERO,
            input: Bytes::new(),
            access_list: Default::default(),
        }
        .into_signed(Signature::test_signature())
        .into();
        let tx = tx.try_into_recovered().unwrap();
        let config = OutbeEvmConfig::new(test_chain_spec());
        let mut state = state_with_active_proposer_and_funded_account(OWNER, tx.signer());
        let evm = config.evm_with_env(&mut state, test_evm_env(2, REWARDS_ADDRESS));
        let mut executor = config.create_executor(evm, execution_ctx(None, Bytes::new()));
        let budget = BlockGasBudget::new(executor.inner.evm.block().gas_limit()).unwrap();
        let used = budget.pre_end_capacity() - tx.gas_limit();
        executor.inner.cumulative_tx_gas_used = used;
        executor.execute_transaction(tx.clone()).unwrap();
        let actual_gas = used + 21_000;
        assert_eq!(executor.inner.cumulative_tx_gas_used, actual_gas);
        let receipts = executor.receipts().to_vec();
        let body_index = executor.body_tx_index;
        executor.complete_end_block_phase().unwrap();
        assert_eq!(
            executor.system_tx_phase_cursor,
            SystemTxPhase::EndBlockComplete {
                gas_capacity: executor.inner.evm.block().gas_limit() - actual_gas,
            }
        );
        executor.complete_end_block_phase().unwrap();
        assert_eq!(executor.receipts(), receipts);
        assert_eq!(executor.body_tx_index, body_index);
        assert!(executor
            .execute_transaction(tx)
            .unwrap_err()
            .to_string()
            .contains("after the end-block"));
        assert_eq!(executor.receipts(), receipts);
    }
}

#[test]
fn end_phase_checks_begin_completion_pending_context_and_authoritative_counters() {
    for hooks in [true, false] {
        let config = OutbeEvmConfig::new(test_chain_spec());
        let mut state = State::builder()
            .with_database(CacheDB::<EmptyDBTyped<ProviderError>>::default())
            .with_bundle_update()
            .build();
        let evm = config.evm_with_env(&mut state, test_evm_env(2, REWARDS_ADDRESS));
        let mut ctx = execution_ctx(None, Bytes::new());
        ctx.execute_outbe_block_hooks = hooks;
        let mut executor = config.create_executor(evm, ctx);
        executor.system_tx_phase_cursor = SystemTxPhase::HookEvents { body_index: 4 };
        if hooks {
            assert!(executor
                .complete_end_block_phase()
                .unwrap_err()
                .to_string()
                .contains("begin witnesses"));
            executor.system_tx_phase_cursor = SystemTxPhase::UserTxs;
        }
        let budget = BlockGasBudget::new(executor.inner.evm.block().gas_limit()).unwrap();
        executor.inner.cumulative_tx_gas_used = 18_000_000;
        executor.complete_end_block_phase().unwrap();
        assert_eq!(
            executor.system_tx_phase_cursor,
            SystemTxPhase::EndBlockComplete {
                gas_capacity: executor.inner.evm.block().gas_limit() - 18_000_000,
            }
        );
        assert!(executor.receipts().is_empty());
        // Direct commit adapters cannot hide a reserve violation at finish.
        executor.inner.cumulative_tx_gas_used = budget.pre_end_capacity() + 1;
        assert!(executor.finish().is_err());
    }
}

#[test]
fn eip8037_reservation_preserves_regular_cap_and_full_state_admission() {
    let config = OutbeEvmConfig::new(test_chain_spec());
    let mut state = State::builder()
        .with_database(CacheDB::<EmptyDBTyped<ProviderError>>::default())
        .with_bundle_update()
        .build();
    let mut env = test_evm_env(2, REWARDS_ADDRESS);
    env.cfg_env.enable_amsterdam_eip8037 = true;
    env.cfg_env.tx_gas_limit_cap = Some(16_000_000);
    let evm = config.evm_with_env(&mut state, env);
    let mut executor = config.create_executor(evm, execution_ctx(None, Bytes::new()));
    let budget = BlockGasBudget::new(executor.inner.evm.block().gas_limit()).unwrap();
    let cap = executor.inner.evm.cfg_env().tx_gas_limit_cap();
    let declared = cap + 1;
    executor.inner.block_regular_gas_used = budget.pre_end_capacity() - cap;
    executor.inner.block_state_gas_used = budget.pre_end_capacity() - declared;
    executor.check_user_pre_end_gas(declared).unwrap();
    executor.inner.block_state_gas_used += 1;
    assert!(executor.check_user_pre_end_gas(declared).is_err());
    executor.inner.block_state_gas_used -= 1;
    executor.inner.block_regular_gas_used += 1;
    assert!(executor.check_user_pre_end_gas(declared).is_err());
    executor.inner.block_regular_gas_used = 1;
    executor.inner.block_state_gas_used = 18_000_000;
    executor.complete_end_block_phase().unwrap();
    assert_eq!(
        executor.system_tx_phase_cursor,
        SystemTxPhase::EndBlockComplete {
            gas_capacity: executor.inner.evm.block().gas_limit() - 18_000_000,
        }
    );
}

#[test]
fn hook_events_rejection_preserves_logs_and_begin_receipt_accounting() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let parent = B256::with_last_byte(0xA5);
    let metadata = CertifiedParentAccountingMetadata {
        finalized_block_number: 1,
        finalized_block_hash: parent,
        ..Default::default()
    };
    let config = OutbeEvmConfig::new(test_chain_spec()).with_evm_signer(signer);
    let begin = begin_system_txs_for_test(
        &config,
        BeginBlockFixture {
            block_number: 2,
            parent_hash: parent,
            extra_data: &Bytes::new(),
            parent_consensus_metadata: Some(metadata.clone()),
            proposer,
            bootstrap: BootstrapFixture::Absent,
        },
    );
    let hook = begin.last().unwrap().clone();
    let ordinal = u8::try_from(begin.len() - 1).unwrap();
    let mut state = state_with_active_proposer(proposer);
    let evm = config.evm_with_env(&mut state, test_evm_env(2, REWARDS_ADDRESS));
    let mut ctx = execution_ctx(None, Bytes::new());
    ctx.inner.parent_hash = parent;
    ctx.proposer_evm_address = Some(proposer);
    ctx.parent_consensus_metadata = Some(metadata);
    ctx.expected_begin_system_txs = begin;
    let mut executor = config.create_executor(evm, ctx);
    executor.system_tx_phase_cursor = SystemTxPhase::HookEvents {
        body_index: ordinal,
    };
    let logs = vec![Log::default()];
    executor.whitelisted_hook_event_logs = logs.clone();
    let budget = BlockGasBudget::new(executor.inner.evm.block().gas_limit()).unwrap();
    executor.inner.cumulative_tx_gas_used = budget.pre_end_capacity();
    assert!(executor.execute_transaction(hook).is_err());
    assert_eq!(executor.whitelisted_hook_event_logs, logs);
    assert!(executor.receipts().is_empty());
    assert_eq!(
        executor.system_tx_phase_cursor,
        SystemTxPhase::HookEvents {
            body_index: ordinal
        }
    );
    assert_eq!(
        executor.inner.cumulative_tx_gas_used,
        budget.pre_end_capacity()
    );
    assert!(executor.visible_system_gas(1, 0, 1).is_err());
    assert!(executor
        .push_system_failure_receipt(SystemFailureReceiptInput {
            tx_type: alloy_consensus::TxType::Eip1559,
            log_address: OUTBE_SYSTEM_TX_ADDRESS,
            code: 201,
            reason: "test".into(),
            visible_base_gas: 1,
            additional_visible_gas: 0,
            signed_gas_limit: 1,
            internal_gas_used: 1,
        })
        .is_err());
    assert!(executor.receipts().is_empty());
    assert_eq!(executor.system_tx_execution_gas, 0);
}
