use super::*;

#[test]
fn delayed_first_cycle_tick_starts_all_fresh_passes_at_chainspec_genesis() {
    use outbe_primitives::time::{date_key_to_utc_timestamp, WorldwideDay};
    use outbe_system_common::pass_budget::BlockAllowance;

    let genesis_ts = date_key_to_utc_timestamp(20_240_131).unwrap() + 86_390;
    let block_ts = date_key_to_utc_timestamp(20_240_203).unwrap() + 60;
    let signer = test_evm_signer();
    let proposer = signer.address();
    let spec = test_chain_spec_with_genesis_timestamp(genesis_ts);
    let mut seed = HashMapStorageProvider::new_with_chain_identity(CHAIN_ID, spec.genesis_hash());
    StorageHandle::enter(&mut seed, |storage| {
        seed_registered_active_validator(storage.clone(), proposer, &dummy_pubkey(0xA2));
        outbe_oracle::schema::OracleContract::new(storage)
            .utc_day_vwap_last_finalized
            .write(20_240_202)
            .unwrap();
    });
    let mut db = cache_db_from_storage(seed);
    let marker = Bytecode::new_legacy([0xef].into());
    outbe_evm::test_support::install_call_scan_markers(&mut db, &marker);
    for address in [
        outbe_primitives::addresses::VALIDATOR_SET_ADDRESS,
        ORACLE_ADDRESS,
        outbe_primitives::addresses::METADOSIS_ADDRESS,
        CYCLE_ADDRESS,
        outbe_primitives::addresses::TRIBUTE_ADDRESS,
    ] {
        db.insert_account_info(
            address,
            AccountInfo {
                code_hash: marker.hash_slow(),
                code: Some(marker.clone()),
                ..Default::default()
            },
        );
    }
    let mut state = State::builder()
        .with_database(db)
        .with_bundle_update()
        .build();
    let config = OutbeEvmConfig::new(
        spec,
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(signer);
    let mut env = test_evm_env(1, REWARDS_ADDRESS);
    env.block_env.timestamp = U256::from(block_ts);
    let bootstrap = sample_tee_bootstrap_payload_at(1, block_ts);
    let begin = begin_system_txs_for_test(
        &config,
        BeginBlockFixture {
            block_number: 1,
            parent_hash: B256::ZERO,
            extra_data: &Bytes::new(),
            parent_consensus_metadata: None,
            proposer,
            bootstrap: BootstrapFixture::explicit(Some(bootstrap.clone())),
        },
    );
    let evm = config.evm_with_env(&mut state, env);
    let mut execution =
        execution_ctx_with_tee_bootstrap(Some(begin.len()), Bytes::new(), bootstrap);
    execution.expected_begin_system_txs = begin.clone();
    execution.proposer_evm_address = Some(proposer);
    let mut executor = config.create_executor(evm, execution);
    executor.apply_pre_execution_changes().unwrap();
    for tx in begin {
        let is_cycle = matches!(
            SystemTx::decode(tx.tx().input().as_ref()).unwrap(),
            SystemTx::BeginBlock { step_index: 8 }
        );
        executor.execute_transaction(tx).unwrap();
        if is_cycle {
            break;
        }
    }
    drop(executor);

    // Call passes now run in the end zone, which starts at block 2.
    // Isolate that suffix after the delayed block-1 CycleTick initialized genesis.
    let mut env = test_evm_env(2, REWARDS_ADDRESS);
    env.block_env.timestamp = U256::from(block_ts + 1);
    let evm = config.evm_with_env(&mut state, env);
    let mut execution = execution_ctx(Some(2), Bytes::new());
    execution.proposer_evm_address = Some(proposer);
    let mut executor = config.create_executor(evm, execution);
    executor.apply_end_block_phase().unwrap();
    assert_eq!(executor.receipts().len(), 5);
    assert!(executor.receipts().iter().all(|receipt| receipt.success));
    drop(executor);

    let block = BlockContext {
        block_gas_limit: 0,
        user_gas_used: 0,
        block_number: 2,
        timestamp: block_ts + 1,
        chain_id: CHAIN_ID,
        genesis_hash: alloy_primitives::B256::ZERO,
        genesis_timestamp: genesis_ts,
        proposer: alloy_primitives::Address::ZERO,
        validators: Vec::new(),
    };
    let mut provider =
        outbe_primitives::storage::direct::DirectStorageProvider::new(&mut state, block);
    StorageHandle::enter(&mut provider, |storage| {
        let nod = outbe_nod::NodContract::new(storage.clone());
        let gem = outbe_gem::GemContract::new(storage.clone());
        let intex = outbe_intex::IntexContract::new(storage.clone());
        let credis = outbe_credis::CredisContract::new(storage.clone());
        for (budget, current, latest) in [
            (
                nod.daily_budget.read().unwrap(),
                nod.call_sweep_day.read().unwrap(),
                nod.call_latest_day.read().unwrap(),
            ),
            (
                gem.daily_budget.read().unwrap(),
                gem.call_sweep_day.read().unwrap(),
                gem.call_latest_day.read().unwrap(),
            ),
            (
                intex.daily_budget.read().unwrap(),
                intex.call_sweep_day.read().unwrap(),
                intex.call_latest_day.read().unwrap(),
            ),
            (
                credis.daily_budget.read().unwrap(),
                credis.call_sweep_day.read().unwrap(),
                credis.call_latest_day.read().unwrap(),
            ),
        ] {
            assert_eq!(
                BlockAllowance::unpack(budget).unwrap().unwrap().day,
                20_240_131
            );
            assert_eq!(
                current, 20_240_201,
                "one genesis day completes, without skipping February 1"
            );
            assert_eq!(latest, 20_240_202);
        }
        assert!(outbe_metadosis::api::worldwide_day(
            storage.clone(),
            WorldwideDay::from_timestamp(block_ts)
        )
        .unwrap()
        .is_some());
        assert!(
            outbe_metadosis::api::worldwide_day(storage, WorldwideDay::from_timestamp(genesis_ts))
                .unwrap()
                .is_some(),
            "Metadosis retains its existing catch-up and lookahead initialization"
        );
    });
}

#[test]
fn evm_level_system_tx_err_must_not_be_soft_receipted() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let mut state = state_with_active_proposer(proposer);
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(signer.clone());
    let evm = config.evm_with_env(&mut state, test_evm_env(1, REWARDS_ADDRESS));
    let mut executor = config.create_executor(evm, block_one_execution_ctx(Some(1), Bytes::new()));
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

    let receipt_count_before = executor.receipts().len();
    let err = outbe_evm::factory::with_forced_outbe_system_call_error(|| {
        executor.execute_transaction(cycle_tx)
    })
    .expect_err("GAS-01: raw system-call engine errors must not be converted into soft receipts");
    let msg = err.to_string();
    assert!(
        msg.contains("forced Outbe system-call error")
            || msg.contains("system tx")
            || msg.contains("Phase"),
        "GAS-01: unexpected hard error for raw system-call Err: {msg}"
    );
    assert_eq!(
        executor.receipts().len(),
        receipt_count_before,
        "GAS-01: raw system-call Err must not synthesize a receipt"
    );
}

#[test]
fn phase1_preexec_failure_must_consume_body0_or_abort() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let parent_hash = B256::with_last_byte(0xA1);
    let mut metadata = test_metadata();
    metadata.finalized_block_number = 1;
    metadata.finalized_block_hash = parent_hash;
    metadata.ordered_committee = vec![proposer];
    metadata.signer_bitmap = vec![1];

    let mut state = state_with_active_proposer(proposer);
    let chain_spec = test_chain_spec();
    let receipt_builder = reth_ethereum::evm::RethReceiptBuilder::default();
    let config = OutbeEvmConfig::new(
        chain_spec.clone(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(signer.clone());
    let evm = config.evm_with_env(&mut state, test_evm_env(2, REWARDS_ADDRESS));
    let mut ctx = execution_ctx(Some(3), Bytes::new());
    ctx.inner.parent_hash = parent_hash;
    ctx.parent_consensus_metadata = Some(metadata.clone());
    let mut executor = OutbeBlockExecutor::new(
        EthBlockExecutor::new(evm, ctx.inner.clone(), &chain_spec, &receipt_builder),
        {
            let mut inputs = fixtures::executor_inputs_from_ctx(&ctx, Some(signer.clone()), false);
            inputs.identity.parent_hash = parent_hash;
            inputs.system_plan.proposer_evm_address = Some(proposer);
            inputs.system_plan.execute_outbe_block_hooks = true;
            inputs.system_plan.prebuilt_phase1_tx = None;
            inputs.parent_accounting.parent_artifact_hint = Some(AccountedParentArtifact {
                summary: ExecutionSummaryArtifact {
                    validator_fee_sum: U256::ZERO,
                },
                timestamp: 1,
                state_root: None,
            });
            inputs
        },
    );
    executor.fsm = outbe_evm::system_tx::BlockExecutionFsm::for_block(
        2,
        Default::default(),
        &outbe_runtime::BLOCK_ORDER,
    );

    let block_artifacts = OutbeBlockArtifacts::default();
    let preexec_err = outbe_evm::factory::with_forced_outbe_system_call_error(|| {
        executor.apply_phase1_commit_in_preexec(2, &block_artifacts)
    })
    .expect_err("GAS-02: forced system-call error in Phase 1 pre-exec must abort with Err");
    assert!(
        preexec_err
            .to_string()
            .contains("forced Outbe system-call error"),
        "GAS-02: unexpected pre-exec error: {preexec_err}"
    );
    assert!(
        executor.receipts().is_empty(),
        "GAS-02: failed Phase 1 pre-exec must not emit a receipt"
    );

    let phase1_tx = begin_system_txs_for_test(
        &config,
        BeginBlockFixture {
            block_number: 2,
            parent_hash,
            extra_data: &Bytes::new(),
            parent_consensus_metadata: Some(metadata),
            proposer,
            bootstrap: BootstrapFixture::StandardForBlock,
        },
    )
    .into_iter()
    .next()
    .expect("Phase 1 system tx should be present");
    let body0_err = outbe_evm::factory::with_forced_outbe_system_call_error(|| {
        executor.execute_transaction(phase1_tx)
    })
    .expect_err("GAS-02: forced system-call error on body[0] must also abort with Err");
    assert!(
        body0_err
            .to_string()
            .contains("forced Outbe system-call error"),
        "GAS-02: unexpected body[0] error: {body0_err}"
    );
    assert!(
        executor.receipts().is_empty(),
        "GAS-02: Phase 1 failure must never leave a synthetic receipt"
    );
}

#[test]
fn without_commit_reserved_system_tx_must_not_use_user_lane_admission() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let mut state = state_with_active_proposer(proposer);
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(signer.clone());
    let evm = config.evm_with_env(&mut state, test_evm_env(1, REWARDS_ADDRESS));
    let mut executor = config.create_executor(evm, execution_ctx(Some(1), Bytes::new()));
    executor
        .apply_pre_execution_changes()
        .expect("pre-execution changes should apply");
    let system_tx = begin_system_txs_for_test(
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
    .into_iter()
    .next()
    .expect("CycleTick system tx should be present");

    let result = executor.execute_transaction_without_commit(system_tx);
    let Err(err) = result else {
        panic!("reserved system tx without_commit must not be accepted as a user tx");
    };
    let msg = err.to_string();
    assert!(
        msg.contains("reserved system transaction")
            || msg.contains("Outbe system tx without_commit"),
        "GAS-03: without_commit rejected through the wrong lane or wrong error: {msg}"
    );
}

#[test]
fn reverted_noncritical_begin_zone_system_tx_soft_fails_and_keeps_user_lane_clean() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let user_tx = test_regular_tx()
        .try_into_recovered()
        .expect("regular tx signer should recover");
    let mut state =
        state_with_active_validators_seeded(&[(proposer, dummy_pubkey(0xA2))], |storage| {
            seed_valid_tee_nodes(storage, &[proposer]);
        });
    state.database.insert_account_info(
        Address::from(*user_tx.signer()),
        AccountInfo {
            balance: U256::from(1_000_000u64),
            ..Default::default()
        },
    );
    let chain_spec = test_chain_spec();
    let config = OutbeEvmConfig::new(
        chain_spec.clone(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(signer.clone());
    let parent_hash = B256::with_last_byte(0x11);
    let (ctx, metadata) = block_two_execution_ctx(Some(1), Bytes::new(), proposer, parent_hash);
    let evm = config.evm_with_env(&mut state, test_evm_env(2, REWARDS_ADDRESS));
    let mut executor = config.create_executor(evm, ctx);

    super::with_phase1_verify_disabled(|| {
        executor
            .apply_pre_execution_changes()
            .expect("pre-execution changes should apply");
    });
    let mut system_txs = begin_system_txs_for_test(
        &config,
        BeginBlockFixture {
            block_number: 2,
            parent_hash,
            extra_data: &Bytes::new(),
            parent_consensus_metadata: Some(metadata),
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
    let oracle_slash_tx = system_txs
        .next()
        .expect("OracleSlash system tx should be present at index 9");
    let hyperlane_slash_tx = system_txs
        .next()
        .expect("HyperlaneSlash system tx should be present at index 10");
    let cycle_tx = system_txs
        .next()
        .expect("CycleTick system tx should be present at index 11");
    assert_eq!(oracle_slash_tx.tx().gas_limit(), 0);

    // OracleSlash has revert_fails_block == false, so an ordinary revert records
    // a 0-gas failure receipt and later begin-zone/user transactions still execute.
    let revert_gas = outbe_evm::factory::with_forced_outbe_system_call_revert(|| {
        executor.execute_transaction(oracle_slash_tx)
    })
    .expect("OracleSlash revert should soft-fail")
    .tx_gas_used();
    assert_eq!(revert_gas, 0);
    let failure_receipt = executor
        .receipts()
        .get(9)
        .expect("reverted OracleSlash must emit a failure receipt at index 9");
    assert!(!failure_receipt.success);
    assert_eq!(failure_receipt.cumulative_gas_used, 0);
    assert_eq!(failure_receipt.logs.len(), 1);
    assert_eq!(failure_receipt.logs[0].address, OUTBE_SYSTEM_TX_ADDRESS);
    assert_eq!(
        failure_receipt.logs[0].data.topics().first(),
        Some(&outbe_evm::failure_receipt::OUTBE_FAILURE_TOPIC0),
        "GAS-11: system revert soft-failure receipt must carry OutbeFailure"
    );
    let mut expected_code_topic = [0u8; 32];
    expected_code_topic[31] = 201;
    assert_eq!(
        failure_receipt.logs[0].data.topics()[1].as_slice(),
        expected_code_topic,
        "GAS-11: system revert soft-failure receipt must use OutbeFailure code 201"
    );
    let expected_reason = concat!(
        "system tx oracle_slash did not succeed at body_index=9: ",
        "Revert { gas: ResultGas { total_gas_spent: 42000, state_gas_spent: 0, ",
        "refunded: 0, floor_gas: 0 }, logs: [], output: ",
        "0x666f72636564204f757462652073797374656d2d63616c6c20726576657274 }"
    );
    let mut expected_log_data = vec![0u8; 64];
    expected_log_data[31] = 32;
    expected_log_data[32..64]
        .copy_from_slice(&U256::from(expected_reason.len()).to_be_bytes::<32>());
    expected_log_data.extend_from_slice(expected_reason.as_bytes());
    expected_log_data.resize(64 + expected_reason.len().div_ceil(32) * 32, 0);
    assert_eq!(
        failure_receipt.logs[0].data.data.as_ref(),
        expected_log_data.as_slice(),
        "GAS-11: soft-failure reason bytes must remain unchanged"
    );

    // HyperlaneSlash also has revert_fails_block == false: an ordinary revert emits a 0-gas
    // failure receipt and continues to CycleTick and user transactions.
    let hyperlane_gas = outbe_evm::factory::with_forced_outbe_system_call_revert(|| {
        executor.execute_transaction(hyperlane_slash_tx)
    })
    .expect("HyperlaneSlash revert should also soft-fail")
    .tx_gas_used();
    assert_eq!(hyperlane_gas, 0);
    assert!(!executor.receipts()[10].success);
    assert_eq!(executor.receipts()[10].cumulative_gas_used, 0);

    let cycle_gas = executor
        .execute_transaction(cycle_tx)
        .expect("CycleTick should execute after the soft failure")
        .tx_gas_used();
    assert_eq!(cycle_gas, 0);
    let user_gas = executor
        .execute_transaction(user_tx)
        .expect("user txs must execute after a soft-failed non-critical begin-zone system tx")
        .tx_gas_used();
    assert_eq!(
        executor.inner.cumulative_tx_gas_used, user_gas,
        "GAS-11: soft-failed system tx must contribute 0 gas to cumulative block gas"
    );
}

enum CriticalCycleFault {
    Revert,
    OutOfGas,
}

fn execute_critical_cycle_fault(
    fault: CriticalCycleFault,
) -> (
    Result<(), alloy_evm::block::BlockExecutionError>,
    Vec<Receipt>,
) {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let mut state = state_with_active_proposer(proposer);
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(signer.clone());
    let evm = config.evm_with_env(&mut state, test_evm_env(1, REWARDS_ADDRESS));
    let mut executor = config.create_executor(evm, block_one_execution_ctx(Some(1), Bytes::new()));
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
    let cycle_tx = system_txs
        .pop()
        .expect("CycleTick system tx should be last in begin prefix");
    for tx in system_txs {
        executor
            .execute_transaction(tx)
            .expect("earlier begin system tx should succeed");
    }
    let receipts_before = executor.receipts().len();

    let result = match fault {
        CriticalCycleFault::Revert => {
            outbe_evm::factory::with_forced_outbe_system_call_revert(|| {
                executor.execute_transaction(cycle_tx).map(|_| ())
            })
        }
        CriticalCycleFault::OutOfGas => {
            outbe_evm::factory::with_forced_outbe_system_call_oog_halt(|| {
                executor.execute_transaction(cycle_tx).map(|_| ())
            })
        }
    };
    assert_eq!(executor.receipts().len(), receipts_before);
    (result, Vec::new())
}

/// A revert in a consensus-critical begin-zone phase (here
/// CycleTick) is a hard block failure, not a soft-receipt skip. Its one-shot
/// work (a day's emission / terminal Metadosis) must never be silently
/// dropped. The executor pushes no receipt, and the block aborts.
#[test]
fn critical_cycle_tick_revert_fails_block() {
    let (result, receipts) = execute_critical_cycle_fault(CriticalCycleFault::Revert);
    let err = result.expect_err("a revert in the critical CycleTick phase must fail the block");
    assert!(
        err.to_string()
            .contains("critical system tx cycle_tick did not succeed"),
        "unexpected error: {err}"
    );
    assert!(
        receipts.is_empty(),
        "a critical-phase revert must not push a soft receipt"
    );
}

/// With a stale reward price at the UTC-day boundary, CycleTick credits the
/// validator pool and the user-lane feeder vote executes.
#[test]
fn stale_oracle_cycle_credits_validator_reward_and_allows_later_feeder_vote() {
    const GENESIS_TS: u64 = 1_704_067_200;
    use outbe_primitives::time::SECONDS_PER_DAY;
    const PREVIOUS_DAY: u32 = 20_240_101;

    #[derive(Debug, Eq, PartialEq)]
    struct Observation {
        active_day: u32,
        last_executed_at: u64,
        day_settled: bool,
        voter_claimable: U256,
        voter_gems: u32,
        feeder_vote_exists: bool,
        formation_exists: bool,
    }

    fn run_once() -> Observation {
        let signer = test_evm_signer();
        let proposer = signer.address();
        let proposer_key = dummy_pubkey(0xA2);
        let feeder_vote = test_oracle_submit_vote_tx()
            .try_into_recovered()
            .expect("test feeder vote signer should recover");
        let feeder = Address::from(*feeder_vote.signer());
        let voter = Address::repeat_byte(0x71);
        let emission_trigger = outbe_cycle::triggers::TriggerId::ProtocolCycle.as_u32();

        let mut state = state_with_active_validators_seeded_at_block_with_cycle_frames(
            &[(proposer, proposer_key)],
            1,
            4,
            |storage| {
                seed_cycle_tick_genesis(storage.clone(), GENESIS_TS, proposer).unwrap();
                seed_valid_tee_nodes(storage.clone(), &[proposer]);

                let rewards = outbe_rewards::schema::Rewards::new(storage.clone());
                rewards.day_voter_count.write(&PREVIOUS_DAY, 1).unwrap();
                rewards
                    .day_total_votes
                    .write(&PREVIOUS_DAY, U256::from(1))
                    .unwrap();
                rewards
                    .day_voter_at
                    .get_nested(&PREVIOUS_DAY)
                    .write(&0, voter)
                    .unwrap();
                rewards
                    .day_votes
                    .get_nested(&PREVIOUS_DAY)
                    .write(&voter, 1)
                    .unwrap();

                let mut validator_set =
                    outbe_validatorset::contract::ValidatorSet::new(storage.clone());
                validator_set
                    .set_delegate(
                        proposer,
                        outbe_validatorset::delegation::ValidatorDelegateRole::Oracle,
                        feeder,
                    )
                    .unwrap();

                // The shared state fixture seeds COEN/840 at timestamp zero.
                // That is intentionally stale under the live six-hour policy.
                let (.., pair_index) =
                    outbe_oracle::api::require_coen_pair(storage.clone(), 840).unwrap();
                let oracle = outbe_oracle::schema::OracleContract::new(storage);
                assert_eq!(oracle.exchange_rate_timestamp.read(&pair_index).unwrap(), 0);
            },
        );

        let block_timestamp = GENESIS_TS + SECONDS_PER_DAY + 60;
        let parent_hash = B256::with_last_byte(0x12);
        let (mut execution, metadata) =
            block_two_execution_ctx(None, Bytes::new(), proposer, parent_hash);
        execution.parent_artifact_hint.as_mut().unwrap().timestamp = block_timestamp - 30;
        let mut evm_env = test_evm_env(2, REWARDS_ADDRESS);
        evm_env.block_env.timestamp = U256::from(block_timestamp);
        let config = OutbeEvmConfig::new(
            test_chain_spec_with_genesis_timestamp(GENESIS_TS),
            std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
        )
        .with_evm_signer(signer.clone());
        let begin = begin_system_txs_for_test(
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
        let mut body = begin.clone();
        body.push(feeder_vote);

        let evm = config.evm_with_env(&mut state, evm_env);
        execution.inner.tx_count_hint = Some(body.len());
        execution.expected_begin_system_txs = begin;
        let mut executor = config.create_executor(evm, execution);
        super::with_phase1_verify_disabled(|| {
            executor
                .apply_pre_execution_changes()
                .expect("pre-execution hooks must succeed");
        });
        for tx in body {
            executor
                .execute_transaction(tx)
                .expect("stale reward price must not fail the block");
        }
        drop(executor);

        let read_ctx = BlockContext {
            block_gas_limit: 0,
            user_gas_used: 0,
            block_number: 2,
            timestamp: block_timestamp,
            chain_id: CHAIN_ID,
            genesis_hash: alloy_primitives::B256::ZERO,
            genesis_timestamp: block_timestamp.max(1),
            proposer,
            validators: vec![proposer],
        };
        let mut provider =
            outbe_primitives::storage::direct::DirectStorageProvider::new(&mut state, read_ctx);
        StorageHandle::enter(&mut provider, |storage| {
            let cycle = outbe_cycle::schema::Cycle::new(storage.clone());
            let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
            Observation {
                active_day: outbe_cycle::handler::derive_active_utc_day(
                    storage.clone(),
                    GENESIS_TS,
                    block_timestamp,
                )
                .unwrap(),
                last_executed_at: cycle.last_executed_at.read(&emission_trigger).unwrap(),
                day_settled: outbe_emission::api::get_emission_day_record(
                    storage.clone(),
                    PREVIOUS_DAY,
                )
                .unwrap()
                .is_some_and(|r| {
                    r.state == outbe_emission::schema::emission_day_state::PROCESSING
                        || r.state == outbe_emission::schema::emission_day_state::COMPLETED
                }),
                voter_claimable: outbe_agentreward::AgentRewardContract::new(storage.clone())
                    .get_pool_claimable_reward(
                        outbe_agentreward::schema::RewardPool::Validator,
                        voter,
                    )
                    .unwrap(),
                voter_gems: outbe_gem::api::balance_of(&storage, voter).unwrap(),
                feeder_vote_exists: oracle.vote_exists.read(&proposer).unwrap(),
                formation_exists: outbe_metadosis::api::emission_day(storage, PREVIOUS_DAY)
                    .unwrap()
                    .is_some(),
            }
        })
    }

    let first = run_once();
    assert_eq!(first.active_day, 20_240_102);
    assert_eq!(
        first.last_executed_at,
        GENESIS_TS + SECONDS_PER_DAY + outbe_cycle::triggers::PROTOCOL_CYCLE_START_OFFSET_SECONDS
    );
    assert!(first.day_settled);
    assert!(!first.voter_claimable.is_zero());
    assert_eq!(first.voter_gems, 0);
    assert!(first.feeder_vote_exists);
    assert!(first.formation_exists);

    let replay = run_once();
    assert_eq!(
        replay, first,
        "an exact execution from the same semantic pre-state must settle identically"
    );
}

/// An OOG halt in a consensus-critical begin-zone phase also
/// fails the block (not a soft skip), via the same `revert_fails_block` gate.
#[test]
fn critical_cycle_tick_oog_fails_block() {
    let (result, receipts) = execute_critical_cycle_fault(CriticalCycleFault::OutOfGas);
    let err = result.expect_err("an OOG halt in the critical CycleTick phase must fail the block");
    assert!(
        err.to_string()
            .contains("critical system tx cycle_tick did not succeed"),
        "unexpected error: {err}"
    );
    assert!(
        receipts.is_empty(),
        "a critical-phase OOG halt must not push a soft receipt"
    );
}
