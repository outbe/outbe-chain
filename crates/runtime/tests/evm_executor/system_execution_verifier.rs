use super::*;

#[test]
fn verifier_rejects_boundary_outcome_system_tx_artifact_mismatch() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let mut state = state_with_active_proposer(proposer);
    let evm_env = test_evm_env(1, REWARDS_ADDRESS);
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    );
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

    let boundary_unsigned = build_unsigned_system_tx(
        0,
        1,
        CHAIN_ID,
        SystemTx::BoundaryOutcome {
            artifact: tx_artifact,
        }
        .encode()
        .unwrap(),
    )
    .unwrap();
    let boundary_signed = signer.sign_unsigned(boundary_unsigned).unwrap();
    let boundary_recovered =
        reth_primitives_traits::Recovered::new_unchecked(boundary_signed, proposer);
    let mut ctx = execution_ctx(Some(1), extra_data);
    ctx.expected_begin_system_txs = vec![boundary_recovered.clone()];
    ctx.proposer_evm_address = Some(proposer);

    let mut executor = config.create_executor(evm, ctx);
    executor
        .apply_pre_execution_changes()
        .expect("pre-execution changes should apply before verifier tx loop");
    let err = executor
        .execute_transaction(boundary_recovered)
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
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(proposer_signer.clone());
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
    let unsigned =
        build_unsigned_system_tx(0, 1, CHAIN_ID, canonical[0].tx().input().clone()).unwrap();
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
    let setup = BlockContext {
        block_gas_limit: 0,
        user_gas_used: 0,
        block_number: 7,
        timestamp: 1_000,
        chain_id: CHAIN_ID,
        genesis_hash: alloy_primitives::B256::ZERO,
        genesis_timestamp: 1_000,
        proposer: OWNER,
        validators: Vec::new(),
    };
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

    let run = BlockContext {
        block_gas_limit: 0,
        user_gas_used: 0,
        block_number: 8,
        timestamp: 2_000,
        chain_id: CHAIN_ID,
        genesis_hash: alloy_primitives::B256::ZERO,
        genesis_timestamp: 2_000,
        proposer: OWNER,
        validators: Vec::new(),
    };
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

    let read = BlockContext {
        block_gas_limit: 0,
        user_gas_used: 0,
        block_number: 8,
        timestamp: 2_000,
        chain_id: CHAIN_ID,
        genesis_hash: alloy_primitives::B256::ZERO,
        genesis_timestamp: 2_000,
        proposer: OWNER,
        validators: Vec::new(),
    };
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
    let ctx = BlockContext {
        block_gas_limit: 0,
        user_gas_used: 0,
        block_number: 7,
        timestamp: 84,
        chain_id: CHAIN_ID,
        genesis_hash: alloy_primitives::B256::ZERO,
        genesis_timestamp: 84,
        proposer: OWNER,
        validators: Vec::new(),
    };
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

    let ctx = BlockContext {
        block_gas_limit: 0,
        user_gas_used: 0,
        block_number: 8,
        timestamp: 96,
        chain_id: CHAIN_ID,
        genesis_hash: alloy_primitives::B256::ZERO,
        genesis_timestamp: 96,
        proposer: OWNER,
        validators: Vec::new(),
    };
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
    let ctx = BlockContext {
        block_gas_limit: 0,
        user_gas_used: 0,
        block_number: 7,
        timestamp: 84,
        chain_id: CHAIN_ID,
        genesis_hash: alloy_primitives::B256::ZERO,
        genesis_timestamp: 84,
        proposer: OWNER,
        validators: Vec::new(),
    };

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

#[test]
fn begin_block_soft_fail_rolls_back_dirty_storage_writes_and_logs_preserving_earlier_state() {
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

    // Execute mandatory prefix (0..8) before OracleSlash (OSOS at index 8).
    for _ in 0..8 {
        let tx = system_txs.next().expect("prefix tx before OracleSlash");
        let gas = executor
            .execute_transaction(tx)
            .expect("prefix tx should succeed")
            .tx_gas_used();
        assert_eq!(gas, 0);
    }

    let oracle_slash_tx = system_txs.next().expect("OracleSlash tx at index 8");
    let dirty_addr = outbe_primitives::addresses::ORACLE_ADDRESS;
    let dirty_slot = U256::from(0xDEAD_BEEFu64);
    let dirty_val = U256::from(0xCAFE_BABEu64);
    let dirty_topic = keccak256("DirtySoftFailEvent()");

    let gas = outbe_evm::sys_tx_precompile::with_forced_dirty_system_precompile_revert(
        dirty_addr,
        dirty_slot,
        dirty_val,
        || executor.execute_transaction(oracle_slash_tx),
    )
    .expect("soft-fail OracleSlash revert must not abort block")
    .tx_gas_used();
    assert_eq!(gas, 0);

    let osos_receipt = executor.receipts().last().expect("OSOS receipt");
    assert!(!osos_receipt.success);
    assert_eq!(osos_receipt.cumulative_gas_used, 0);
    assert_eq!(osos_receipt.logs.len(), 1);
    assert!(
        osos_receipt
            .logs
            .iter()
            .all(|log| log.data.topics().first() != Some(&dirty_topic)),
        "dirty event log emitted before revert must be rolled back"
    );

    // Remaining BeginBlock system txs (HyperlaneSlash and CycleTick) must succeed.
    for remaining_tx in system_txs {
        let gas = executor
            .execute_transaction(remaining_tx)
            .expect("remaining BeginBlock tx should succeed")
            .tx_gas_used();
        assert_eq!(gas, 0);
    }
    drop(executor);

    // Verify dirty storage slot was rolled back while earlier ValidatorLiveness state persisted.
    assert_eq!(
        state.storage(dirty_addr, dirty_slot).unwrap(),
        U256::ZERO,
        "dirty storage write inside reverted soft-fail OSOS must be rolled back"
    );
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
        let record = vs
            .get_validator(proposer)
            .unwrap()
            .expect("proposer exists");
        assert_eq!(
            record.blocks_proposed, 1,
            "earlier committed ValidatorLiveness state must be preserved"
        );
    });
}

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
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    );
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
fn block_one_rejects_user_transaction_after_begin_block_with_unexpected_user_tx() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let user_tx = test_regular_tx()
        .try_into_recovered()
        .expect("regular tx signer should recover");
    let mut state = state_with_active_proposer(proposer);
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(signer);
    let evm = config.evm_with_env(&mut state, test_evm_env(1, REWARDS_ADDRESS));
    let mut executor = config.create_executor(evm, block_one_execution_ctx(Some(1), Bytes::new()));
    executor
        .apply_pre_execution_changes()
        .expect("block 1 pre-execution changes should apply");
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
        executor
            .execute_transaction(tx)
            .expect("block 1 begin-zone system tx should succeed");
    }
    assert_eq!(
        executor.fsm.stage(),
        outbe_primitives::system_tx::BlockExecutionStage::Completed
    );
    let err = executor
        .execute_transaction(user_tx)
        .expect_err("block 1 must reject any MidBlock user transaction");
    assert!(
        err.to_string()
            .contains("unexpected user transaction in stage Completed"),
        "expected UnexpectedUserTx in stage Completed, got: {err}"
    );
}

#[test]
fn verifier_rejects_begin_system_tx_signature_hash_mismatch() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let mut state = state_with_active_proposer(proposer);
    let evm_env = test_evm_env(1, REWARDS_ADDRESS);
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(signer.clone());
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
    let wrong_unsigned =
        build_unsigned_system_tx(0, 2, CHAIN_ID, canonical[0].tx().input().clone()).unwrap();
    let wrong_signed = signer.sign_unsigned(wrong_unsigned).unwrap();
    let wrong_recovered = reth_primitives_traits::Recovered::new_unchecked(wrong_signed, proposer);
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
