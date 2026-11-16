use super::*;

use super::receipts_and_gas_support::*;

#[test]
fn priority_fees_credit_rewards_escrow_in_production_fee_path() {
    let chain_spec = test_chain_spec();
    let receipt_builder = reth_ethereum::evm::RethReceiptBuilder::default();
    let config = OutbeEvmConfig::new(
        chain_spec.clone(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    );
    let tx = test_priority_fee_tx();
    let recovered = tx
        .clone()
        .try_into_recovered()
        .expect("priority-fee tx signer should recover");

    let mut db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
    db.insert_account_info(
        recovered.signer(),
        AccountInfo {
            balance: U256::from(1_000_000u64),
            ..Default::default()
        },
    );

    let mut state = State::builder()
        .with_database(db)
        .with_bundle_update()
        .build();
    let evm_env = EvmEnv {
        cfg_env: CfgEnv::new()
            .with_chain_id(CHAIN_ID)
            .with_spec_and_mainnet_gas_params(SpecId::SHANGHAI),
        block_env: BlockEnv {
            number: U256::from(1u64),
            gas_limit: 30_000_000,
            basefee: MIN_PROTOCOL_BASE_FEE,
            beneficiary: REWARDS_ADDRESS,
            timestamp: U256::from(1u64),
            ..Default::default()
        },
    };
    let evm = config.evm_with_env(&mut state, evm_env);
    let ctx = execution_ctx(Some(1), Bytes::new());
    let mut executor = OutbeBlockExecutor::new(
        EthBlockExecutor::new(evm, ctx.inner.clone(), &chain_spec, &receipt_builder),
        fixtures::executor_inputs_from_ctx(&ctx, None, false),
    );

    executor
        .execute_transaction(recovered)
        .expect("priority-fee tx should execute");

    assert_eq!(executor.receipts()[0].cumulative_gas_used, 21_000);
    let expected_fee = U256::from(21_000u64) * U256::from(MIN_PROTOCOL_BASE_FEE);
    assert_eq!(
        executor.current_execution_summary().validator_fee_sum,
        expected_fee
    );

    drop(executor);

    let rewards_balance = state
        .basic(REWARDS_ADDRESS)
        .expect("rewards escrow read should succeed")
        .map(|account| account.balance)
        .unwrap_or_default();
    assert_eq!(rewards_balance, expected_fee);
}

#[test]
fn apply_pre_execution_changes_executes_cycle_tick_system_tx_receipt() {
    let signer = test_evm_signer();
    let proposer = signer.address();

    let mut state = state_with_active_proposer(proposer);
    let evm_env = test_evm_env(1, REWARDS_ADDRESS);
    let chain_spec = test_chain_spec();
    let receipt_builder = reth_ethereum::evm::RethReceiptBuilder::default();
    let config = OutbeEvmConfig::new(
        chain_spec.clone(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(signer.clone());
    let evm = config.evm_with_env(&mut state, evm_env);
    let ctx = block_one_execution_ctx(Some(0), Bytes::new());
    let mut executor = OutbeBlockExecutor::new(
        EthBlockExecutor::new(evm, ctx.inner.clone(), &chain_spec, &receipt_builder),
        fixtures::executor_inputs_with_bootstrap(&ctx, Some(signer.clone()), false),
    );

    executor
        .apply_pre_execution_changes()
        .expect("block 1 pre-execution changes should apply");
    let system_txs = begin_system_txs_for_test(
        &config,
        BeginBlockFixture {
            block_number: 1,
            parent_hash: B256::ZERO,
            extra_data: &Bytes::new(),
            parent_consensus_metadata: None,
            proposer,
            bootstrap: BootstrapFixture::StandardForBlock,
        },
    );
    let mut visible_system_gas_used = 0u64;
    for tx in system_txs.clone() {
        let signed_gas_limit = tx.tx().gas_limit();
        assert_eq!(signed_gas_limit, 0);
        let gas_used = executor
            .execute_transaction(tx)
            .expect("begin-zone system tx should execute in tx loop");
        assert_eq!(gas_used.tx_gas_used(), 0);
        visible_system_gas_used += gas_used.tx_gas_used();
        assert_eq!(
            executor
                .receipts()
                .last()
                .expect("system tx receipt must be present")
                .cumulative_gas_used,
            visible_system_gas_used
        );
    }

    assert_eq!(executor.receipts().len(), 12);
    assert!(executor.receipts().iter().all(|receipt| receipt.success));
    assert!(
        executor.system_tx_execution_gas > 0,
        "system tx internal execution gas must still be measured"
    );
    assert_eq!(
        executor.inner.cumulative_tx_gas_used, 0,
        "system tx must charge 0 gas to block accounting under Tempo model"
    );
    assert_eq!(
        executor.inner.block_regular_gas_used, 0,
        "system tx regular gas must be 0 under Tempo model"
    );
    assert!(executor
        .receipts()
        .iter()
        .all(|receipt| receipt.tx_type == reth_ethereum::TxType::Legacy));
    assert_eq!(executor.receipts()[11].cumulative_gas_used, 0);

    assert_eq!(system_txs.len(), 12);
    assert_eq!(Address::from(*system_txs[0].signer()), proposer);
    assert_eq!(system_txs[0].tx().chain_id(), Some(CHAIN_ID));
    assert_eq!(system_txs[0].tx().tx_type(), reth_ethereum::TxType::Legacy);
    let mut encoded = Vec::new();
    system_txs[0].tx().encode_2718(&mut encoded);
    assert!(
        encoded.first().is_some_and(|byte| *byte >= 0xc0),
        "legacy transaction body must RLP-encode as a list, not a typed envelope"
    );
    assert!(matches!(
        SystemTx::decode(system_txs[1].tx().input().as_ref()).unwrap(),
        SystemTx::Consensus {
            step_index: outbe_primitives::system_tx::CONSENSUS_TEE_BOOTSTRAP_STEP_INDEX,
            ..
        }
    ));
    assert!(matches!(
        SystemTx::decode(system_txs[2].tx().input().as_ref()).unwrap(),
        SystemTx::Consensus {
            step_index: outbe_primitives::system_tx::CONSENSUS_VALIDATOR_LIVENESS_STEP_INDEX,
            ..
        }
    ));
    assert!(matches!(
        SystemTx::decode(system_txs[9].tx().input().as_ref()).unwrap(),
        SystemTx::BeginBlock { step_index: 6 }
    ));
    assert!(matches!(
        SystemTx::decode(system_txs[10].tx().input().as_ref()).unwrap(),
        SystemTx::BeginBlock { step_index: 7 }
    ));
    assert!(matches!(
        SystemTx::decode(system_txs[11].tx().input().as_ref()).unwrap(),
        SystemTx::BeginBlock { step_index: 8 }
    ));
    drop(executor);

    let read_ctx = BlockContext {
        block_gas_limit: 0,
        user_gas_used: 0,
        block_number: 1,
        timestamp: 1,
        chain_id: CHAIN_ID,
        genesis_hash: alloy_primitives::B256::ZERO,
        genesis_timestamp: 1,
        proposer,
        validators: vec![proposer],
    };
    let mut provider =
        outbe_primitives::storage::direct::DirectStorageProvider::new(&mut state, read_ctx);
    StorageHandle::enter(&mut provider, |storage| {
        let vs = outbe_validatorset::contract::ValidatorSet::new(storage);
        let record = vs.get_validator(proposer)?.expect("validator should exist");
        assert_eq!(record.blocks_proposed, 1);
        Ok::<_, outbe_primitives::error::PrecompileError>(())
    })
    .expect("validator state should be readable");
}

#[test]
fn system_prefix_charges_visible_gas_and_receipt_cumulative_contract() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let user_tx = test_regular_tx()
        .try_into_recovered()
        .expect("regular tx signer should recover");

    let mut state = state_with_active_proposer_and_funded_account(proposer, user_tx.signer());
    let chain_spec = test_chain_spec();
    let receipt_builder = reth_ethereum::evm::RethReceiptBuilder::default();
    let config = OutbeEvmConfig::new(
        chain_spec.clone(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(signer.clone());
    let evm = config.evm_with_env(&mut state, test_evm_env(2, REWARDS_ADDRESS));
    let parent_hash = B256::with_last_byte(0x21);
    let (ctx, metadata) = block_two_execution_ctx(Some(3), Bytes::new(), proposer, parent_hash);
    let mut executor = OutbeBlockExecutor::new(
        EthBlockExecutor::new(evm, ctx.inner.clone(), &chain_spec, &receipt_builder),
        fixtures::executor_inputs_with_bootstrap(&ctx, Some(signer), false),
    );

    super::with_phase1_verify_disabled(|| {
        executor
            .apply_pre_execution_changes()
            .expect("block 2 pre-execution changes should apply");
    });
    let system_txs = begin_system_txs_for_test(
        &config,
        BeginBlockFixture {
            block_number: 2,
            parent_hash,
            extra_data: &Bytes::new(),
            parent_consensus_metadata: Some(metadata),
            proposer,
            bootstrap: BootstrapFixture::StandardForBlock,
        },
    );
    let visible_system_gas = fixtures::assert_begin_prefix_gas(
        &mut executor,
        system_txs,
        fixtures::BeginPrefixCheck {
            execution_message: "begin-zone system tx should execute",
            cumulative_receipts: false,
        },
    );

    let system_receipt_cumulative = executor
        .receipts()
        .last()
        .expect("system receipt must be present")
        .cumulative_gas_used;
    assert_eq!(
        system_receipt_cumulative, visible_system_gas,
        "begin-zone system receipts must contribute only visible envelope gas"
    );
    assert_eq!(
        executor.inner.cumulative_tx_gas_used, visible_system_gas,
        "system tx gas must expose only the small envelope gas before user txs"
    );

    let user_gas = executor
        .execute_transaction(user_tx)
        .expect("funded regular user tx should execute");
    let user_receipt_cumulative = executor
        .receipts()
        .last()
        .expect("user receipt must be present")
        .cumulative_gas_used;

    assert_eq!(
        executor.inner.cumulative_tx_gas_used,
        visible_system_gas + user_gas.tx_gas_used(),
        "header gas accounting must include visible system envelope gas plus user gas"
    );
    assert_eq!(
        user_receipt_cumulative,
        visible_system_gas + user_gas.tx_gas_used(),
        "receipt cumulative gas must include visible system envelope gas plus user gas"
    );

    executor
        .apply_end_block_phase()
        .expect("end-block phase should execute");
    executor
        .prepare_final_header_artifacts(0)
        .expect("final extra_data should encode");
    let (_evm, block_result) = executor.finish().expect("executor finish should succeed");
    assert_eq!(
        block_result.gas_used,
        visible_system_gas + user_gas.tx_gas_used(),
        "block header gas_used must include visible system envelope gas"
    );
}

#[test]
fn cycle_tick_utc_boundary_gas_usage() {
    const GENESIS_TS: u64 = 1_704_067_200;
    use outbe_system_common::time::SECONDS_PER_DAY;

    let signer = test_evm_signer();
    let proposer = signer.address();
    let mut state =
        state_with_active_validators_seeded(&[(proposer, dummy_pubkey(0xA2))], |storage| {
            seed_cycle_tick_genesis(storage, GENESIS_TS, proposer)
                .expect("cycle-tick genesis fixture");
        });
    let mut evm_env = test_evm_env(1, REWARDS_ADDRESS);
    let block_timestamp = GENESIS_TS + SECONDS_PER_DAY + 60;
    let tee_bootstrap = sample_tee_bootstrap_payload_at(1, block_timestamp);
    evm_env.block_env.timestamp = U256::from(block_timestamp);
    let config = OutbeEvmConfig::new(
        test_chain_spec_with_genesis_timestamp(GENESIS_TS),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(signer.clone());
    let evm = config.evm_with_env(&mut state, evm_env);
    let mut executor = config.create_executor(
        evm,
        execution_ctx_with_tee_bootstrap(Some(0), Bytes::new(), tee_bootstrap.clone()),
    );

    executor
        .apply_pre_execution_changes()
        .expect("pre-execution changes should apply before begin-zone system txs");
    let system_txs = begin_system_txs_for_test(
        &config,
        BeginBlockFixture {
            block_number: 1,
            parent_hash: B256::ZERO,
            extra_data: &Bytes::new(),
            parent_consensus_metadata: None,
            proposer,
            bootstrap: BootstrapFixture::explicit(Some(tee_bootstrap)),
        },
    );
    let mut cycle_tick_visible_gas = None;
    for tx in system_txs {
        let is_cycle = matches!(
            SystemTx::decode(tx.tx().input().as_ref()).unwrap(),
            SystemTx::BeginBlock { step_index: 8 }
        );
        let gas_output = executor
            .execute_transaction(tx)
            .expect("begin-zone system tx should execute in tx loop");
        if is_cycle {
            cycle_tick_visible_gas = Some(gas_output.tx_gas_used());
        }
    }

    assert_eq!(executor.receipts().len(), 12);
    let visible_gas = cycle_tick_visible_gas.expect("CycleTick visible gas must be captured");
    let cycle_tick_receipt = &executor.receipts()[11];
    assert!(
        cycle_tick_receipt.success,
        "CycleTick must succeed, not OOG"
    );
    let cycle_event = keccak256("CycleTaskExecuted(uint32,uint64,uint64,uint64)");
    assert!(
        cycle_tick_receipt.logs.iter().any(|log| {
            log.address == CYCLE_ADDRESS && log.data.topics().first() == Some(&cycle_event)
        }),
        "CycleTaskExecuted must be present in the system-tx receipt logs"
    );
    assert_eq!(
        cycle_tick_receipt.cumulative_gas_used, visible_gas,
        "system receipt cumulative gas must expose visible envelope gas"
    );
    assert_eq!(visible_gas, 0);
}

#[test]
fn tee_expiry_worst_case_active_sweep_fits_cycle_tick_budget() {
    super::receipts_and_gas_expiry::run();
}

#[test]
fn cycle_tick_gas_regression_exercises_dense_agentreward_state() {
    outbe_cycle::runtime::take_task_gas_samples();
    outbe_cycle::runtime::take_cycle_gas_samples();
    const GENESIS_TS: u64 = 1_704_067_200;
    use outbe_system_common::time::SECONDS_PER_DAY;
    // Validators fill the first shared slice; WAA and SRA await a later block.
    const DENSE_ADDRESS_COUNT: u64 =
        outbe_agentreward::constants::ACCRUAL_RECIPIENTS_PER_BLOCK as u64 / 2;
    const DENSE_VALIDATOR_COUNT: u32 = outbe_consensus::bls::MAX_VALIDATORS;
    assert_eq!(
        DENSE_VALIDATOR_COUNT,
        outbe_agentreward::constants::ACCRUAL_RECIPIENTS_PER_BLOCK
    );

    let signer = test_evm_signer();
    let proposer = signer.address();
    let block_ts = GENESIS_TS + SECONDS_PER_DAY + 60;
    let prev_day = outbe_system_common::time::previous_date_key(
        outbe_system_common::time::timestamp_to_date_key(block_ts),
    )
    .expect("date step");
    let mut state =
        state_with_active_validators_seeded(&[(proposer, dummy_pubkey(0xA2))], |storage| {
            assert_eq!(
                outbe_promislimit::PromisLimitContract::new(storage.clone())
                    .promis_limit()
                    .unwrap(),
                U256::ZERO,
                "GAS-05 fixture must start with zero carry-over"
            );
            seed_cycle_tick_genesis(storage.clone(), GENESIS_TS, proposer).unwrap();
            for spec in outbe_cycle::tasks::ACTIVE_TASKS {
                outbe_cycle::schema::Cycle::new(storage.clone())
                    .last_executed_at
                    .write(&spec.id, GENESIS_TS + 60)
                    .unwrap();
            }

            outbe_oracle::api::set_exchange_rate(
                storage.clone(),
                Address::ZERO,
                outbe_oracle::api::DAY_TYPE_PAIR,
                outbe_oracle::api::RateObservation {
                    rate: U256::from(1_000_000u64),
                    block_number: 1,
                    timestamp: block_ts,
                },
            )
            .unwrap();
            seed_previous_day_vwap(&storage, block_ts, U256::from(1_000_000u64));
            outbe_oracle::schema::OracleContract::new(storage.clone())
                .utc_day_vwap_last_finalized
                .write(prev_day)
                .unwrap();
            seed_dense_voter_participation(storage.clone(), prev_day, DENSE_VALIDATOR_COUNT)
                .expect("seed dense voter participation fixture succeeds");
            seed_dense_agent_recipients(storage, prev_day, DENSE_ADDRESS_COUNT)
                .expect("seed dense agent recipients fixture succeeds");
        });
    let tee_bootstrap = sample_tee_bootstrap_payload_at(1, block_ts);
    let mut evm_env = test_evm_env(1, REWARDS_ADDRESS);
    evm_env.block_env.timestamp = U256::from(block_ts);
    let config = OutbeEvmConfig::new(
        test_chain_spec_with_genesis_timestamp(GENESIS_TS),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(signer.clone());
    let evm = config.evm_with_env(&mut state, evm_env);
    let mut executor = config.create_executor(
        evm,
        execution_ctx_with_tee_bootstrap(Some(0), Bytes::new(), tee_bootstrap.clone()),
    );

    executor
        .apply_pre_execution_changes()
        .expect("pre-execution changes should apply");
    let mut system_txs = begin_system_txs_for_test(
        &config,
        BeginBlockFixture {
            block_number: 1,
            parent_hash: B256::ZERO,
            extra_data: &Bytes::new(),
            parent_consensus_metadata: None,
            proposer,
            bootstrap: BootstrapFixture::explicit(Some(tee_bootstrap)),
        },
    );
    let cycle_tx = system_txs.pop().expect("CycleTick is last in begin prefix");
    for tx in system_txs {
        executor
            .execute_transaction(tx)
            .expect("earlier begin system tx should execute");
    }
    let cycle_signed_gas_limit = cycle_tx.tx().gas_limit();
    let gross_before = executor.system_tx_gross_gas;
    let net_before = executor.system_tx_execution_gas;
    let cycle_gas = executor
        .execute_transaction(cycle_tx)
        .expect("dense CycleTick should execute")
        .tx_gas_used();
    let gross_gas = executor.system_tx_gross_gas - gross_before;
    let net_gas = executor.system_tx_execution_gas - net_before;
    assert!(gross_gas >= net_gas, "refunds cannot increase gross work");
    eprintln!(
        "Cycle calibration: validators={DENSE_VALIDATOR_COUNT}, addresses_per_pool={DENSE_ADDRESS_COUNT}, gross={gross_gas}, net={net_gas}, visible={cycle_gas}"
    );
    let samples = outbe_cycle::runtime::take_task_gas_samples();
    eprintln!("Dense simultaneous task gross: {samples:?}");
    eprintln!(
        "Dense Cycle body: {:?}",
        outbe_cycle::runtime::take_cycle_gas_samples()
    );
    assert_eq!(samples.len(), outbe_cycle::tasks::ACTIVE_TASKS.len());

    let cycle_receipt = executor
        .receipts()
        .last()
        .expect("CycleTick receipt should be present");
    assert!(
        cycle_receipt.success,
        "GAS-05: dense CycleTick must succeed"
    );
    assert_eq!(
        cycle_receipt.cumulative_gas_used, cycle_gas,
        "GAS-05: dense CycleTick receipt must expose actual visible gas"
    );
    assert!(
        cycle_gas <= cycle_signed_gas_limit,
        "GAS-05: CycleTick receipt gas exceeded signed gas limit"
    );
    drop(executor);
    let read_ctx = BlockContext {
        block_gas_limit: 0,
        user_gas_used: 0,
        block_number: 1,
        timestamp: block_ts,
        chain_id: CHAIN_ID,
        genesis_hash: alloy_primitives::B256::ZERO,
        genesis_timestamp: block_ts.max(1),
        proposer,
        validators: vec![proposer],
    };
    let mut provider =
        outbe_primitives::storage::direct::DirectStorageProvider::new(&mut state, read_ctx);
    StorageHandle::enter(&mut provider, |storage| {
        assert_dense_agent_accrual_pending(storage.clone(), prev_day, DENSE_ADDRESS_COUNT)?;
        assert_dense_validator_settlement(storage.clone(), DENSE_VALIDATOR_COUNT)?;
        let day_record = outbe_emission::api::get_emission_day_record(storage.clone(), prev_day)?
            .expect("settled EmissionDay must exist");
        assert!(
            day_record.state == outbe_emission::schema::emission_day_state::PROCESSING
                || day_record.state == outbe_emission::schema::emission_day_state::COMPLETED
        );
        for n in 0..DENSE_ADDRESS_COUNT {
            for prefix in [0x10, 0x11] {
                let agent = numbered_test_address(prefix, n);
                assert_eq!(outbe_gem::api::balance_of(&storage, agent)?, 0);
            }
        }
        Ok::<_, outbe_primitives::error::PrecompileError>(())
    })
    .expect("GAS-05 dense AgentReward state should be readable after CycleTick");
}

#[test]
fn noncritical_system_oog_emits_zero_gas_failure_receipt_and_continues_block() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let mut state = state_with_active_proposer(proposer);
    let chain_spec = test_chain_spec();
    let receipt_builder = reth_ethereum::evm::RethReceiptBuilder::default();
    let config = OutbeEvmConfig::new(
        chain_spec.clone(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(signer.clone());
    let evm = config.evm_with_env(&mut state, test_evm_env(1, REWARDS_ADDRESS));
    let ctx = block_one_execution_ctx(Some(3), Bytes::new());
    let mut executor = OutbeBlockExecutor::new(
        EthBlockExecutor::new(evm, ctx.inner.clone(), &chain_spec, &receipt_builder),
        fixtures::executor_inputs_with_bootstrap(&ctx, Some(signer.clone()), true),
    );

    executor
        .apply_pre_execution_changes()
        .expect("pre-execution changes should apply");
    let mut system_txs = begin_system_txs_for_test(
        &config,
        BeginBlockFixture {
            block_number: 1,
            parent_hash: B256::ZERO,
            extra_data: &Bytes::new(),
            parent_consensus_metadata: None,
            proposer,
            bootstrap: BootstrapFixture::StandardForBlock,
        },
    )
    .into_iter();
    for _ in 0..9 {
        let tx = system_txs
            .next()
            .expect("mandatory prefix system tx before OracleSlash");
        let gas = executor
            .execute_transaction(tx)
            .expect("mandatory prefix system tx should execute")
            .tx_gas_used();
        assert_eq!(gas, 0);
    }
    let oracle_tx = system_txs
        .next()
        .expect("OracleSlash system tx should be present at index 9");

    let receipts_before = executor.receipts().len();
    let cumulative_visible_gas_before = executor.inner.cumulative_tx_gas_used;
    let internal_work_before = executor.system_tx_execution_gas;
    let gas_output = outbe_evm::factory::with_forced_outbe_system_call_oog_halt(|| {
        executor.execute_transaction(oracle_tx)
    })
    .expect("soft-fail BeginBlock OOG must emit a status=false receipt and continue");

    assert_eq!(gas_output.tx_gas_used(), 0);
    assert_eq!(
        executor.receipts().len(),
        receipts_before + 1,
        "GAS-09: soft-fail OOG must append a synthetic status=false receipt"
    );
    let receipt = executor.receipts().last().expect("OracleSlash receipt");
    assert!(!receipt.success);
    assert_eq!(receipt.cumulative_gas_used, cumulative_visible_gas_before);
    assert_eq!(
        executor.inner.cumulative_tx_gas_used, cumulative_visible_gas_before,
        "GAS-09: soft-fail OOG must not change visible gas accounting"
    );
    assert_eq!(
        executor.system_tx_execution_gas,
        internal_work_before
            .saturating_add(outbe_primitives::system_tx::SYSTEM_TX_ARTIFACT_GAS_LIMIT),
        "GAS-09: diagnostic internal-work counter records spent system gas"
    );

    for remaining_tx in system_txs {
        let gas = executor
            .execute_transaction(remaining_tx)
            .expect("remaining BeginBlock system txs should continue after soft-fail OOG")
            .tx_gas_used();
        assert_eq!(gas, 0);
    }
}

#[test]
fn system_receipt_rpc_gas_delta_is_visible_envelope_gas() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let user_tx = test_regular_tx()
        .try_into_recovered()
        .expect("regular tx signer should recover");
    let mut state = state_with_active_proposer_and_funded_account(proposer, user_tx.signer());
    let chain_spec = test_chain_spec();
    let config = OutbeEvmConfig::new(
        chain_spec.clone(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(signer.clone());
    let parent_hash = B256::with_last_byte(0x13);
    let (ctx, metadata) = block_two_execution_ctx(Some(3), Bytes::new(), proposer, parent_hash);
    let evm = config.evm_with_env(&mut state, test_evm_env(2, REWARDS_ADDRESS));
    let mut executor = config.create_executor(evm, ctx);

    super::with_phase1_verify_disabled(|| {
        executor
            .apply_pre_execution_changes()
            .expect("pre-execution changes should apply");
    });
    let mut expected_rpc_gas_deltas = Vec::new();
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
        let signed_gas_limit = tx.tx().gas_limit();
        let gas_used = executor
            .execute_transaction(tx)
            .expect("system tx should execute")
            .tx_gas_used();
        assert!(gas_used <= signed_gas_limit);
        expected_rpc_gas_deltas.push(gas_used);
    }
    let user_gas = executor
        .execute_transaction(user_tx)
        .expect("funded regular user tx should execute")
        .tx_gas_used();
    expected_rpc_gas_deltas.push(user_gas);

    let mut previous = 0;
    let rpc_gas_deltas: Vec<u64> = executor
        .receipts()
        .iter()
        .map(|receipt| {
            let delta = receipt.cumulative_gas_used.saturating_sub(previous);
            previous = receipt.cumulative_gas_used;
            delta
        })
        .collect();

    assert_eq!(rpc_gas_deltas, expected_rpc_gas_deltas);
}

#[test]
fn system_gross_work_retains_refunds_without_changing_visible_receipts() {
    use alloy_evm::eth::EthTxResult;
    use revm::context::result::{Output, ResultAndState, ResultGas, SuccessReason};

    let signer = test_evm_signer();
    let mut state = state_with_active_proposer(signer.address());
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(signer);
    let evm = config.evm_with_env(&mut state, test_evm_env(1, REWARDS_ADDRESS));
    let mut executor = config.create_executor(evm, execution_ctx(Some(0), Bytes::new()));
    for count in 1..=2 {
        let output = EthTxResult {
            result: ResultAndState::new(
                ExecutionResult::Success {
                    reason: SuccessReason::Return,
                    gas: ResultGas::new_with_state_gas(100_000, 20_000, 0, 0),
                    logs: Vec::new(),
                    output: Output::Call(Bytes::new()),
                },
                Default::default(),
            ),
            blob_gas_used: 0,
            tx_type: alloy_consensus::TxType::Legacy,
        };
        let visible = executor
            .commit_system_transaction(output)
            .expect("refunded system result commits");
        assert_eq!(visible.tx_gas_used(), 0);
        assert_eq!(executor.system_tx_execution_gas, count * 80_000);
        assert_eq!(executor.system_tx_gross_gas, count * 100_000);
        assert_eq!(
            executor
                .receipts()
                .last()
                .expect("receipt")
                .cumulative_gas_used,
            0
        );
        assert_eq!(executor.inner.cumulative_tx_gas_used, 0);
    }
}

#[test]
fn system_protocol_precharge_and_ce_gas_are_published_and_block_limited() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let mut state = state_with_active_proposer(proposer);
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(signer);
    let evm = config.evm_with_env(&mut state, test_evm_env(1, REWARDS_ADDRESS));
    let mut executor = config.create_executor(evm, execution_ctx(Some(0), Bytes::new()));

    let output = executor
        .push_system_failure_receipt(SystemFailureReceiptInput {
            tx_type: alloy_consensus::TxType::Legacy,
            log_address: outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS,
            code: 299,
            reason: "deterministic test failure".into(),
            internal_gas_used: 123,
            internal_gross_gas_used: 123,
        })
        .expect("0-gas failure receipt should commit");
    assert_eq!(output.tx_gas_used(), 0);
    assert_eq!(executor.receipts().last().unwrap().cumulative_gas_used, 0);
    assert_eq!(executor.inner.cumulative_tx_gas_used, 0);
    assert_eq!(executor.inner.block_regular_gas_used, 0);
    assert_eq!(executor.inner.block_state_gas_used, 0);
    assert_eq!(executor.system_tx_execution_gas, 123);
    assert_eq!(executor.system_tx_gross_gas, 123);
}

#[test]
fn executor_finish_sets_visible_system_gas_for_fee_history_input() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let mut state = state_with_active_proposer(proposer);
    let chain_spec = test_chain_spec();
    let receipt_builder = reth_ethereum::evm::RethReceiptBuilder::default();
    let config = OutbeEvmConfig::new(
        chain_spec.clone(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(signer.clone());
    let evm = config.evm_with_env(&mut state, test_evm_env(1, REWARDS_ADDRESS));
    let ctx = block_one_execution_ctx(Some(2), Bytes::new());
    let mut executor = OutbeBlockExecutor::new(
        EthBlockExecutor::new(evm, ctx.inner.clone(), &chain_spec, &receipt_builder),
        fixtures::executor_inputs_with_bootstrap(&ctx, Some(signer.clone()), true),
    );

    executor
        .apply_pre_execution_changes()
        .expect("pre-execution changes should apply");
    let system_txs = begin_system_txs_for_test(
        &config,
        BeginBlockFixture {
            block_number: 1,
            parent_hash: B256::ZERO,
            extra_data: &Bytes::new(),
            parent_consensus_metadata: None,
            proposer,
            bootstrap: BootstrapFixture::StandardForBlock,
        },
    );
    let mut visible_system_gas = 0u64;
    let mut expected_system_deltas = Vec::with_capacity(system_txs.len());
    for tx in system_txs {
        let signed_gas_limit = tx.tx().gas_limit();
        assert_eq!(signed_gas_limit, 0);
        let visible_gas = executor
            .execute_transaction(tx)
            .expect("system tx should execute")
            .tx_gas_used();
        assert_eq!(visible_gas, 0);
        expected_system_deltas.push(visible_gas);
        visible_system_gas += visible_gas;
    }
    assert_eq!(visible_system_gas, 0);

    let mut previous = 0;
    let receipt_deltas: Vec<u64> = executor
        .receipts()
        .iter()
        .map(|receipt| {
            let delta = receipt.cumulative_gas_used.saturating_sub(previous);
            previous = receipt.cumulative_gas_used;
            delta
        })
        .collect();
    assert_eq!(receipt_deltas, expected_system_deltas);

    executor
        .prepare_final_header_artifacts(0)
        .expect("final extra_data should encode");
    let (_evm, result) = executor.finish().expect("finish should succeed");
    assert_eq!(
        result.gas_used, 0,
        "GAS-14: system-only block gas_used must be 0 under Tempo model"
    );
    assert_eq!(result.receipts.len(), expected_system_deltas.len());

    let gas_limit = 30_000_000u64;
    let gas_used_ratio = result.gas_used as f64 / gas_limit as f64;
    assert_eq!(
        gas_used_ratio, 0.0,
        "GAS-14: fee-history input ratio for system-only block must be 0.0, got {gas_used_ratio}"
    );
}

#[test]
fn mixed_system_and_user_block_finish_uses_visible_system_gas() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let user_tx = test_regular_tx()
        .try_into_recovered()
        .expect("regular tx signer should recover");
    let mut state = state_with_active_proposer_and_funded_account(proposer, user_tx.signer());
    let chain_spec = test_chain_spec();
    let config = OutbeEvmConfig::new(
        chain_spec.clone(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(signer.clone());
    let parent_hash = B256::with_last_byte(0x16);
    let (ctx, metadata) = block_two_execution_ctx(Some(3), Bytes::new(), proposer, parent_hash);
    let evm = config.evm_with_env(&mut state, test_evm_env(2, REWARDS_ADDRESS));
    let mut executor = config.create_executor(evm, ctx);

    super::with_phase1_verify_disabled(|| {
        executor
            .apply_pre_execution_changes()
            .expect("pre-execution changes should apply");
    });
    let system_txs = begin_system_txs_for_test(
        &config,
        BeginBlockFixture {
            block_number: 2,
            parent_hash,
            extra_data: &Bytes::new(),
            parent_consensus_metadata: Some(metadata),
            proposer,
            bootstrap: BootstrapFixture::StandardForBlock,
        },
    );
    let visible_system_gas = fixtures::assert_begin_prefix_gas(
        &mut executor,
        system_txs,
        fixtures::BeginPrefixCheck {
            execution_message: "system tx should execute",
            cumulative_receipts: false,
        },
    );
    assert_eq!(visible_system_gas, 0);
    let user_gas = executor
        .execute_transaction(user_tx)
        .expect("funded regular user tx should execute")
        .tx_gas_used();

    executor
        .apply_end_block_phase()
        .expect("end-block phase should execute");
    executor
        .prepare_final_header_artifacts(0)
        .expect("final extra_data should encode");
    let (_evm, result) = executor.finish().expect("finish should succeed");
    assert_eq!(result.gas_used, user_gas);
    assert_eq!(result.receipts.len(), 18);
    assert_eq!(result.receipts[0].cumulative_gas_used, 0);
    assert_eq!(result.receipts[11].cumulative_gas_used, 0);
    assert_eq!(result.receipts[12].cumulative_gas_used, user_gas);
    assert_eq!(result.receipts[17].cumulative_gas_used, user_gas);
}
