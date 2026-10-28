use super::*;

fn test_reserved_system_address_tx() -> reth_ethereum::TransactionSigned {
    TxEip1559 {
        chain_id: CHAIN_ID,
        nonce: 0,
        gas_limit: 100_000,
        max_fee_per_gas: MIN_PROTOCOL_BASE_FEE as u128,
        max_priority_fee_per_gas: 0,
        to: TxKind::Call(OUTBE_SYSTEM_TX_ADDRESS),
        value: U256::ZERO,
        input: Bytes::new(),
        access_list: Default::default(),
    }
    .into_signed(Signature::test_signature())
    .into()
}

#[test]
fn executor_rejects_user_tx_to_reserved_system_address() {
    let config = OutbeEvmConfig::new(test_chain_spec());
    let reserved_tx = test_reserved_system_address_tx()
        .try_into_recovered()
        .expect("reserved-address tx signer should recover");

    let mut db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
    db.insert_account_info(
        reserved_tx.signer(),
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
            basefee: 1_000_000_000,
            beneficiary: OWNER,
            timestamp: U256::from(1u64),
            ..Default::default()
        },
    };
    let evm = config.evm_with_env(&mut state, evm_env);
    let ctx = execution_ctx(Some(1), Bytes::new());
    let mut executor = config.create_executor(evm, ctx);

    let err = executor
        .execute_transaction(reserved_tx)
        .expect_err("user tx to reserved system address must be rejected");

    let err = err.to_string();
    assert!(
        err.contains("reserved system transaction address") || err.contains("decode system tx"),
        "unexpected reserved-address rejection error: {err}"
    );
    assert!(executor.receipts().is_empty());
}

#[test]
fn gas_01_evm_level_system_tx_err_must_not_be_soft_receipted() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let mut state = state_with_active_proposer(proposer);
    let config = OutbeEvmConfig::new(test_chain_spec()).with_evm_signer(signer.clone());
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
    let err = crate::factory::with_forced_outbe_system_call_error(|| {
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
fn gas_02_phase1_preexec_failure_must_consume_body0_or_abort() {
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
    let config = OutbeEvmConfig::new(chain_spec.clone()).with_evm_signer(signer.clone());
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
    executor.system_tx_phase_cursor = crate::system_tx::SystemTxPhase::initial_for_block(
        2,
        crate::system_tx::GENESIS_BOOTSTRAP_BLOCK_NUMBER,
    );

    let block_artifacts = OutbeBlockArtifacts::default();
    let preexec_err = crate::factory::with_forced_outbe_system_call_error(|| {
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
    let body0_err = crate::factory::with_forced_outbe_system_call_error(|| {
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
fn gas_03_without_commit_reserved_system_tx_must_not_use_user_lane_admission() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let mut state = state_with_active_proposer(proposer);
    let config = OutbeEvmConfig::new(test_chain_spec()).with_evm_signer(signer.clone());
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
fn gas_11_reverted_noncritical_begin_zone_system_tx_soft_fails_and_keeps_user_lane_clean() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let user_tx = test_regular_tx()
        .try_into_recovered()
        .expect("regular tx signer should recover");
    let mut state =
        state_with_active_validators_seeded(&[(proposer, dummy_pubkey(0xA2))], |storage| {
            let seed_context = BlockContext::new(0, 1, CHAIN_ID, proposer, vec![proposer]);
            let ctx = BlockRuntimeContext::new(seed_context, storage);
            outbe_rewards::runtime::ensure_genesis_anchor(&ctx).unwrap();
        });
    state.database.insert_account_info(
        Address::from(*user_tx.signer()),
        AccountInfo {
            balance: U256::from(1_000_000u64),
            ..Default::default()
        },
    );
    let chain_spec = test_chain_spec();
    let receipt_builder = reth_ethereum::evm::RethReceiptBuilder::default();
    let config = OutbeEvmConfig::new(chain_spec.clone()).with_evm_signer(signer.clone());
    let evm = config.evm_with_env(&mut state, test_evm_env(1, REWARDS_ADDRESS));
    let ctx = block_one_execution_ctx(Some(1), Bytes::new());
    let mut executor = OutbeBlockExecutor::new(
        EthBlockExecutor::new(evm, ctx.inner.clone(), &chain_spec, &receipt_builder),
        fixtures::executor_inputs_with_bootstrap(&ctx, Some(signer.clone()), false),
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
    let tee_bootstrap_tx = system_txs
        .next()
        .expect("TeeBootstrap system tx should be present");
    let oracle_tx = system_txs
        .next()
        .expect("OracleSlashWindow system tx should be present");
    let hook_events_tx = system_txs
        .next()
        .expect("HookEvents system tx should be present");
    let oracle_signed_gas = oracle_tx.tx().gas_limit();
    let oracle_intrinsic_gas =
        outbe_primitives::system_tx::system_tx_intrinsic_gas(oracle_tx.tx().input())
            .expect("OracleSlashWindow intrinsic gas computes");

    // CycleTick and TeeBootstrap are consensus-critical. OracleSlashWindow is
    // soft, so an ordinary revert records a failure receipt and later
    // begin-zone/user transactions still execute.
    let cycle_gas = executor
        .execute_transaction(cycle_tx)
        .expect("CycleTick should execute successfully")
        .tx_gas_used();
    let tee_bootstrap_gas = executor
        .execute_transaction(tee_bootstrap_tx)
        .expect("mandatory TeeBootstrap should execute before the non-critical phase")
        .tx_gas_used();
    let revert_gas = crate::factory::with_forced_outbe_system_call_revert(|| {
        executor.execute_transaction(oracle_tx)
    })
    .expect("OracleSlashWindow revert should soft-fail")
    .tx_gas_used();
    // The envelope signs only its intrinsic gas, and its failure receipt charges exactly that.
    assert_eq!(oracle_signed_gas, oracle_intrinsic_gas);
    assert_eq!(
        revert_gas, oracle_intrinsic_gas,
        "GAS-11: reverted OracleSlashWindow charges {revert_gas}, expected {oracle_intrinsic_gas}"
    );
    let failure_receipt = executor
        .receipts()
        .get(2)
        .expect("reverted OracleSlashWindow must emit a failure receipt");
    assert!(!failure_receipt.success);
    assert_eq!(
        failure_receipt.cumulative_gas_used,
        cycle_gas + tee_bootstrap_gas + revert_gas
    );
    assert_eq!(failure_receipt.logs.len(), 1);
    assert_eq!(failure_receipt.logs[0].address, OUTBE_SYSTEM_TX_ADDRESS);
    assert_eq!(
        failure_receipt.logs[0].data.topics().first(),
        Some(&crate::failure_receipt::OUTBE_FAILURE_TOPIC0),
        "GAS-11: system revert soft-failure receipt must carry OutbeFailure"
    );
    let mut expected_code_topic = [0u8; 32];
    expected_code_topic[31] = 201;
    assert_eq!(
        failure_receipt.logs[0].data.topics()[1].as_slice(),
        expected_code_topic,
        "GAS-11: system revert soft-failure receipt must use OutbeFailure code 201"
    );

    let hook_events_gas = executor
        .execute_transaction(hook_events_tx)
        .expect("HookEvents should execute after the soft failure")
        .tx_gas_used();
    let user_gas = executor
        .execute_transaction(user_tx)
        .expect("user txs must execute after a soft-failed non-critical begin-zone system tx")
        .tx_gas_used();
    assert_eq!(
        executor.inner.cumulative_tx_gas_used,
        cycle_gas + tee_bootstrap_gas + revert_gas + hook_events_gas + user_gas,
        "GAS-11: soft-failed system tx must charge only its published visible gas"
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
    let config = OutbeEvmConfig::new(test_chain_spec()).with_evm_signer(signer.clone());
    let evm = config.evm_with_env(&mut state, test_evm_env(1, REWARDS_ADDRESS));
    let mut executor = config.create_executor(evm, block_one_execution_ctx(Some(1), Bytes::new()));
    executor
        .apply_pre_execution_changes()
        .expect("pre-execution changes should apply");
    let cycle_tx = begin_system_txs_for_test(
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

    let result = match fault {
        CriticalCycleFault::Revert => crate::factory::with_forced_outbe_system_call_revert(|| {
            executor.execute_transaction(cycle_tx).map(|_| ())
        }),
        CriticalCycleFault::OutOfGas => {
            crate::factory::with_forced_outbe_system_call_oog_halt(|| {
                executor.execute_transaction(cycle_tx).map(|_| ())
            })
        }
    };
    (result, executor.receipts().to_vec())
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
            .contains("critical system tx CycleTick did not succeed"),
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
        voter_gems: u64,
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
                let genesis_ctx = BlockRuntimeContext::new(
                    BlockContext::new(0, GENESIS_TS, CHAIN_ID, proposer, vec![proposer]),
                    storage.clone(),
                );
                outbe_rewards::runtime::ensure_genesis_anchor(&genesis_ctx).unwrap();

                let cycle = outbe_cycle::schema::Cycle::new(storage.clone());
                cycle
                    .last_executed_at
                    .write(&emission_trigger, GENESIS_TS + 60)
                    .unwrap();

                let rewards = outbe_rewards::schema::Rewards::new(storage.clone());
                rewards.daily_voter_count.write(&PREVIOUS_DAY, 1).unwrap();
                rewards
                    .daily_voter_at
                    .get_nested(&PREVIOUS_DAY)
                    .write(&0, voter)
                    .unwrap();
                rewards
                    .daily_participation
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
        let tee_bootstrap = sample_tee_bootstrap_payload_at(1, block_timestamp);
        let mut evm_env = test_evm_env(1, REWARDS_ADDRESS);
        evm_env.block_env.timestamp = U256::from(block_timestamp);
        let config = OutbeEvmConfig::new(test_chain_spec_with_genesis_timestamp(GENESIS_TS))
            .with_evm_signer(signer.clone());
        let begin = begin_system_txs_for_test(
            &config,
            BeginBlockFixture {
                block_number: 1,
                parent_hash: B256::ZERO,
                extra_data: &Bytes::new(),
                parent_consensus_metadata: None,
                proposer,
                bootstrap: BootstrapFixture::explicit(Some(tee_bootstrap.clone())),
            },
        );
        let mut body = begin.clone();
        body.push(feeder_vote);

        let evm = config.evm_with_env(&mut state, evm_env);
        let mut execution =
            execution_ctx_with_tee_bootstrap(Some(body.len()), Bytes::new(), tee_bootstrap);
        execution.expected_begin_system_txs = begin;
        execution.proposer_evm_address = Some(proposer);
        let mut executor = config.create_executor(evm, execution);
        executor
            .apply_pre_execution_changes()
            .expect("pre-execution hooks must succeed");
        for tx in body {
            executor
                .execute_transaction(tx)
                .expect("stale reward price must not fail the block");
        }
        drop(executor);

        let read_ctx = BlockContext::new(1, block_timestamp, CHAIN_ID, proposer, vec![proposer]);
        let mut provider =
            outbe_primitives::storage::direct::DirectStorageProvider::new(&mut state, read_ctx);
        StorageHandle::enter(&mut provider, |storage| {
            let cycle = outbe_cycle::schema::Cycle::new(storage.clone());
            let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
            let gem = outbe_gem::GemContract::new(storage.clone());
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
                voter_gems: gem.total_supply().unwrap(),
                feeder_vote_exists: oracle.vote_exists.read(&proposer).unwrap(),
                formation_exists: outbe_metadosis::api::emission_day(storage, PREVIOUS_DAY)
                    .unwrap()
                    .is_some(),
            }
        })
    }

    let first = run_once();
    assert_eq!(first.active_day, 20_240_102);
    assert_eq!(first.last_executed_at, GENESIS_TS + SECONDS_PER_DAY);
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
            .contains("critical system tx CycleTick did not succeed"),
        "unexpected error: {err}"
    );
    assert!(
        receipts.is_empty(),
        "a critical-phase OOG halt must not push a soft receipt"
    );
}

#[test]
fn verifier_rejects_begin_system_tx_signature_hash_mismatch() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let mut state = state_with_active_proposer(proposer);
    let evm_env = test_evm_env(1, REWARDS_ADDRESS);
    let config = OutbeEvmConfig::new(test_chain_spec()).with_evm_signer(signer.clone());
    let evm = config.evm_with_env(&mut state, evm_env);

    let wrong_unsigned = build_unsigned_system_tx(
        SystemTxKind::CycleTick,
        0,
        2,
        CHAIN_ID,
        SystemTxInputV2::CycleTick.encode().unwrap(),
    )
    .unwrap();
    let wrong_signed = signer.sign_unsigned(wrong_unsigned).unwrap();
    let wrong_recovered = reth_primitives_traits::Recovered::new_unchecked(wrong_signed, proposer);
    let canonical = begin_system_txs_for_test(
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
    let mut expected = vec![wrong_recovered.clone()];
    expected.extend(canonical.into_iter().skip(1));
    let mut ctx = execution_ctx(Some(1), Bytes::new());
    ctx.expected_begin_system_txs = expected;

    let mut executor = config.create_executor(evm, ctx);
    executor
        .apply_pre_execution_changes()
        .expect("pre-execution changes should apply before verifier tx loop");
    let err = executor
        .execute_transaction(wrong_recovered)
        .expect_err("verifier must reject mismatched system tx signature hash");

    assert!(err.to_string().contains("signature_hash mismatch"));
    assert!(executor.receipts().is_empty());
}

#[test]
fn verifier_rejects_boundary_outcome_system_tx_artifact_mismatch() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let mut state = state_with_active_proposer(proposer);
    let evm_env = test_evm_env(1, REWARDS_ADDRESS);
    let config = OutbeEvmConfig::new(test_chain_spec());
    let evm = config.evm_with_env(&mut state, evm_env);

    let header_artifact = boundary_with(true, vec![(proposer, dummy_pubkey(0xA2))]);
    let mut tx_artifact = header_artifact.clone();
    tx_artifact.dkg_cycle = 1;
    let extra_data = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        radicle_publication_enabled: false,
        radicle_publication: None,
        execution_summary: None,
        consensus_header_artifact: Some(ConsensusHeaderArtifact::BoundaryOutcome(header_artifact)),
        timestamp_millis_part: 0,
        late_finalize_credits: None,
        enclave_witness: None,
    })
    .expect("extra_data encodes");

    let cycle_unsigned = build_unsigned_system_tx(
        SystemTxKind::CycleTick,
        0,
        1,
        CHAIN_ID,
        SystemTxInputV2::CycleTick.encode().unwrap(),
    )
    .unwrap();
    let boundary_unsigned = build_unsigned_system_tx(
        SystemTxKind::BoundaryOutcome,
        1,
        1,
        CHAIN_ID,
        SystemTxInputV2::BoundaryOutcome {
            artifact: tx_artifact,
        }
        .encode()
        .unwrap(),
    )
    .unwrap();
    let cycle_signed = signer.sign_unsigned(cycle_unsigned).unwrap();
    let boundary_signed = signer.sign_unsigned(boundary_unsigned).unwrap();
    let cycle_recovered = reth_primitives_traits::Recovered::new_unchecked(cycle_signed, proposer);
    let boundary_recovered =
        reth_primitives_traits::Recovered::new_unchecked(boundary_signed, proposer);
    let mut ctx = execution_ctx(Some(2), extra_data);
    ctx.expected_begin_system_txs = vec![cycle_recovered.clone(), boundary_recovered.clone()];
    ctx.proposer_evm_address = Some(proposer);

    let mut executor = config.create_executor(evm, ctx);
    executor
        .apply_pre_execution_changes()
        .expect("pre-execution changes should apply before verifier tx loop");
    let err = executor
        .execute_transaction(cycle_recovered)
        .expect_err("verifier must reject BoundaryOutcome tx/header mismatch");

    assert!(err
        .to_string()
        .contains("BoundaryOutcome system tx artifact mismatch"));
    assert!(executor.receipts().is_empty());
}

#[test]
fn verifier_rejects_begin_system_tx_signer_mismatch() {
    let proposer_signer = test_evm_signer();
    let proposer = proposer_signer.address();
    let wrong_signer = Arc::new(OutbeEvmSigner::from_secret_bytes([2u8; 32]).unwrap());
    let mut state = state_with_active_proposer(proposer);
    let evm_env = test_evm_env(1, REWARDS_ADDRESS);
    let config = OutbeEvmConfig::new(test_chain_spec()).with_evm_signer(proposer_signer.clone());
    let evm = config.evm_with_env(&mut state, evm_env);

    let canonical = begin_system_txs_for_test(
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
    let cycle_input = SystemTxInputV2::CycleTick.encode().unwrap();
    let unsigned = build_unsigned_system_tx_with_gas_limit(
        outbe_primitives::system_tx::SystemTxEnvelopeInput {
            kind: SystemTxKind::CycleTick,
            ordinal: 0,
            block_number: 1,
            chain_id: CHAIN_ID,
            calldata: cycle_input,
            gas_limit: canonical[0].tx().gas_limit(),
        },
    )
    .unwrap();
    let wrong_signed = wrong_signer.sign_unsigned(unsigned).unwrap();
    let wrong_recovered =
        reth_primitives_traits::Recovered::new_unchecked(wrong_signed, wrong_signer.address());
    let mut expected = vec![wrong_recovered.clone()];
    expected.extend(canonical.into_iter().skip(1));
    let mut ctx = execution_ctx(Some(1), Bytes::new());
    ctx.expected_begin_system_txs = expected;
    ctx.proposer_evm_address = Some(proposer);

    let mut executor = config.create_executor(evm, ctx);
    executor
        .apply_pre_execution_changes()
        .expect("pre-execution changes should apply before verifier tx loop");
    let err = executor
        .execute_transaction(wrong_recovered)
        .expect_err("verifier must reject system tx signed by non-proposer");

    assert!(err.to_string().contains("system tx signer mismatch"));
    assert!(executor.receipts().is_empty());
}

#[test]
fn corrupt_unbonding_aborts_the_whole_hook_with_typed_validator_cause() {
    for corrupt_first in [false, true] {
        assert_corrupt_unbonding_hook_rollback(corrupt_first);
    }
}

fn assert_corrupt_unbonding_hook_rollback(corrupt_first: bool) {
    use outbe_staking::{contract::Staking, errors::StakingError};
    use outbe_validatorset::contract::ValidatorSet;

    let healthy = Address::repeat_byte(0x71);
    let corrupt = Address::repeat_byte(0x72);
    let order = if corrupt_first {
        [corrupt, healthy]
    } else {
        [healthy, corrupt]
    };
    let db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
    let mut state = State::builder()
        .with_database(db)
        .with_bundle_update()
        .build();
    let setup = BlockContext::new(7, 1_000, CHAIN_ID, OWNER, Vec::new());
    super::run_atomic_storage_hooks(&mut state, setup, |ctx| {
        for address in [
            outbe_primitives::addresses::VALIDATOR_SET_ADDRESS,
            STAKING_ADDRESS,
        ] {
            ctx.storage
                .set_code(address, Bytecode::new_raw(Bytes::from_static(&[0xef])))?;
        }
        let mut vs = ValidatorSet::new(ctx.storage.clone());
        vs.test_configure_registry(OWNER)?;
        vs.test_seed_authoritative_min_stake(U256::from(1_000u64))?;
        for (index, validator) in order.into_iter().enumerate() {
            vs.test_register_active_validator(
                validator,
                &dummy_pubkey(index as u8 + 1),
                U256::from(1_000u64),
            )?;
        }
        for validator in order {
            vs.deactivate_validator(OWNER, validator)?;
        }
        vs.test_activate_validated_boundary_set(&[], B256::ZERO, 7)?;
        let staking = Staking::new(ctx.storage.clone());
        staking.config_unbonding_period.write(3_600)?;
        staking.stake_amount.write(&healthy, U256::from(1_000u64))?;
        staking.stake_amount.write(&corrupt, U256::from(2_000u64))?;
        staking.total_staked.write(U256::from(1_500u64))?;
        ctx.storage
            .set_balance(STAKING_ADDRESS, U256::from(3_000u64))?;
        for validator in [healthy, corrupt] {
            assert!(matches!(
                vs.validator_lifecycle(validator)?,
                outbe_validatorset::ValidatorLifecycle::Unbonding(_)
            ));
        }
        Ok(())
    })
    .expect("unbonding fixture");

    let run = BlockContext::new(8, 2_000, CHAIN_ID, OWNER, Vec::new());
    let error = super::run_atomic_storage_hooks(&mut state, run, |ctx| {
        assert_eq!(
            ValidatorSet::new(ctx.storage.clone()).registered_validator_addresses()?,
            order
        );
        ctx.storage
            .transfer_balance(STAKING_ADDRESS, healthy, U256::from(50u64))?;
        ctx.storage.emit_event(
            STAKING_ADDRESS,
            alloy_primitives::LogData::new_unchecked(
                Vec::new(),
                Bytes::from_static(b"earlier hook"),
            ),
        )?;
        let result = outbe_staking::hooks::process_unbonding(ctx.storage.clone(), 2_000);
        let staking = Staking::new(ctx.storage.clone());
        assert_eq!(
            staking.unbonding_count.read()?,
            u32::from(!corrupt_first),
            "unbonding result: {result:?}"
        );
        assert_eq!(
            staking.stake_amount.read(&healthy)?,
            if corrupt_first {
                U256::from(1_000u64)
            } else {
                U256::ZERO
            }
        );
        result
    })
    .expect_err("corruption must abort the complete hook");
    let cause = error
        .as_internal()
        .and_then(|inner| inner.downcast_other::<outbe_primitives::error::PrecompileError>())
        .and_then(std::error::Error::source)
        .and_then(|cause| cause.downcast_ref::<StakingError>());
    assert!(
        matches!(cause, Some(StakingError::StakeAccountingUnderflow { validator }) if *validator == corrupt)
    );

    let read = BlockContext::new(8, 2_000, CHAIN_ID, OWNER, Vec::new());
    let (_, logs) = super::run_atomic_storage_hooks(&mut state, read, |ctx| {
        let staking = Staking::new(ctx.storage.clone());
        assert_eq!(staking.stake_amount.read(&healthy)?, U256::from(1_000u64));
        assert_eq!(staking.stake_amount.read(&corrupt)?, U256::from(2_000u64));
        assert_eq!(staking.total_staked.read()?, U256::from(1_500u64));
        assert_eq!(staking.unbonding_count.read()?, 0);
        assert_eq!(staking.per_val_unbonding_head.read(&healthy)?, 0);
        assert_eq!(staking.unbonding_amount.read(&0)?, U256::ZERO);
        assert_eq!(staking.unbonding_complete_time.read(&0)?, 0);
        assert_eq!(ctx.storage.balance(STAKING_ADDRESS)?, U256::from(3_000u64));
        assert_eq!(ctx.storage.balance(healthy)?, U256::ZERO);
        let vs = ValidatorSet::new(ctx.storage.clone());
        for validator in [healthy, corrupt] {
            assert!(matches!(
                vs.validator_lifecycle(validator)?,
                outbe_validatorset::ValidatorLifecycle::Unbonding(_)
            ));
        }
        Ok(())
    })
    .expect("rolled-back state remains readable");
    assert!(logs.is_empty());
}

#[test]
fn begin_block_hook_batch_rolls_back_code_and_reports_committed_code_changes() {
    let db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
    let mut state = State::builder()
        .with_database(db)
        .with_bundle_update()
        .build();
    let ctx = BlockContext::new(7, 84, CHAIN_ID, OWNER, Vec::new());
    let address = address!("0x1111111111111111111111111111111111111111");
    let slot = U256::from(0x46u64);
    let value = U256::from(0x193u64);
    let marker = Bytecode::new_raw(Bytes::from_static(&[0xef]));
    let marker_hash = marker.hash_slow();

    let err = super::run_atomic_storage_hooks(&mut state, ctx, |hook_ctx| {
        hook_ctx.storage.set_code(address, marker.clone())?;
        hook_ctx.storage.sstore(address, slot, value)?;
        assert_eq!(hook_ctx.storage.sload(address, slot)?, value);
        Err(outbe_primitives::error::PrecompileError::Fatal(
            "oracle hook failed".into(),
        ))
    })
    .expect_err("late hook failure must abort the whole hook batch");

    assert!(err.to_string().contains("oracle hook failed"));
    assert_eq!(state.storage(address, slot).unwrap(), U256::ZERO);
    assert!(
        state
            .basic(address)
            .unwrap()
            .is_none_or(|info| info.is_empty_code_hash()),
        "failed hook batch must not persist code"
    );

    let ctx = BlockContext::new(8, 96, CHAIN_ID, OWNER, Vec::new());
    let (changes, events) = super::run_atomic_storage_hooks(&mut state, ctx, |hook_ctx| {
        hook_ctx.storage.set_code(address, marker.clone())?;
        hook_ctx.storage.sstore(address, slot, value)?;
        Ok(())
    })
    .expect("successful hook batch must flush state");

    let account = changes
        .get(&address)
        .expect("successful batch must report changed account");
    assert_eq!(account.info.code_hash, marker_hash);
    assert_eq!(account.info.code, Some(marker));
    let changed_slot = account
        .storage
        .get(&slot)
        .expect("successful batch must report changed slot");
    assert_eq!(changed_slot.present_value(), value);
    assert!(events.is_empty());
}

#[test]
fn hook_readiness_error_keeps_its_type_across_the_executor_boundary() {
    let db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
    let mut state = State::builder()
        .with_database(db)
        .with_bundle_update()
        .build();
    let ctx = BlockContext::new(7, 84, CHAIN_ID, OWNER, Vec::new());

    let error = super::run_atomic_storage_hooks(&mut state, ctx, |_hook_ctx| {
        Err(outbe_primitives::error::PrecompileError::Storage(
            "native historical state is temporarily unavailable".into(),
        ))
    })
    .expect_err("native state unavailability must abort this payload execution");

    assert!(matches!(
        error
            .as_internal()
            .and_then(|inner| inner.downcast_other::<outbe_primitives::error::PrecompileError>()),
        Some(outbe_primitives::error::PrecompileError::Storage(_))
    ));
}
