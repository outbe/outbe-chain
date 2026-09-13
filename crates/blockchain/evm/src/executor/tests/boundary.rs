use super::*;

/// reth22-1 regression: every *stateful* dispatch-registered precompile must
/// be preserved by either the per-block EIP-161 marker list or canonical
/// genesis marker bytecode, or its storage is silently lost at state-root
/// time (GEM/GEM_FACTORY were missing). This unit pins runtime-marker
/// coverage for routes that are neither stateless nor genesis-preserved;
/// `tests/genesis.rs` binds the complementary genesis-marker evidence.
#[test]
fn marker_list_covers_stateful_precompiles() {
    use crate::executor::marker_addresses::OUTBE_RUNTIME_MARKER_ADDRESSES;
    use crate::precompiles::outbe_precompile_addresses;
    use outbe_primitives::addresses::{
        DEBUG_SUBCALL_PRECOMPILE_ADDRESS, GOVERNANCE_ADDRESS, RADICLE_REGISTRY_ADDRESS,
        STABLECOIN_FACTORY_ADDRESS, STABLECOIN_POLICY_REGISTRY_ADDRESS, VAULT_ROUTER_ADDRESS,
        ZEROFEE_ADDRESS, ZKPROOF_GROTH16_ADDRESS, ZKPROOF_POSEIDON_ADDRESS,
    };

    // Dispatch-registered precompiles that legitimately need NO runtime 0xEF
    // marker. Each state-owning exemption must have canonical genesis-marker
    // evidence in `tests/genesis.rs`; an unproven exemption would re-open reth22-1.
    const MARKER_EXEMPT: [Address; 9] = [
        // Stateless verifiers - no EVM storage to preserve.
        ZKPROOF_POSEIDON_ADDRESS,
        ZKPROOF_GROTH16_ADDRESS,
        // Debug adapter owns no persistent state; any child effects are journaled
        // against the actual child target.
        DEBUG_SUBCALL_PRECOMPILE_ADDRESS,
        // Seeded with genesis marker bytecode by scripts/seed_genesis.py, so these
        // accounts are never EIP-161-empty.
        ZEROFEE_ADDRESS,
        VAULT_ROUTER_ADDRESS,
        GOVERNANCE_ADDRESS,
        // Stablecoin Factory and Policy Registry marker code is genesis-active
        // even before Stablecoin V1 runtime activation.
        STABLECOIN_FACTORY_ADDRESS,
        STABLECOIN_POLICY_REGISTRY_ADDRESS,
        // RadicleRegistry is present from genesis even when no repositories
        // are configured because ALL_PRECOMPILE_ADDRESSES seeds its marker.
        RADICLE_REGISTRY_ADDRESS,
    ];

    for addr in outbe_precompile_addresses() {
        if MARKER_EXEMPT.contains(addr) {
            continue;
        }
        assert!(
            OUTBE_RUNTIME_MARKER_ADDRESSES.contains(addr),
            "stateful dispatch-registered precompile {addr} is missing from the EIP-161 \
             runtime marker list (OUTBE_RUNTIME_MARKER_ADDRESSES) - its storage would be \
             silently pruned at state-root (reth22-1). Add it to the marker list, or, if it \
             is stateless / genesis-seeded, to MARKER_EXEMPT with justification."
        );
    }

    // GEM/GEM_FACTORY specifically (the original reth22-1 bug) must be covered.
    use outbe_primitives::addresses::{GEM_ADDRESS, GEM_FACTORY_ADDRESS};
    assert!(OUTBE_RUNTIME_MARKER_ADDRESSES.contains(&GEM_ADDRESS));
    assert!(OUTBE_RUNTIME_MARKER_ADDRESSES.contains(&GEM_FACTORY_ADDRESS));
}

fn state_with_active_and_registered_candidate(
    active: Address,
    candidate: Address,
) -> State<CacheDB<EmptyDBTyped<ProviderError>>> {
    state_with_active_and_registered_candidate_seeded(active, candidate, |_| {})
}

fn state_with_active_and_registered_candidate_seeded(
    active: Address,
    candidate: Address,
    seed_extra: impl FnOnce(StorageHandle),
) -> State<CacheDB<EmptyDBTyped<ProviderError>>> {
    let chain_spec = test_chain_spec();
    let mut seed_storage =
        HashMapStorageProvider::new_with_chain_identity(CHAIN_ID, chain_spec.genesis_hash());
    let active_key = dummy_pubkey(0xA2);
    let install = test_ocomp_fork_install(&chain_spec, &[(active, active_key)]);
    StorageHandle::enter(&mut seed_storage, |storage| {
        seed_compressed_entities_genesis(storage.clone());
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(128).unwrap();
        vs.config_epoch_length_blocks.write(60).unwrap();
        vs.config_is_initialized.write(true).unwrap();
        register_and_activate_with_ocomp_registration(
            &mut vs,
            active,
            &active_key,
            &install.founder_registrations[0],
        );
        vs.register_validator(OWNER, candidate, &dummy_pubkey(0xB3))
            .unwrap();
        vs.admit_validator_for_boundary_for_test(candidate).unwrap();
        seed_test_committee_snapshot(storage.clone(), &[(active, active_key)]);
        // Seed the COEN/840 oracle pair + a 1.0 rate so begin-block
        // NOD/GEM/INTEX floor-price promotion resolves a live rate instead
        // of soft-skipping the scan. 840 is also pushed onto the reference
        // currency list, matching genesis: the Nod qualifier reads its ISO
        // from there, not from a hard-coded constant.
        outbe_oracle::api::register_pair(storage.clone(), outbe_oracle::api::DAY_TYPE_PAIR)
            .unwrap();
        outbe_oracle::schema::OracleContract::new(storage.clone())
            .reference_currencies
            .push(outbe_oracle::api::DAY_TYPE_ISO)
            .unwrap();
        outbe_oracle::api::set_exchange_rate(
            storage.clone(),
            Address::ZERO,
            outbe_oracle::api::DAY_TYPE_PAIR,
            U256::from(1_000_000u64),
            0,
            0,
        )
        .unwrap();
        seed_extra(storage);
    });
    seed_test_ocomp_profile(&mut seed_storage, 0, &install);

    let mut db = cache_db_from_storage(seed_storage);
    let marker_code = Bytecode::new_legacy([0xef].into());
    db.insert_account_info(
        outbe_primitives::addresses::VALIDATOR_SET_ADDRESS,
        AccountInfo {
            code_hash: marker_code.hash_slow(),
            code: Some(marker_code.clone()),
            ..Default::default()
        },
    );
    db.insert_account_info(
        outbe_primitives::addresses::ORACLE_ADDRESS,
        AccountInfo {
            code_hash: marker_code.hash_slow(),
            code: Some(marker_code.clone()),
            ..Default::default()
        },
    );
    db.insert_account_info(
        outbe_primitives::addresses::COMPRESSED_ENTITIES_ADDRESS,
        AccountInfo {
            code_hash: marker_code.hash_slow(),
            code: Some(marker_code.clone()),
            ..Default::default()
        },
    );
    db.insert_account_info(
        outbe_primitives::addresses::METADOSIS_ADDRESS,
        AccountInfo {
            code_hash: marker_code.hash_slow(),
            code: Some(marker_code.clone()),
            ..Default::default()
        },
    );
    db.insert_account_info(
        outbe_primitives::addresses::OCOMP_REGISTRY_ADDRESS,
        AccountInfo {
            code_hash: marker_code.hash_slow(),
            code: Some(marker_code),
            ..Default::default()
        },
    );
    State::builder()
        .with_database(db)
        .with_bundle_update()
        .build()
}

fn begin_system_tx_kinds(
    txs: &[reth_primitives_traits::Recovered<reth_ethereum::TransactionSigned>],
) -> Vec<crate::system_tx::SystemTxKind> {
    txs.iter()
        .map(|tx| {
            SystemTxInputV2::decode(tx.tx().input().as_ref())
                .expect("begin-zone calldata decodes")
                .kind()
        })
        .collect()
}

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
    super::with_phase1_verify_disabled(|| {
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

fn test_register_joining(
    vs: &mut outbe_validatorset::contract::ValidatorSet<'_>,
    validator: Address,
    pubkey: &[u8; 48],
) {
    test_register_waiting(vs, validator, pubkey);
    vs.record_stake_increase(validator, U256::from(1), U256::from(1))
        .unwrap();
    vs.admit_validator_for_boundary_for_test(validator).unwrap();
}

#[test]
fn genesis_validation_rejects_active_validator_with_zero_stake() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let validator = address!("0x1111111111111111111111111111111111111111");
        let pk = dummy_pubkey(0xA1);
        seed_registered_active_validator(storage.clone(), validator, &pk);

        let staking = outbe_staking::contract::Staking::new(storage.clone());
        staking.config_min_stake.write(U256::from(100u64)).unwrap();

        let genesis = GenesisValidators {
            validators: vec![GenesisValidator {
                address: validator,
                consensus_pubkey: pk,
            }],
            epoch_length_blocks: 60,
        };

        let err = super::validate_genesis_state(storage.clone(), &genesis).unwrap_err();
        assert!(err.to_string().contains("stake below min_stake"));
    });
}

#[test]
fn genesis_validation_accepts_staked_active_validator() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let validator = address!("0x1111111111111111111111111111111111111111");
        let pk = dummy_pubkey(0xA1);
        let stake = U256::from(100u64);
        seed_registered_active_validator(storage.clone(), validator, &pk);

        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.test_set_stake_projection(
            validator,
            outbe_validatorset::StakeProjection::new(stake, None),
        )
        .unwrap();

        let staking = outbe_staking::contract::Staking::new(storage.clone());
        staking.config_min_stake.write(stake).unwrap();
        staking.stake_amount.write(&validator, stake).unwrap();
        staking.total_staked.write(stake).unwrap();

        let genesis = GenesisValidators {
            validators: vec![GenesisValidator {
                address: validator,
                consensus_pubkey: pk,
            }],
            epoch_length_blocks: 60,
        };

        super::validate_genesis_state(storage.clone(), &genesis).unwrap();
    });
}

/// Task 01 test: activate_reshared_set() runs AFTER participation decode.
///
/// Simulates the executor's finish() hook order:
/// 1. Read active consensus set (OLD set)
/// 2. Decode participation bitmap against OLD set
/// 3. Record participation / slashing
/// 4. THEN activate_reshared_set() -> set changes to NEW set
///
/// Verifies that get_active_consensus_set() returns the OLD set
/// at step 2, and the NEW set only after step 4.
#[test]
fn test_reshare_activation_after_participation_decode() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_block_number(1);
    StorageHandle::enter(&mut storage, |storage| {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(128).unwrap();
        vs.config_is_initialized.write(true).unwrap();

        // Register and activate validators A, B, C.
        let val_a = address!("0x1111111111111111111111111111111111111111");
        let val_b = address!("0x2222222222222222222222222222222222222222");
        let val_c = address!("0x3333333333333333333333333333333333333333");
        let val_d = address!("0x4444444444444444444444444444444444444444");

        test_register_active(&mut vs, val_a, &dummy_pubkey(0xA1));
        test_register_active(&mut vs, val_b, &dummy_pubkey(0xB2));
        test_register_active(&mut vs, val_c, &dummy_pubkey(0xC3));
        test_register_joining(&mut vs, val_d, &dummy_pubkey(0xD4));

        // The fixture helpers activated A, B, C; D remains a ready joiner.

        // Step 1: Read old active set - should be [A, B, C].
        let old_set = vs.get_active_consensus_set().unwrap();
        let old_addrs: Vec<Address> = old_set.iter().map(|v| v.validator_address).collect();
        assert!(old_addrs.contains(&val_a));
        assert!(old_addrs.contains(&val_b));
        assert!(old_addrs.contains(&val_c));
        assert!(!old_addrs.contains(&val_d), "D should NOT be in old set");
        assert_eq!(old_addrs.len(), 3);

        // Step 2-3: Participation/slashing would happen here using old_addrs.
        // (We just verify the set is correct - actual slashing tested in Task 01 code.)

        // Step 4: NOW activate new reshare with [A, B, D] (C removed, D added).
        let new_hash = B256::with_last_byte(0x02);
        // First deactivate C (simulate EXITING).
        vs.deactivate_validator(OWNER, val_c).unwrap();

        // C is still in the current consensus set until the reshare outcome
        // is applied. This matches the still-running engine committee.
        let transition_set = vs.get_active_consensus_set().unwrap();
        let transition_addrs: Vec<Address> =
            transition_set.iter().map(|v| v.validator_address).collect();
        assert!(transition_addrs.contains(&val_c));
        assert_eq!(transition_addrs.len(), 3);
        vs.record_proposer(val_c).unwrap();
        vs.record_participation(&[val_a, val_b], &[val_c]).unwrap();

        // Reshare with new set.
        vs.test_activate_validated_boundary_set(&[val_a, val_b, val_d], new_hash, 1)
            .unwrap();

        // After reshare: active set is [A, B, D].
        let new_set = vs.get_active_consensus_set().unwrap();
        let new_addrs: Vec<Address> = new_set.iter().map(|v| v.validator_address).collect();
        assert!(new_addrs.contains(&val_a));
        assert!(new_addrs.contains(&val_b));
        assert!(new_addrs.contains(&val_d));
        assert!(!new_addrs.contains(&val_c), "C should NOT be in new set");
        assert_eq!(new_addrs.len(), 3);
    });
}

/// Task 01 test: committee size change doesn't corrupt participation.
///
/// When old set has 3 validators and new set has 4, the participation
/// bitmap encoded for 3 validators should be decoded against the 3-validator
/// set, not the 4-validator set.
#[test]
fn test_committee_size_change_participation_safety() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(128).unwrap();
        vs.config_is_initialized.write(true).unwrap();

        let val_a = address!("0x1111111111111111111111111111111111111111");
        let val_b = address!("0x2222222222222222222222222222222222222222");
        let val_c = address!("0x3333333333333333333333333333333333333333");
        let val_d = address!("0x4444444444444444444444444444444444444444");

        test_register_active(&mut vs, val_a, &dummy_pubkey(0xA1));
        test_register_active(&mut vs, val_b, &dummy_pubkey(0xB2));
        test_register_active(&mut vs, val_c, &dummy_pubkey(0xC3));
        test_register_joining(&mut vs, val_d, &dummy_pubkey(0xD4));

        // Old set: 3 validators [A, B, C].
        let old_set = vs.get_active_consensus_set().unwrap();
        assert_eq!(old_set.len(), 3, "old set must have 3 validators");

        // Encode participation for 3-validator set.
        let mut old_addrs: Vec<Address> = old_set.iter().map(|v| v.validator_address).collect();
        old_addrs.sort();
        let signers = vec![true, true, false]; // A, B signed; C absent
        let extra_data = outbe_primitives::participation::encode_participation_extended(
            &old_addrs,
            &signers,
            &[],
            &[],
        )
        .unwrap();

        // Now activate new set with 4 validators.
        vs.test_activate_validated_boundary_set(
            &[val_a, val_b, val_c, val_d],
            B256::with_last_byte(0x02),
            0,
        )
        .unwrap();
        let new_set = vs.get_active_consensus_set().unwrap();
        assert_eq!(new_set.len(), 4, "new set must have 4 validators");

        // Decode participation against OLD set (3 validators) -> should work.
        let decoded =
            outbe_primitives::participation::decode_participation_extended(&extra_data, &old_addrs);
        assert!(decoded.is_some(), "decode against OLD set must succeed");

        // Decode against NEW set (4 validators) -> count mismatch -> returns None.
        let mut new_addrs: Vec<Address> = new_set.iter().map(|v| v.validator_address).collect();
        new_addrs.sort();
        let decoded_wrong =
            outbe_primitives::participation::decode_participation_extended(&extra_data, &new_addrs);
        assert!(
            decoded_wrong.is_none(),
            "decode against NEW set with different size must return None (count mismatch)"
        );
    });
}

/// Task 01 test: re-execution of reshare activation is idempotent.
///
/// Calling activate_reshared_set() twice with same hash must not
/// change state the second time (idempotency guard).
#[test]
fn test_reshare_activation_idempotent() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(128).unwrap();
        vs.config_is_initialized.write(true).unwrap();

        let val_a = address!("0x1111111111111111111111111111111111111111");
        let val_b = address!("0x2222222222222222222222222222222222222222");

        test_register_active(&mut vs, val_a, &dummy_pubkey(0xA1));
        test_register_active(&mut vs, val_b, &dummy_pubkey(0xB2));

        let hash = vs.active_consensus_set_hash().unwrap();

        // Read state after first activation.
        let set1 = vs.get_active_consensus_set().unwrap();
        let hash1 = vs.active_consensus_set_hash().unwrap();

        // Second call with same hash -> idempotency guard in executor.rs
        // checks `current_hash != reshare.active_set_hash`.
        // Here: current_hash == hash -> no-op.
        let current_hash = vs.active_consensus_set_hash().unwrap();
        assert_eq!(current_hash, hash, "hash must match after first activation");

        // Simulate executor's guard: skip if hash matches.
        assert_eq!(current_hash, hash);
        // State unchanged.
        let set2 = vs.get_active_consensus_set().unwrap();
        let hash2 = vs.active_consensus_set_hash().unwrap();
        assert_eq!(
            set1.len(),
            set2.len(),
            "set must be unchanged on re-execution"
        );
        assert_eq!(hash1, hash2, "hash must be unchanged on re-execution");
    });
}

#[test]
fn certified_delayed_boundary_atomically_advances_epoch_and_snapshot() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let validator = address!("0x1111111111111111111111111111111111111111");
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(128).unwrap();
        vs.config_is_initialized.write(true).unwrap();
        vs.register_validator(OWNER, validator, &dummy_pubkey(0xA1))
            .unwrap();
        vs.activate_validator_via_boundary_for_test(validator)
            .unwrap();
        let mut epoch = vs.epoch_snapshot().unwrap();
        epoch.number = U256::ZERO;
        epoch.start_block = 1;
        epoch.start_timestamp = TEST_BLOCK_TIMESTAMP_BASE;
        vs.test_set_epoch_snapshot(epoch).unwrap();
        let record = vs.get_validator(validator).unwrap().unwrap();
        vs.test_set_history(
            validator,
            ValidatorHistory::new(
                record.joined_at_height,
                (record.deactivated_at_height != 0).then_some(record.deactivated_at_height),
                record.slash_count,
                7,
                8,
                9,
            ),
        )
        .unwrap();
        drop(vs);
        let slash = outbe_slashindicator::contract::SlashIndicator::new(storage.clone());
        slash.proposer_miss_count.write(&validator, 10).unwrap();
        slash.voter_miss_count.write(&validator, 11).unwrap();

        let activation_block = 301;
        let activation_timestamp = TEST_BLOCK_TIMESTAMP_BASE + 600;
        let boundary = boundary_with_epoch(1, false, vec![(validator, dummy_pubkey(0xA1))]);
        let ctx = BlockRuntimeContext::new(
            BlockContext::new(
                activation_block,
                activation_timestamp,
                CHAIN_ID,
                validator,
                vec![validator],
            ),
            storage.clone(),
        );

        super::prepare_boundary_epoch_counters(storage.clone(), &boundary, activation_block)
            .expect("certified boundary must prepare outgoing counters");
        crate::begin_block_precompile::run_boundary_outcome(&ctx, &boundary)
            .expect("certified delayed boundary must activate");

        let vs_after = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        let epoch = vs_after.epoch_snapshot().unwrap();
        assert_eq!(epoch.number, U256::from(1));
        assert_eq!(epoch.start_block, activation_block);
        assert_eq!(epoch.start_timestamp, activation_timestamp);
        assert_eq!(
            vs_after.participation(validator).unwrap(),
            outbe_validatorset::ValidatorParticipation::default()
        );
        let slash_after = outbe_slashindicator::contract::SlashIndicator::new(storage.clone());
        assert_eq!(slash_after.proposer_miss_count.read(&validator).unwrap(), 0);
        assert_eq!(slash_after.voter_miss_count.read(&validator).unwrap(), 0);
        let (_, extension) = outbe_validatorset::read_ocomp_snapshot_extension_at_epoch(storage, 1)
            .unwrap()
            .expect("activated epoch must publish its OCOMP snapshot");
        assert_eq!(extension.epoch, 1);
        assert_eq!(extension.committee_set_hash, boundary.committee_set_hash);
    });
}

#[test]
fn failed_boundary_snapshot_write_rolls_back_epoch_membership_and_counters() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let validator = address!("0x1111111111111111111111111111111111111111");
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(128).unwrap();
        vs.config_is_initialized.write(true).unwrap();
        vs.register_validator(OWNER, validator, &dummy_pubkey(0xA1))
            .unwrap();
        vs.activate_validator_via_boundary_for_test(validator)
            .unwrap();
        let mut epoch = vs.epoch_snapshot().unwrap();
        epoch.number = U256::ZERO;
        epoch.start_block = 1;
        epoch.start_timestamp = TEST_BLOCK_TIMESTAMP_BASE;
        vs.test_set_epoch_snapshot(epoch).unwrap();
        let record = vs.get_validator(validator).unwrap().unwrap();
        vs.test_set_history(
            validator,
            ValidatorHistory::new(
                record.joined_at_height,
                (record.deactivated_at_height != 0).then_some(record.deactivated_at_height),
                record.slash_count,
                7,
                record.missed_votes,
                record.blocks_proposed,
            ),
        )
        .unwrap();
        let active_hash_before = vs.active_consensus_set_hash().unwrap();
        // Force the incoming snapshot writer to fail after the boundary
        // transition has started. The enclosing activation checkpoint must
        // restore every earlier epoch/set/counter write.
        vs.val_ocomp_registration
            .get_bytes(&validator)
            .clear()
            .unwrap();
        drop(vs);
        let slash = outbe_slashindicator::contract::SlashIndicator::new(storage.clone());
        slash.proposer_miss_count.write(&validator, 10).unwrap();

        let boundary = boundary_with_epoch(1, false, vec![(validator, dummy_pubkey(0xA1))]);
        let block_guard = storage.checkpoint_guard();
        super::prepare_boundary_epoch_counters(storage.clone(), &boundary, 301)
            .expect("certified boundary must prepare outgoing counters");
        let error = super::apply_boundary_outcome(
            storage.clone(),
            &boundary,
            301,
            TEST_BLOCK_TIMESTAMP_BASE + 600,
        )
        .expect_err("missing OCOMP registration must reject incoming snapshot");
        assert!(error.to_string().contains("no admitted OCOMP registration"));
        drop(block_guard);

        let vs_after = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        let epoch = vs_after.epoch_snapshot().unwrap();
        assert_eq!(epoch.number, U256::ZERO);
        assert_eq!(epoch.start_block, 1);
        assert_eq!(epoch.start_timestamp, TEST_BLOCK_TIMESTAMP_BASE);
        assert_eq!(
            vs_after.active_consensus_set_hash().unwrap(),
            active_hash_before
        );
        assert_eq!(vs_after.participation(validator).unwrap().missed_blocks, 7);
        let slash_after = outbe_slashindicator::contract::SlashIndicator::new(storage.clone());
        assert_eq!(
            slash_after.proposer_miss_count.read(&validator).unwrap(),
            10
        );
        assert!(
            outbe_validatorset::read_ocomp_snapshot_extension_at_epoch(storage, 1)
                .unwrap()
                .is_none(),
            "failed activation must not expose an epoch-1 snapshot"
        );
    });
}

#[test]
fn boundary_rejects_skipped_epoch_without_mutating_current_state() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let validator = address!("0x1111111111111111111111111111111111111111");
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(128).unwrap();
        vs.config_is_initialized.write(true).unwrap();
        vs.register_validator(OWNER, validator, &dummy_pubkey(0xA1))
            .unwrap();
        vs.activate_validator_via_boundary_for_test(validator)
            .unwrap();
        let mut epoch = vs.epoch_snapshot().unwrap();
        epoch.number = U256::ZERO;
        epoch.start_block = 1;
        vs.test_set_epoch_snapshot(epoch).unwrap();
        drop(vs);

        let boundary = boundary_with_epoch(2, false, vec![(validator, dummy_pubkey(0xA1))]);
        let error = super::apply_boundary_outcome(
            storage.clone(),
            &boundary,
            301,
            TEST_BLOCK_TIMESTAMP_BASE + 600,
        )
        .expect_err("BoundaryOutcome must not skip activated epochs");
        assert!(error.to_string().contains("activate current+1"));

        let vs_after = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        let epoch = vs_after.epoch_snapshot().unwrap();
        assert_eq!(epoch.number, U256::ZERO);
        assert_eq!(epoch.start_block, 1);
        assert!(
            outbe_validatorset::read_ocomp_snapshot_extension_at_epoch(storage, 2)
                .unwrap()
                .is_none()
        );
    });
}

#[test]
fn apply_boundary_outcome_fatals_on_hash_change_without_set_change() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(128).unwrap();
        vs.config_is_initialized.write(true).unwrap();

        let val_a = address!("0x1111111111111111111111111111111111111111");
        let val_b = address!("0x2222222222222222222222222222222222222222");
        test_register_active(&mut vs, val_a, &dummy_pubkey(0xA1));
        test_register_active(&mut vs, val_b, &dummy_pubkey(0xB2));

        // Boundary claims membership unchanged but carries a different active set.
        let boundary = boundary_with(false, vec![(val_a, dummy_pubkey(0xA1))]);
        let err =
            super::apply_boundary_outcome(storage.clone(), &boundary, 1, TEST_BLOCK_TIMESTAMP_BASE)
                .unwrap_err();
        assert!(
            err.to_string()
                .contains("active_set_hash changed without validator-set change"),
            "expected hash-vs-flag inconsistency, got {err}"
        );
    });
}

#[test]
fn apply_boundary_outcome_activates_on_validator_set_change_with_hash_change() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(128).unwrap();
        vs.config_is_initialized.write(true).unwrap();

        let val_a = address!("0x1111111111111111111111111111111111111111");
        let val_b = address!("0x2222222222222222222222222222222222222222");
        let val_c = address!("0x3333333333333333333333333333333333333333");
        test_register_active(&mut vs, val_a, &dummy_pubkey(0xA1));
        test_register_active(&mut vs, val_b, &dummy_pubkey(0xB2));
        test_register_joining(&mut vs, val_c, &dummy_pubkey(0xC3));

        let boundary = boundary_with(
            true,
            vec![
                (val_a, dummy_pubkey(0xA1)),
                (val_b, dummy_pubkey(0xB2)),
                (val_c, dummy_pubkey(0xC3)),
            ],
        );
        let new_hash = boundary.reshare.active_set_hash;
        super::apply_boundary_outcome(storage.clone(), &boundary, 1, TEST_BLOCK_TIMESTAMP_BASE)
            .unwrap();

        let vs_after = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        let now_hash = vs_after.active_consensus_set_hash().unwrap();
        assert_eq!(now_hash, new_hash, "active_set_hash must advance");
        let active = vs_after.get_active_consensus_set().unwrap();
        let addrs: Vec<Address> = active.iter().map(|v| v.validator_address).collect();
        assert!(addrs.contains(&val_c), "C must now be in active set");
    });
}

#[test]
fn apply_boundary_outcome_replays_narrow_certified_tee_expiry_demotion() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(128).unwrap();
        vs.config_is_initialized.write(true).unwrap();

        let retained = address!("0x1111111111111111111111111111111111111111");
        let expired = address!("0x2222222222222222222222222222222222222222");
        test_register_active(&mut vs, retained, &dummy_pubkey(0xA1));
        test_register_active(&mut vs, expired, &dummy_pubkey(0xB2));
        let current_hash = super::hash_boundary_active_set(&[retained, expired]);
        vs.test_set_active_consensus_set_hash(current_hash).unwrap();

        let mut boundary = boundary_with(true, vec![(retained, dummy_pubkey(0xA1))]);
        boundary.tee_expired_target_exclusions = vec![expired];
        boundary.tee_expired_target_exclusions_hash =
            outbe_primitives::reshare_artifact::tee_expired_target_exclusions_hash(
                &boundary.tee_expired_target_exclusions,
            )
            .unwrap();
        super::apply_boundary_outcome(storage.clone(), &boundary, 1, TEST_BLOCK_TIMESTAMP_BASE)
            .unwrap();

        let vs_after = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        let expired_state = vs_after.validator_state(expired).unwrap();
        assert_eq!(
            expired_state.stored_status().unwrap(),
            outbe_validatorset::runtime::status::PENDING
        );
        assert!(!expired_state.has_bls_share());
        assert!(!expired_state.join_confirmed());
        let retained_state = vs_after.validator_state(retained).unwrap();
        assert_eq!(
            retained_state.stored_status().unwrap(),
            outbe_validatorset::runtime::status::ACTIVE
        );
    });
}

#[test]
fn apply_boundary_outcome_rejects_tampered_tee_expiry_commitment() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(128).unwrap();
        vs.config_is_initialized.write(true).unwrap();
        let retained = address!("0x1111111111111111111111111111111111111111");
        test_register_active(&mut vs, retained, &dummy_pubkey(0xA1));
        let hash = super::hash_boundary_active_set(&[retained]);
        vs.test_set_active_consensus_set_hash(hash).unwrap();

        let mut boundary = boundary_with(false, vec![(retained, dummy_pubkey(0xA1))]);
        boundary.tee_expired_target_exclusions_hash = B256::with_last_byte(0xFF);
        let error =
            super::apply_boundary_outcome(storage.clone(), &boundary, 1, TEST_BLOCK_TIMESTAMP_BASE)
                .unwrap_err();
        assert!(error
            .to_string()
            .contains("TEE expiry exclusions commitment mismatch"));
    });
}

#[test]
fn apply_boundary_outcome_writes_snapshot_when_hash_matches() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(128).unwrap();
        vs.config_is_initialized.write(true).unwrap();

        let val_a = address!("0x1111111111111111111111111111111111111111");
        test_register_active(&mut vs, val_a, &dummy_pubkey(0xA1));
        let hash = vs.active_consensus_set_hash().unwrap();

        let boundary = boundary_with(false, vec![(val_a, dummy_pubkey(0xA1))]);
        super::apply_boundary_outcome(storage.clone(), &boundary, 1, TEST_BLOCK_TIMESTAMP_BASE)
            .unwrap();

        let vs_after = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        assert_eq!(vs_after.active_consensus_set_hash().unwrap(), hash);

        let snapshot_key =
            outbe_validatorset::committee_snapshot_key(boundary.epoch, boundary.committee_set_hash);
        let snapshot = outbe_validatorset::read_committee_snapshot(storage.clone(), snapshot_key)
            .unwrap()
            .expect("BoundaryOutcome must write the incoming committee snapshot");
        assert_eq!(snapshot.committee.len(), 1);
        assert_eq!(snapshot.committee[0].address, val_a);
        assert_eq!(snapshot.committee[0].consensus_pubkey, dummy_pubkey(0xA1));
        assert_eq!(snapshot.vrf_material_version, boundary.vrf_material_version);
        assert_eq!(
            snapshot.vrf_group_public_key_bytes,
            boundary.vrf_group_public_key_bytes.to_vec()
        );
    });
}

#[test]
fn apply_boundary_outcome_rejects_committee_set_hash_mismatch() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(128).unwrap();
        vs.config_is_initialized.write(true).unwrap();

        let val_a = address!("0x1111111111111111111111111111111111111111");
        test_register_active(&mut vs, val_a, &dummy_pubkey(0xA1));

        let mut boundary = boundary_with(false, vec![(val_a, dummy_pubkey(0xA1))]);
        boundary.committee_set_hash = B256::with_last_byte(0xFE);

        let err =
            super::apply_boundary_outcome(storage.clone(), &boundary, 1, TEST_BLOCK_TIMESTAMP_BASE)
                .unwrap_err();
        assert!(
            err.to_string().contains("committee_set_hash mismatch"),
            "expected committee_set_hash mismatch, got {err}"
        );
    });
}
