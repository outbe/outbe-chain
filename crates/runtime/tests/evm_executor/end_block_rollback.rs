use super::*;
use outbe_primitives::system_tx::SystemTxPhase;

#[test]
fn begin_block_system_tx_rejection_preserves_logs_and_begin_receipt_accounting() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let parent = B256::with_last_byte(0xA5);
    let metadata = CertifiedParentAccountingMetadata {
        finalized_block_number: 1,
        finalized_block_hash: parent,
        ..Default::default()
    };
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(signer);
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
    let cycle_tx = begin.last().unwrap().clone();
    let ordinal = u8::try_from(begin.len() - 1).unwrap();
    let mut state = state_with_active_proposer(proposer);
    let evm = config.evm_with_env(&mut state, test_evm_env(2, REWARDS_ADDRESS));
    let mut ctx = execution_ctx(None, Bytes::new());
    ctx.inner.parent_hash = parent;
    ctx.proposer_evm_address = Some(proposer);
    ctx.parent_consensus_metadata = Some(metadata);
    ctx.expected_begin_system_txs = begin;
    let mut executor = config.create_executor(evm, ctx);
    executor.system_tx_phase_cursor = SystemTxPhase::CycleTick {
        body_index: ordinal,
    };
    let block_gas_limit = executor.inner.evm.block().gas_limit();
    executor.inner.cumulative_tx_gas_used = block_gas_limit;
    assert!(
        outbe_evm::factory::with_forced_outbe_system_call_revert(|| {
            executor.execute_transaction(cycle_tx)
        })
        .is_err()
    );
    assert!(executor.receipts().is_empty());
    assert_eq!(
        executor.system_tx_phase_cursor,
        SystemTxPhase::CycleTick {
            body_index: ordinal
        }
    );
    assert_eq!(executor.inner.cumulative_tx_gas_used, block_gas_limit);
    assert_eq!(executor.system_tx_execution_gas, 0);
}

#[test]
fn end_block_soft_failure_receipt_preserves_cleanup_budget_and_continues_block() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(signer);
    let mut state = state_with_active_proposer(proposer);
    let evm = config.evm_with_env(&mut state, test_evm_env(2, REWARDS_ADDRESS));
    let mut ctx = execution_ctx(None, Bytes::new());
    ctx.execute_outbe_block_hooks = false;
    ctx.proposer_evm_address = Some(proposer);
    let mut executor = config.create_executor(evm, ctx);

    // Force a revert on all 5 END_BLOCK_ORDER kinds in turn (0 visible gas, emits a
    // soft-failure receipt with status = false, advances cursor, and continues block).
    for (idx, (input, expected_cursor)) in [
        (SystemTx::NodFactory, SystemTxPhase::EndBlockGemFactory),
        (SystemTx::GemFactory, SystemTxPhase::EndBlockIntexFactory),
        (SystemTx::IntexFactory, SystemTxPhase::EndBlockCredisFactory),
        (SystemTx::CredisFactory, SystemTxPhase::EndBlockGemCleanup),
        (SystemTx::GemCleanup, SystemTxPhase::EndBlock),
    ]
    .into_iter()
    .enumerate()
    {
        let tx = config
            .build_end_system_tx(2, CHAIN_ID, proposer, input)
            .unwrap();
        let gas = outbe_evm::factory::with_forced_outbe_system_call_revert(|| {
            executor.execute_transaction(tx)
        })
        .unwrap()
        .tx_gas_used();
        assert_eq!(gas, 0);
        assert_eq!(executor.receipts().len(), idx + 1);
        assert!(!executor.receipts()[idx].success);
        assert_eq!(executor.receipts()[idx].cumulative_gas_used, 0);
        assert_eq!(executor.system_tx_phase_cursor, expected_cursor);
    }
    assert_eq!(executor.receipts().len(), 5);
    assert!(executor.receipts().iter().all(|r| !r.success));
    assert_eq!(executor.system_tx_phase_cursor, SystemTxPhase::EndBlock);
    executor.prepare_final_header_artifacts(0).unwrap();
    let (_, result) = executor.finish().unwrap();
    assert_eq!(result.receipts.len(), 5);
}

#[test]
fn all_soft_fail_kinds_abort_block_on_fatal_system_call_error() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(signer);

    // 1. BeginBlock soft-fail kinds (OracleSlash, HyperlaneSlash) abort block on Fatal/system error.
    for target_kind in [
        outbe_primitives::system_tx::SystemTxKind::OracleSlash,
        outbe_primitives::system_tx::SystemTxKind::HyperlaneSlash,
    ] {
        let mut state = state_with_active_proposer_and_funded_account(proposer, Address::ZERO);
        let parent_hash = B256::with_last_byte(0x41);
        let (ctx, metadata) = block_two_execution_ctx(Some(1), Bytes::new(), proposer, parent_hash);
        let evm = config.evm_with_env(&mut state, test_evm_env(2, REWARDS_ADDRESS));
        let mut executor = config.create_executor(evm, ctx);
        super::with_phase1_verify_disabled(|| {
            executor.apply_pre_execution_changes().unwrap();
        });
        for tx in begin_system_txs_for_test(
            &config,
            BeginBlockFixture {
                block_number: 2,
                parent_hash,
                extra_data: &Bytes::new(),
                parent_consensus_metadata: Some(metadata),
                proposer,
                bootstrap: BootstrapFixture::StandardForBlock,
            },
        ) {
            let decoded = SystemTx::decode(tx.tx().input().as_ref()).unwrap();
            if decoded.kind() == target_kind {
                let receipts_before = executor.receipts().len();
                let res = outbe_evm::factory::with_forced_outbe_system_call_error(|| {
                    executor.execute_transaction(tx)
                });
                assert!(
                    res.is_err(),
                    "fatal system call error on {target_kind:?} must abort block execution"
                );
                assert_eq!(
                    executor.receipts().len(),
                    receipts_before,
                    "fatal system call error must not append a soft-failure receipt"
                );
                break;
            }
            executor.execute_transaction(tx).unwrap();
        }
    }

    // 2. EndBlock soft-fail kinds (OSND..OSGC) abort block on Fatal/system error.
    let end_inputs = [
        SystemTx::NodFactory,
        SystemTx::GemFactory,
        SystemTx::IntexFactory,
        SystemTx::CredisFactory,
        SystemTx::GemCleanup,
    ];
    for target_idx in 0..end_inputs.len() {
        let mut state = state_with_active_proposer(proposer);
        let evm = config.evm_with_env(&mut state, test_evm_env(2, REWARDS_ADDRESS));
        let mut ctx = execution_ctx(None, Bytes::new());
        ctx.execute_outbe_block_hooks = false;
        ctx.proposer_evm_address = Some(proposer);
        let mut executor = config.create_executor(evm, ctx);
        for (idx, input) in end_inputs.iter().cloned().enumerate() {
            let tx = config
                .build_end_system_tx(2, CHAIN_ID, proposer, input)
                .unwrap();
            if idx == target_idx {
                let receipts_before = executor.receipts().len();
                let res = outbe_evm::factory::with_forced_outbe_system_call_error(|| {
                    executor.execute_transaction(tx)
                });
                assert!(
                    res.is_err(),
                    "fatal error at EndBlock index {idx} must abort"
                );
                assert_eq!(executor.receipts().len(), receipts_before);
                break;
            }
            executor.execute_transaction(tx).unwrap();
        }
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

    let gas = outbe_evm::executor::with_forced_dirty_system_precompile_revert(
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
