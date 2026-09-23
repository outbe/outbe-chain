use super::*;

fn test_priority_fee_tx() -> reth_ethereum::TransactionSigned {
    TxEip1559 {
        chain_id: CHAIN_ID,
        nonce: 0,
        gas_limit: 21_000,
        max_fee_per_gas: (MIN_PROTOCOL_BASE_FEE * 2) as u128,
        max_priority_fee_per_gas: MIN_PROTOCOL_BASE_FEE as u128,
        to: TxKind::Call(Address::ZERO),
        value: U256::ZERO,
        input: Bytes::new(),
        access_list: Default::default(),
    }
    .into_signed(Signature::test_signature())
    .into()
}

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
        None,
        Bytes::new(),
        None,
        false,
        None,
        ctx.inner.parent_hash,
        None,
        ctx.expected_begin_system_txs.clone(),
        ctx.expected_end_system_txs.clone(),
        ctx.system_layout_error.clone(),
        ctx.parent_consensus_metadata.clone(),
        ctx.proposer_evm_address,
        ctx.execute_outbe_block_hooks,
        ctx.prebuilt_phase1_tx.clone(),
        ctx.parent_artifact_hint,
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
        None,
        Bytes::new(),
        None,
        false,
        None,
        ctx.inner.parent_hash,
        Some(signer.clone()),
        ctx.expected_begin_system_txs.clone(),
        ctx.expected_end_system_txs.clone(),
        ctx.system_layout_error.clone(),
        ctx.parent_consensus_metadata.clone(),
        ctx.proposer_evm_address,
        ctx.execute_outbe_block_hooks,
        ctx.prebuilt_phase1_tx.clone(),
        ctx.parent_artifact_hint,
    )
    .with_pending_tee_bootstrap(ctx.pending_tee_bootstrap.clone());

    executor
        .apply_pre_execution_changes()
        .expect("block 1 pre-execution changes should apply");
    let system_txs =
        begin_system_txs_for_test(&config, 1, B256::ZERO, &Bytes::new(), None, proposer);
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
        None,
        Bytes::new(),
        None,
        false,
        None,
        ctx.inner.parent_hash,
        Some(signer),
        ctx.expected_begin_system_txs.clone(),
        ctx.expected_end_system_txs.clone(),
        ctx.system_layout_error.clone(),
        ctx.parent_consensus_metadata.clone(),
        ctx.proposer_evm_address,
        ctx.execute_outbe_block_hooks,
        ctx.prebuilt_phase1_tx.clone(),
        ctx.parent_artifact_hint,
    )
    .with_pending_tee_bootstrap(ctx.pending_tee_bootstrap.clone());

    executor
        .apply_pre_execution_changes()
        .expect("block 1 pre-execution changes should apply");
    let mut visible_system_gas = 0u64;
    for tx in begin_system_txs_for_test(&config, 1, B256::ZERO, &Bytes::new(), None, proposer) {
        let signed_gas_limit = tx.tx().gas_limit();
        let gas_used = executor
            .execute_transaction(tx)
            .expect("begin-zone system tx should execute")
            .tx_gas_used();
        assert!(gas_used <= signed_gas_limit);
        visible_system_gas += gas_used;
    }

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
    let emission_trigger = outbe_cycle::triggers::TriggerId::ProtocolCycle.as_u32();
    let mut state =
        state_with_active_validators_seeded(&[(proposer, dummy_pubkey(0xA2))], |storage| {
            let genesis_ctx = BlockRuntimeContext::new(
                BlockContext::new(0, GENESIS_TS, CHAIN_ID, proposer, vec![proposer]),
                storage.clone(),
            );
            outbe_rewards::runtime::ensure_genesis_anchor(&genesis_ctx).unwrap();
            let cycle = outbe_cycle::schema::Cycle::new(storage);
            cycle
                .active_utc_day
                .write(outbe_primitives::time::timestamp_to_date_key(GENESIS_TS))
                .unwrap();
            cycle
                .last_executed_at
                .write(&emission_trigger, GENESIS_TS + 60)
                .unwrap();
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
    let system_txs = begin_system_txs_for_test_with_bootstrap(
        &config,
        1,
        B256::ZERO,
        &Bytes::new(),
        None,
        proposer,
        Some(tee_bootstrap),
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
    let emission_trigger = outbe_cycle::triggers::TriggerId::ProtocolCycle.as_u32();
    let mut state =
        state_with_active_validators_seeded(&[(proposer, dummy_pubkey(0xA2))], |storage| {
            let genesis_ctx = BlockRuntimeContext::new(
                BlockContext::new(0, GENESIS_TS, CHAIN_ID, proposer, vec![proposer]),
                storage.clone(),
            );
            outbe_rewards::runtime::ensure_genesis_anchor(&genesis_ctx).unwrap();
            let cycle = outbe_cycle::schema::Cycle::new(storage);
            cycle
                .active_utc_day
                .write(outbe_primitives::time::timestamp_to_date_key(GENESIS_TS))
                .unwrap();
            cycle
                .last_executed_at
                .write(&emission_trigger, GENESIS_TS + 60)
                .unwrap();
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
    let system_txs = begin_system_txs_for_test_with_bootstrap(
        &config,
        1,
        B256::ZERO,
        &Bytes::new(),
        None,
        proposer,
        Some(tee_bootstrap),
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
    const ACTIVE_COUNT: usize = 128;
    const DEADLINE: u64 = 2;

    let signer = test_evm_signer();
    let proposer = signer.address();
    let mut validators = Vec::with_capacity(ACTIVE_COUNT);
    validators.push((proposer, dummy_pubkey(0x80)));
    for index in 1..ACTIVE_COUNT {
        validators.push((
            numbered_test_address(0x81, index as u64),
            dummy_pubkey(index as u8),
        ));
    }
    let addresses: Vec<_> = validators.iter().map(|(address, _)| *address).collect();
    let mut state = state_with_active_validators_seeded_at_block(&validators, 1, |storage| {
        let registry = outbe_teeregistry::TeeRegistry::new(storage);
        for (index, validator) in addresses.iter().enumerate() {
            let node_hash = keccak256((index as u64).to_be_bytes());
            registry
                .validator_v1_node_hash
                .write(validator, node_hash)
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
                .write(&node_hash, DEADLINE)
                .unwrap();
        }
    });
    let mut evm_env = test_evm_env(2, REWARDS_ADDRESS);
    evm_env.block_env.timestamp = U256::from(DEADLINE);
    let config = OutbeEvmConfig::new(test_chain_spec()).with_evm_signer(signer.clone());
    let parent_hash = B256::repeat_byte(0x91);
    let mut parent_metadata = metadata_with(addresses.clone(), vec![1; ACTIVE_COUNT], Vec::new());
    parent_metadata.finalized_block_number = 1;
    parent_metadata.finalized_block_hash = parent_hash;
    let evm = config.evm_with_env(&mut state, evm_env);
    let mut execution = execution_ctx(Some(0), Bytes::new());
    execution.inner.parent_hash = parent_hash;
    execution.parent_consensus_metadata = Some(parent_metadata.clone());
    execution.parent_artifact_hint = Some(AccountedParentArtifact {
        summary: ExecutionSummaryArtifact {
            validator_fee_sum: U256::ZERO,
        },
        timestamp: 1,
        state_root: Some(B256::repeat_byte(0x92)),
    });
    execution.proposer_evm_address = Some(proposer);
    let mut executor = config.create_executor(evm, execution);
    super::with_phase1_verify_disabled(|| {
        executor
            .apply_pre_execution_changes()
            .expect("pre-execution changes should apply");
    });

    let system_txs = begin_system_txs_for_test(
        &config,
        2,
        parent_hash,
        &Bytes::new(),
        Some(parent_metadata),
        proposer,
    );
    let mut cycle_gas = None;
    let mut cycle_internal_gas = None;
    let mut cycle_receipt_index = None;
    for tx in system_txs {
        let kind = SystemTxInputV2::decode(tx.tx().input().as_ref())
            .expect("valid begin-zone system transaction")
            .kind();
        let internal_before = executor.system_tx_execution_gas;
        let output = executor
            .execute_transaction(tx)
            .expect("TEE expiry begin-zone prefix should execute");
        if kind == SystemTxKind::CycleTick {
            cycle_gas = Some(output.tx_gas_used());
            cycle_internal_gas = Some(
                executor
                    .system_tx_execution_gas
                    .saturating_sub(internal_before),
            );
            cycle_receipt_index = Some(executor.receipts().len() - 1);
            break;
        }
    }
    let cycle_gas = cycle_gas.expect("CycleTick gas must be captured");
    let cycle_internal_gas = cycle_internal_gas.expect("CycleTick internal gas must be captured");
    let receipt = &executor.receipts()[cycle_receipt_index.expect("CycleTick receipt index")];
    assert!(receipt.success, "worst-case TEE expiry sweep must not OOG");
    assert_eq!(
        receipt
            .logs
            .iter()
            .filter(|log| {
                log.address == outbe_primitives::addresses::VALIDATOR_SET_ADDRESS
                    && log.data.topics().first()
                        == Some(&keccak256("ValidatorJailed(address,uint64)"))
            })
            .count(),
        ACTIVE_COUNT
    );
    eprintln!(
        "TEE expiry CycleTick gas: active={ACTIVE_COUNT}, visible={cycle_gas}, internal={cycle_internal_gas}, limit=30000000"
    );
    assert!(cycle_gas < 30_000_000);
    assert!(cycle_internal_gas < 30_000_000);
}

#[test]
fn capacity_forfeiture_cycle_tick_keeps_twenty_percent_block_headroom() {
    use reth_trie::{test_utils::state_root_prehashed, HashedPostState, KeccakKeyHasher};

    const BLOCK_GAS_LIMIT: u64 = 30_000_000;
    const REQUIRED_HEADROOM_BPS: u64 = 2_000;
    const BPS_DENOMINATOR: u64 = 10_000;
    const SECONDS_PER_DAY: u64 = 86_400;

    fn post_state_root(state: &revm::database::BundleState) -> B256 {
        let sorted =
            HashedPostState::from_bundle_state::<KeccakKeyHasher>(state.state()).into_sorted();
        let storages = sorted.storages;
        let accounts = sorted
            .accounts
            .into_iter()
            .filter_map(|(address, account)| {
                account.map(|account| {
                    let storage = storages
                        .get(&address)
                        .map(|storage| storage.storage_slots.clone())
                        .unwrap_or_default();
                    (address, (account, storage))
                })
            });
        state_root_prehashed(accounts)
    }

    let run = || {
        let signer = test_evm_signer();
        let proposer = signer.address();
        let victim = WorldwideDay::new(2023_1101);
        let day_limit = U256::from(100);
        let mut fire_at = 0_u64;
        let (tree_directory, tree_service) = persistent_test_tree(B256::ZERO);
        let empty_root = outbe_compressed_entities::sealed_root(B256::ZERO).unwrap();
        let parent_tree = tree_service
            .open_parent(ExactParentIdentity {
                commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                block_number: 0,
                block_hash: B256::ZERO,
                root: empty_root,
            })
            .expect("open exact empty CE parent");
        let seed_scope =
            ExecutionScope::with_parent_tree(parent_tree, CeWorkConfig::new(0, 0, u64::MAX));
        let body_storage = Arc::new(MemoryStorage::new());
        let body_reader: StorageReaderHandle = body_storage;
        let tribute_parent = TributeRepositoryReader::new(body_reader.clone());
        let mut staged_tree_batch = None;
        let mut state = state_with_active_validators_seeded_at_block_with_cycle_frames(
            &[(proposer, dummy_pubkey(0xA3))],
            1,
            4,
            |storage| {
                outbe_compressed_entities::begin_block(storage.clone(), &seed_scope)
                    .expect("open CE seed block");
                let genesis_ctx = BlockRuntimeContext::new(
                    BlockContext::new(0, 1_704_067_200, CHAIN_ID, proposer, vec![proposer]),
                    storage.clone(),
                );
                outbe_rewards::runtime::ensure_genesis_anchor(&genesis_ctx).unwrap();
                let mut tribute = TributeContract::new(storage.clone());
                tribute.initialize_fresh_ocomp_profile().unwrap();
                let retained = (0..outbe_metadosis::constants::MAX_RETAINED_WWDS)
                    .map(|offset| {
                        let days_before = outbe_metadosis::constants::MAX_RETAINED_WWDS - offset;
                        WorldwideDay::from_timestamp(
                            victim.start_timestamp()
                                - u64::try_from(days_before).unwrap() * SECONDS_PER_DAY,
                        )
                    })
                    .collect::<Vec<_>>();
                outbe_metadosis::test_support::seed_ready_worldwide_days_for_capacity(
                    storage.clone(),
                    &retained,
                )
                .unwrap();
                let victim_ctx = BlockRuntimeContext::new(
                    BlockContext::new(
                        1,
                        victim.start_timestamp() + 2 * 3_600,
                        CHAIN_ID,
                        proposer,
                        vec![proposer],
                    ),
                    storage.clone(),
                );
                outbe_metadosis::commands::apply_cycle_day_limit(&victim_ctx, day_limit).unwrap();
                let victim_projection =
                    outbe_metadosis::api::worldwide_day(storage.clone(), victim)
                        .unwrap()
                        .unwrap();
                tribute.unseal_day(victim).unwrap();
                tribute
                    .issue(
                        &seed_scope,
                        &tribute_parent,
                        &TributeData {
                            tribute_id: outbe_compressed_entities::derive_poseidon_entity_id(
                                proposer, victim,
                            )
                            .unwrap(),
                            owner: proposer,
                            worldwide_day: victim,
                            issuance_amount_minor: U256::from(1),
                            issuance_currency: 840,
                            nominal_amount_minor: U256::from(1),
                            reference_currency: 840,
                            tribute_price_minor: U256::from(1),
                            exclude_from_intex_issuance: false,
                        },
                    )
                    .unwrap();
                for boundary in [
                    victim_projection.forming_end,
                    victim_projection.lookback_end,
                    victim_projection.offering_end,
                ] {
                    let ctx = BlockRuntimeContext::new(
                        BlockContext::new(1, boundary, CHAIN_ID, proposer, vec![proposer]),
                        storage.clone(),
                    );
                    outbe_metadosis::commands::advance_active_worldwide_days(&ctx, &seed_scope)
                        .unwrap();
                }
                assert_eq!(
                    outbe_metadosis::api::worldwide_day(storage.clone(), victim)
                        .unwrap()
                        .unwrap()
                        .status,
                    outbe_metadosis::api::WorldwideDayStatus::Waiting
                );
                tribute
                    .day_totals
                    .update(&outbe_tribute::DayTotals {
                        worldwide_day: victim,
                        initialized: true,
                        tribute_count: u32::MAX,
                        tribute_nominal_amount: U256::MAX,
                        is_sealed: true,
                    })
                    .unwrap();
                tribute.total_supply.write(u64::from(u32::MAX)).unwrap();
                let scheduled = victim_projection.scheduled_process_time;
                let protocol_cycle_period = 3_600;
                fire_at = scheduled.div_ceil(protocol_cycle_period) * protocol_cycle_period;
                let cycle = outbe_cycle::schema::Cycle::new(storage.clone());
                cycle
                    .active_utc_day
                    .write(outbe_primitives::time::timestamp_to_date_key(fire_at))
                    .unwrap();
                for spec in outbe_cycle::triggers::ACTIVE_TRIGGERS {
                    cycle
                        .last_executed_at
                        .write(
                            &spec.id,
                            if spec.id == outbe_cycle::triggers::TriggerId::ProtocolCycle.as_u32() {
                                fire_at - protocol_cycle_period
                            } else {
                                fire_at
                            },
                        )
                        .unwrap();
                }
                staged_tree_batch = Some(
                    outbe_compressed_entities::end_block(storage, &seed_scope)
                        .expect("seal populated Tribute seed block")
                        .staged_tree_batch,
                );
            },
        );
        let staged_tree_batch = staged_tree_batch.expect("seed block must stage CE work");
        let seed_hash = B256::repeat_byte(0xA5);
        let seed_root = staged_tree_batch.new_root();
        tree_service
            .publish_candidate(seed_hash, staged_tree_batch)
            .expect("publish populated Tribute seed");
        tree_service
            .apply_finalized(1, seed_hash, seed_root)
            .expect("finalize populated Tribute seed");

        let mut evm_env = test_evm_env(2, REWARDS_ADDRESS);
        evm_env.block_env.timestamp = U256::from(fire_at);
        let config = OutbeEvmConfig::new_with_runtime_body_readers(
            test_chain_spec(),
            RuntimeBodyReaders::new(body_reader),
        )
        .with_evm_signer(signer.clone())
        .with_compressed_tree_service(tree_service);
        let mut parent_metadata = metadata_with(vec![proposer], vec![1], Vec::new());
        parent_metadata.finalized_block_number = 1;
        parent_metadata.finalized_block_hash = seed_hash;
        let evm = config.evm_with_env(&mut state, evm_env);
        let mut execution = execution_ctx(Some(0), Bytes::new());
        execution.inner.parent_hash = seed_hash;
        execution.parent_consensus_metadata = Some(parent_metadata.clone());
        execution.parent_artifact_hint = Some(AccountedParentArtifact {
            summary: ExecutionSummaryArtifact {
                validator_fee_sum: U256::ZERO,
            },
            timestamp: 0,
            state_root: Some(B256::repeat_byte(0x91)),
        });
        execution.proposer_evm_address = Some(proposer);
        let mut executor = config.create_executor(evm, execution);
        super::with_phase1_verify_disabled(|| {
            executor
                .apply_pre_execution_changes()
                .expect("pre-execution changes should apply");
        });
        let system_txs = begin_system_txs_for_test(
            &config,
            2,
            seed_hash,
            &Bytes::new(),
            Some(parent_metadata),
            proposer,
        );
        let mut visible_gas = None;
        let mut cycle_receipt_index = None;
        for tx in system_txs {
            let kind = SystemTxInputV2::decode(tx.tx().input().as_ref())
                .expect("valid begin-zone system tx")
                .kind();
            let output = executor
                .execute_transaction(tx)
                .expect("begin-zone prefix through CapacityForfeiture CycleTick must execute");
            if kind == SystemTxKind::CycleTick {
                visible_gas = Some(output.tx_gas_used());
                cycle_receipt_index = Some(executor.receipts().len() - 1);
                break;
            }
        }
        let visible_gas = visible_gas.expect("CycleTick visible gas");
        let cycle_receipt_index = cycle_receipt_index.expect("CycleTick receipt index");
        let maximum_used =
            BLOCK_GAS_LIMIT * (BPS_DENOMINATOR - REQUIRED_HEADROOM_BPS) / BPS_DENOMINATOR;
        eprintln!(
            "CapacityForfeiture CycleTick visible gas: used={visible_gas}, max_for_20pct_headroom={maximum_used}"
        );
        assert!(executor.receipts()[cycle_receipt_index].success);
        assert!(
            visible_gas <= maximum_used,
            "CapacityForfeiture CycleTick visible gas {visible_gas} leaves less than 20% headroom"
        );
        let capacity_event = keccak256(
            "WorldwideDayCapacityForfeited(uint32,uint32,uint32,uint256,uint256,uint256,bytes32,uint32,uint256,uint64,uint64,uint8,uint64)",
        );
        assert!(executor.receipts()[cycle_receipt_index]
            .logs
            .iter()
            .any(|log| {
                log.address == outbe_primitives::addresses::METADOSIS_ADDRESS
                    && log.data.topics().first() == Some(&capacity_event)
            }));
        let retirement_event = keccak256("TributePartitionRetired(uint32)");
        assert!(executor.receipts()[cycle_receipt_index]
            .logs
            .iter()
            .any(|log| {
                log.address == outbe_primitives::addresses::TRIBUTE_ADDRESS
                    && log.data.topics().first() == Some(&retirement_event)
            }));
        let capacity_log = executor.receipts()[cycle_receipt_index]
            .logs
            .iter()
            .find_map(|log| {
                outbe_metadosis::precompile::IMetadosis::WorldwideDayCapacityForfeited::decode_log(
                    log,
                )
                .ok()
            })
            .expect("typed capacity-forfeiture event");
        assert_eq!(capacity_log.forfeitedTributeCount, u32::MAX);
        assert_eq!(capacity_log.forfeitedTributeNominal, U256::MAX);
        assert_eq!(capacity_log.retirementOutcome, 2);
        let receipt = executor.receipts()[cycle_receipt_index].clone();
        drop(executor);
        (
            visible_gas,
            receipt,
            post_state_root(&state.bundle_state),
            seed_root,
            tree_directory,
        )
    };

    let proposer = run();
    let replay = run();
    assert_eq!(
        proposer.0, replay.0,
        "same-parent replay must reproduce visible gas"
    );
    assert_eq!(
        proposer.1, replay.1,
        "same-parent replay must reproduce the exact receipt and events"
    );
    assert_eq!(
        proposer.2, replay.2,
        "re-executing the same CapacityForfeiture CycleTick from the same parent must reproduce gas, receipt/events, and state root"
    );
    assert_eq!(proposer.3, replay.3, "seeded parent roots must match");
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
            let rewards = outbe_rewards::schema::Rewards::new(storage.clone());
            rewards
                .daily_voter_count
                .write(&prev_day, DENSE_VALIDATOR_COUNT)
                .unwrap();
            rewards
                .daily_total_participation
                .write(&prev_day, u64::from(DENSE_VALIDATOR_COUNT))
                .unwrap();
            for index in 0..DENSE_VALIDATOR_COUNT {
                let voter = numbered_test_address(0x12, u64::from(index));
                rewards
                    .daily_voter_at
                    .get_nested(&prev_day)
                    .write(&index, voter)
                    .unwrap();
                rewards
                    .daily_participation
                    .get_nested(&prev_day)
                    .write(&voter, 1)
                    .unwrap();
            }

            let mut agent = outbe_agentreward::AgentRewardContract::new(storage);
            for n in 0..DENSE_ADDRESS_COUNT {
                let waa = numbered_test_address(0x10, n);
                let sra = numbered_test_address(0x11, n);
                agent.increment_waa_tribute(prev_day.into(), waa).unwrap();
                agent.increment_sra_tribute(prev_day.into(), sra).unwrap();
            }
            assert_eq!(
                agent.get_all_waa_counts(prev_day.into()).unwrap().len(),
                DENSE_ADDRESS_COUNT as usize,
                "GAS-05 fixture must seed all dense WAA recipients"
            );
            assert_eq!(
                agent.get_all_sra_counts(prev_day.into()).unwrap().len(),
                DENSE_ADDRESS_COUNT as usize,
                "GAS-05 fixture must seed all dense SRA recipients"
            );
        });
    let mut evm_env = test_evm_env(1, REWARDS_ADDRESS);
    evm_env.block_env.timestamp = U256::from(block_ts);
    let config = OutbeEvmConfig::new(test_chain_spec()).with_evm_signer(signer.clone());
    let evm = config.evm_with_env(&mut state, evm_env);
    let mut executor = config.create_executor(evm, block_one_execution_ctx(Some(0), Bytes::new()));

    executor
        .apply_pre_execution_changes()
        .expect("pre-execution changes should apply");
    let mut system_txs =
        begin_system_txs_for_test(&config, 1, B256::ZERO, &Bytes::new(), None, proposer);
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
        let agent = outbe_agentreward::AgentRewardContract::new(storage.clone());
        assert!(
            agent.get_all_waa_counts(prev_day.into())?.is_empty(),
            "GAS-05: dense WAA day index must be cleared after CycleTick settlement"
        );
        assert!(
            agent.get_all_sra_counts(prev_day.into())?.is_empty(),
            "GAS-05: dense SRA day index must be cleared after CycleTick settlement"
        );

        let mut claimable_total = U256::ZERO;
        for n in 0..DENSE_ADDRESS_COUNT {
            let waa = numbered_test_address(0x10, n);
            let sra = numbered_test_address(0x11, n);
            let waa_claimable = agent.get_claimable_reward(waa)?;
            let sra_claimable = agent.get_claimable_reward(sra)?;
            assert!(
                !waa_claimable.is_zero(),
                "GAS-05: dense WAA recipient {waa} received zero claimable reward"
            );
            assert!(
                !sra_claimable.is_zero(),
                "GAS-05: dense SRA recipient {sra} received zero claimable reward"
            );
            claimable_total += waa_claimable + sra_claimable;
        }
        assert!(
            !claimable_total.is_zero(),
            "GAS-05: dense CycleTick must credit claimable AgentReward balances"
        );
        assert_eq!(
            storage.balance(outbe_primitives::addresses::AGENT_REWARD_ADDRESS)?,
            claimable_total,
            "GAS-05: AgentReward backing balance must match dense claimable total"
        );
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
        None,
        Bytes::new(),
        None,
        true,
        None,
        ctx.inner.parent_hash,
        Some(signer.clone()),
        ctx.expected_begin_system_txs.clone(),
        ctx.expected_end_system_txs.clone(),
        ctx.system_layout_error.clone(),
        ctx.parent_consensus_metadata.clone(),
        ctx.proposer_evm_address,
        ctx.execute_outbe_block_hooks,
        ctx.prebuilt_phase1_tx.clone(),
        ctx.parent_artifact_hint,
    )
    .with_pending_tee_bootstrap(ctx.pending_tee_bootstrap.clone());

    executor
        .apply_pre_execution_changes()
        .expect("pre-execution changes should apply");
    let mut system_txs =
        begin_system_txs_for_test(&config, 1, B256::ZERO, &Bytes::new(), None, proposer)
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
    // A forced OOG does not model Oracle performing ten billion units of
    // useful work: revm charges the complete system-call gas limit for any
    // OOG. Because mandatory phases have already consumed internal work,
    // accepting it as a soft failure would exceed the aggregate block
    // budget. The failure must therefore be hard and atomic even though an
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
        None,
        Bytes::new(),
        None,
        true,
        None,
        ctx.inner.parent_hash,
        Some(signer.clone()),
        ctx.expected_begin_system_txs.clone(),
        ctx.expected_end_system_txs.clone(),
        ctx.system_layout_error.clone(),
        ctx.parent_consensus_metadata.clone(),
        ctx.proposer_evm_address,
        ctx.execute_outbe_block_hooks,
        ctx.prebuilt_phase1_tx.clone(),
        ctx.parent_artifact_hint,
    )
    .with_pending_tee_bootstrap(ctx.pending_tee_bootstrap.clone());

    executor
        .apply_pre_execution_changes()
        .expect("pre-execution changes should apply");
    let mut expected_rpc_gas_deltas = Vec::new();
    for tx in begin_system_txs_for_test(&config, 1, B256::ZERO, &Bytes::new(), None, proposer) {
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
        None,
        Bytes::new(),
        None,
        true,
        None,
        ctx.inner.parent_hash,
        Some(signer.clone()),
        ctx.expected_begin_system_txs.clone(),
        ctx.expected_end_system_txs.clone(),
        ctx.system_layout_error.clone(),
        ctx.parent_consensus_metadata.clone(),
        ctx.proposer_evm_address,
        ctx.execute_outbe_block_hooks,
        ctx.prebuilt_phase1_tx.clone(),
        ctx.parent_artifact_hint,
    )
    .with_pending_tee_bootstrap(ctx.pending_tee_bootstrap.clone());

    executor
        .apply_pre_execution_changes()
        .expect("pre-execution changes should apply");
    let system_txs =
        begin_system_txs_for_test(&config, 1, B256::ZERO, &Bytes::new(), None, proposer);
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
        None,
        Bytes::new(),
        None,
        true,
        None,
        ctx.inner.parent_hash,
        Some(signer.clone()),
        ctx.expected_begin_system_txs.clone(),
        ctx.expected_end_system_txs.clone(),
        ctx.system_layout_error.clone(),
        ctx.parent_consensus_metadata.clone(),
        ctx.proposer_evm_address,
        ctx.execute_outbe_block_hooks,
        ctx.prebuilt_phase1_tx.clone(),
        ctx.parent_artifact_hint,
    )
    .with_pending_tee_bootstrap(ctx.pending_tee_bootstrap.clone());

    executor
        .apply_pre_execution_changes()
        .expect("pre-execution changes should apply");
    let mut visible_system_gas = 0u64;
    for tx in begin_system_txs_for_test(&config, 1, B256::ZERO, &Bytes::new(), None, proposer) {
        let signed_gas_limit = tx.tx().gas_limit();
        let gas_used = executor
            .execute_transaction(tx)
            .expect("system tx should execute")
            .tx_gas_used();
        assert!(gas_used <= signed_gas_limit);
        visible_system_gas += gas_used;
    }
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

#[test]
fn apply_pre_execution_changes_emits_phase1_slashing_logs_in_system_receipt() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let absent = address!("0x2222222222222222222222222222222222222222");
    let parent_hash = B256::with_last_byte(0xAA);
    let mut state = state_with_active_validators_seeded(
        &[(proposer, dummy_pubkey(0xA2)), (absent, dummy_pubkey(0xB3))],
        |storage| {
            let si = outbe_slashindicator::contract::SlashIndicator::new(storage);
            si.config_voter_misdemeanor_threshold.write(1).unwrap();
            si.config_proposer_felony_threshold.write(1).unwrap();
        },
    );
    let mut metadata = test_metadata();
    metadata.finalized_block_number = 1;
    metadata.finalized_block_hash = parent_hash;
    metadata.ordered_committee = vec![proposer, absent];
    metadata.signer_bitmap = vec![1, 0];
    metadata.missed_proposers = vec![outbe_primitives::consensus_metadata::MissedProposerEvent {
        view: 0,
        validator: absent,
    }];

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
    let config =
        OutbeEvmConfig::new_with_bridge(test_chain_spec(), bridge).with_evm_signer(signer.clone());
    let evm_env = test_evm_env(2, REWARDS_ADDRESS);
    let evm = config.evm_with_env(&mut state, evm_env);
    let mut ctx = execution_ctx(Some(0), Bytes::new());
    ctx.inner.parent_hash = parent_hash;
    ctx.parent_consensus_metadata = Some(metadata.clone());
    let mut executor = config.create_executor(evm, ctx);

    // opt out of Phase 1 `verify_v2_proof` preflight - this
    // unit test exercises the slashing log emission path, not the
    // verifier itself, and does not seed a matching committee snapshot.
    super::with_phase1_verify_disabled(|| {
        executor
            .apply_pre_execution_changes()
            .expect("pre-execution changes should apply before Phase 1 system tx");
    });
    let system_txs = begin_system_txs_for_test(
        &config,
        2,
        parent_hash,
        &Bytes::new(),
        Some(metadata),
        proposer,
    );
    for tx in system_txs {
        executor
            .execute_transaction(tx)
            .expect("Phase 1 slashing system tx should execute");
    }

    // CPA(0) + LateFinalizeCredits(1) + CycleTick(2) + RewardsGemDelivery(3)
    // + OracleSlashWindow(4) + HookEvents(5).
    assert_eq!(executor.receipts().len(), 6);
    let phase1_logs = &executor.receipts()[0].logs;
    let voter_misdemeanor = keccak256("VoterMisdemeanor(address,uint64)");
    let voter_felony = keccak256("VoterFelony(address,uint64,uint64)");
    let proposer_felony = keccak256("ProposerFelony(address,uint64,uint64)");
    // voter miss / slashing accounting moved OFF Phase 1 (CPA)
    // to the inclusion-window close at N+K, so CPA emits no voter slashing log.
    assert!(
        !phase1_logs.iter().any(|log| {
            log.address == SLASH_INDICATOR_ADDRESS
                && matches!(
                    log.data.topics().first(),
                    Some(topic) if *topic == voter_misdemeanor || *topic == voter_felony
                )
        }),
        "Phase 1 (CPA) must no longer emit voter slashing - it is relocated to window close"
    );
    // Proposer slashing stays in Phase 1 (driven by `missed_proposers` metadata).
    assert!(
        phase1_logs.iter().any(|log| {
            log.address == SLASH_INDICATOR_ADDRESS
                && log.data.topics().first() == Some(&proposer_felony)
        }),
        "Phase 1 proposer slashing must emit receipt-visible ProposerFelony"
    );
    drop(executor);

    let read_ctx = BlockContext::new(2, 2, CHAIN_ID, proposer, vec![proposer, absent]);
    let mut provider =
        outbe_primitives::storage::direct::DirectStorageProvider::new(&mut state, read_ctx);
    StorageHandle::enter(&mut provider, |storage| {
        let si = outbe_slashindicator::contract::SlashIndicator::new(storage.clone());
        // Voter miss is now counted at the inclusion-window close (N+K), not at
        // CPA: block 2's CPA leaves voter_miss_count untouched.
        assert_eq!(si.voter_miss_count.read(&absent)?, 0);
        // Proposer slashing stays at CPA; the missed proposer is JAILED
        // (felony threshold 1) and its proposer miss recorded.
        assert_eq!(si.proposer_miss_count.read(&absent)?, 1);
        let vs = outbe_validatorset::contract::ValidatorSet::new(storage);
        let record = vs.get_validator(absent)?.expect("absent validator exists");
        assert_eq!(record.status, outbe_validatorset::logic::status::JAILED);
        Ok::<_, outbe_primitives::error::PrecompileError>(())
    })
    .expect("slashing state should be readable");
}

#[test]
fn pre_exec_hooks_emit_whitelisted_update_activation_event() {
    use alloy_sol_types::SolEvent;
    use outbe_update::payload::encode_schedule_update_json;
    use outbe_update::precompile::IUpdate;
    use serde_json::Value;

    let proposer = test_evm_signer().address();
    const ACTIVATION_BLOCK: u64 = 101;
    let protocol_version = outbe_update::constants::PROTOCOL_VERSION;

    let mut state =
        state_with_active_validators_seeded(&[(proposer, dummy_pubkey(0xA2))], |storage| {
            let proposal_id = U256::from(1);
            let payload: Value = serde_json::from_str(&encode_schedule_update_json(
                protocol_version,
                ACTIVATION_BLOCK,
                "",
            ))
            .expect("schedule update JSON should parse");
            let mut update = outbe_update::schema::Update::new(storage.clone());
            update
                .schedule_update_from_propose(proposal_id, &payload, 1)
                .expect("schedule update");
        });

    let ctx = BlockContext::new(
        ACTIVATION_BLOCK,
        ACTIVATION_BLOCK,
        CHAIN_ID,
        proposer,
        vec![proposer],
    );
    let (_, hook_events) = super::run_atomic_storage_hooks(&mut state, ctx, |hook_ctx| {
        super::run_outbe_pre_execution_hooks(hook_ctx, None)
    })
    .expect("pre-exec hooks should run");
    let (whitelisted, _) = partition_hook_events(&hook_events);
    let upgrade_activated = IUpdate::UpgradeActivated::SIGNATURE_HASH;
    assert!(
        whitelisted.iter().any(|log| {
            log.address == UPDATE_ADDRESS && log.data.topics().first() == Some(&upgrade_activated)
        }),
        "pre-exec hooks must emit whitelisted UpgradeActivated for HookEvents receipt"
    );
}

#[test]
fn hook_events_receipt_carries_whitelisted_update_activation_log() {
    use alloy_sol_types::SolEvent;
    use outbe_update::payload::encode_schedule_update_json;
    use outbe_update::precompile::IUpdate;
    use serde_json::Value;

    let proposer = test_evm_signer().address();
    const ACTIVATION_BLOCK: u64 = 101;
    let protocol_version = outbe_update::constants::PROTOCOL_VERSION;

    let mut state =
        state_with_active_validators_seeded(&[(proposer, dummy_pubkey(0xA2))], |storage| {
            let proposal_id = U256::from(1);
            let payload: Value = serde_json::from_str(&encode_schedule_update_json(
                protocol_version,
                ACTIVATION_BLOCK,
                "",
            ))
            .expect("schedule update JSON should parse");
            let mut update = outbe_update::schema::Update::new(storage.clone());
            update
                .schedule_update_from_propose(proposal_id, &payload, 1)
                .expect("schedule update");
        });

    let ctx = BlockContext::new(
        ACTIVATION_BLOCK,
        ACTIVATION_BLOCK,
        CHAIN_ID,
        proposer,
        vec![proposer],
    );
    let (_, hook_events) = super::run_atomic_storage_hooks(&mut state, ctx, |hook_ctx| {
        super::run_outbe_pre_execution_hooks(hook_ctx, None)
    })
    .expect("pre-exec hooks should emit activation events");
    let (whitelisted_logs, _) = partition_hook_events(&hook_events);

    let config = OutbeEvmConfig::new(test_chain_spec());
    let evm = config.evm_with_env(&mut state, test_evm_env(ACTIVATION_BLOCK, REWARDS_ADDRESS));
    let mut executor = config.create_executor(evm, execution_ctx(None, Bytes::new()));
    executor
        .push_hook_events_receipt(alloy_consensus::TxType::Legacy, whitelisted_logs, 21_000)
        .expect("HookEvents receipt should publish captured hook logs");

    let hook_receipt = executor.receipts().last().expect("HookEvents receipt");
    assert!(hook_receipt.success);
    let upgrade_activated = IUpdate::UpgradeActivated::SIGNATURE_HASH;
    assert!(
        hook_receipt.logs.iter().any(|log| {
            log.address == UPDATE_ADDRESS && log.data.topics().first() == Some(&upgrade_activated)
        }),
        "HookEvents receipt must carry UpgradeActivated from pre-exec hook events"
    );
}

#[test]
fn real_factory_approval_is_published_in_hook_events_receipt() {
    const CREATION_BLOCK: u64 = 7;
    const HOOK_EVENTS_GAS: u64 = 21_000;
    let issuer = Address::repeat_byte(0x11);
    let validators = [
        (Address::repeat_byte(0xa1), dummy_pubkey(0xa1)),
        (Address::repeat_byte(0xa2), dummy_pubkey(0xa2)),
        (Address::repeat_byte(0xa3), dummy_pubkey(0xa3)),
    ];
    let payload = encode_canonical_stablecoin_create(&StablecoinCreatePayload {
        issuer,
        name: "Example Dollar".into(),
        ticker: "EXUSD".into(),
        iso4217: 840,
        decimals: 6,
        supply_cap: U256::from(1_000_000u64),
        policy_id: U256::from(1u64),
    })
    .expect("canonical Factory payload");
    let payload = core::str::from_utf8(&payload).expect("canonical payload is UTF-8");
    let forced_surplus = U256::from(7u64);
    let mut expected_token_id = B256::ZERO;
    let mut expected_token = Address::ZERO;
    let mut state =
        state_with_active_validators_seeded_at_block(&validators, CREATION_BLOCK, |storage| {
            storage
                .set_balance(VOTE_ADDRESS, STABLECOIN_CREATE_BOND + forced_surplus)
                .unwrap();
            (expected_token_id, expected_token) = StablecoinFactoryContract::new(storage.clone())
                .predict_token_address(issuer, "EXUSD")
                .unwrap();
            let mut vote = Vote::new(storage);
            let proposal_id = vote
                .create_proposal_with_value(
                    issuer,
                    STABLECOIN_FACTORY_ADDRESS,
                    payload,
                    CREATION_BLOCK,
                    STABLECOIN_CREATE_BOND,
                    crate::handlers::vote::registry(),
                )
                .unwrap();
            assert_eq!(proposal_id, U256::from(1u64));
            vote.cast_vote_approve(proposal_id, validators[0].0, true, CREATION_BLOCK + 1)
                .unwrap();
            vote.cast_vote_approve(proposal_id, validators[1].0, true, CREATION_BLOCK + 1)
                .unwrap();
        });

    let finalization_block = CREATION_BLOCK + VOTING_WINDOW_BLOCKS + 1;
    let block_context = BlockContext::new(
        finalization_block,
        1_700_000_000,
        CHAIN_ID,
        issuer,
        validators.iter().map(|(address, _)| *address).collect(),
    );
    let (_, hook_events) =
        super::run_atomic_storage_hooks(&mut state, block_context.clone(), |hook_ctx| {
            super::run_outbe_pre_execution_hooks(hook_ctx, None)
        })
        .expect("real Vote -> Factory pre-exec lifecycle should commit");
    let (receipt_logs, _) = partition_hook_events(&hook_events);

    let factory_logs: Vec<_> = receipt_logs
        .iter()
        .filter(|log| {
            log.address == STABLECOIN_FACTORY_ADDRESS
                && log.data.topics().first()
                    == Some(&IStablecoinFactory::StablecoinCreated::SIGNATURE_HASH)
        })
        .collect();
    assert_eq!(factory_logs.len(), 1);
    let factory_log_index = receipt_logs
        .iter()
        .position(|log| {
            log.address == STABLECOIN_FACTORY_ADDRESS
                && log.data.topics().first()
                    == Some(&IStablecoinFactory::StablecoinCreated::SIGNATURE_HASH)
        })
        .unwrap();
    let refund_log_index = receipt_logs
        .iter()
        .position(|log| {
            log.address == VOTE_ADDRESS
                && log.data.topics().first() == Some(&IVote::ProposalBondRefunded::SIGNATURE_HASH)
        })
        .expect("Approved proposal must emit one refund");
    assert!(
        factory_log_index < refund_log_index,
        "target event must precede settlement event in committed hook order"
    );

    {
        let mut provider = super::DirectStorageProvider::new(&mut state, block_context.clone());
        let storage = StorageHandle::new(&mut provider);
        let vote = Vote::new(storage.clone());
        let factory = StablecoinFactoryContract::new(storage.clone());
        assert_eq!(
            vote.proposals
                .get(U256::from(1u64))
                .unwrap()
                .unwrap()
                .proposal_status()
                .unwrap(),
            ProposalStatus::Approved
        );
        assert_eq!(
            vote.proposal_bond(U256::from(1u64)).unwrap().settlement,
            BondSettlement::Refunded
        );
        assert_eq!(vote.bond_liabilities().unwrap(), U256::ZERO);
        assert_eq!(storage.balance(VOTE_ADDRESS).unwrap(), forced_surplus);
        assert_eq!(storage.balance(issuer).unwrap(), STABLECOIN_CREATE_BOND);
        assert_eq!(factory.token_count().unwrap(), U256::from(1u64));
        assert_eq!(
            factory.registered_token_id(expected_token).unwrap(),
            Some(expected_token_id)
        );
        assert_eq!(
            factory.token_id_of(expected_token).unwrap(),
            expected_token_id
        );
        assert!(!factory.reservations.exists(U256::from(1u64)).unwrap());
    }
    let token_account = state
        .basic(expected_token)
        .expect("token account read")
        .expect("created token account");
    assert_eq!(
        token_account
            .code
            .as_ref()
            .expect("created token marker")
            .original_bytes()
            .as_ref(),
        outbe_primitives::addresses::STABLECOIN_MARKER_CODE
    );

    let config = OutbeEvmConfig::new(test_chain_spec());
    let evm = config.evm_with_env(
        &mut state,
        test_evm_env(finalization_block, REWARDS_ADDRESS),
    );
    let mut executor = config.create_executor(evm, execution_ctx(None, Bytes::new()));
    executor
        .push_hook_events_receipt(
            alloy_consensus::TxType::Legacy,
            receipt_logs,
            HOOK_EVENTS_GAS,
        )
        .expect("HookEvents receipt should publish committed Factory log");

    let receipt = executor.receipts().last().expect("HookEvents receipt");
    assert!(receipt.success);
    assert_eq!(receipt.cumulative_gas_used, HOOK_EVENTS_GAS);
    assert_eq!(
        receipt
            .logs
            .iter()
            .filter(|log| {
                log.address == STABLECOIN_FACTORY_ADDRESS
                    && log.data.topics().first()
                        == Some(&IStablecoinFactory::StablecoinCreated::SIGNATURE_HASH)
            })
            .count(),
        1
    );
    let with_factory_root =
        alloy_consensus::proofs::calculate_receipt_root(&[receipt.with_bloom_ref()]);
    let mut without_factory_log = receipt.clone();
    without_factory_log.logs.retain(|log| {
        log.address != STABLECOIN_FACTORY_ADDRESS
            || log.data.topics().first()
                != Some(&IStablecoinFactory::StablecoinCreated::SIGNATURE_HASH)
    });
    let without_factory_root =
        alloy_consensus::proofs::calculate_receipt_root(&[without_factory_log.with_bloom_ref()]);
    assert_ne!(
        with_factory_root, without_factory_root,
        "StablecoinCreated must contribute to the receipts root"
    );
    assert_ne!(
        logs_bloom(receipt.logs.iter()),
        logs_bloom(without_factory_log.logs.iter()),
        "StablecoinCreated must contribute to the logs bloom"
    );
}

#[test]
fn real_factory_execution_error_has_no_factory_receipt_log() {
    const CREATION_BLOCK: u64 = 7;
    let issuer = Address::repeat_byte(0x11);
    let validators = [
        (Address::repeat_byte(0xb1), dummy_pubkey(0xb1)),
        (Address::repeat_byte(0xb2), dummy_pubkey(0xb2)),
        (Address::repeat_byte(0xb3), dummy_pubkey(0xb3)),
    ];
    let payload = encode_canonical_stablecoin_create(&StablecoinCreatePayload {
        issuer,
        name: "Example Dollar".into(),
        ticker: "EXUSD".into(),
        iso4217: 840,
        decimals: 6,
        supply_cap: U256::from(1_000_000u64),
        policy_id: U256::from(1u64),
    })
    .expect("canonical Factory payload");
    let payload = core::str::from_utf8(&payload).expect("canonical payload is UTF-8");
    let mut state =
        state_with_active_validators_seeded_at_block(&validators, CREATION_BLOCK, |storage| {
            storage
                .set_balance(VOTE_ADDRESS, STABLECOIN_CREATE_BOND)
                .unwrap();
            let mut vote = Vote::new(storage);
            let proposal_id = vote
                .create_proposal_with_value(
                    issuer,
                    STABLECOIN_FACTORY_ADDRESS,
                    payload,
                    CREATION_BLOCK,
                    STABLECOIN_CREATE_BOND,
                    crate::handlers::vote::registry(),
                )
                .unwrap();
            let mut corrupted = vote.proposals.get(proposal_id).unwrap().unwrap();
            corrupted.payload = "{".into();
            vote.proposals.update(&corrupted).unwrap();
            vote.cast_vote_approve(proposal_id, validators[0].0, true, CREATION_BLOCK + 1)
                .unwrap();
            vote.cast_vote_approve(proposal_id, validators[1].0, true, CREATION_BLOCK + 1)
                .unwrap();
        });

    let finalization_block = CREATION_BLOCK + VOTING_WINDOW_BLOCKS + 1;
    let block_context = BlockContext::new(
        finalization_block,
        1_700_000_000,
        CHAIN_ID,
        issuer,
        validators.iter().map(|(address, _)| *address).collect(),
    );
    let (_, hook_events) =
        super::run_atomic_storage_hooks(&mut state, block_context.clone(), |hook_ctx| {
            super::run_outbe_pre_execution_hooks(hook_ctx, None)
        })
        .expect("typed target Error must not fail the outer hook batch");
    let (receipt_logs, _) = partition_hook_events(&hook_events);
    assert!(receipt_logs.iter().all(|log| {
        log.address != STABLECOIN_FACTORY_ADDRESS
            || log.data.topics().first()
                != Some(&IStablecoinFactory::StablecoinCreated::SIGNATURE_HASH)
    }));
    assert!(receipt_logs.iter().all(|log| {
        log.address != VOTE_ADDRESS
            || (log.data.topics().first() != Some(&IVote::ProposalBondRefunded::SIGNATURE_HASH)
                && log.data.topics().first() != Some(&IVote::ProposalBondBurned::SIGNATURE_HASH))
    }));
    {
        let config = OutbeEvmConfig::new(test_chain_spec());
        let evm = config.evm_with_env(
            &mut state,
            test_evm_env(finalization_block, REWARDS_ADDRESS),
        );
        let mut executor = config.create_executor(evm, execution_ctx(None, Bytes::new()));
        executor
            .push_hook_events_receipt(alloy_consensus::TxType::Legacy, receipt_logs, 21_000)
            .expect("Error outcome HookEvents receipt");
        let receipt = executor.receipts().last().expect("HookEvents receipt");
        assert!(receipt.logs.iter().all(|log| {
            log.address != STABLECOIN_FACTORY_ADDRESS
                || log.data.topics().first()
                    != Some(&IStablecoinFactory::StablecoinCreated::SIGNATURE_HASH)
        }));
    }

    let mut provider = super::DirectStorageProvider::new(&mut state, block_context);
    let storage = StorageHandle::new(&mut provider);
    let vote = Vote::new(storage.clone());
    let factory = StablecoinFactoryContract::new(storage.clone());
    assert_eq!(
        vote.proposals
            .get(U256::from(1u64))
            .unwrap()
            .unwrap()
            .proposal_status()
            .unwrap(),
        ProposalStatus::Error
    );
    assert_eq!(
        vote.proposal_bond(U256::from(1u64)).unwrap().settlement,
        BondSettlement::Unsettled
    );
    assert_eq!(vote.bond_liabilities().unwrap(), STABLECOIN_CREATE_BOND);
    assert_eq!(
        storage.balance(VOTE_ADDRESS).unwrap(),
        STABLECOIN_CREATE_BOND
    );
    assert_eq!(factory.token_count().unwrap(), U256::ZERO);
    assert!(factory.reservations.exists(U256::from(1u64)).unwrap());
}
