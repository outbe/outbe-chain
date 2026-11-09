use super::*;
use outbe_primitives::system_tx::SystemTxPhase;

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
fn end_block_soft_fail_rolls_back_dirty_storage_writes_and_logs_preserving_earlier_state() {
    outbe_chain_constants::initialize(None).unwrap();
    let signer = test_evm_signer();
    let proposer = signer.address();
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(signer);
    let preserved_addr = outbe_primitives::addresses::NOD_FACTORY_ADDRESS;
    let preserved_slot = U256::from(0x1111u64);
    let preserved_val = U256::from(0x2222u64);
    let mut state =
        state_with_active_validators_seeded(&[(proposer, dummy_pubkey(0xA2))], |storage| {
            seed_valid_tee_nodes(storage.clone(), &[proposer]);
            storage
                .sstore(preserved_addr, preserved_slot, preserved_val)
                .unwrap();
        });

    let evm = config.evm_with_env(&mut state, test_evm_env(2, REWARDS_ADDRESS));
    let mut ctx = execution_ctx(None, Bytes::new());
    ctx.execute_outbe_block_hooks = false;
    ctx.proposer_evm_address = Some(proposer);
    let mut executor = config.create_executor(evm, ctx);

    // 1. OSND succeeds.
    let nod_tx = config
        .build_end_system_tx(2, CHAIN_ID, proposer, SystemTx::NodFactory)
        .unwrap();
    executor.execute_transaction(nod_tx).unwrap();

    // 2. OSGM writes dirty storage + emits dirty log, then reverts (soft-fail).
    let gem_tx = config
        .build_end_system_tx(2, CHAIN_ID, proposer, SystemTx::GemFactory)
        .unwrap();
    let dirty_addr = outbe_primitives::addresses::GEM_FACTORY_ADDRESS;
    let dirty_slot = U256::from(0xDEAD_BEEFu64);
    let dirty_val = U256::from(0xCAFE_BABEu64);
    let dirty_topic = keccak256("DirtySoftFailEvent()");

    let gas = outbe_evm::sys_tx_precompile::with_forced_dirty_system_precompile_revert(
        dirty_addr,
        dirty_slot,
        dirty_val,
        || executor.execute_transaction(gem_tx),
    )
    .expect("soft-fail OSGM revert must emit status=false receipt and continue")
    .tx_gas_used();
    assert_eq!(gas, 0);

    let osgm_receipt = executor.receipts().last().expect("OSGM receipt");
    assert!(!osgm_receipt.success);
    assert_eq!(osgm_receipt.cumulative_gas_used, 0);
    assert_eq!(osgm_receipt.logs.len(), 1);
    assert!(
        osgm_receipt
            .logs
            .iter()
            .all(|log| log.data.topics().first() != Some(&dirty_topic)),
        "dirty event log emitted inside reverted OSGM must be rolled back"
    );

    // 3. Remaining EndBlock system txs (OSIX, OSCR, OSGC) continue and succeed.
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
    assert!(executor.receipts()[0].success);
    assert!(!executor.receipts()[1].success);
    assert!(executor.receipts()[2..].iter().all(|r| r.success));
    drop(executor);

    assert_eq!(
        state.storage(preserved_addr, preserved_slot).unwrap(),
        preserved_val,
        "earlier committed state must be preserved across soft-fail EndBlock revert"
    );
    assert_eq!(
        state.storage(dirty_addr, dirty_slot).unwrap(),
        U256::ZERO,
        "dirty storage write inside reverted OSGM must be rolled back"
    );
}

#[test]
fn end_block_out_of_gas_emits_zero_gas_failure_receipts_and_continues_block() {
    outbe_chain_constants::initialize(None).unwrap();
    let signer = test_evm_signer();
    let proposer = signer.address();
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(signer);
    let mut state =
        state_with_active_validators_seeded(&[(proposer, dummy_pubkey(0xA2))], |storage| {
            seed_valid_tee_nodes(storage, &[proposer]);
        });

    let evm = config.evm_with_env(&mut state, test_evm_env(2, REWARDS_ADDRESS));
    let mut ctx = execution_ctx(None, Bytes::new());
    ctx.execute_outbe_block_hooks = false;
    ctx.proposer_evm_address = Some(proposer);
    let mut executor = config.create_executor(evm, ctx);

    let user_gas_before = 42_000u64;
    executor.inner.cumulative_tx_gas_used = user_gas_before;
    executor.inner.block_regular_gas_used = user_gas_before;

    for input in [
        SystemTx::NodFactory,
        SystemTx::GemFactory,
        SystemTx::IntexFactory,
        SystemTx::CredisFactory,
        SystemTx::GemCleanup,
    ] {
        let tx = config
            .build_end_system_tx(2, CHAIN_ID, proposer, input)
            .unwrap();
        let gas = outbe_evm::factory::with_forced_outbe_system_call_oog_halt(|| {
            executor.execute_transaction(tx)
        })
        .expect("EndBlock OutOfGas must be soft-fail and continue block execution")
        .tx_gas_used();
        assert_eq!(gas, 0);
    }

    assert_eq!(executor.system_tx_phase_cursor, SystemTxPhase::EndBlock);
    assert_eq!(executor.receipts().len(), 5);
    for receipt in executor.receipts() {
        assert!(
            !receipt.success,
            "EndBlock OOG must emit status=false receipt"
        );
        assert_eq!(
            receipt.cumulative_gas_used, user_gas_before,
            "EndBlock OOG must contribute 0 visible gas"
        );
        assert_eq!(receipt.logs.len(), 1);
    }
    assert_eq!(executor.inner.cumulative_tx_gas_used, user_gas_before);

    executor
        .prepare_final_header_artifacts(0)
        .expect("header artifact preparation should succeed after EndBlock OOG");
    let (_, result) = executor
        .finish()
        .expect("block must finish cleanly after EndBlock OOG");
    assert_eq!(result.gas_used, user_gas_before);
}
