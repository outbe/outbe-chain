// Unconditional reservation, phase cursor, pass ordering, and mid-block gas snapshot tests.

use super::*;
use outbe_primitives::system_tx::{BlockExecutionStage, SystemTxPhase};

#[test]
fn header_preparation_does_not_run_post_execution_or_enter_end_block_phase() {
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    );
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
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    );
    let mut state = state_with_active_proposer(OWNER);
    let evm = config.evm_with_env(&mut state, test_evm_env(0, REWARDS_ADDRESS));
    let mut ctx = execution_ctx(None, Bytes::new());
    ctx.execute_outbe_block_hooks = true;
    let mut executor = config.create_executor(evm, ctx);
    executor.apply_pre_execution_changes().unwrap();
    assert_eq!(executor.system_tx_phase_cursor, SystemTxPhase::UserTxs);
    executor.apply_end_block_phase().unwrap();
    executor.prepare_final_header_artifacts(0).unwrap();
    assert_eq!(executor.system_tx_phase_cursor, SystemTxPhase::EndBlock);
    assert_eq!(executor.fsm.stage(), BlockExecutionStage::Completed);
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
            let config = OutbeEvmConfig::new(
                test_chain_spec(),
                std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
            )
            .with_evm_signer(signer.clone());
            let mut state = state_with_active_proposer_and_funded_account(OWNER, tx.signer());
            let evm = config.evm_with_env(&mut state, test_evm_env(block_number, REWARDS_ADDRESS));
            let mut ctx = execution_ctx(None, Bytes::new());
            ctx.proposer_evm_address = Some(signer.address());
            let mut executor = config.create_executor(evm, ctx);
            let block_gas_limit = executor.inner.evm.block().gas_limit();
            let used = block_gas_limit - tx.gas_limit() + 1;
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
        let config = OutbeEvmConfig::new(
            test_chain_spec(),
            std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
        )
        .with_evm_signer(signer.clone());
        let mut state = state_with_active_proposer_and_funded_account(OWNER, tx.signer());
        let evm = config.evm_with_env(&mut state, test_evm_env(2, REWARDS_ADDRESS));
        let mut ctx = execution_ctx(None, Bytes::new());
        ctx.execute_outbe_block_hooks = false;
        ctx.proposer_evm_address = Some(signer.address());
        let mut executor = config.create_executor(evm, ctx);
        let block_gas_limit = executor.inner.evm.block().gas_limit();
        let used = block_gas_limit - tx.gas_limit();
        executor.inner.cumulative_tx_gas_used = used;
        executor.execute_transaction(tx.clone()).unwrap();
        let actual_gas = used + 21_000;
        assert_eq!(executor.inner.cumulative_tx_gas_used, actual_gas);
        let receipts = executor.receipts().len();
        let body_index = executor.body_tx_index;
        executor.apply_end_block_phase().unwrap();
        assert_eq!(executor.receipts().len(), receipts + 5);
        assert_eq!(executor.system_tx_phase_cursor, SystemTxPhase::EndBlock);
        executor.apply_end_block_phase().unwrap();
        assert_eq!(executor.receipts().len(), receipts + 5);
        assert_eq!(executor.body_tx_index, body_index);
    }
}

#[test]
fn end_phase_uses_authoritative_counters_without_rechecking_begin_cursor() {
    for hooks in [true, false] {
        let signer = test_evm_signer();
        let config = OutbeEvmConfig::new(
            test_chain_spec(),
            std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
        )
        .with_evm_signer(signer.clone());
        let mut state = State::builder()
            .with_database(CacheDB::<EmptyDBTyped<ProviderError>>::default())
            .with_bundle_update()
            .build();
        let evm = config.evm_with_env(&mut state, test_evm_env(2, REWARDS_ADDRESS));
        let mut ctx = execution_ctx(None, Bytes::new());
        ctx.execute_outbe_block_hooks = hooks;
        ctx.proposer_evm_address = Some(signer.address());
        let mut executor = config.create_executor(evm, ctx);
        executor.system_tx_phase_cursor = SystemTxPhase::UserTxs;
        if hooks {
            executor.fsm =
                outbe_primitives::system_tx::BlockExecutionFsm::for_block(2, false, false, false);
            for kind in
                outbe_primitives::system_tx::BlockSystemPlan::for_block(2, false, false, false)
                    .begin_zone_kinds()
            {
                executor.fsm.on_system_tx(kind).unwrap();
            }
        }
        executor.inner.cumulative_tx_gas_used = 18_000_000;
        executor.apply_end_block_phase().unwrap();
        assert_eq!(executor.system_tx_phase_cursor, SystemTxPhase::EndBlock);
        assert_eq!(executor.receipts().len(), 5);
        executor.inner.cumulative_tx_gas_used = executor.inner.evm.block().gas_limit() + 1;
        assert!(executor.finish().is_err());
    }
}

#[test]
fn eip8037_reservation_preserves_regular_cap_and_full_state_admission() {
    let signer = test_evm_signer();
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(signer.clone());
    let mut state = State::builder()
        .with_database(CacheDB::<EmptyDBTyped<ProviderError>>::default())
        .with_bundle_update()
        .build();
    let mut env = test_evm_env(2, REWARDS_ADDRESS);
    env.cfg_env.enable_amsterdam_eip8037 = true;
    env.cfg_env.tx_gas_limit_cap = Some(16_000_000);
    let evm = config.evm_with_env(&mut state, env);
    let mut ctx = execution_ctx(None, Bytes::new());
    ctx.execute_outbe_block_hooks = false;
    ctx.proposer_evm_address = Some(signer.address());
    let mut executor = config.create_executor(evm, ctx);
    assert_eq!(executor.tx_gas_limit_cap(), 16_000_000);
    executor.inner.block_regular_gas_used = 1;
    executor.inner.block_state_gas_used = 18_000_000;
    assert_eq!(executor.accounted_block_gas(), 18_000_000);
    executor.apply_end_block_phase().unwrap();
    assert_eq!(executor.system_tx_phase_cursor, SystemTxPhase::EndBlock);
}

#[test]
fn end_block_parity_proposer_vs_verifier_across_regimes() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(signer);

    for block_number in [0u64, 1, 2] {
        let mut proposer_state = state_with_active_proposer(proposer);
        let proposer_evm = config.evm_with_env(
            &mut proposer_state,
            test_evm_env(block_number, REWARDS_ADDRESS),
        );
        let mut proposer_ctx = execution_ctx(None, Bytes::new());
        proposer_ctx.execute_outbe_block_hooks = false;
        proposer_ctx.proposer_evm_address = Some(proposer);
        let mut proposer_exec = config.create_executor(proposer_evm, proposer_ctx);
        proposer_exec.apply_end_block_phase().unwrap();
        let staged = proposer_exec.take_staged_end_block_txs();

        if block_number < 2 {
            assert!(staged.is_empty());
            assert_eq!(
                proposer_exec.system_tx_phase_cursor,
                SystemTxPhase::EndBlock
            );
            // Any end-block system tx submitted at Genesis or GenesisBootstrap must be rejected.
            let fake_call = config
                .build_end_system_tx(2, CHAIN_ID, proposer, SystemTx::NodFactory)
                .unwrap();
            let mut verifier_state = state_with_active_proposer(proposer);
            let verifier_evm = config.evm_with_env(
                &mut verifier_state,
                test_evm_env(block_number, REWARDS_ADDRESS),
            );
            let mut verifier_ctx = execution_ctx(None, Bytes::new());
            verifier_ctx.execute_outbe_block_hooks = false;
            verifier_ctx.proposer_evm_address = Some(proposer);
            let mut verifier_exec = config.create_executor(verifier_evm, verifier_ctx);
            assert!(verifier_exec.execute_transaction(fake_call).is_err());
        } else {
            assert_eq!(staged.len(), 5);
            let mut verifier_state = state_with_active_proposer(proposer);
            let verifier_evm = config.evm_with_env(
                &mut verifier_state,
                test_evm_env(block_number, REWARDS_ADDRESS),
            );
            let mut verifier_ctx = execution_ctx(None, Bytes::new());
            verifier_ctx.execute_outbe_block_hooks = false;
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
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(signer.clone());
    let mut state = state_with_active_proposer(proposer);
    let evm = config.evm_with_env(&mut state, test_evm_env(2, REWARDS_ADDRESS));
    let mut ctx = execution_ctx(None, Bytes::new());
    ctx.execute_outbe_block_hooks = false;
    ctx.proposer_evm_address = Some(proposer);
    let mut executor = config.create_executor(evm, ctx);

    // 1. GemFactory before NodFactory must fail with SystemTxPhaseMismatch.
    let premature_gem = config
        .build_end_system_tx(2, CHAIN_ID, proposer, SystemTx::GemFactory)
        .unwrap();
    let err = executor.execute_transaction(premature_gem).unwrap_err();
    assert!(err.to_string().contains("NodFactory"));

    // 2. NodFactory with non-zero gas_limit must fail with EndBlockGasLimitMismatch.
    let mut bad_unsigned = outbe_primitives::system_tx::build_unsigned_system_tx(
        outbe_primitives::system_tx::NOD_FACTORY_ORDINAL,
        2,
        CHAIN_ID,
        SystemTx::NodFactory.encode().unwrap(),
    )
    .unwrap();
    bad_unsigned.gas_limit = 1;
    let bad_limit_nod = reth_primitives_traits::Recovered::new_unchecked(
        signer.sign_unsigned(bad_unsigned).unwrap(),
        proposer,
    );
    let err = executor.execute_transaction(bad_limit_nod).unwrap_err();
    assert!(err.to_string().contains("NodFactory"));

    // 3. Valid NodFactory transitions cursor to EndBlockGemFactory.
    let valid_nod = config
        .build_end_system_tx(2, CHAIN_ID, proposer, SystemTx::NodFactory)
        .unwrap();
    let nod_gas = executor
        .execute_transaction(valid_nod.clone())
        .unwrap()
        .tx_gas_used();
    assert_eq!(nod_gas, 0);
    assert_eq!(
        executor.system_tx_phase_cursor,
        SystemTxPhase::EndBlockGemFactory
    );

    // 4. Duplicate NodFactory when cursor is EndBlockGemFactory must fail.
    let err = executor.execute_transaction(valid_nod).unwrap_err();
    assert!(err.to_string().contains("GemFactory"));

    // 5. Remaining 4 EndBlock system txs transition cursor to EndBlock, and a duplicate is rejected.
    for (input, expected_cursor) in [
        (SystemTx::GemFactory, SystemTxPhase::EndBlockIntexFactory),
        (SystemTx::IntexFactory, SystemTxPhase::EndBlockCredisFactory),
        (SystemTx::CredisFactory, SystemTxPhase::EndBlockGemCleanup),
        (SystemTx::GemCleanup, SystemTxPhase::EndBlock),
    ] {
        let tx = config
            .build_end_system_tx(2, CHAIN_ID, proposer, input)
            .unwrap();
        let gas = executor
            .execute_transaction(tx.clone())
            .unwrap()
            .tx_gas_used();
        assert_eq!(gas, 0);
        assert_eq!(executor.system_tx_phase_cursor, expected_cursor);
        if matches!(expected_cursor, SystemTxPhase::EndBlock) {
            assert!(executor.execute_transaction(tx).is_err());
        }
    }
}

#[test]
fn end_block_order_executes_interleaved_factory_passes_and_gem_expire_through_before_intex_and_credis(
) {
    use outbe_credis::{CleanupQueue as CredisCleanupQueue, CredisContract};
    use outbe_gemfactory::{schema::GemPosition, GemFactoryContract};
    use outbe_intex::SeriesId;
    use outbe_intexfactory::IntexFactoryContract;
    use outbe_oracle::schema::OracleContract;
    use outbe_primitives::time::{timestamp_to_date_key, SECONDS_PER_DAY};
    use outbe_promislimit::PromisLimitContract;

    outbe_chain_constants::initialize(None).unwrap();
    let signer = test_evm_signer();
    let proposer = signer.address();
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(signer);
    let block_ts = TEST_BLOCK_TIMESTAMP_BASE + 2;
    let expires_at = TEST_BLOCK_TIMESTAMP_BASE - SECONDS_PER_DAY;
    let expires_day = timestamp_to_date_key(expires_at);
    let expired_hour = expires_at / 3_600;
    let expired_capacity = U256::from(2_000_000u64);
    let stale_series = SeriesId::pack(20_240_101, *b"USD", b'U').unwrap();

    struct TestExpiryQueue<'a, 's>(&'a GemFactoryContract<'s>);
    outbe_primitives::impl_deadline_queue!(TestExpiryQueue<u64> {
        root: expiry_tree_root,
        mid: expiry_tree_mid,
        leaf: expiry_tree_leaf,
        len: expiry_day_len,
        live: expiry_day_live,
        at: expiry_day_at,
        slot: expiry_entry_slot,
        deadline: expiry_deadline,
        sweep_day: expiry_sweep_day,
        cursor: expiry_cursor,
    });

    let mut state =
        state_with_active_validators_seeded(&[(proposer, dummy_pubkey(0xA2))], |storage| {
            seed_valid_tee_nodes(storage.clone(), &[proposer]);
            OracleContract::new(storage.clone())
                .utc_day_vwap_last_finalized
                .write(expires_day)
                .unwrap();
            // Seed IntexFactory call notice queue for OSIX (send_call_notices).
            let intex_factory = IntexFactoryContract::new(storage.clone());
            intex_factory.notify_head.write(0).unwrap();
            intex_factory.notify_tail.write(1).unwrap();
            intex_factory
                .notify_at
                .write(
                    &0,
                    outbe_intexfactory::notify::pack_call_notice(stale_series, expires_at),
                )
                .unwrap();
            // Seed CredisFactory cleanup queue in phase 2 for OSCR (cleanup_through).
            let credis = CredisContract::new(storage.clone());
            credis.call_sweep_day.write(expires_day).unwrap();
            credis.call_latest_day.write(expires_day).unwrap();
            credis.daily_phase.write(2).unwrap();
            outbe_primitives::deadline_queue::push(
                &CredisCleanupQueue(&credis),
                stale_series,
                expires_at,
            )
            .unwrap();

            let gf = GemFactoryContract::new(storage);
            let pos = GemPosition {
                position_id: U256::from(1u64),
                merchant: OWNER,
                source_series_id: stale_series,
                remaining_capacity_minor: expired_capacity,
                issued_at: expires_at - SECONDS_PER_DAY,
                expires_at,
            };
            gf.positions.create(&pos).unwrap();
            outbe_primitives::storage::indexed_list::IndexedList::new(
                gf.owner_position_ids.list(&OWNER),
                gf.owner_position_index.clone(),
            )
            .insert(pos.position_id)
            .unwrap();
            outbe_primitives::deadline_queue::push(
                &TestExpiryQueue(&gf),
                expired_hour + 1,
                expires_at,
            )
            .unwrap();
            outbe_primitives::storage::indexed_list::IndexedList::new(
                gf.hour_positions.list(&expired_hour),
                gf.hour_position_index.clone(),
            )
            .insert(pos.position_id)
            .unwrap();
            gf.hour_capacity_minor
                .write(&expired_hour, expired_capacity)
                .unwrap();
        });

    let evm = config.evm_with_env(&mut state, test_evm_env(2, REWARDS_ADDRESS));
    let mut ctx = execution_ctx(None, Bytes::new());
    ctx.execute_outbe_block_hooks = false;
    ctx.proposer_evm_address = Some(proposer);
    let mut executor = config.create_executor(evm, ctx);

    let read_ctx = BlockContext {
        block_gas_limit: 0,
        user_gas_used: 0,
        block_number: 2,
        timestamp: block_ts,
        chain_id: CHAIN_ID,
        genesis_hash: alloy_primitives::B256::ZERO,
        genesis_timestamp: block_ts,
        proposer,
        validators: vec![proposer],
    };

    // 1. OSND (NodFactory) executes first; GemFactory expired hour is not yet swept.
    let nod_tx = config
        .build_end_system_tx(2, CHAIN_ID, proposer, SystemTx::NodFactory)
        .unwrap();
    executor.execute_transaction(nod_tx).unwrap();
    {
        let mut provider = outbe_primitives::storage::direct::DirectStorageProvider::new(
            executor.inner.evm.db_mut(),
            read_ctx.clone(),
        );
        StorageHandle::enter(&mut provider, |storage| {
            let gf = GemFactoryContract::new(storage.clone());
            assert_eq!(
                gf.hour_capacity_minor.read(&expired_hour).unwrap(),
                expired_capacity,
                "after OSND alone, GemFactory expired hour must not yet be swept"
            );
            assert_eq!(
                PromisLimitContract::new(storage).promis_limit().unwrap(),
                U256::ZERO
            );
        });
    }

    // 2. OSGM (GemFactory) executes second and runs phase 2 expire_through BEFORE OSIX and OSCR.
    let gem_tx = config
        .build_end_system_tx(2, CHAIN_ID, proposer, SystemTx::GemFactory)
        .unwrap();
    executor.execute_transaction(gem_tx).unwrap();
    assert_eq!(
        executor.system_tx_phase_cursor,
        SystemTxPhase::EndBlockIntexFactory
    );
    {
        let mut provider = outbe_primitives::storage::direct::DirectStorageProvider::new(
            executor.inner.evm.db_mut(),
            read_ctx.clone(),
        );
        StorageHandle::enter(&mut provider, |storage| {
            let gf = GemFactoryContract::new(storage.clone());
            assert_eq!(
                gf.hour_capacity_minor.read(&expired_hour).unwrap(),
                U256::ZERO,
                "immediately after OSGM (before OSIX and OSCR), GemFactory phase 2 expire_through must release expired hour capacity"
            );
            assert_eq!(
                PromisLimitContract::new(storage).promis_limit().unwrap(),
                expired_capacity,
                "Promis reserve must receive released Gem capacity before OSIX and OSCR run"
            );
            assert!(
                gf.positions.get(U256::from(1u64)).unwrap().is_some(),
                "position deletion is deferred to OSGC (GemCleanup)"
            );
        });
    }

    // 3. OSIX (IntexFactory), OSCR (CredisFactory), and OSGC (GemCleanup) execute in order.
    for input in [
        SystemTx::IntexFactory,
        SystemTx::CredisFactory,
        SystemTx::GemCleanup,
    ] {
        let tx = config
            .build_end_system_tx(2, CHAIN_ID, proposer, input)
            .unwrap();
        executor.execute_transaction(tx).unwrap();
    }
    assert_eq!(executor.system_tx_phase_cursor, SystemTxPhase::EndBlock);
    assert_eq!(executor.receipts().len(), 5);
    assert!(executor
        .receipts()
        .iter()
        .all(|r| r.success && r.cumulative_gas_used == 0));
    {
        let mut provider = outbe_primitives::storage::direct::DirectStorageProvider::new(
            executor.inner.evm.db_mut(),
            read_ctx,
        );
        StorageHandle::enter(&mut provider, |storage| {
            let intex_factory = IntexFactoryContract::new(storage.clone());
            assert_eq!(
                intex_factory.notify_tail.read().unwrap(),
                0,
                "OSIX (IntexFactory) must run send_call_notices and drain queued notices"
            );
            let credis = CredisContract::new(storage.clone());
            assert_eq!(
                credis.call_sweep_day.read().unwrap(),
                0,
                "OSCR (CredisFactory) must run cleanup_through and finish the pinned sweep day"
            );
            assert_eq!(
                credis.cleanup_series_slot.read(&stale_series).unwrap(),
                0,
                "OSCR (CredisFactory) cleanup_through must drop due cleanup queue entry"
            );
            let gf = GemFactoryContract::new(storage);
            assert!(
                gf.positions.get(U256::from(1u64)).unwrap().is_none(),
                "OSGC (GemCleanup) must delete expired positions handed off by OSGM"
            );
        });
    }
}

#[test]
fn end_block_modules_and_detached_trace_observe_exact_mid_block_user_gas_snapshot() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(signer);
    let refund_contract = Address::with_last_byte(0xC9);
    // `PUSH1 0; PUSH1 0; SSTORE; STOP`: clearing non-zero slot 0 grants an SSTORE
    // gas refund (`refunded() > 0`), so `tx_gas_used() < total_gas_spent()`.
    let refund_code =
        Bytecode::new_legacy(Bytes::from_static(&[0x60, 0x00, 0x60, 0x00, 0x55, 0x00]));
    let seed_refund_contract = |state: &mut State<CacheDB<EmptyDBTyped<ProviderError>>>| {
        state.database.insert_account_info(
            refund_contract,
            AccountInfo {
                code_hash: refund_code.hash_slow(),
                code: Some(refund_code.clone()),
                ..Default::default()
            },
        );
        state
            .database
            .insert_account_storage(refund_contract, U256::ZERO, U256::from(1))
            .unwrap();
    };
    let refund_tx = TxEip1559 {
        chain_id: CHAIN_ID,
        nonce: 0,
        gas_limit: 50_000,
        max_fee_per_gas: MIN_PROTOCOL_BASE_FEE as u128,
        max_priority_fee_per_gas: 0,
        to: TxKind::Call(refund_contract),
        value: U256::ZERO,
        input: Bytes::new(),
        access_list: Default::default(),
    }
    .into_signed(Signature::test_signature());
    let refund_tx: Recovered<TransactionSigned> = TransactionSigned::from(refund_tx)
        .try_into_recovered()
        .unwrap();
    let regular_tx = test_regular_tx().try_into_recovered().unwrap();

    for (tx, full_block, expect_refund) in [
        (regular_tx.clone(), false, false),
        (regular_tx, true, false),
        (refund_tx, false, true),
    ] {
        let mut state = state_with_active_proposer_and_funded_account(proposer, tx.signer());
        if expect_refund {
            seed_refund_contract(&mut state);
        }
        let evm = config.evm_with_env(&mut state, test_evm_env(2, REWARDS_ADDRESS));
        let mut ctx = execution_ctx(None, Bytes::new());
        ctx.execute_outbe_block_hooks = false;
        ctx.proposer_evm_address = Some(proposer);
        let mut executor = config.create_executor(evm, ctx);
        let block_gas_limit = executor.inner.evm.block().gas_limit();
        if full_block {
            executor.inner.cumulative_tx_gas_used = block_gas_limit - tx.gas_limit();
        }
        let gas_output = executor.execute_transaction(tx.clone()).unwrap();
        let expected_user_gas = executor.accounted_block_gas();
        if full_block {
            assert_eq!(expected_user_gas, block_gas_limit);
        } else if expect_refund {
            assert_eq!(expected_user_gas, gas_output.tx_gas_used());
        } else {
            assert_eq!(expected_user_gas, 21_000);
        }

        let _ = outbe_evm::executor::take_observed_runtime_block_contexts();
        executor.apply_end_block_phase().unwrap();
        let observed = outbe_evm::executor::take_observed_runtime_block_contexts();
        assert_eq!(
            observed.len(),
            5,
            "all 5 EndBlock modules must build a BlockRuntimeContext"
        );
        for ctx in &observed {
            assert_eq!(ctx.block_gas_limit, block_gas_limit);
            assert_eq!(ctx.user_gas_used, expected_user_gas);
            assert_eq!(
                ctx.remaining_block_gas(),
                block_gas_limit.saturating_sub(expected_user_gas)
            );
        }
        drop(executor);

        // Also verify detached TxTracer replay freezes MidBlock user_gas_used across EndBlock txs.
        let mut trace_state = state_with_active_proposer_and_funded_account(proposer, tx.signer());
        if expect_refund {
            seed_refund_contract(&mut trace_state);
        }
        let parent_hash = B256::with_last_byte(0x52);
        let (preexec_ctx, _) =
            block_two_execution_ctx(Some(6), Bytes::new(), proposer, parent_hash);
        {
            let preexec_evm =
                config.evm_with_env(&mut trace_state, test_evm_env(2, REWARDS_ADDRESS));
            let mut temp_executor = config.create_executor(preexec_evm, preexec_ctx);
            super::with_phase1_verify_disabled(|| {
                temp_executor.apply_pre_execution_changes().unwrap();
            });
        }
        if full_block {
            outbe_evm::executor::witness::record_detached_trace_user_gas(
                block_gas_limit - tx.gas_limit(),
            );
        }
        let trace_evm = config.evm_with_env(&mut trace_state, test_evm_env(2, REWARDS_ADDRESS));
        let mut tracer = alloy_evm::tracing::TxTracer::new(trace_evm);
        let user_tx_env = config.tx_env(&tx);
        let traced_user = tracer.trace(user_tx_env).unwrap();
        assert!(traced_user.result.is_success());
        if expect_refund {
            let traced_gas = traced_user.result.gas();
            assert!(
                traced_gas.tx_gas_used() < traced_gas.total_gas_spent(),
                "post-refund tx_gas_used ({}) must be strictly less than gross total_gas_spent ({})",
                traced_gas.tx_gas_used(),
                traced_gas.total_gas_spent()
            );
            assert_eq!(gas_output.tx_gas_used(), traced_gas.tx_gas_used());
        }
        let _ = outbe_evm::executor::take_observed_runtime_block_contexts();
        for input in [
            SystemTx::NodFactory,
            SystemTx::GemFactory,
            SystemTx::IntexFactory,
            SystemTx::CredisFactory,
            SystemTx::GemCleanup,
        ] {
            let end_tx = config
                .build_end_system_tx(2, CHAIN_ID, proposer, input)
                .unwrap();
            assert!(tracer
                .trace(config.tx_env(&end_tx))
                .unwrap()
                .result
                .is_success());
        }
        let traced_observed = outbe_evm::executor::take_observed_runtime_block_contexts();
        assert_eq!(traced_observed.len(), 5);
        for ctx in &traced_observed {
            assert_eq!(ctx.block_gas_limit, block_gas_limit);
            assert_eq!(ctx.user_gas_used, expected_user_gas);
            assert_eq!(
                ctx.remaining_block_gas(),
                block_gas_limit.saturating_sub(expected_user_gas)
            );
        }
        outbe_evm::executor::witness::clear_detached_trace_witness();
    }
}
