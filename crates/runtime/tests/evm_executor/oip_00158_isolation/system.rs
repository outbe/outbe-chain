use super::*;
use outbe_evm::executor::witness::{
    match_detached_trace_tx, DetachedTraceTxAction, EvmTransactWitnessInput,
};
use outbe_primitives::projection::NodeLocalExecutionAbort;
use outbe_tee::witness::{
    encode_witness_payload_v1, EnclaveWitnessEntryV1, EnclaveWitnessTapeV1, TxWitnessV1,
    WitnessExecutionMode, WitnessPolicy,
};

#[test]
fn system_panic_criticality_preserves_state_and_soft_steps_advance() {
    for mode in [Failure::HardBegin, Failure::SoftBegin, Failure::EndPanic] {
        let config = config(mode);
        let mut state = seeded(|_| {});
        let (ctx, begin) = context(&config);
        let prefix_len = begin.len();
        let evm = config.evm_with_env(&mut state, test_evm_env(BLOCK, REWARDS_ADDRESS));
        let mut executor = config.create_executor(evm, ctx);
        with_phase1_verify_disabled(|| executor.apply_pre_execution_changes()).unwrap();
        for tx in begin {
            let target = matches!(
                SystemTx::decode(tx.tx().input()).unwrap(),
                SystemTx::BeginBlock { step_index: 0 }
            );
            let before = executor.receipts().len();
            let fsm = executor.fsm.clone();
            let outcome = executor.execute_transaction(tx);
            if target && matches!(mode, Failure::HardBegin) {
                let error = outcome.expect_err("hard step must reject block");
                assert!(error.to_string().contains("299"), "{error}");
                assert_eq!(executor.receipts().len(), before);
                assert_eq!(executor.fsm, fsm);
                assert_eq!(executor.inner.cumulative_tx_gas_used, 0);
                break;
            }
            outcome.unwrap();
            if target && matches!(mode, Failure::SoftBegin) {
                failure(&executor.receipts()[before], 299);
                assert_ne!(executor.fsm, fsm);
            }
        }
        if !matches!(mode, Failure::HardBegin) {
            executor
                .execute_transaction(signed(
                    0,
                    TxKind::Call(Address::repeat_byte(0x94)),
                    b"",
                    21_000,
                ))
                .unwrap();
            assert!(executor.receipts().last().unwrap().success);
            executor.apply_end_block_phase().unwrap();
            if matches!(mode, Failure::EndPanic) {
                let receipt = &executor.receipts()[prefix_len + 1];
                assert!(!receipt.success);
                assert_eq!(receipt.cumulative_gas_used, 21_000);
                let mut zero_gas = receipt.clone();
                zero_gas.cumulative_gas_used -= 21_000;
                failure(&zero_gas, 299);
                assert!(executor.receipts()[prefix_len + 2..]
                    .iter()
                    .all(|r| r.success));
            }
            assert_eq!(
                executor.fsm.stage(),
                outbe_primitives::system_tx::BlockExecutionStage::Completed
            );
            executor.prepare_final_header_artifacts(0).unwrap();
            let (evm, _) = executor.finish().unwrap();
            drop(evm);
        } else {
            drop(executor);
        }
        assert_eq!(state.storage(PROBE, U256::ZERO).unwrap(), U256::ZERO);
    }
}

#[test]
fn local_transport_panics_abort_block_in_both_enclave_roles_without_failure_receipt() {
    for verifier in [false, true] {
        outbe_tee::witness::set_witness_policy_for_test(Some(WitnessPolicy::EnclaveBacked));
        let config = config(Failure::None);
        let mut state = seeded(|_| {});
        let (mut ctx, begin) = context(&config);
        if verifier {
            let entry =
                EnclaveWitnessEntryV1::try_from(outbe_tee::protocol::EnclaveResponse::Payload {
                    recordable: true,
                    payload: vec![1],
                    inputs_canonical_hash: B256::ZERO,
                    attestation_tag: vec![0; 64],
                })
                .unwrap();
            let tape = EnclaveWitnessTapeV1 {
                prefix: vec![],
                txs: vec![TxWitnessV1 {
                    tx_index: begin.len() as u32,
                    entries: vec![entry],
                }],
            };
            ctx.inner.extra_data = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
                enclave_witness: Some(encode_witness_payload_v1(&tape).unwrap()),
                ..Default::default()
            })
            .unwrap();
            ctx.expected_begin_system_txs = begin.clone();
            ctx.block_hash = Some(B256::repeat_byte(0x95));
        }
        let evm = config.evm_with_env(&mut state, test_evm_env(BLOCK, REWARDS_ADDRESS));
        let mut executor = config.create_executor(evm, ctx);
        with_phase1_verify_disabled(|| executor.apply_pre_execution_changes()).unwrap();
        let witness = executor.witness_state.as_ref().unwrap().clone();
        assert_eq!(
            witness.lock().unwrap().mode,
            if verifier {
                WitnessExecutionMode::VerifierWithEnclave
            } else {
                WitnessExecutionMode::ProposerRecord
            }
        );
        for tx in begin {
            executor.execute_transaction(tx).unwrap();
        }
        let before = executor.receipts().len();
        let error = executor
            .execute_transaction(signed(0, TxKind::Call(PROBE), b"transport", 150_000))
            .expect_err("local failure must abort, not commit a failed user receipt");
        assert!(NodeLocalExecutionAbort::find(&error).is_some(), "{error}");
        assert_eq!(executor.receipts().len(), before);
        assert_eq!(
            witness.lock().unwrap().first_witness_fault,
            Some(outbe_tee::witness::BlockWitnessFault::LocalUnavailable)
        );
        drop(executor);
        outbe_tee::witness::set_witness_policy_for_test(None);
    }
}

struct ProposedEndOogBlock {
    begin: Vec<Recovered<TransactionSigned>>,
    ends: Vec<Recovered<TransactionSigned>>,
    receipts: Vec<Receipt>,
    final_extra_data: Bytes,
    state_root: B256,
}

fn propose_end_oog_block(config: &OutbeEvmConfig) -> ProposedEndOogBlock {
    let mut state = seeded(|_| {});
    let (ctx, begin) = context(config);
    let prefix_len = begin.len();
    let evm = config.evm_with_env(&mut state, test_evm_env(BLOCK, REWARDS_ADDRESS));
    let mut executor = config.create_executor(evm, ctx);
    assert!(!executor.is_verifier_witness_mode());
    with_phase1_verify_disabled(|| executor.apply_pre_execution_changes()).unwrap();
    for tx in begin.clone() {
        executor.execute_transaction(tx).unwrap();
    }
    executor.apply_end_block_phase().unwrap();
    failure(&executor.receipts()[prefix_len], 202);
    assert!(executor.receipts()[prefix_len + 1..]
        .iter()
        .all(|r| r.success));
    let ends = executor.take_staged_end_block_txs();
    assert_eq!(ends.len(), 5);
    assert!(ends.iter().all(|tx| tx.tx().gas_limit() == 0));
    executor.prepare_final_header_artifacts(0).unwrap();
    let final_extra_data = executor.final_extra_data.clone();
    let (evm, result) = executor.finish().unwrap();
    drop(evm);
    assert_eq!(result.gas_used, 0);
    for slot in 0..32 {
        assert_eq!(state.storage(PROBE, U256::from(slot)).unwrap(), U256::ZERO);
    }
    state.merge_transitions(BundleRetention::Reverts);
    let state_root = post_state_root(&state.bundle_state);
    ProposedEndOogBlock {
        begin,
        ends,
        receipts: result.receipts,
        final_extra_data,
        state_root,
    }
}

fn validate_end_oog_block(proposed: &ProposedEndOogBlock) {
    let validator_config =
        OutbeEvmConfig::new(test_chain_spec(), IsolationHost::new(Failure::EndOog));
    let mut state = seeded(|_| {});
    let proposer = test_evm_signer().address();
    let tx_count = proposed.begin.len() + proposed.ends.len();
    let (mut ctx, _) = block_two_execution_ctx(
        Some(tx_count),
        proposed.final_extra_data.clone(),
        proposer,
        PARENT,
    );
    ctx.expected_begin_system_txs = proposed.begin.clone();
    ctx.block_hash = Some(B256::repeat_byte(0x97));
    let evm = validator_config.evm_with_env(&mut state, test_evm_env(BLOCK, REWARDS_ADDRESS));
    let mut validator = validator_config.create_executor(evm, ctx);
    assert!(validator.is_verifier_witness_mode());
    with_phase1_verify_disabled(|| validator.apply_pre_execution_changes()).unwrap();
    for tx in proposed.begin.iter().chain(proposed.ends.iter()).cloned() {
        validator.execute_transaction(tx).unwrap();
    }
    let (evm, result) = validator.finish().unwrap();
    drop(evm);
    failure(&result.receipts[proposed.begin.len()], 202);
    assert_eq!(result.receipts, proposed.receipts);
    assert_eq!(result.gas_used, 0);
    for slot in 0..32 {
        assert_eq!(state.storage(PROBE, U256::from(slot)).unwrap(), U256::ZERO);
    }
    state.merge_transitions(BundleRetention::Reverts);
    assert_eq!(post_state_root(&state.bundle_state), proposed.state_root);
}

#[test]
fn tight_step_gas_rolls_back_and_detached_replay_uses_reserved_system_call() {
    let config = config(Failure::EndOog);
    let proposed = propose_end_oog_block(&config);
    validate_end_oog_block(&proposed);

    let mut replay = seeded(|_| {});
    trace_state(
        &config,
        &mut replay,
        proposed.begin.len() + proposed.ends.len(),
    );
    let mut tracer =
        TxTracer::new(config.evm_with_env(&mut replay, test_evm_env(BLOCK, REWARDS_ADDRESS)));
    for tx in proposed.begin {
        assert!(tracer
            .trace(config.tx_env(&tx))
            .unwrap()
            .result
            .is_success());
    }
    let tx_env = config.tx_env(&proposed.ends[0]);
    let matched = match_detached_trace_tx(
        &EvmTransactWitnessInput {
            attached_state: None,
            owned_by_executor: false,
            precompiles_mut_called: false,
            chain_id: CHAIN_ID,
            block_number: BLOCK,
            block_timestamp: TEST_BLOCK_TIMESTAMP_BASE + BLOCK,
        },
        &tx_env,
    )
    .expect("historical system transaction must match detached replay");
    assert!(matches!(
        matched.action,
        DetachedTraceTxAction::ReservedSystemCall { .. }
    ));
    let traced = tracer.trace(tx_env).unwrap();
    assert!(matches!(
        traced.result,
        ExecutionResult::Halt {
            reason: HaltReason::OutOfGas(_),
            ..
        }
    ));
    assert_eq!(traced.result.tx_gas_used(), 60_000);
    assert!(traced.result.logs().is_empty());
    for tx in proposed.ends.into_iter().skip(1) {
        assert!(tracer
            .trace(config.tx_env(&tx))
            .unwrap()
            .result
            .is_success());
    }
    drop(tracer);
    for slot in 0..32 {
        assert_eq!(replay.storage(PROBE, U256::from(slot)).unwrap(), U256::ZERO);
    }
    clear_detached_trace_witness();
}
