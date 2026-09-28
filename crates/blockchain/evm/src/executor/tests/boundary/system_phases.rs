use super::super::*;
use super::fixtures::*;
#[test]
fn proposer_injects_tee_bootstrap_after_boundary_when_payload_pending() {
    use crate::system_tx::SystemTxKind;
    let signer = test_evm_signer();
    let proposer = signer.address();
    let config = OutbeEvmConfig::new(test_chain_spec()).with_evm_signer(signer);

    // Block 1, empty extra_data: begin-zone is CycleTick + RewardsGemDelivery
    // + OracleSlashWindow + HookEvents. A pending bootstrap is injected
    // between delivery and OracleSlashWindow.
    let with_bootstrap = begin_system_txs_for_test_with_bootstrap(
        &config,
        1,
        B256::ZERO,
        &Bytes::new(),
        None,
        proposer,
        Some(sample_tee_bootstrap_payload(1)),
    );
    assert_eq!(
        begin_system_tx_kinds(&with_bootstrap),
        vec![
            SystemTxKind::CycleTick,
            SystemTxKind::RewardsGemDelivery,
            SystemTxKind::TeeBootstrap,
            SystemTxKind::OracleSlashWindow,
            SystemTxKind::HookEvents,
        ],
        "proposer must inject TeeBootstrap before OracleSlashWindow",
    );

    // Block 1 is not buildable without the mandatory OST3 payload.
    let error = config
        .build_begin_system_txs(
            1,
            CHAIN_ID,
            outbe_primitives::system_tx::protocol_block_gas_limit(1),
            B256::ZERO,
            &Bytes::new(),
            None,
            Some(proposer),
            None,
            None,
        )
        .expect_err("block 1 without OST3 must fail closed");
    assert!(
        error
            .to_string()
            .contains("missing mandatory block-1 OST3 bootstrap payload"),
        "{error}"
    );

    // A pending OST3 at genesis is a startup invariant violation, not a value
    // that may be silently dropped.
    let error = config
        .build_begin_system_txs(
            0,
            CHAIN_ID,
            outbe_primitives::system_tx::protocol_block_gas_limit(0),
            B256::ZERO,
            &Bytes::new(),
            None,
            Some(proposer),
            None,
            Some(sample_tee_bootstrap_payload(1)),
        )
        .expect_err("OST3 may only be queued for block 1");
    assert!(
        error
            .to_string()
            .contains("OST3 bootstrap payload is invalid at genesis"),
        "{error}"
    );
}

#[test]
fn apply_pre_execution_changes_rejects_non_rewards_beneficiary() {
    let signer = test_evm_signer();
    let proposer = signer.address();

    let mut state = state_with_active_proposer(proposer);
    let evm_env = test_evm_env(1, OWNER);
    let config = OutbeEvmConfig::new(test_chain_spec()).with_evm_signer(signer.clone());
    let evm = config.evm_with_env(&mut state, evm_env);
    let ctx = execution_ctx(Some(1), Bytes::new());
    let mut executor = config.create_executor(evm, ctx);

    let err = executor
        .apply_pre_execution_changes()
        .expect_err("non-rewards beneficiary must be rejected");
    assert!(err
        .to_string()
        .contains("beneficiary must be REWARDS_ADDRESS"));
}

#[test]
fn boundary_activation_allows_registered_next_epoch_proposer() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let old_active_secret = [2; 32];
    let old_active = OutbeEvmSigner::from_secret_bytes(old_active_secret)
        .expect("old active test signer")
        .address();
    let mut state = state_with_active_and_registered_candidate(old_active, proposer);
    let evm_env = test_evm_env(1, REWARDS_ADDRESS);
    let boundary = boundary_with(
        true,
        vec![
            (old_active, dummy_pubkey(0xA2)),
            (proposer, dummy_pubkey(0xB3)),
        ],
    );
    let tee_bootstrap = sample_tee_bootstrap_payload_for(
        1,
        boundary.committee_set_hash,
        TEST_BLOCK_TIMESTAMP_BASE + 1 + 3_600,
        &[
            outbe_primitives::tee_test_utils::DevValidatorV1 {
                evm_secret: old_active_secret,
                bls_minpk_public: dummy_pubkey(0xA2),
            },
            outbe_primitives::tee_test_utils::DevValidatorV1 {
                evm_secret: [1; 32],
                bls_minpk_public: dummy_pubkey(0xB3),
            },
        ],
    );
    let extra_data = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        execution_summary: None,
        consensus_header_artifact: Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary)),
        timestamp_millis_part: 0,
        late_finalize_credits: None,
        compressed_entities_root: None,
    })
    .expect("extra_data encodes");
    let config = OutbeEvmConfig::new(test_chain_spec()).with_evm_signer(signer.clone());
    let evm = config.evm_with_env(&mut state, evm_env);
    let mut executor = config.create_executor(
        evm,
        execution_ctx_with_tee_bootstrap(Some(0), extra_data.clone(), tee_bootstrap.clone()),
    );

    executor
        .apply_pre_execution_changes()
        .expect("activation block pre-execution should apply");
    let system_txs = begin_system_txs_for_test_with_bootstrap(
        &config,
        1,
        B256::ZERO,
        &extra_data,
        None,
        proposer,
        Some(tee_bootstrap),
    );
    for tx in system_txs {
        executor
            .execute_transaction(tx)
            .expect("activation block begin-zone system tx should execute");
    }

    assert_eq!(executor.receipts().len(), 6);
    assert!(executor.receipts().iter().all(|receipt| receipt.success));
    drop(executor);

    let read_ctx = BlockContext::new(1, 1, CHAIN_ID, proposer, vec![proposer]);
    let mut provider =
        outbe_primitives::storage::direct::DirectStorageProvider::new(&mut state, read_ctx);
    StorageHandle::enter(&mut provider, |storage| {
        let vs = outbe_validatorset::contract::ValidatorSet::new(storage);
        assert!(vs.is_consensus_participant(proposer)?);
        let record = vs.get_validator(proposer)?.expect("candidate should exist");
        assert_eq!(record.blocks_proposed, 1);
        Ok::<_, outbe_primitives::error::PrecompileError>(())
    })
    .expect("validator state should be readable");
}

#[test]
fn full_begin_phases_then_user_tx_observes_boundary_activation() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let joining = address!("0x3333333333333333333333333333333333333333");
    let parent_hash = B256::with_last_byte(0xBC);
    let mut state = state_with_active_and_registered_candidate(proposer, joining);

    let mut metadata = test_metadata();
    metadata.finalized_block_number = 1;
    metadata.finalized_block_hash = parent_hash;
    metadata.ordered_committee = vec![proposer];
    metadata.signer_bitmap = vec![1];

    let bridge = ConsensusExecutionBridge::new();
    bridge.record_execution_summary_with_state_root(
        1,
        parent_hash,
        ExecutionSummaryArtifact {
            validator_fee_sum: U256::ZERO,
        },
        1,
        B256::repeat_byte(0x91),
    );
    // Block 2 activates current+1, so the boundary carries epoch 1.
    let boundary = boundary_with_epoch(
        1,
        true,
        vec![
            (proposer, dummy_pubkey(0xA2)),
            (joining, dummy_pubkey(0xB3)),
        ],
    );
    let extra_data = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        execution_summary: None,
        consensus_header_artifact: Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary)),
        timestamp_millis_part: 0,
        late_finalize_credits: None,
        compressed_entities_root: None,
    })
    .expect("extra_data encodes");
    let config =
        OutbeEvmConfig::new_with_bridge(test_chain_spec(), bridge).with_evm_signer(signer.clone());
    let mut evm_env = test_evm_env(2, REWARDS_ADDRESS);
    evm_env.block_env.basefee = 0;
    let evm = config.evm_with_env(&mut state, evm_env);
    let mut ctx = execution_ctx(Some(1), extra_data.clone());
    ctx.inner.parent_hash = parent_hash;
    ctx.parent_consensus_metadata = Some(metadata.clone());
    let mut executor = config.create_executor(evm, ctx);
    // this unit test does not seed a committee snapshot
    // matching the V2 metadata's `(epoch, committee_set_hash)` pair, so
    // the Phase 1 `verify_v2_proof` preflight would reject. The test
    // exercises pre-exec + begin-zone receipts, not the verifier
    // itself; opt out via the test-only escape hatch.
    super::super::with_phase1_verify_disabled(|| {
        executor
            .apply_pre_execution_changes()
            .expect("pre-execution changes should apply before begin-zone system txs");
    });
    let system_txs = begin_system_txs_for_test(
        &config,
        2,
        parent_hash,
        &extra_data,
        Some(metadata),
        proposer,
    );
    let mut visible_system_gas_used = 0u64;
    for tx in system_txs {
        let signed_gas_limit = tx.tx().gas_limit();
        let gas_output = executor
            .execute_transaction(tx)
            .expect("Phase 1+2+3+OracleSlashWindow begin-zone system tx should execute");
        assert!(gas_output.tx_gas_used() <= signed_gas_limit);
        visible_system_gas_used += gas_output.tx_gas_used();
        assert_eq!(
            executor
                .receipts()
                .last()
                .expect("system receipt should be present")
                .cumulative_gas_used,
            visible_system_gas_used
        );
    }
    // CPA + LateFinalizeCredits + CycleTick + RewardsGemDelivery +
    // BoundaryOutcome + OracleSlashWindow + HookEvents.
    assert_eq!(executor.receipts().len(), 7);
    assert!(executor.receipts().iter().all(|receipt| receipt.success));
    assert!(
        visible_system_gas_used < 30_000_000,
        "visible system gas used {visible_system_gas_used} should fit within block gas limit"
    );

    let deactivate_input = outbe_validatorset::precompile::IValidatorSet::deactivateValidatorCall {
        validatorAddress: joining,
    }
    .abi_encode();
    let deactivate_tx: reth_ethereum::TransactionSigned = TxEip1559 {
        chain_id: CHAIN_ID,
        nonce: 0,
        gas_limit: 200_000,
        max_fee_per_gas: 0,
        max_priority_fee_per_gas: 0,
        to: TxKind::Call(outbe_primitives::addresses::VALIDATOR_SET_ADDRESS),
        value: U256::ZERO,
        input: Bytes::from(deactivate_input),
        access_list: Default::default(),
    }
    .into_signed(Signature::test_signature())
    .into();
    let recovered_deactivate =
        reth_primitives_traits::Recovered::new_unchecked(deactivate_tx, joining);

    executor
        .execute_transaction(recovered_deactivate)
        .expect("same-block user tx should see joining validator as active");

    // 7 begin-zone receipts + 1 user (deactivate) tx.
    assert_eq!(executor.receipts().len(), 8);
    assert!(executor.receipts()[7].success);
    drop(executor);

    let read_ctx = BlockContext::new(2, 2, CHAIN_ID, proposer, vec![proposer, joining]);
    let mut provider =
        outbe_primitives::storage::direct::DirectStorageProvider::new(&mut state, read_ctx);
    StorageHandle::enter(&mut provider, |storage| {
        let vs = outbe_validatorset::contract::ValidatorSet::new(storage);
        let record = vs
            .get_validator(joining)?
            .expect("joining validator exists");
        assert_eq!(record.status, outbe_validatorset::logic::status::EXITING);
        assert!(vs.has_pending_set_change()?);
        Ok::<_, outbe_primitives::error::PrecompileError>(())
    })
    .expect("same-block user mutation should be readable");
}

#[test]
fn oracle_slash_window_runs_after_boundary_activation() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let old_active_secret = [2; 32];
    let old_active = OutbeEvmSigner::from_secret_bytes(old_active_secret)
        .expect("old active test signer")
        .address();
    let mut state =
        state_with_active_and_registered_candidate_seeded(old_active, proposer, |storage| {
            let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
            oracle.config_is_initialized.write(true).unwrap();
            oracle.config_enabled.write(true).unwrap();
            oracle.config_vote_period.write(0).unwrap();
            oracle.config_slash_window.write(1).unwrap();
            oracle
                .config_slash_fraction
                .write(U256::from(10_000_000_000_000_000u64))
                .unwrap(); // 1% in 1e18 fixed point.
            oracle
                .config_min_valid_per_window
                .write(U256::from(1u64))
                .unwrap();
            oracle.penalty_miss_count.write(&old_active, 1).unwrap();

            let stake = U256::from(1_000u64);
            let staking = outbe_staking::contract::Staking::new(storage.clone());
            staking.stake_amount.write(&old_active, stake).unwrap();
            staking.total_staked.write(stake).unwrap();
            staking.config_min_stake.write(U256::from(1u64)).unwrap();
            let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
            vs.test_set_stake_projection(
                old_active,
                outbe_validatorset::StakeProjection::new(stake, None),
            )
            .unwrap();
        });
    let stake = U256::from(1_000u64);
    let mut setup_provider = outbe_primitives::storage::direct::DirectStorageProvider::new(
        &mut state,
        BlockContext::new(1, 1, CHAIN_ID, proposer, vec![old_active, proposer]),
    );
    StorageHandle::enter(&mut setup_provider, |storage| {
        storage.set_balance(STAKING_ADDRESS, stake)?;
        // Re-write the Staking slots in the same account-info flush so the
        // balance seed cannot replace the account with an empty storage map.
        let staking = outbe_staking::contract::Staking::new(storage.clone());
        staking.stake_amount.write(&old_active, stake)?;
        staking.total_staked.write(stake)?;
        staking.config_min_stake.write(U256::from(1u64))?;
        Ok::<_, outbe_primitives::error::PrecompileError>(())
    })
    .expect("staking backing balance must be seeded");
    setup_provider
        .flush()
        .expect("staking backing balance seed must flush");

    let evm_env = test_evm_env(1, REWARDS_ADDRESS);
    let boundary = boundary_with(
        true,
        vec![
            (old_active, dummy_pubkey(0xA2)),
            (proposer, dummy_pubkey(0xB3)),
        ],
    );
    let tee_bootstrap = sample_tee_bootstrap_payload_for(
        1,
        boundary.committee_set_hash,
        TEST_BLOCK_TIMESTAMP_BASE + 1 + 3_600,
        &[
            outbe_primitives::tee_test_utils::DevValidatorV1 {
                evm_secret: old_active_secret,
                bls_minpk_public: dummy_pubkey(0xA2),
            },
            outbe_primitives::tee_test_utils::DevValidatorV1 {
                evm_secret: [1; 32],
                bls_minpk_public: dummy_pubkey(0xB3),
            },
        ],
    );
    let extra_data = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        execution_summary: None,
        consensus_header_artifact: Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary)),
        timestamp_millis_part: 0,
        late_finalize_credits: None,
        compressed_entities_root: None,
    })
    .expect("extra_data encodes");
    let config = OutbeEvmConfig::new(test_chain_spec()).with_evm_signer(signer.clone());
    let evm = config.evm_with_env(&mut state, evm_env);
    let mut executor = config.create_executor(
        evm,
        execution_ctx_with_tee_bootstrap(Some(0), extra_data.clone(), tee_bootstrap.clone()),
    );

    executor
        .apply_pre_execution_changes()
        .expect("pre-execution changes should apply before Oracle slash system tx");
    let system_txs = begin_system_txs_for_test_with_bootstrap(
        &config,
        1,
        B256::ZERO,
        &extra_data,
        None,
        proposer,
        Some(tee_bootstrap),
    );
    let mut visible_system_gas_used = 0u64;
    for tx in system_txs {
        let signed_gas_limit = tx.tx().gas_limit();
        let gas_output = executor
            .execute_transaction(tx)
            .expect("Oracle slash must not invalidate same-block BoundaryOutcome activation");
        assert!(gas_output.tx_gas_used() <= signed_gas_limit);
        visible_system_gas_used += gas_output.tx_gas_used();
        assert_eq!(
            executor
                .receipts()
                .last()
                .expect("system receipt should be present")
                .cumulative_gas_used,
            visible_system_gas_used
        );
    }

    assert_eq!(executor.receipts().len(), 6);
    assert!(executor.receipts().iter().all(|receipt| receipt.success));
    let oracle_forced_exit = keccak256("ValidatorForcedExit(address)");
    assert!(
        executor.receipts()[4].logs.iter().any(|log| {
            log.address == ORACLE_ADDRESS && log.data.topics().first() == Some(&oracle_forced_exit)
        }),
        "Oracle slash-window force exit must be receipt-visible"
    );
    let oracle_slashed = keccak256("ValidatorSlashed(address,uint64)");
    assert!(
        executor.receipts()[4].logs.iter().any(|log| {
            log.address == ORACLE_ADDRESS && log.data.topics().first() == Some(&oracle_slashed)
        }),
        "Oracle slash-window stake slash must be receipt-visible"
    );
    let hook_events_receipt = &executor.receipts()[5];
    assert!(
        hook_events_receipt.success,
        "mandatory HookEvents receipt must succeed even when empty"
    );
    assert!(
        !hook_events_receipt
            .logs
            .iter()
            .any(|log| log.address == ORACLE_ADDRESS),
        "non-whitelisted oracle hook events must not appear in HookEvents receipt"
    );
    assert!(
        visible_system_gas_used < 30_000_000,
        "visible system gas used {visible_system_gas_used} should fit within block gas limit"
    );
    drop(executor);

    let read_ctx = BlockContext::new(1, 1, CHAIN_ID, proposer, vec![old_active, proposer]);
    let mut provider =
        outbe_primitives::storage::direct::DirectStorageProvider::new(&mut state, read_ctx);
    StorageHandle::enter(&mut provider, |storage| {
        let vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        assert!(vs.is_consensus_participant(proposer)?);
        let old_record = vs
            .get_validator(old_active)?
            .expect("old active validator should still exist");
        assert_eq!(
            old_record.status,
            outbe_validatorset::logic::status::JAILED,
            "Oracle slash applies after activation without making the block invalid"
        );
        let staking = outbe_staking::contract::Staking::new(storage.clone());
        assert_eq!(staking.stake_amount.read(&old_active)?, U256::from(990u64));
        assert_eq!(storage.balance(STAKING_ADDRESS)?, U256::from(990u64));
        Ok::<_, outbe_primitives::error::PrecompileError>(())
    })
    .expect("validator state should be readable");
}
