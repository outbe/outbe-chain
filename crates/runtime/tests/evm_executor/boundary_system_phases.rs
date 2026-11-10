use super::boundary_fixtures::*;
use super::*;
#[test]
fn proposer_injects_tee_bootstrap_after_boundary_when_payload_pending() {
    use outbe_evm::system_tx::{
        CYCLE_TICK_SELECTOR, ENCLAVE_UPGRADE_DEADLINE_SELECTOR, HYPERLANE_SLASH_SELECTOR,
        ORACLE_SELECTOR, ORACLE_SLASH_SELECTOR, REWARDS_SELECTOR, STAKING_SELECTOR,
        TEE_BOOTSTRAP_SELECTOR, UPDATE_SELECTOR, VALIDATOR_LIVENESS_SELECTOR, VOTE_SELECTOR,
    };
    let signer = test_evm_signer();
    let proposer = signer.address();
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(signer);

    let with_bootstrap = begin_system_txs_for_test(
        &config,
        BeginBlockFixture {
            block_number: 1,
            parent_hash: B256::ZERO,
            extra_data: &Bytes::new(),
            parent_consensus_metadata: None,
            proposer,
            bootstrap: BootstrapFixture::explicit(Some(sample_tee_bootstrap_payload(1))),
        },
    );
    assert_eq!(
        begin_system_tx_selectors(&with_bootstrap),
        vec![
            TEE_BOOTSTRAP_SELECTOR,
            VALIDATOR_LIVENESS_SELECTOR,
            VOTE_SELECTOR,
            UPDATE_SELECTOR,
            ENCLAVE_UPGRADE_DEADLINE_SELECTOR,
            REWARDS_SELECTOR,
            STAKING_SELECTOR,
            ORACLE_SELECTOR,
            ORACLE_SLASH_SELECTOR,
            HYPERLANE_SLASH_SELECTOR,
            CYCLE_TICK_SELECTOR,
        ],
        "proposer must inject TeeBootstrap in CONSENSUS_ORDER before ValidatorLiveness and BEGIN_BLOCK_ORDER",
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
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(signer.clone());
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
    let tee_bootstrap = activation_bootstrap(boundary.committee_set_hash, old_active_secret);
    let extra_data = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        radicle_publication_enabled: false,
        radicle_publication: None,
        execution_summary: None,
        consensus_header_artifact: Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary)),
        timestamp_millis_part: 0,
        late_finalize_credits: None,
        enclave_witness: None,
    })
    .expect("extra_data encodes");
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(signer.clone());
    let evm = config.evm_with_env(&mut state, evm_env);
    let mut executor = config.create_executor(
        evm,
        execution_ctx_with_tee_bootstrap(Some(0), extra_data.clone(), tee_bootstrap.clone()),
    );

    executor
        .apply_pre_execution_changes()
        .expect("activation block pre-execution should apply");
    let system_txs = begin_system_txs_for_test(
        &config,
        BeginBlockFixture {
            block_number: 1,
            parent_hash: B256::ZERO,
            extra_data: &extra_data,
            parent_consensus_metadata: None,
            proposer,
            bootstrap: BootstrapFixture::explicit(Some(tee_bootstrap)),
        },
    );
    for tx in system_txs {
        executor
            .execute_transaction(tx)
            .expect("activation block begin-zone system tx should execute");
    }

    assert_eq!(executor.receipts().len(), 12);
    assert!(executor.receipts().iter().all(|receipt| receipt.success));
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
    let mut state =
        state_with_active_and_registered_candidate_seeded(proposer, joining, |storage| {
            let registry = outbe_teeregistry::TeeRegistry::new(storage);
            for (index, validator) in [proposer, joining].into_iter().enumerate() {
                let node_hash = keccak256((index as u64).to_be_bytes());
                registry
                    .validator_v1_node_hash
                    .write(&validator, node_hash)
                    .unwrap();
                registry
                    .v1_node_enclave_id
                    .write(&node_hash, B256::with_last_byte(0x11))
                    .unwrap();
                registry
                    .v1_node_binding_id
                    .write(&node_hash, B256::with_last_byte(0x12))
                    .unwrap();
                registry
                    .v1_node_intent_hash
                    .write(&node_hash, B256::with_last_byte(0x13))
                    .unwrap();
                registry
                    .v1_node_valid_until
                    .write(&node_hash, u64::MAX)
                    .unwrap();
            }
        });

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
        radicle_publication_enabled: false,
        radicle_publication: None,
        execution_summary: None,
        consensus_header_artifact: Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary)),
        timestamp_millis_part: 0,
        late_finalize_credits: None,
        enclave_witness: None,
    })
    .expect("extra_data encodes");
    let config = OutbeEvmConfig::new_with_bridge(
        test_chain_spec(),
        bridge,
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(signer.clone());
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
    // itself. Disable the preflight with the test-only escape hatch.
    super::with_phase1_verify_disabled(|| {
        executor
            .apply_pre_execution_changes()
            .expect("pre-execution changes should apply before begin-zone system txs");
    });
    let system_txs = begin_system_txs_for_test(
        &config,
        BeginBlockFixture {
            block_number: 2,
            parent_hash,
            extra_data: &extra_data,
            parent_consensus_metadata: Some(metadata),
            proposer,
            bootstrap: BootstrapFixture::StandardForBlock,
        },
    );
    let visible_system_gas_used = fixtures::assert_begin_prefix_gas(
        &mut executor,
        system_txs,
        fixtures::BeginPrefixCheck {
            execution_message: "Consensus and BeginBlock begin-zone system tx should execute",
            cumulative_receipts: true,
        },
    );
    // 4 CONSENSUS_ORDER txs (CPA + LateFinalizeCredits + BoundaryOutcome + ValidatorLiveness)
    // + 9 BEGIN_BLOCK_ORDER txs = 13 receipts.
    assert_eq!(executor.receipts().len(), 13);
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

    // 13 begin-zone receipts + 1 user (deactivate) tx.
    assert_eq!(executor.receipts().len(), 14);
    assert!(executor.receipts()[13].success);
    drop(executor);

    let read_ctx = BlockContext {
        block_gas_limit: 0,
        user_gas_used: 0,
        block_number: 2,
        timestamp: 2,
        chain_id: CHAIN_ID,
        genesis_hash: alloy_primitives::B256::ZERO,
        genesis_timestamp: 2,
        proposer,
        validators: vec![proposer, joining],
    };
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
        BlockContext {
            block_gas_limit: 0,
            user_gas_used: 0,
            block_number: 1,
            timestamp: 1,
            chain_id: CHAIN_ID,
            genesis_hash: alloy_primitives::B256::ZERO,
            genesis_timestamp: 1,
            proposer,
            validators: vec![old_active, proposer],
        },
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
    let tee_bootstrap = activation_bootstrap(boundary.committee_set_hash, old_active_secret);
    let extra_data = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        radicle_publication_enabled: false,
        radicle_publication: None,
        execution_summary: None,
        consensus_header_artifact: Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary)),
        timestamp_millis_part: 0,
        late_finalize_credits: None,
        enclave_witness: None,
    })
    .expect("extra_data encodes");
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(signer.clone());
    let evm = config.evm_with_env(&mut state, evm_env);
    let mut executor = config.create_executor(
        evm,
        execution_ctx_with_tee_bootstrap(Some(0), extra_data.clone(), tee_bootstrap.clone()),
    );

    executor
        .apply_pre_execution_changes()
        .expect("pre-execution changes should apply before Oracle slash system tx");
    let system_txs = begin_system_txs_for_test(
        &config,
        BeginBlockFixture {
            block_number: 1,
            parent_hash: B256::ZERO,
            extra_data: &extra_data,
            parent_consensus_metadata: None,
            proposer,
            bootstrap: BootstrapFixture::explicit(Some(tee_bootstrap)),
        },
    );
    let visible_system_gas_used = fixtures::assert_begin_prefix_gas(
        &mut executor,
        system_txs,
        fixtures::BeginPrefixCheck {
            execution_message:
                "Oracle slash must not invalidate same-block BoundaryOutcome activation",
            cumulative_receipts: true,
        },
    );

    assert_eq!(executor.receipts().len(), 12);
    assert!(executor.receipts().iter().all(|receipt| receipt.success));
    let oracle_forced_exit = keccak256("ValidatorForcedExit(address)");
    assert!(
        executor.receipts()[9].logs.iter().any(|log| {
            log.address == ORACLE_ADDRESS && log.data.topics().first() == Some(&oracle_forced_exit)
        }),
        "Oracle slash-window force exit must be receipt-visible"
    );
    let oracle_slashed = keccak256("ValidatorSlashed(address,uint64)");
    assert!(
        executor.receipts()[9].logs.iter().any(|log| {
            log.address == ORACLE_ADDRESS && log.data.topics().first() == Some(&oracle_slashed)
        }),
        "Oracle slash-window stake slash must be receipt-visible"
    );
    let cycle_tick_receipt = &executor.receipts()[11];
    assert!(
        cycle_tick_receipt.success,
        "mandatory CycleTick receipt must succeed"
    );
    assert!(
        !cycle_tick_receipt
            .logs
            .iter()
            .any(|log| log.address == ORACLE_ADDRESS),
        "oracle slash events are emitted directly on the OracleSlash receipt, not CycleTick"
    );
    assert_eq!(visible_system_gas_used, 0);
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
        validators: vec![old_active, proposer],
    };
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

fn activation_bootstrap(
    committee_set_hash: B256,
    old_active_secret: [u8; 32],
) -> outbe_primitives::tee_bootstrap_v2::TeeBootstrapV2 {
    sample_tee_bootstrap_payload_for(
        1,
        committee_set_hash,
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
    )
}

#[test]
fn enclave_upgrade_deadline_uses_configured_slash_and_credits_promis_limit_once() {
    use outbe_primitives::units::UNITS_PER_PROTOCOL_MINOR;
    use outbe_slashindicator::enforce_enclave_upgrade_deadline;

    for (configured_percent, expected_percent) in [(0, 8u64), (17, 17)] {
        let validator = Address::repeat_byte(0x73);
        let proposal_id = U256::from(9u64);
        let stake = UNITS_PER_PROTOCOL_MINOR * U256::from(100u64) + U256::from(100u64);
        let slashed =
            UNITS_PER_PROTOCOL_MINOR * U256::from(expected_percent) + U256::from(expected_percent);
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        provider.set_block_number(100);
        provider.set_balance(STAKING_ADDRESS, stake);
        provider.enter(|storage| {
            seed_registered_active_validator(storage.clone(), validator, &dummy_pubkey(0x73));
            let staking = outbe_staking::contract::Staking::new(storage.clone());
            staking.stake_amount.write(&validator, stake).unwrap();
            staking.total_staked.write(stake).unwrap();
            storage.set_balance(STAKING_ADDRESS, stake).unwrap();
            outbe_slashindicator::schema::SlashIndicator::new(storage.clone())
                .config_slash_amount_percent
                .write(configured_percent)
                .unwrap();
            let registry = outbe_teeregistry::TeeRegistry::new(storage.clone());
            registry.strict_upgrade_proposal.write(proposal_id).unwrap();
            registry.strict_upgrade_height.write(100).unwrap();
            registry
                .strict_upgrade_mrenclave
                .write(B256::repeat_byte(0x74))
                .unwrap();
            registry
                .strict_upgrade_successor
                .write(B256::repeat_byte(0x75))
                .unwrap();
            registry
                .active_v1_policy_proposal_id
                .write(proposal_id)
                .unwrap();
            let ctx = BlockRuntimeContext::new(
                BlockContext {
                    block_gas_limit: 0,
                    user_gas_used: 0,
                    block_number: 100,
                    timestamp: TEST_BLOCK_TIMESTAMP_BASE,
                    chain_id: CHAIN_ID,
                    genesis_hash: alloy_primitives::B256::ZERO,
                    genesis_timestamp: TEST_BLOCK_TIMESTAMP_BASE.max(1),
                    proposer: alloy_primitives::Address::ZERO,
                    validators: Vec::new(),
                },
                storage.clone(),
            );

            enforce_enclave_upgrade_deadline(&ctx).unwrap();
            assert_eq!(staking.get_stake(validator).unwrap(), stake - slashed);
            assert_eq!(staking.get_total_staked().unwrap(), stake - slashed);
            assert_eq!(storage.balance(STAKING_ADDRESS).unwrap(), stake - slashed);
            let validators = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
            assert_eq!(
                validators.get_validator(validator).unwrap().unwrap().status,
                outbe_validatorset::runtime::status::JAILED
            );
            assert!(registry
                .upgrade_penalty_applied_v1(proposal_id, validator)
                .unwrap());
            assert!(registry.upgrade_sweep_due_v1().unwrap().is_none());
            let limit = outbe_promislimit::PromisLimitContract::new(storage.clone());
            assert_eq!(limit.promis_limit().unwrap(), U256::from(expected_percent));
            assert_eq!(limit.rest_minor().unwrap(), U256::from(expected_percent));

            enforce_enclave_upgrade_deadline(&ctx).unwrap();
            assert_eq!(staking.get_stake(validator).unwrap(), stake - slashed);
            assert_eq!(storage.balance(STAKING_ADDRESS).unwrap(), stake - slashed);
            assert_eq!(limit.promis_limit().unwrap(), U256::from(expected_percent));
            assert_eq!(limit.rest_minor().unwrap(), U256::from(expected_percent));
        });
    }
}

#[test]
fn update_activates_enclave_upgrade_and_enclave_upgrade_deadline_enforces_it_in_same_begin_block() {
    use outbe_primitives::{
        block::ProtocolModule as _, tee_test_utils::gramine_direct_policy_v1,
        units::UNITS_PER_PROTOCOL_MINOR,
    };

    let validator = Address::repeat_byte(0x76);
    let proposal_id = U256::from(11u64);
    let activation_height = 100u64;
    let stake = UNITS_PER_PROTOCOL_MINOR * U256::from(100u64);
    let expected_slashed = UNITS_PER_PROTOCOL_MINOR * U256::from(10u64);
    let new_mrenclave = B256::repeat_byte(0x88);

    let genesis_hash = B256::repeat_byte(0x11);
    let mut provider = HashMapStorageProvider::new_with_chain_identity(CHAIN_ID, genesis_hash);
    provider.set_block_number(10);
    provider.set_balance(STAKING_ADDRESS, stake);
    provider.enter(|storage| {
        seed_registered_active_validator(storage.clone(), validator, &dummy_pubkey(0x76));
        let staking = outbe_staking::contract::Staking::new(storage.clone());
        staking.stake_amount.write(&validator, stake).unwrap();
        staking.total_staked.write(stake).unwrap();
        storage.set_balance(STAKING_ADDRESS, stake).unwrap();
        outbe_slashindicator::schema::SlashIndicator::new(storage.clone())
            .config_slash_amount_percent
            .write(10)
            .unwrap();

        let initial_policy = gramine_direct_policy_v1(CHAIN_ID, genesis_hash).unwrap();
        let initial_policy_hash = initial_policy.policy_hash().unwrap();
        let mut registry = outbe_teeregistry::TeeRegistry::new(storage.clone());
        registry.install_initial_policy_v1(&initial_policy).unwrap();
        registry
            .stage_measurement_upgrade_v1(
                proposal_id,
                new_mrenclave,
                initial_policy_hash,
                activation_height,
            )
            .unwrap();

        let mut update = outbe_update::schema::Update::new(storage);
        update
            .write_scheduled_update(
                proposal_id,
                outbe_update::constants::PROTOCOL_VERSION,
                activation_height,
                "enclave upgrade",
            )
            .unwrap();
    });

    provider.set_block_number(activation_height);
    provider.enter(|storage| {
        let staking = outbe_staking::contract::Staking::new(storage.clone());
        let registry = outbe_teeregistry::TeeRegistry::new(storage.clone());
        // Before OSUP runs at activation_height, the staged upgrade is pending and sweep is not due.
        assert!(registry.strict_upgrade_pending_v1().unwrap());
        assert!(registry.upgrade_sweep_due_v1().unwrap().is_none());

        let ctx = BlockRuntimeContext::new(
            BlockContext {
                block_gas_limit: 0,
                user_gas_used: 0,
                block_number: activation_height,
                timestamp: TEST_BLOCK_TIMESTAMP_BASE,
                chain_id: CHAIN_ID,
                genesis_hash: alloy_primitives::B256::ZERO,
                genesis_timestamp: TEST_BLOCK_TIMESTAMP_BASE.max(1),
                proposer: alloy_primitives::Address::ZERO,
                validators: Vec::new(),
            },
            storage.clone(),
        );

        // 1. OSUP (UpdateModule::begin_block) promotes the staged policy at height H.
        outbe_update::UpdateModule::begin_block(&ctx).unwrap();
        assert!(!registry.strict_upgrade_pending_v1().unwrap());
        assert!(
            registry.upgrade_sweep_due_v1().unwrap().is_some(),
            "OSUP promotion at height H must make the enclave upgrade sweep due in the same block"
        );

        // 2. OSEU (EnclaveUpgradeDeadline::begin_block) immediately follows OSUP in BeginBlock at height H.
        outbe_slashindicator::EnclaveUpgradeDeadline::begin_block(&ctx).unwrap();
        assert!(registry.upgrade_sweep_due_v1().unwrap().is_none());
        assert_eq!(
            staking.get_stake(validator).unwrap(),
            stake - expected_slashed
        );
        let validators = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        assert_eq!(
            validators.get_validator(validator).unwrap().unwrap().status,
            outbe_validatorset::runtime::status::JAILED
        );
        assert!(registry
            .upgrade_penalty_applied_v1(proposal_id, validator)
            .unwrap());
    });
}
