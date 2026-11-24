use super::*;

#[test]
fn user_panic_rolls_back_burns_full_gas_and_detached_replay_matches() {
    let config = config(Failure::None);
    let first = signed(0, TxKind::Call(PROBE), b"panic", 150_000);
    let second = signed(1, TxKind::Call(PROBE), b"ok", 100_000);
    let mut state = seeded(|_| {});
    let (ctx, begin) = context(&config);
    let prefix_len = begin.len();
    let evm = config.evm_with_env(&mut state, test_evm_env(BLOCK, REWARDS_ADDRESS));
    let mut executor = config.create_executor(evm, ctx);
    with_phase1_verify_disabled(|| executor.apply_pre_execution_changes()).unwrap();
    for tx in begin.clone() {
        executor.execute_transaction(tx).unwrap();
    }
    let first_gas = executor
        .execute_transaction(first.clone())
        .unwrap()
        .tx_gas_used();
    assert_eq!(first_gas, 150_000);
    let receipt = &executor.receipts()[prefix_len];
    assert!(!receipt.success);
    assert!(receipt.logs.is_empty());
    assert_eq!(receipt.cumulative_gas_used, 150_000);
    assert!(executor.evm().state().journal().is_empty());
    let second_gas = executor
        .execute_transaction(second.clone())
        .unwrap()
        .tx_gas_used();
    assert!(executor.receipts()[prefix_len + 1].success);
    assert_eq!(
        executor.receipts()[prefix_len + 1].cumulative_gas_used,
        150_000 + second_gas
    );
    executor.apply_end_block_phase().unwrap();
    finalize_executor_header_artifacts(&mut executor, 0).unwrap();
    let output = executor.finish().unwrap();
    state.commit_source(&BundleSource(&output.state));
    assert_eq!(output.result.receipts.len(), prefix_len + 2 + 5);
    assert_eq!(storage_slot(&mut state, PROBE, U256::ZERO), U256::ZERO);
    assert_eq!(storage_slot(&mut state, PROBE, U256::ONE), U256::ONE);

    let mut replay = seeded(|_| {});
    trace_state(&config, &mut replay, prefix_len + 2);
    let mut trace_evm = config.evm_with_env(&mut replay, test_evm_env(BLOCK, REWARDS_ADDRESS));
    let mut inspector = evm2::inspector::NoopInspector::default();
    for tx in begin {
        assert!(trace_evm
            .transact_with_inspector(&tx_env(&config, &tx), &mut inspector)
            .unwrap()
            .commit()
            .is_success());
    }
    let traced = trace_evm
        .transact_with_inspector(&tx_env(&config, &first), &mut inspector)
        .unwrap()
        .commit();
    assert!(
        matches!(traced.stop, evm2::interpreter::InstrStop::PrecompileError),
        "expected PrecompileError halt, got {:?}",
        traced.stop
    );
    assert_eq!(traced.tx_gas_used(), first_gas);
    assert!(traced.logs.is_empty());
    let traced = trace_evm
        .transact_with_inspector(&tx_env(&config, &second), &mut inspector)
        .unwrap()
        .commit();
    assert!(traced.is_success());
    assert_eq!(traced.tx_gas_used(), second_gas);
    assert_eq!(
        storage_slot(trace_evm.state_mut().overlay_db_mut(), PROBE, U256::ZERO),
        U256::ZERO
    );
    assert_eq!(
        storage_slot(trace_evm.state_mut().overlay_db_mut(), PROBE, U256::ONE),
        U256::ONE
    );
    drop(trace_evm);
    clear_detached_trace_witness();
}

#[test]
fn bytecode_catches_panicking_child_and_commits_recovery_marker() {
    let config = config(Failure::None);
    let mut state = seeded(|_| {});
    // MSTORE("panic"), CALL(PROBE, 50_000), store CALL's zero return, then slot[0] = 1.
    let mut code = vec![
        0x64, b'p', b'a', b'n', b'i', b'c', 0x60, 0xd8, 0x1b, 0x5f, 0x52, 0x5f, 0x5f, 0x60, 0x05,
        0x5f, 0x5f, 0x73,
    ];
    code.extend_from_slice(PROBE.as_slice());
    code.extend_from_slice(&[
        0x61, 0xc3, 0x50, 0xf1, 0x60, 0x01, 0x55, 0x5f, 0x5f, 0x5f, 0x5f, 0x5f, 0x73,
    ]);
    code.extend_from_slice(PROBE.as_slice());
    code.extend_from_slice(&[
        0x61, 0xc3, 0x50, 0xf1, 0x60, 0x02, 0x55, 0x60, 0x01, 0x5f, 0x55, 0x00,
    ]);
    let length = u16::try_from(code.len()).unwrap().to_be_bytes();
    let mut init = vec![
        0x61, length[0], length[1], 0x60, 0x0c, 0x5f, 0x39, 0x61, length[0], length[1], 0x5f, 0xf3,
    ];
    init.extend_from_slice(&code);
    let catcher = test_evm_signer().address().create(0);
    let (ctx, begin) = context(&config);
    let prefix_len = begin.len();
    let evm = config.evm_with_env(&mut state, test_evm_env(BLOCK, REWARDS_ADDRESS));
    let mut executor = config.create_executor(evm, ctx);
    with_phase1_verify_disabled(|| executor.apply_pre_execution_changes()).unwrap();
    for tx in begin {
        executor.execute_transaction(tx).unwrap();
    }
    executor
        .execute_transaction(signed(0, TxKind::Create, &init, 100_000))
        .unwrap();
    assert!(executor.receipts()[prefix_len].success);
    executor
        .execute_transaction(signed(1, TxKind::Call(catcher), b"", 150_000))
        .unwrap();
    assert!(executor.receipts()[prefix_len + 1].success);
    assert!(executor.receipts()[prefix_len + 1].logs.is_empty());
    assert!(executor.evm().state().journal().is_empty());

    executor.apply_end_block_phase().unwrap();
    finalize_executor_header_artifacts(&mut executor, 0).unwrap();
    let output = executor.finish().unwrap();
    state.commit_source(&BundleSource(&output.state));
    assert_eq!(storage_slot(&mut state, catcher, U256::ZERO), U256::ONE);
    assert_eq!(storage_slot(&mut state, catcher, U256::ONE), U256::ZERO);
    assert_eq!(storage_slot(&mut state, catcher, U256::from(2)), U256::ONE);
    assert_eq!(storage_slot(&mut state, PROBE, U256::ZERO), U256::ZERO);
}
