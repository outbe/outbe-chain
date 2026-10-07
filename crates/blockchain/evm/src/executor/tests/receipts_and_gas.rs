use super::*;

mod capacity;
mod expiry;
mod factory_approval;
mod hook_events;
mod native_sinks;
mod support;

use support::*;

#[test]
fn priority_fees_credit_rewards_escrow_in_production_fee_path() {
    let chain_spec = test_chain_spec();
    let receipt_builder = reth_ethereum::evm::RethReceiptBuilder::default();
    let config = OutbeEvmConfig::new(chain_spec.clone());
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

    let expected_fee = super::validator_fee_for_gas(
        tx.max_fee_per_gas(),
        tx.max_priority_fee_per_gas(),
        executor.receipts()[0].cumulative_gas_used,
        u128::from(MIN_PROTOCOL_BASE_FEE),
    );
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
    let config = OutbeEvmConfig::new(chain_spec.clone()).with_evm_signer(signer.clone());
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
        let intrinsic_gas = system_tx_intrinsic_gas(tx.tx().input()).unwrap();
        let gas_used = executor
            .execute_transaction(tx)
            .expect("begin-zone system tx should execute in tx loop");
        assert!(
            (intrinsic_gas..=signed_gas_limit).contains(&gas_used.tx_gas_used()),
            "receipt gas must stay between intrinsic gas and the signed envelope limit"
        );
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

    assert_eq!(executor.receipts().len(), 5);
    assert!(executor.receipts().iter().all(|receipt| receipt.success));
    assert!(
        executor.system_tx_execution_gas > 0,
        "system tx internal execution gas must still be measured"
    );
    assert_eq!(
        executor.inner.cumulative_tx_gas_used, visible_system_gas_used,
        "system tx must charge only visible envelope gas to block accounting"
    );
    assert_eq!(
        executor.inner.block_regular_gas_used, visible_system_gas_used,
        "system tx regular gas must expose only visible envelope gas"
    );
    assert!(executor
        .receipts()
        .iter()
        .all(|receipt| receipt.tx_type == reth_ethereum::TxType::Legacy));
    assert_eq!(
        executor.receipts()[4].cumulative_gas_used,
        visible_system_gas_used
    );

    assert_eq!(system_txs.len(), 5);
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
        SystemTxInputV2::decode(system_txs[0].tx().input().as_ref()).unwrap(),
        SystemTxInputV2::CycleTick
    ));
    assert!(matches!(
        SystemTxInputV2::decode(system_txs[1].tx().input().as_ref()).unwrap(),
        SystemTxInputV2::RewardsGemDelivery
    ));
    assert!(matches!(
        SystemTxInputV2::decode(system_txs[2].tx().input().as_ref()).unwrap(),
        SystemTxInputV2::TeeBootstrap { .. }
    ));
    assert!(matches!(
        SystemTxInputV2::decode(system_txs[3].tx().input().as_ref()).unwrap(),
        SystemTxInputV2::OracleSlashWindow
    ));
    assert!(matches!(
        SystemTxInputV2::decode(system_txs[4].tx().input().as_ref()).unwrap(),
        SystemTxInputV2::HookEvents
    ));
    drop(executor);

    let read_ctx = BlockContext::new(1, 1, CHAIN_ID, proposer, vec![proposer]);
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
    let config = OutbeEvmConfig::new(chain_spec.clone()).with_evm_signer(signer.clone());
    let evm = config.evm_with_env(&mut state, test_evm_env(1, REWARDS_ADDRESS));
    let ctx = block_one_execution_ctx(Some(3), Bytes::new());
    let mut executor = OutbeBlockExecutor::new(
        EthBlockExecutor::new(evm, ctx.inner.clone(), &chain_spec, &receipt_builder),
        fixtures::executor_inputs_with_bootstrap(&ctx, Some(signer), false),
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
        .finalize_compressed_entities()
        .expect("compressed entities should finalize");
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
fn apply_pre_execution_changes_emits_cycle_tick_event_in_system_receipt() {
    const GENESIS_TS: u64 = 1_704_067_200;
    const SECONDS_PER_DAY: u64 = 86_400;

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
    let config = OutbeEvmConfig::new(test_chain_spec()).with_evm_signer(signer.clone());
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
    for tx in system_txs {
        executor
            .execute_transaction(tx)
            .expect("begin-zone system tx should execute in tx loop");
    }

    assert_eq!(executor.receipts().len(), 5);
    let cycle_event = keccak256("CycleTriggerExecuted(uint32,uint64,uint64,uint64)");
    assert!(
        executor.receipts()[0].logs.iter().any(|log| {
            log.address == CYCLE_ADDRESS && log.data.topics().first() == Some(&cycle_event)
        }),
        "CycleTriggerExecuted must be present in the system-tx receipt logs"
    );
}

#[test]
fn cycle_tick_utc_boundary_gas_usage() {
    const GENESIS_TS: u64 = 1_704_067_200;
    const SECONDS_PER_DAY: u64 = 86_400;

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
    let config = OutbeEvmConfig::new(test_chain_spec()).with_evm_signer(signer.clone());
    let evm = config.evm_with_env(&mut state, evm_env);
    let mut executor = config.create_executor(
        evm,
        execution_ctx_with_tee_bootstrap(Some(0), Bytes::new(), tee_bootstrap.clone()),
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
            bootstrap: BootstrapFixture::explicit(Some(tee_bootstrap)),
        },
    );
    let mut cycle_tick_visible_gas = None;
    for tx in system_txs {
        let gas_output = executor
            .execute_transaction(tx)
            .expect("begin-zone system tx should execute");
        if cycle_tick_visible_gas.is_none() {
            cycle_tick_visible_gas = Some(gas_output.tx_gas_used());
        }
    }

    let visible_gas = cycle_tick_visible_gas.expect("CycleTick visible gas must be captured");
    let cycle_tick_receipt = &executor.receipts()[0];
    assert!(
        cycle_tick_receipt.success,
        "CycleTick must succeed, not OOG"
    );
    assert_eq!(
        cycle_tick_receipt.cumulative_gas_used, visible_gas,
        "system receipt cumulative gas must expose visible envelope gas"
    );
    eprintln!("CycleTick UTC boundary visible gas: used={visible_gas}, block_limit=30_000_000");
    assert!(
        visible_gas < 30_000_000,
        "CycleTick visible gas {visible_gas} must fit within the block gas limit"
    );
}

#[test]
fn tee_expiry_worst_case_active_sweep_fits_cycle_tick_budget() {
    expiry::run();
}

#[test]
fn capacity_forfeiture_cycle_tick_keeps_twenty_percent_block_headroom() {
    let _enclave = outbe_tribute::enclave_client::test_enclave::scope();
    capacity::run();
}

#[test]
fn gas_05_cycle_tick_gas_regression_exercises_dense_agentreward_state() {
    const GENESIS_TS: u64 = 1_704_067_200;
    const SECONDS_PER_DAY: u64 = 86_400;
    const DENSE_ADDRESS_COUNT: u64 = 512;
    const DENSE_VALIDATOR_COUNT: u32 = outbe_consensus::bls::MAX_VALIDATORS;

    let signer = test_evm_signer();
    let proposer = signer.address();
    let block_ts = GENESIS_TS + SECONDS_PER_DAY + 60;
    let prev_day = outbe_primitives::time::previous_date_key(
        outbe_primitives::time::timestamp_to_date_key(block_ts),
    );
    let emission_trigger = outbe_cycle::triggers::TriggerId::ProtocolCycle.as_u32();
    let mut state =
        state_with_active_validators_seeded(&[(proposer, dummy_pubkey(0xA2))], |storage| {
            let genesis_ctx = BlockRuntimeContext::new(
                BlockContext::new(0, GENESIS_TS, CHAIN_ID, proposer, vec![proposer]),
                storage.clone(),
            );
            outbe_rewards::runtime::ensure_genesis_anchor(&genesis_ctx).unwrap();
            let cycle = outbe_cycle::schema::Cycle::new(storage.clone());
            cycle
                .active_utc_day
                .write(outbe_primitives::time::timestamp_to_date_key(GENESIS_TS))
                .unwrap();
            cycle
                .last_executed_at
                .write(&emission_trigger, GENESIS_TS + 60)
                .unwrap();

            outbe_oracle::api::set_exchange_rate(
                storage.clone(),
                Address::ZERO,
                outbe_oracle::api::DAY_TYPE_PAIR,
                U256::from(1_000_000u64),
                1,
                block_ts,
            )
            .unwrap();
            seed_previous_day_vwap(&storage, block_ts, U256::from(1_000_000u64));
            seed_dense_voter_participation(storage.clone(), prev_day, DENSE_VALIDATOR_COUNT)
                .expect("seed dense voter participation fixture succeeds");
            seed_dense_agent_recipients(storage, prev_day, DENSE_ADDRESS_COUNT)
                .expect("seed dense agent recipients fixture succeeds");
        });
    let mut evm_env = test_evm_env(1, REWARDS_ADDRESS);
    evm_env.block_env.timestamp = U256::from(block_ts);
    let config = OutbeEvmConfig::new(test_chain_spec()).with_evm_signer(signer.clone());
    let evm = config.evm_with_env(&mut state, evm_env);
    let mut executor = config.create_executor(evm, block_one_execution_ctx(Some(0), Bytes::new()));

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
    );
    let cycle_tx = system_txs.remove(0);
    let delivery_tx = system_txs.remove(0);
    let cycle_signed_gas_limit = cycle_tx.tx().gas_limit();
    let cycle_gas = executor
        .execute_transaction(cycle_tx)
        .expect("dense CycleTick should execute")
        .tx_gas_used();

    let cycle_receipt = executor
        .receipts()
        .first()
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
    let delivery_signed_gas_limit = delivery_tx.tx().gas_limit();
    let delivery_gas = executor
        .execute_transaction(delivery_tx)
        .expect("dense RewardsGemDelivery should execute")
        .tx_gas_used();
    assert!(delivery_gas <= delivery_signed_gas_limit);

    drop(executor);
    let read_ctx = BlockContext::new(1, block_ts, CHAIN_ID, proposer, vec![proposer]);
    let mut provider =
        outbe_primitives::storage::direct::DirectStorageProvider::new(&mut state, read_ctx);
    StorageHandle::enter(&mut provider, |storage| {
        assert_dense_agent_settlement(storage.clone(), prev_day, DENSE_ADDRESS_COUNT)?;
        let rewards = outbe_rewards::schema::Rewards::new(storage.clone());
        assert!(rewards.daily_topup_prepared.read(&prev_day)?);
        assert!(rewards.daily_topup_settled.read(&prev_day)?);
        assert_eq!(rewards.reward_gem_queue_head.read()?, 1);
        assert_eq!(rewards.reward_gem_queue_tail.read()?, 1);
        let gem = outbe_gem::GemContract::new(storage);
        for index in 0..DENSE_VALIDATOR_COUNT {
            let voter = numbered_test_address(0x12, u64::from(index));
            assert_eq!(gem.balance_of(voter)?, 1);
        }
        Ok::<_, outbe_primitives::error::PrecompileError>(())
    })
    .expect("GAS-05 dense AgentReward state should be readable after CycleTick");
}

#[test]
fn gas_09_noncritical_system_oog_exhausts_aggregate_budget_atomically() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let mut state = state_with_active_proposer(proposer);
    let chain_spec = test_chain_spec();
    let receipt_builder = reth_ethereum::evm::RethReceiptBuilder::default();
    let config = OutbeEvmConfig::new(chain_spec.clone()).with_evm_signer(signer.clone());
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
    let cycle_tx = system_txs
        .next()
        .expect("CycleTick system tx should be present");
    let rewards_tx = system_txs
        .next()
        .expect("RewardsGemDelivery system tx should be present");
    let tee_bootstrap_tx = system_txs
        .next()
        .expect("TeeBootstrap system tx should be present");
    let oracle_tx = system_txs
        .next()
        .expect("OracleSlashWindow system tx should be present");
    let cycle_signed_gas_limit = cycle_tx.tx().gas_limit();
    // A forced OOG does not model Oracle performing ten billion units of useful work. revm
    // charges the complete system-call gas limit for any OOG. The mandatory phases already
    // consumed internal work. Thus, to accept the OOG as a soft failure would exceed the
    // aggregate block budget. Therefore the failure must be hard and atomic, even though an
    // ordinary OracleSlashWindow revert remains soft.
    let cycle_gas = executor
        .execute_transaction(cycle_tx)
        .expect("CycleTick should execute successfully")
        .tx_gas_used();
    assert!(cycle_gas <= cycle_signed_gas_limit);
    executor
        .execute_transaction(rewards_tx)
        .expect("RewardsGemDelivery should execute before TeeBootstrap");
    let _tee_bootstrap_gas = executor
        .execute_transaction(tee_bootstrap_tx)
        .expect("mandatory TeeBootstrap should execute before the non-critical phase")
        .tx_gas_used();

    let receipts_before = executor.receipts().len();
    let cumulative_visible_gas_before = executor.inner.cumulative_tx_gas_used;
    let internal_work_before = executor.system_tx_execution_gas;
    let error = crate::factory::with_forced_outbe_system_call_oog_halt(|| {
        executor.execute_transaction(oracle_tx)
    })
    .expect_err("forced system OOG must exhaust the aggregate internal-work budget");

    assert!(
        error.to_string().contains("internal system-work budget"),
        "GAS-09: OOG must fail through the aggregate budget guard: {error}"
    );
    assert_eq!(
        executor.receipts().len(),
        receipts_before,
        "GAS-09: aggregate exhaustion must not append a failure receipt"
    );
    assert_eq!(
        executor.inner.cumulative_tx_gas_used, cumulative_visible_gas_before,
        "GAS-09: aggregate exhaustion must not change visible gas accounting"
    );
    assert_eq!(
        executor.system_tx_execution_gas, internal_work_before,
        "GAS-09: aggregate exhaustion must not commit internal-work accounting"
    );
}

#[test]
fn gas_13_system_receipt_rpc_gas_delta_is_visible_envelope_gas() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let user_tx = test_regular_tx()
        .try_into_recovered()
        .expect("regular tx signer should recover");
    let mut state = state_with_active_proposer_and_funded_account(proposer, user_tx.signer());
    let chain_spec = test_chain_spec();
    let receipt_builder = reth_ethereum::evm::RethReceiptBuilder::default();
    let config = OutbeEvmConfig::new(chain_spec.clone()).with_evm_signer(signer.clone());
    let evm = config.evm_with_env(&mut state, test_evm_env(1, REWARDS_ADDRESS));
    let ctx = block_one_execution_ctx(Some(3), Bytes::new());
    let mut executor = OutbeBlockExecutor::new(
        EthBlockExecutor::new(evm, ctx.inner.clone(), &chain_spec, &receipt_builder),
        fixtures::executor_inputs_with_bootstrap(&ctx, Some(signer.clone()), true),
    );

    executor
        .apply_pre_execution_changes()
        .expect("pre-execution changes should apply");
    let mut expected_rpc_gas_deltas = Vec::new();
    for tx in begin_system_txs_for_test(
        &config,
        BeginBlockFixture {
            block_number: 1,
            parent_hash: B256::ZERO,
            extra_data: &Bytes::new(),
            parent_consensus_metadata: None,
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
fn system_protocol_precharge_and_ce_gas_are_published_and_block_limited() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let mut state = state_with_active_proposer_without_ocomp(proposer);
    let config = OutbeEvmConfig::new(test_chain_spec()).with_evm_signer(signer);
    let evm = config.evm_with_env(&mut state, test_evm_env(1, REWARDS_ADDRESS));
    let mut executor = config.create_executor(evm, execution_ctx(Some(0), Bytes::new()));

    let intrinsic_gas = 21_000;
    let protocol_precharge = 300_000;
    let visible_base_gas = intrinsic_gas + protocol_precharge;
    let compressed_entities_gas = 70_000;
    let output = executor
        .push_system_failure_receipt(SystemFailureReceiptInput {
            tx_type: alloy_consensus::TxType::Legacy,
            log_address: outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS,
            code: 299,
            reason: "deterministic test failure".into(),
            visible_base_gas,
            compressed_entities_gas,
            signed_gas_limit: visible_base_gas + compressed_entities_gas,
            internal_gas_used: 123,
        })
        .expect("visible envelope plus CE gas should fit");
    let expected_visible = intrinsic_gas + protocol_precharge + compressed_entities_gas;
    assert_eq!(output.tx_gas_used(), expected_visible);
    assert_eq!(
        executor.receipts().last().unwrap().cumulative_gas_used,
        expected_visible
    );
    assert_eq!(executor.inner.cumulative_tx_gas_used, expected_visible);
    assert_eq!(executor.inner.block_regular_gas_used, expected_visible);
    assert_eq!(executor.inner.block_state_gas_used, expected_visible);
    assert_eq!(executor.system_tx_execution_gas, 123);

    let receipts_before = executor.receipts().len();
    let cumulative_before = executor.inner.cumulative_tx_gas_used;
    let block_limit = executor.inner.evm.block.gas_limit;
    let remaining = block_limit - cumulative_before;
    let error = executor
        .push_system_failure_receipt(SystemFailureReceiptInput {
            tx_type: alloy_consensus::TxType::Legacy,
            log_address: outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS,
            code: 299,
            reason: "must not commit".into(),
            visible_base_gas: remaining,
            compressed_entities_gas: 1,
            signed_gas_limit: remaining + 1,
            internal_gas_used: 456,
        })
        .expect_err("CE delta must not push cumulative gas past the block limit");
    assert!(error.to_string().contains("exceeds block gas limit"));
    assert_eq!(executor.receipts().len(), receipts_before);
    assert_eq!(executor.inner.cumulative_tx_gas_used, cumulative_before);
    assert_eq!(executor.system_tx_execution_gas, 123);
}

#[test]
fn system_internal_work_budget_rejects_before_receipt_or_state_accounting() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let mut state = state_with_active_proposer_without_ocomp(proposer);
    let config = OutbeEvmConfig::new(test_chain_spec()).with_evm_signer(signer);
    let evm = config.evm_with_env(&mut state, test_evm_env(1, REWARDS_ADDRESS));
    let mut executor = config.create_executor(evm, execution_ctx(Some(0), Bytes::new()));
    executor.system_tx_execution_gas =
        outbe_primitives::system_tx::SYSTEM_TX_ARTIFACT_GAS_LIMIT - 1;

    let receipts_before = executor.receipts().len();
    let cumulative_before = executor.inner.cumulative_tx_gas_used;
    let error = executor
        .push_system_failure_receipt(SystemFailureReceiptInput {
            tx_type: alloy_consensus::TxType::Eip1559,
            log_address: outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS,
            code: 299,
            reason: "must not commit past the internal system-work budget".into(),
            visible_base_gas: 0,
            compressed_entities_gas: 0,
            signed_gas_limit: 0,
            internal_gas_used: 2,
        })
        .expect_err("system internal work must be bounded independently from user gas");

    assert!(error.to_string().contains("internal system-work budget"));
    assert_eq!(executor.receipts().len(), receipts_before);
    assert_eq!(executor.inner.cumulative_tx_gas_used, cumulative_before);
    assert_eq!(
        executor.system_tx_execution_gas,
        outbe_primitives::system_tx::SYSTEM_TX_ARTIFACT_GAS_LIMIT - 1
    );
}

#[test]
fn gas_14_executor_finish_sets_visible_system_gas_for_fee_history_input() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let mut state = state_with_active_proposer(proposer);
    let chain_spec = test_chain_spec();
    let receipt_builder = reth_ethereum::evm::RethReceiptBuilder::default();
    let config = OutbeEvmConfig::new(chain_spec.clone()).with_evm_signer(signer.clone());
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
        let visible_gas = executor
            .execute_transaction(tx)
            .expect("system tx should execute")
            .tx_gas_used();
        assert!(visible_gas <= signed_gas_limit);
        expected_system_deltas.push(visible_gas);
        visible_system_gas += visible_gas;
    }
    assert!(visible_system_gas > 0);

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
        .finalize_compressed_entities()
        .expect("compressed entities should finalize");
    executor
        .prepare_final_header_artifacts(0)
        .expect("final extra_data should encode");
    let (_evm, result) = executor.finish().expect("finish should succeed");
    assert_eq!(
        result.gas_used, visible_system_gas,
        "GAS-14: system-only block gas_used must expose visible system envelope gas"
    );
    assert_eq!(result.receipts.len(), expected_system_deltas.len());

    let gas_limit = 30_000_000u64;
    let gas_used_ratio = result.gas_used as f64 / gas_limit as f64;
    assert!(
        gas_used_ratio > 0.0 && gas_used_ratio <= 1.0,
        "GAS-14: fee-history input ratio must expose complete mandatory block-1 system gas, got {gas_used_ratio}"
    );
}

#[test]
fn gas_16_mixed_system_and_user_block_finish_uses_visible_system_gas() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let user_tx = test_regular_tx()
        .try_into_recovered()
        .expect("regular tx signer should recover");
    let mut state = state_with_active_proposer_and_funded_account(proposer, user_tx.signer());
    let chain_spec = test_chain_spec();
    let receipt_builder = reth_ethereum::evm::RethReceiptBuilder::default();
    let config = OutbeEvmConfig::new(chain_spec.clone()).with_evm_signer(signer.clone());
    let evm = config.evm_with_env(&mut state, test_evm_env(1, REWARDS_ADDRESS));
    let ctx = block_one_execution_ctx(Some(3), Bytes::new());
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
    let visible_system_gas = fixtures::assert_begin_prefix_gas(
        &mut executor,
        system_txs,
        fixtures::BeginPrefixCheck {
            execution_message: "system tx should execute",
            cumulative_receipts: false,
        },
    );
    let user_gas = executor
        .execute_transaction(user_tx)
        .expect("funded regular user tx should execute")
        .tx_gas_used();

    executor
        .finalize_compressed_entities()
        .expect("compressed entities should finalize");
    executor
        .prepare_final_header_artifacts(0)
        .expect("final extra_data should encode");
    let (_evm, result) = executor.finish().expect("finish should succeed");
    assert_eq!(result.gas_used, visible_system_gas + user_gas);
    assert_eq!(result.receipts.len(), 6);
    let first_visible_gas = result.receipts[0].cumulative_gas_used;
    assert!(first_visible_gas >= outbe_primitives::system_tx::SYSTEM_TX_VISIBLE_GAS_FLOOR);
    assert_eq!(result.receipts[4].cumulative_gas_used, visible_system_gas);
    assert_eq!(
        result.receipts[5].cumulative_gas_used,
        visible_system_gas + user_gas
    );
}
