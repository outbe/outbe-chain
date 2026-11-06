//! Unconditional reservation and terminal-phase regression tests.

use super::*;
use outbe_primitives::system_tx::{BlockGasBudget, SystemTxPhase};
use revm::context_interface::Cfg;

#[test]
fn header_preparation_does_not_run_post_execution_or_enter_end_block_phase() {
    let config = OutbeEvmConfig::new(test_chain_spec());
    let mut state = state_with_active_proposer(OWNER);
    let evm = config.evm_with_env(&mut state, test_evm_env(2, REWARDS_ADDRESS));
    let mut executor = config.create_executor(evm, execution_ctx(None, Bytes::new()));
    assert_eq!(executor.system_tx_phase_cursor, SystemTxPhase::UserTxs);
    executor.prepare_final_header_artifacts(0).unwrap();
    assert_eq!(executor.system_tx_phase_cursor, SystemTxPhase::UserTxs);
    assert!(executor.ethereum_post_execution_requests.is_none());
    assert!(executor.receipts().is_empty());
}

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
    executor.apply_end_block_phase().unwrap();
    executor.prepare_final_header_artifacts(0).unwrap();
    assert_eq!(
        executor.system_tx_phase_cursor,
        SystemTxPhase::EndBlock {
            gas_capacity: executor.inner.evm.block().gas_limit(),
        }
    );
    assert!(executor.receipts().is_empty());
    executor.prepare_final_header_artifacts(0).unwrap();
    assert_eq!(executor.inner.cumulative_tx_gas_used, 0);
    let (_, result) = executor.finish().unwrap();
    assert_eq!(result.gas_used, 0);
    assert!(result.receipts.is_empty());
}

#[test]
fn reservation_admission_is_atomic_for_paid_and_synthetic_routes() {
    for block_number in [0, 1, 2] {
        for tx in [test_regular_tx(), test_oracle_submit_vote_tx()] {
            let tx = tx.try_into_recovered().unwrap();
            let signer = test_evm_signer();
            let config = OutbeEvmConfig::new(test_chain_spec()).with_evm_signer(signer.clone());
            let mut state = state_with_active_proposer_and_funded_account(OWNER, tx.signer());
            let evm = config.evm_with_env(&mut state, test_evm_env(block_number, REWARDS_ADDRESS));
            let mut ctx = execution_ctx(None, Bytes::new());
            ctx.proposer_evm_address = Some(signer.address());
            let mut executor = config.create_executor(evm, ctx);
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
fn exact_capacity_commits_actual_gas_then_captures_end_phase_capacity() {
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
        let signer = test_evm_signer();
        let config = OutbeEvmConfig::new(test_chain_spec()).with_evm_signer(signer.clone());
        let mut state = state_with_active_proposer_and_funded_account(OWNER, tx.signer());
        let evm = config.evm_with_env(&mut state, test_evm_env(2, REWARDS_ADDRESS));
        let mut ctx = execution_ctx(None, Bytes::new());
        ctx.proposer_evm_address = Some(signer.address());
        let mut executor = config.create_executor(evm, ctx);
        let budget = BlockGasBudget::new(executor.inner.evm.block().gas_limit()).unwrap();
        let used = budget.pre_end_capacity() - tx.gas_limit();
        executor.inner.cumulative_tx_gas_used = used;
        executor.execute_transaction(tx.clone()).unwrap();
        let actual_gas = used + 21_000;
        assert_eq!(executor.inner.cumulative_tx_gas_used, actual_gas);
        let receipts = executor.receipts().len();
        let body_index = executor.body_tx_index;
        executor.apply_end_block_phase().unwrap();
        assert_eq!(executor.receipts().len(), receipts + 2);
        assert_eq!(
            executor.system_tx_phase_cursor,
            SystemTxPhase::EndBlock {
                gas_capacity: executor.inner.evm.block().gas_limit() - actual_gas,
            }
        );
        executor.apply_end_block_phase().unwrap();
        assert_eq!(executor.receipts().len(), receipts + 2);
        assert_eq!(executor.body_tx_index, body_index);
    }
}

#[test]
fn end_phase_uses_authoritative_counters_without_rechecking_begin_cursor() {
    for hooks in [true, false] {
        let signer = test_evm_signer();
        let config = OutbeEvmConfig::new(test_chain_spec()).with_evm_signer(signer.clone());
        let mut state = State::builder()
            .with_database(CacheDB::<EmptyDBTyped<ProviderError>>::default())
            .with_bundle_update()
            .build();
        let evm = config.evm_with_env(&mut state, test_evm_env(2, REWARDS_ADDRESS));
        let mut ctx = execution_ctx(None, Bytes::new());
        ctx.execute_outbe_block_hooks = hooks;
        ctx.proposer_evm_address = Some(signer.address());
        let mut executor = config.create_executor(evm, ctx);
        executor.system_tx_phase_cursor = SystemTxPhase::HookEvents { body_index: 4 };
        executor.inner.cumulative_tx_gas_used = 18_000_000;
        executor.apply_end_block_phase().unwrap();
        assert_eq!(
            executor.system_tx_phase_cursor,
            SystemTxPhase::EndBlock {
                gas_capacity: executor.inner.evm.block().gas_limit() - 18_000_000,
            }
        );
        assert_eq!(executor.receipts().len(), 2);
        executor.inner.cumulative_tx_gas_used = executor.inner.evm.block().gas_limit() + 1;
        assert!(executor.finish().is_err());
    }
}

#[test]
fn eip8037_reservation_preserves_regular_cap_and_full_state_admission() {
    let signer = test_evm_signer();
    let config = OutbeEvmConfig::new(test_chain_spec()).with_evm_signer(signer.clone());
    let mut state = State::builder()
        .with_database(CacheDB::<EmptyDBTyped<ProviderError>>::default())
        .with_bundle_update()
        .build();
    let mut env = test_evm_env(2, REWARDS_ADDRESS);
    env.cfg_env.enable_amsterdam_eip8037 = true;
    env.cfg_env.tx_gas_limit_cap = Some(16_000_000);
    let evm = config.evm_with_env(&mut state, env);
    let mut ctx = execution_ctx(None, Bytes::new());
    ctx.proposer_evm_address = Some(signer.address());
    let mut executor = config.create_executor(evm, ctx);
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
    executor.apply_end_block_phase().unwrap();
    assert_eq!(
        executor.system_tx_phase_cursor,
        SystemTxPhase::EndBlock {
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
            internal_gross_gas_used: 1,
        })
        .is_err());
    assert!(executor.receipts().is_empty());
    assert_eq!(executor.system_tx_execution_gas, 0);
}

#[test]
fn end_block_parity_proposer_vs_verifier_across_regimes() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let config = OutbeEvmConfig::new(test_chain_spec()).with_evm_signer(signer);

    for block_number in [0u64, 1, 2] {
        let mut proposer_state = state_with_active_proposer(proposer);
        let proposer_evm = config.evm_with_env(
            &mut proposer_state,
            test_evm_env(block_number, REWARDS_ADDRESS),
        );
        let mut proposer_ctx = execution_ctx(None, Bytes::new());
        proposer_ctx.proposer_evm_address = Some(proposer);
        let mut proposer_exec = config.create_executor(proposer_evm, proposer_ctx);
        proposer_exec.apply_end_block_phase().unwrap();
        let staged = proposer_exec.take_staged_end_block_txs();

        if block_number < 2 {
            assert!(staged.is_empty());
            assert_eq!(
                proposer_exec.system_tx_phase_cursor,
                SystemTxPhase::EndBlock {
                    gas_capacity: proposer_exec.inner.evm.block().gas_limit(),
                }
            );
            // Any end-block system tx submitted at Genesis or GenesisBootstrap must be rejected.
            let fake_call = config
                .build_end_system_tx(
                    block_number,
                    CHAIN_ID,
                    proposer,
                    SystemTx::CallEventProcessing,
                    100_000,
                )
                .unwrap();
            let mut verifier_state = state_with_active_proposer(proposer);
            let verifier_evm = config.evm_with_env(
                &mut verifier_state,
                test_evm_env(block_number, REWARDS_ADDRESS),
            );
            let mut verifier_ctx = execution_ctx(None, Bytes::new());
            verifier_ctx.proposer_evm_address = Some(proposer);
            let mut verifier_exec = config.create_executor(verifier_evm, verifier_ctx);
            assert!(verifier_exec.execute_transaction(fake_call).is_err());
        } else {
            assert_eq!(staged.len(), 2);
            let mut verifier_state = state_with_active_proposer(proposer);
            let verifier_evm = config.evm_with_env(
                &mut verifier_state,
                test_evm_env(block_number, REWARDS_ADDRESS),
            );
            let mut verifier_ctx = execution_ctx(None, Bytes::new());
            verifier_ctx.proposer_evm_address = Some(proposer);
            let mut verifier_exec = config.create_executor(verifier_evm, verifier_ctx);
            for tx in staged {
                verifier_exec.execute_transaction(tx).unwrap();
            }
            assert_eq!(verifier_exec.receipts(), proposer_exec.receipts());
            assert_eq!(
                verifier_exec.inner.cumulative_tx_gas_used,
                proposer_exec.inner.cumulative_tx_gas_used
            );
            assert_eq!(
                verifier_exec.system_tx_phase_cursor,
                proposer_exec.system_tx_phase_cursor
            );
        }
    }
}

#[test]
fn end_block_cursor_rejects_wrong_order_duplicate_or_mismatched_gas_limit() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let config = OutbeEvmConfig::new(test_chain_spec()).with_evm_signer(signer);
    let mut state = state_with_active_proposer(proposer);
    let evm = config.evm_with_env(&mut state, test_evm_env(2, REWARDS_ADDRESS));
    let mut ctx = execution_ctx(None, Bytes::new());
    ctx.proposer_evm_address = Some(proposer);
    let mut executor = config.create_executor(evm, ctx);

    let capacity = executor.inner.evm.block().gas_limit();
    let expected_call_limit =
        outbe_primitives::end_gas::expected_call_event_gas_limit(capacity).unwrap();

    // 1. CleanupProcessing before CallEventProcessing must fail with SystemTxPhaseMismatch.
    let premature_cleanup = config
        .build_end_system_tx(2, CHAIN_ID, proposer, SystemTx::CleanupProcessing, capacity)
        .unwrap();
    let err = executor.execute_transaction(premature_cleanup).unwrap_err();
    assert!(err.to_string().contains("CallEventProcessing"));

    // 2. CallEventProcessing with mismatched gas_limit must fail with EndBlockGasLimitMismatch.
    let bad_limit_call = config
        .build_end_system_tx(
            2,
            CHAIN_ID,
            proposer,
            SystemTx::CallEventProcessing,
            expected_call_limit - 1,
        )
        .unwrap();
    let err = executor.execute_transaction(bad_limit_call).unwrap_err();
    assert!(err.to_string().contains("CallEventProcessing"));

    // 3. Valid CallEventProcessing transitions cursor to EndBlockCleanup.
    let valid_call = config
        .build_end_system_tx(
            2,
            CHAIN_ID,
            proposer,
            SystemTx::CallEventProcessing,
            expected_call_limit,
        )
        .unwrap();
    let call_gas = executor
        .execute_transaction(valid_call.clone())
        .unwrap()
        .tx_gas_used();
    assert_eq!(
        executor.system_tx_phase_cursor,
        SystemTxPhase::EndBlockCleanup {
            gas_capacity: capacity,
            call_gas_used: call_gas,
        }
    );

    // 4. Duplicate CallEventProcessing when cursor is EndBlockCleanup must fail.
    let err = executor.execute_transaction(valid_call).unwrap_err();
    assert!(err.to_string().contains("CleanupProcessing"));

    // 5. Valid CleanupProcessing transitions cursor to EndBlock, and a duplicate CleanupProcessing is rejected.
    let expected_cleanup_limit =
        outbe_primitives::end_gas::expected_cleanup_gas_limit(capacity, call_gas).unwrap();
    let valid_cleanup = config
        .build_end_system_tx(
            2,
            CHAIN_ID,
            proposer,
            SystemTx::CleanupProcessing,
            expected_cleanup_limit,
        )
        .unwrap();
    executor.execute_transaction(valid_cleanup.clone()).unwrap();
    assert_eq!(
        executor.system_tx_phase_cursor,
        SystemTxPhase::EndBlock {
            gas_capacity: capacity,
        }
    );
    assert!(executor.execute_transaction(valid_cleanup).is_err());
}

#[test]
fn end_block_unit_stops_cleanly_when_remaining_gas_below_unit_ceiling() {
    use outbe_primitives::{
        end_gas::{unit_ceiling, UnitKind},
        storage::{hashmap::HashMapStorageProvider, StorageHandle},
    };

    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    // Remaining gas is strictly below unit_ceiling(UnitKind::Call) (1_500_000).
    provider.set_gas_limit(unit_ceiling(UnitKind::Call) - 1);
    StorageHandle::enter(&mut provider, |storage| {
        storage.enable_end_gas_metering().unwrap();
        let mut invoked = false;
        let outcome = storage.run_gas_bounded_unit(UnitKind::Call, || {
            invoked = true;
            Ok(1u32)
        });
        assert!(
            !invoked,
            "unit body must not run when remaining gas < ceiling"
        );
        assert!(matches!(
            outcome,
            Err(outbe_primitives::error::PrecompileError::OutOfGas)
        ));
    });
}

#[test]
fn end_block_soft_failure_receipt_preserves_cleanup_budget_and_continues_block() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let config = OutbeEvmConfig::new(test_chain_spec()).with_evm_signer(signer);
    let mut state = state_with_active_proposer(proposer);
    let evm = config.evm_with_env(&mut state, test_evm_env(2, REWARDS_ADDRESS));
    let mut ctx = execution_ctx(None, Bytes::new());
    ctx.proposer_evm_address = Some(proposer);
    let mut executor = config.create_executor(evm, ctx);

    let capacity = executor.inner.evm.block().gas_limit();
    let call_limit = outbe_primitives::end_gas::expected_call_event_gas_limit(capacity).unwrap();
    let call_tx = config
        .build_end_system_tx(
            2,
            CHAIN_ID,
            proposer,
            SystemTx::CallEventProcessing,
            call_limit,
        )
        .unwrap();

    // Force a revert on CallEventProcessing (charges 42_000 gas and emits a soft-failure receipt).
    let call_gas = crate::factory::with_forced_outbe_system_call_revert(|| {
        executor.execute_transaction(call_tx)
    })
    .unwrap()
    .tx_gas_used();
    assert_eq!(call_gas, 42_000);
    assert_eq!(executor.receipts().len(), 1);
    assert!(!executor.receipts()[0].success);
    assert_eq!(
        executor.system_tx_phase_cursor,
        SystemTxPhase::EndBlockCleanup {
            gas_capacity: capacity,
            call_gas_used: 42_000,
        }
    );

    // CleanupProcessing receives the exact remaining budget (`capacity - 42_000`) and succeeds.
    let cleanup_limit =
        outbe_primitives::end_gas::expected_cleanup_gas_limit(capacity, call_gas).unwrap();
    let cleanup_tx = config
        .build_end_system_tx(
            2,
            CHAIN_ID,
            proposer,
            SystemTx::CleanupProcessing,
            cleanup_limit,
        )
        .unwrap();
    executor.execute_transaction(cleanup_tx).unwrap();
    assert_eq!(executor.receipts().len(), 2);
    assert!(executor.receipts()[1].success);
    assert_eq!(
        executor.system_tx_phase_cursor,
        SystemTxPhase::EndBlock {
            gas_capacity: capacity,
        }
    );
    executor.prepare_final_header_artifacts(0).unwrap();
    let (_, result) = executor.finish().unwrap();
    assert_eq!(result.receipts.len(), 2);
}
