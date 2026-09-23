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

fn test_ocomp_submit_result_vote_tx() -> reth_ethereum::TransactionSigned {
    use outbe_ocomp_protocol::{
        abi::{METADOSIS_ADDRESS, SUBMIT_LYSIS_RESULT_SELECTOR},
        encode_envelope,
        profile::poc_schema_limits,
        registry::ObjectKind,
    };

    let mut body = Vec::new();
    body.extend_from_slice(B256::repeat_byte(0x31).as_slice());
    body.extend_from_slice(B256::repeat_byte(0x32).as_slice());
    body.extend_from_slice(&3_u32.to_be_bytes());
    body.extend_from_slice(&7_u64.to_be_bytes());
    body.extend_from_slice(B256::repeat_byte(0x33).as_slice());
    body.extend_from_slice(B256::repeat_byte(0x34).as_slice());
    body.extend_from_slice(B256::repeat_byte(0x35).as_slice());
    body.extend_from_slice(&1_u64.to_be_bytes());
    body.resize(180, 0);
    let payload = encode_envelope(ObjectKind::ResultVoteV1, &body, poc_schema_limits().codec)
        .expect("canonical OCOMP prefix must encode");
    let padded_len = (payload.len() + 31) & !31;
    let mut input = vec![0_u8; 68 + padded_len];
    input[..4].copy_from_slice(&SUBMIT_LYSIS_RESULT_SELECTOR);
    input[4..36].copy_from_slice(&U256::from(32).to_be_bytes::<32>());
    input[36..68].copy_from_slice(&U256::from(payload.len()).to_be_bytes::<32>());
    input[68..68 + payload.len()].copy_from_slice(&payload);

    TxEip1559 {
        chain_id: CHAIN_ID,
        nonce: 0,
        gas_limit: outbe_ocomp_protocol::system_carrier::OCOMP_SYSTEM_CARRIER_GAS_LIMIT,
        max_fee_per_gas:
            outbe_ocomp_protocol::system_carrier::MIN_OCOMP_SYSTEM_CARRIER_MAX_FEE_PER_GAS,
        max_priority_fee_per_gas: 0,
        to: TxKind::Call(METADOSIS_ADDRESS),
        value: U256::ZERO,
        input: input.into(),
        access_list: Default::default(),
    }
    .into_signed(Signature::test_signature())
    .into()
}

#[test]
fn executor_adapter_classifies_the_canonical_ocomp_system_carrier_prefix() {
    let tx = test_ocomp_submit_result_vote_tx();
    let candidate = outbe_ocomp_protocol::system_carrier::classify_ocomp_system_carrier(
        outbe_ocomp_protocol::system_carrier::OcompSystemCarrierView {
            is_eip1559: true,
            to: tx.to(),
            value: tx.value(),
            input: tx.input().as_ref(),
            gas_limit: tx.gas_limit(),
            max_fee_per_gas: tx.max_fee_per_gas(),
            max_priority_fee_per_gas: tx.max_priority_fee_per_gas(),
        },
        &outbe_ocomp_protocol::profile::poc_schema_limits(),
    )
    .expect("canonical OCOMP envelope must classify")
    .expect("canonical OCOMP envelope must select the system carrier");

    let outbe_ocomp_protocol::system_carrier::OcompSystemCarrierCandidate::ResultVote { prefix } =
        candidate
    else {
        panic!("expected result-vote carrier")
    };
    assert_eq!(prefix.ocomp_key_hash, B256::repeat_byte(0x35));
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
fn active_terminal_request_is_last_semantic_writer_and_rejects_later_transactions() {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ObservedWrite {
        EthereumPostBlock,
        CompressedEntitiesSeal,
        TerminalTransaction,
    }

    let signer = test_evm_signer();
    let proposer = signer.address();
    let mut state = state_with_active_proposer_without_ocomp(proposer);
    let chain_spec = test_chain_spec();
    let install = test_ocomp_fork_install(&chain_spec, &[(proposer, dummy_pubkey(0xA2))]);
    let config = OutbeEvmConfig::new_with_runtime_body_readers(
        chain_spec,
        RuntimeBodyReaders::new(Arc::new(MemoryStorage::new())),
    )
    .with_evm_signer(signer)
    .with_ocomp_lifecycle_activation(OcompLifecycleActivation::at_block(1))
    .with_ocomp_fork_install(install);
    let block_timestamp = 1_700_000_000u64;
    let tee_bootstrap = sample_tee_bootstrap_payload_at(1, block_timestamp);
    let mut evm_env = test_evm_env(1, REWARDS_ADDRESS);
    evm_env.block_env.timestamp = U256::from(block_timestamp);
    let evm = config.evm_with_env(&mut state, evm_env);
    let mut ctx = execution_ctx_with_tee_bootstrap(Some(5), Bytes::new(), tee_bootstrap.clone());
    ctx.inner.withdrawals = Some(std::borrow::Cow::Owned(Vec::new()));
    let mut executor = config.create_executor(evm, ctx);

    let observed_writes = Arc::new(Mutex::new(Vec::new()));
    let hook_writes = observed_writes.clone();
    executor.evm_mut().db_mut().set_state_hook(Some(Box::new(
        move |changes: revm::state::EvmState| {
            // Revm reports committed state directly; distinguish the terminal
            // call target, CE-only seal, and Ethereum post-block commits.
            let observed = if changes
                .contains_key(&outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS)
            {
                Some(ObservedWrite::TerminalTransaction)
            } else if changes.len() == 1
                && changes.contains_key(&outbe_primitives::addresses::COMPRESSED_ENTITIES_ADDRESS)
            {
                Some(ObservedWrite::CompressedEntitiesSeal)
            } else {
                Some(ObservedWrite::EthereumPostBlock)
            };
            if let Some(observed) = observed {
                hook_writes.lock().unwrap().push(observed);
            }
        },
    )));

    executor
        .apply_pre_execution_changes()
        .expect("active block pre-execution succeeds");
    let begin = begin_system_txs_for_test_with_bootstrap(
        &config,
        1,
        B256::ZERO,
        &Bytes::new(),
        None,
        proposer,
        Some(tee_bootstrap),
    );
    assert_eq!(
        begin
            .iter()
            .map(|tx| SystemTxInputV2::decode(tx.tx().input().as_ref())
                .unwrap()
                .kind())
            .collect::<Vec<_>>(),
        vec![
            SystemTxKind::OcompLifecycleBegin,
            SystemTxKind::CycleTick,
            SystemTxKind::RewardsGemDelivery,
            SystemTxKind::TeeBootstrap,
            SystemTxKind::OracleSlashWindow,
            SystemTxKind::HookEvents,
        ]
    );
    for tx in begin.iter().cloned() {
        executor
            .execute_transaction(tx)
            .expect("begin system tx executes");
    }

    observed_writes.lock().unwrap().clear();
    let end = config
        .build_end_system_txs(1, CHAIN_ID, begin.len(), Some(proposer))
        .expect("terminal system tx builds");
    assert_eq!(end.len(), 1);
    executor
        .execute_transaction(end.into_iter().next().unwrap())
        .expect("terminal system tx executes before the final CE seal");

    let writes_after_terminal = observed_writes.lock().unwrap().clone();
    let ethereum_post_block_index = writes_after_terminal
        .iter()
        .position(|write| *write == ObservedWrite::EthereumPostBlock)
        .expect("standard Ethereum post-block changes execute before OSR2");
    let compressed_entities_seal_index = writes_after_terminal
        .iter()
        .position(|write| *write == ObservedWrite::CompressedEntitiesSeal)
        .expect("compressed entities seal executes after OSR2");
    let terminal_transaction_index = writes_after_terminal
        .iter()
        .position(|write| *write == ObservedWrite::TerminalTransaction)
        .expect("OSR2 commits as the terminal transaction");
    assert!(
        ethereum_post_block_index < terminal_transaction_index
            && terminal_transaction_index < compressed_entities_seal_index,
        "semantic write order must be Ethereum post-block -> OSR2 -> final CE seal; got \
         {writes_after_terminal:?}"
    );
    assert!(executor.compressed_entities_seal_output().is_some());
    let receipt_count = executor.receipts().len();
    let later_user = test_regular_tx()
        .try_into_recovered()
        .expect("regular tx signer recovers");
    assert!(executor.execute_transaction(later_user).is_err());
    assert_eq!(executor.receipts().len(), receipt_count);
    assert_eq!(
        observed_writes
            .lock()
            .unwrap()
            .iter()
            .filter(|write| **write == ObservedWrite::CompressedEntitiesSeal)
            .count(),
        1
    );

    executor
        .prepare_final_header_artifacts(0)
        .expect("sealed CE root enters final header");
    let writes_before_finish = observed_writes.lock().unwrap().clone();
    let (_evm, result) = executor.finish().expect("active executor finishes");
    assert_eq!(result.receipts.len(), 7);
    assert_eq!(
        *observed_writes.lock().unwrap(),
        writes_before_finish,
        "finish must not perform any semantic write after OSR2"
    );
}

#[test]
fn whole_committee_ocomp_deadline_and_successor_blocks_execute() {
    const MINIMUM_STAKE: u64 = 1_000;
    const OPEN_HEIGHT: u64 = 1;
    const DEADLINE: u64 = OPEN_HEIGHT + outbe_validatorset::runtime::OCOMP_RECOVERY_WINDOW_BLOCKS;

    fn execute_and_finalize_block(
        state: &mut State<CacheDB<EmptyDBTyped<ProviderError>>>,
        tree: &Arc<CompressedTreeService>,
        signer: Arc<OutbeEvmSigner>,
        committee: &[Address],
        number: u64,
        parent_hash: B256,
    ) -> (B256, Vec<Receipt>) {
        let proposer = signer.address();
        let chain_spec = test_chain_spec();
        let config = OutbeEvmConfig::new(chain_spec)
            .with_evm_signer(signer)
            .with_compressed_tree_service(tree.clone());
        let mut parent_metadata =
            metadata_with(committee.to_vec(), vec![1; committee.len()], Vec::new());
        parent_metadata.finalized_block_number = number - 1;
        parent_metadata.finalized_block_hash = parent_hash;
        let system_txs = begin_system_txs_for_test(
            &config,
            number,
            parent_hash,
            &Bytes::new(),
            Some(parent_metadata.clone()),
            proposer,
        );
        let evm = config.evm_with_env(state, test_evm_env(number, REWARDS_ADDRESS));
        let mut execution = execution_ctx(Some(system_txs.len()), Bytes::new());
        execution.inner.parent_hash = parent_hash;
        execution.parent_consensus_metadata = Some(parent_metadata);
        execution.parent_artifact_hint = Some(AccountedParentArtifact {
            summary: ExecutionSummaryArtifact {
                validator_fee_sum: U256::ZERO,
            },
            timestamp: TEST_BLOCK_TIMESTAMP_BASE.saturating_add(number - 1),
            state_root: Some(B256::repeat_byte(0x91)),
        });
        execution.proposer_evm_address = Some(proposer);
        execution.expected_begin_system_txs = system_txs.clone();
        let mut executor = config.create_executor(evm, execution);
        super::with_phase1_verify_disabled(|| {
            executor
                .apply_pre_execution_changes()
                .expect("deadline block pre-execution must succeed");
        });
        for transaction in system_txs {
            executor
                .execute_transaction(transaction)
                .expect("deadline block system transaction must execute");
        }
        executor
            .finalize_compressed_entities()
            .expect("deadline block CE lifecycle must seal");
        executor
            .prepare_final_header_artifacts(0)
            .expect("deadline block artifacts must encode");
        let sealed = executor
            .compressed_entities_seal_output()
            .expect("deadline block must produce a CE candidate");
        let block_hash = keccak256(number.to_be_bytes());
        tree.publish_candidate(block_hash, sealed.staged_tree_batch)
            .expect("deadline block CE candidate must publish");
        tree.apply_finalized(number, block_hash, sealed.new_root)
            .expect("deadline block CE candidate must finalize");
        let (evm, result) = executor.finish().expect("full block execution must finish");
        drop(evm);
        assert!(result.receipts.iter().all(|receipt| receipt.success));
        (block_hash, result.receipts)
    }

    let first_signer = Arc::new(OutbeEvmSigner::from_secret_bytes([0x21; 32]).unwrap());
    let second_signer = Arc::new(OutbeEvmSigner::from_secret_bytes([0x22; 32]).unwrap());
    let validators = vec![
        (first_signer.address(), dummy_pubkey(0x31)),
        (second_signer.address(), dummy_pubkey(0x32)),
        (numbered_test_address(0x33, 3), dummy_pubkey(0x33)),
        (numbered_test_address(0x34, 4), dummy_pubkey(0x34)),
    ];
    let committee = validators
        .iter()
        .map(|(validator, _)| *validator)
        .collect::<Vec<_>>();
    let mut state =
        state_with_active_validators_seeded_at_block(&validators, OPEN_HEIGHT, |storage| {
            let mut validator_set =
                outbe_validatorset::contract::ValidatorSet::new(storage.clone());
            let mut staking = outbe_staking::contract::Staking::new(storage.clone());
            staking
                .config_min_stake
                .write(U256::from(MINIMUM_STAKE))
                .unwrap();
            staking
                .total_staked
                .write(U256::from(MINIMUM_STAKE * committee.len() as u64))
                .unwrap();
            storage
                .set_balance(
                    STAKING_ADDRESS,
                    U256::from(MINIMUM_STAKE * committee.len() as u64),
                )
                .unwrap();
            for validator in &committee {
                staking
                    .stake_amount
                    .write(validator, U256::from(MINIMUM_STAKE))
                    .unwrap();
                validator_set
                    .test_set_stake_projection(
                        *validator,
                        outbe_validatorset::StakeProjection::new(U256::from(MINIMUM_STAKE), None),
                    )
                    .unwrap();
            }
            for validator in &committee {
                let miss = staking.record_ocomp_miss(*validator).unwrap();
                assert!(miss.first_in_window);
                assert_eq!(miss.slashed_bonded, U256::from(100));
                assert_eq!(miss.recovery_deadline, DEADLINE);
            }
            let registry = outbe_teeregistry::TeeRegistry::new(storage.clone());
            for (index, validator) in committee.iter().enumerate() {
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
                    .write(
                        &node_hash,
                        TEST_BLOCK_TIMESTAMP_BASE.saturating_add(DEADLINE + 3_600),
                    )
                    .unwrap();
            }
            outbe_accounting::schema::Accounting::new(storage)
                .last_accounted_block_number
                .write(DEADLINE - 2)
                .unwrap();
        });

    let empty_root = outbe_compressed_entities::sealed_root(B256::ZERO).unwrap();
    let parent_hash = B256::repeat_byte(0x90);
    let (_tree_directory, tree) = persistent_test_tree_with_marker(
        test_chain_spec().genesis_hash(),
        FinalizedMarker {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            height: DEADLINE - 1,
            block_hash: parent_hash,
            parent_block_hash: B256::repeat_byte(0x8f),
            parent_root: empty_root,
            new_root: empty_root,
        },
    );

    let (deadline_hash, deadline_receipts) = execute_and_finalize_block(
        &mut state,
        &tree,
        first_signer,
        &committee,
        DEADLINE,
        parent_hash,
    );
    let resolutions = deadline_receipts
        .iter()
        .flat_map(|receipt| &receipt.logs)
        .filter_map(|log| {
            outbe_validatorset::precompile::IValidatorSet::OcompRecoveryResolved::decode_log(log)
                .ok()
        })
        .collect::<Vec<_>>();
    assert_eq!(resolutions.len(), committee.len());
    assert!(
        resolutions.iter().all(|resolution| {
            resolution.recoveryDeadline == DEADLINE && resolution.outcome == 2
        }),
        "unexpected OCOMP recovery resolutions: {resolutions:?}"
    );

    let read_ctx = BlockContext::new(
        DEADLINE,
        TEST_BLOCK_TIMESTAMP_BASE + DEADLINE,
        CHAIN_ID,
        committee[0],
        committee.clone(),
    );
    let mut provider =
        outbe_primitives::storage::direct::DirectStorageProvider::new(&mut state, read_ctx);
    StorageHandle::enter(&mut provider, |storage| {
        let validator_set = outbe_validatorset::contract::ValidatorSet::new(storage);
        assert_eq!(
            validator_set
                .get_active_consensus_set()?
                .into_iter()
                .map(|record| record.validator_address)
                .collect::<Vec<_>>(),
            committee
        );
        assert!(validator_set.has_pending_set_change()?);
        for validator in &committee {
            assert!(matches!(
                validator_set.validator_lifecycle(*validator)?,
                ValidatorLifecycle::JailRetained(_)
            ));
            assert!(validator_set.is_consensus_participant(*validator)?);
            assert!(validator_set.ocomp_recovery_window(*validator)?.is_none());
        }
        Ok::<_, outbe_primitives::error::PrecompileError>(())
    })
    .expect("deadline state must retain the current consensus committee");

    let (_successor_hash, successor_receipts) = execute_and_finalize_block(
        &mut state,
        &tree,
        second_signer,
        &committee,
        DEADLINE + 1,
        deadline_hash,
    );
    assert!(!successor_receipts.is_empty());
    assert!(successor_receipts.iter().all(|receipt| receipt.success));
}

#[test]
fn reward_gem_delivery_drains_one_max_batch_while_cycle_appends_the_next() {
    const GENESIS_TS: u64 = 1_704_067_200;
    const SECONDS_PER_DAY: u64 = 86_400;
    const VALIDATOR_COUNT: u32 = outbe_consensus::bls::MAX_VALIDATORS;

    let signer = test_evm_signer();
    let proposer = signer.address();
    let block_ts = GENESIS_TS + 2 * SECONDS_PER_DAY + 60;
    let current_utc_day = outbe_primitives::time::timestamp_to_date_key(block_ts);
    let reward_utc_day = outbe_primitives::time::previous_date_key(current_utc_day);
    let backlog_utc_day = outbe_primitives::time::previous_date_key(reward_utc_day);
    let emission_trigger = outbe_cycle::triggers::TriggerId::ProtocolCycle.as_u32();
    let mut state =
        state_with_active_validators_seeded(&[(proposer, dummy_pubkey(0xA2))], |storage| {
            let genesis_ctx = BlockRuntimeContext::new(
                BlockContext::new(0, GENESIS_TS, CHAIN_ID, proposer, vec![proposer]),
                storage.clone(),
            );
            outbe_rewards::runtime::ensure_genesis_anchor(&genesis_ctx).unwrap();
            let cycle = outbe_cycle::schema::Cycle::new(storage.clone());
            cycle.active_utc_day.write(reward_utc_day).unwrap();
            cycle
                .last_executed_at
                .write(&emission_trigger, block_ts - 3_600)
                .unwrap();

            let backlog_voters = (0..VALIDATOR_COUNT)
                .map(|index| (numbered_test_address(0x13, u64::from(index)), 1))
                .collect::<Vec<_>>();
            outbe_rewards::api::prepare_daily_validator_gem_batch(
                &genesis_ctx,
                backlog_utc_day,
                U256::from(1_000_000u64),
                &backlog_voters,
            )
            .unwrap();

            let rewards = outbe_rewards::schema::Rewards::new(storage.clone());
            rewards
                .daily_voter_count
                .write(&reward_utc_day, VALIDATOR_COUNT)
                .unwrap();
            rewards
                .daily_total_participation
                .write(&reward_utc_day, u64::from(VALIDATOR_COUNT))
                .unwrap();
            for index in 0..VALIDATOR_COUNT {
                let voter = numbered_test_address(0x14, u64::from(index));
                rewards
                    .daily_voter_at
                    .get_nested(&reward_utc_day)
                    .write(&index, voter)
                    .unwrap();
                rewards
                    .daily_participation
                    .get_nested(&reward_utc_day)
                    .write(&voter, 1)
                    .unwrap();
            }
            seed_previous_day_vwap(&storage, block_ts, U256::from(1_000_000u64));
            outbe_oracle::api::set_exchange_rate(
                storage,
                Address::ZERO,
                outbe_oracle::api::DAY_TYPE_PAIR,
                U256::from(1_000_000u64),
                1,
                block_ts,
            )
            .unwrap();
        });

    let mut evm_env = test_evm_env(1, REWARDS_ADDRESS);
    evm_env.block_env.timestamp = U256::from(block_ts);
    let config = OutbeEvmConfig::new(test_chain_spec()).with_evm_signer(signer);
    let evm = config.evm_with_env(&mut state, evm_env);
    let mut executor = config.create_executor(evm, block_one_execution_ctx(Some(0), Bytes::new()));
    executor.apply_pre_execution_changes().unwrap();
    let mut system_txs =
        begin_system_txs_for_test(&config, 1, B256::ZERO, &Bytes::new(), None, proposer);
    let cycle_tx = system_txs.remove(0);
    let delivery_tx = system_txs.remove(0);
    let cycle_limit = cycle_tx.tx().gas_limit();
    let delivery_limit = delivery_tx.tx().gas_limit();
    assert!(
        executor
            .execute_transaction(cycle_tx)
            .unwrap()
            .tx_gas_used()
            <= cycle_limit
    );
    assert!(
        executor
            .execute_transaction(delivery_tx)
            .unwrap()
            .tx_gas_used()
            <= delivery_limit
    );
    drop(executor);

    let read_ctx = BlockContext::new(1, block_ts, CHAIN_ID, proposer, vec![proposer]);
    let mut provider =
        outbe_primitives::storage::direct::DirectStorageProvider::new(&mut state, read_ctx);
    StorageHandle::enter(&mut provider, |storage| {
        let rewards = outbe_rewards::schema::Rewards::new(storage.clone());
        assert_eq!(rewards.reward_gem_queue_head.read()?, 1);
        assert_eq!(rewards.reward_gem_queue_tail.read()?, 2);
        assert!(rewards.daily_topup_settled.read(&backlog_utc_day)?);
        assert!(rewards.daily_topup_prepared.read(&reward_utc_day)?);
        assert!(!rewards.daily_topup_settled.read(&reward_utc_day)?);
        let gem = outbe_gem::GemContract::new(storage);
        for index in 0..VALIDATOR_COUNT {
            assert_eq!(
                gem.balance_of(numbered_test_address(0x13, u64::from(index)))?,
                1
            );
            assert_eq!(
                gem.balance_of(numbered_test_address(0x14, u64::from(index)))?,
                0
            );
        }
        Ok::<_, outbe_primitives::error::PrecompileError>(())
    })
    .unwrap();
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
    let mut system_txs =
        begin_system_txs_for_test(&config, 1, B256::ZERO, &Bytes::new(), None, proposer)
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
        None,
        Bytes::new(),
        None,
        false,
        None,
        parent_hash,
        Some(signer.clone()),
        ctx.expected_begin_system_txs.clone(),
        ctx.expected_end_system_txs.clone(),
        ctx.system_layout_error.clone(),
        ctx.parent_consensus_metadata.clone(),
        Some(proposer),
        true,
        None,
        Some(AccountedParentArtifact {
            summary: ExecutionSummaryArtifact {
                validator_fee_sum: U256::ZERO,
            },
            timestamp: 1,
            state_root: None,
        }),
    );
    executor.system_tx_phase_cursor = crate::system_tx::SystemTxPhase::initial_for_block(
        2,
        crate::system_tx::GENESIS_BOOTSTRAP_BLOCK_NUMBER,
    );

    let block_artifacts = OutbeBlockArtifacts::default();
    let preexec = crate::factory::with_forced_outbe_system_call_error(|| {
        executor.apply_phase1_commit_in_preexec(2, &block_artifacts)
    });
    if preexec.is_err() {
        return;
    }

    let receipt_count_after_preexec_failure = executor.receipts().len();
    let phase1_tx = begin_system_txs_for_test(
        &config,
        2,
        parent_hash,
        &Bytes::new(),
        Some(metadata),
        proposer,
    )
    .into_iter()
    .next()
    .expect("Phase 1 system tx should be present");
    let _ = crate::factory::with_forced_outbe_system_call_error(|| {
        executor.execute_transaction(phase1_tx)
    });

    assert_eq!(
        executor.receipts().len(),
        receipt_count_after_preexec_failure,
        "GAS-02: Phase 1 pre-exec failure returned Ok and body[0] created another \
         receipt instead of being consumed or making pre-exec fatal"
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
    let system_tx =
        begin_system_txs_for_test(&config, 1, B256::ZERO, &Bytes::new(), None, proposer)
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
    let reward_owner_a = Address::repeat_byte(0x71);
    let reward_owner_b = Address::repeat_byte(0x72);
    let user_tx = test_regular_tx()
        .try_into_recovered()
        .expect("regular tx signer should recover");
    let mut state =
        state_with_active_validators_seeded(&[(proposer, dummy_pubkey(0xA2))], |storage| {
            let seed_context = BlockContext::new(0, 1, CHAIN_ID, proposer, vec![proposer]);
            let ctx = BlockRuntimeContext::new(seed_context, storage.clone());
            outbe_rewards::runtime::ensure_genesis_anchor(&ctx).unwrap();
            outbe_oracle::api::set_exchange_rate(
                storage.clone(),
                Address::ZERO,
                outbe_oracle::api::DAY_TYPE_PAIR,
                U256::from(1_000_000u64),
                1,
                TEST_BLOCK_TIMESTAMP_BASE + 1,
            )
            .unwrap();
            seed_previous_day_vwap(
                &storage,
                TEST_BLOCK_TIMESTAMP_BASE + 1,
                U256::from(1_000_000u64),
            );
            // The retry block below runs at timestamp 2.
            seed_previous_day_vwap(&storage, 2, U256::from(1_000_000u64));
            outbe_rewards::api::prepare_daily_validator_gem_batch(
                &ctx,
                20_240_101,
                U256::from(200u64),
                &[(reward_owner_a, 1), (reward_owner_b, 1)],
            )
            .unwrap();
            outbe_gemfactory::schema::GemFactoryContract::new(storage)
                .total_gems_issued
                .write(U256::MAX - U256::ONE)
                .unwrap();
        });
    state.database.insert_account_info(
        Address::from(*user_tx.signer()),
        AccountInfo {
            balance: U256::from(1_000_000u64),
            ..Default::default()
        },
    );
    {
        let read_context = BlockContext::new(0, 1, CHAIN_ID, proposer, vec![proposer]);
        let mut provider =
            outbe_primitives::storage::direct::DirectStorageProvider::new(&mut state, read_context);
        StorageHandle::enter(&mut provider, |storage| {
            let rewards = outbe_rewards::schema::Rewards::new(storage.clone());
            assert_eq!(rewards.reward_gem_queue_head.read()?, 0);
            assert_eq!(rewards.reward_gem_queue_tail.read()?, 1);
            assert_eq!(rewards.reward_gem_pending_batch_count.read()?, 1);
            assert_eq!(
                outbe_gemfactory::schema::GemFactoryContract::new(storage)
                    .total_gems_issued
                    .read()?,
                U256::MAX - U256::ONE
            );
            Ok::<_, outbe_primitives::error::PrecompileError>(())
        })
        .expect("seed one retryable Rewards Gem batch");
    }
    let chain_spec = test_chain_spec();
    let receipt_builder = reth_ethereum::evm::RethReceiptBuilder::default();
    let config = OutbeEvmConfig::new(chain_spec.clone()).with_evm_signer(signer.clone());
    let evm = config.evm_with_env(&mut state, test_evm_env(1, REWARDS_ADDRESS));
    let ctx = block_one_execution_ctx(Some(1), Bytes::new());
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
    let hook_events_tx = system_txs
        .next()
        .expect("HookEvents system tx should be present");
    let rewards_visible_gas = rewards_tx.tx().gas_limit();

    // CycleTick is consensus-critical. RewardsGemDelivery is deliberately
    // retryable, so an ordinary revert records a soft failure and later
    // begin-zone/user transactions still execute.
    let cycle_gas = executor
        .execute_transaction(cycle_tx)
        .expect("CycleTick should execute successfully")
        .tx_gas_used();
    let revert_gas = executor
        .execute_transaction(rewards_tx)
        .expect("RewardsGemDelivery revert should soft-fail")
        .tx_gas_used();
    assert_eq!(
        revert_gas, rewards_visible_gas,
        "GAS-11: reverted delivery should charge visible envelope gas"
    );
    let failure_receipt = executor
        .receipts()
        .get(1)
        .expect("reverted delivery must emit a failure receipt");
    assert!(
        !failure_receipt.success,
        "seeded delivery unexpectedly succeeded: logs={:?}",
        failure_receipt.logs
    );
    assert_eq!(
        failure_receipt.cumulative_gas_used,
        cycle_gas + rewards_visible_gas
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

    let tee_bootstrap_gas = executor
        .execute_transaction(tee_bootstrap_tx)
        .expect("mandatory TeeBootstrap should execute before the non-critical phase")
        .tx_gas_used();
    let oracle_gas = executor
        .execute_transaction(oracle_tx)
        .expect("OracleSlashWindow should execute after delivery")
        .tx_gas_used();
    let hook_events_gas = executor
        .execute_transaction(hook_events_tx)
        .expect("HookEvents should execute after delivery")
        .tx_gas_used();

    let user_gas = executor
        .execute_transaction(user_tx)
        .expect("user txs must execute after a soft-failed non-critical begin-zone system tx")
        .tx_gas_used();
    assert_eq!(
        executor.inner.cumulative_tx_gas_used,
        cycle_gas
            + rewards_visible_gas
            + tee_bootstrap_gas
            + oracle_gas
            + hook_events_gas
            + user_gas,
        "GAS-11: soft-failed system tx must charge only visible envelope gas"
    );
    drop(executor);

    let retry_context = BlockContext::new(2, 2, CHAIN_ID, proposer, vec![proposer]);
    let mut retry_provider = outbe_primitives::storage::direct::DirectStorageProvider::new(
        &mut state,
        retry_context.clone(),
    );
    StorageHandle::enter(&mut retry_provider, |storage| {
        let rewards = outbe_rewards::schema::Rewards::new(storage.clone());
        let gem = outbe_gem::GemContract::new(storage.clone());
        assert_eq!(rewards.reward_gem_queue_head.read()?, 0);
        assert_eq!(rewards.reward_gem_queue_tail.read()?, 1);
        assert_eq!(rewards.reward_gem_pending_batch_count.read()?, 1);
        assert_eq!(gem.balance_of(reward_owner_a)?, 0);
        assert_eq!(gem.balance_of(reward_owner_b)?, 0);

        outbe_gemfactory::schema::GemFactoryContract::new(storage.clone())
            .total_gems_issued
            .write(U256::ZERO)?;
        let retry_ctx = BlockRuntimeContext::new(retry_context, storage.clone());
        assert!(matches!(
            outbe_rewards::api::deliver_oldest_reward_gem_batch(&retry_ctx)?,
            outbe_rewards::api::RewardGemDeliveryOutcome::Delivered {
                reward_utc_day: 20_240_101,
                recipient_count: 2,
                delivered_promis_load_amount,
            } if delivered_promis_load_amount == U256::from(200u64)
        ));
        assert_eq!(rewards.reward_gem_queue_head.read()?, 1);
        assert_eq!(rewards.reward_gem_queue_tail.read()?, 1);
        assert_eq!(rewards.reward_gem_pending_batch_count.read()?, 0);
        assert_eq!(gem.balance_of(reward_owner_a)?, 1);
        assert_eq!(gem.balance_of(reward_owner_b)?, 1);
        Ok::<_, outbe_primitives::error::PrecompileError>(())
    })
    .expect("the unchanged FIFO head must deliver on a later retry");
}

/// A revert in a consensus-critical begin-zone phase (here
/// CycleTick) is a hard block failure, not a soft-receipt skip - its one-shot
/// work (a day's emission / terminal Metadosis) must never be silently
/// dropped. No receipt is pushed; the block aborts.
#[test]
fn critical_cycle_tick_revert_fails_block() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let mut state = state_with_active_proposer(proposer);
    let config = OutbeEvmConfig::new(test_chain_spec()).with_evm_signer(signer.clone());
    let evm = config.evm_with_env(&mut state, test_evm_env(1, REWARDS_ADDRESS));
    let mut executor = config.create_executor(evm, block_one_execution_ctx(Some(1), Bytes::new()));
    executor
        .apply_pre_execution_changes()
        .expect("pre-execution changes should apply");
    let cycle_tx = begin_system_txs_for_test(&config, 1, B256::ZERO, &Bytes::new(), None, proposer)
        .into_iter()
        .next()
        .expect("CycleTick system tx should be present");

    let err = crate::factory::with_forced_outbe_system_call_revert(|| {
        executor.execute_transaction(cycle_tx)
    })
    .expect_err("a revert in the critical CycleTick phase must fail the block");
    assert!(
        err.to_string()
            .contains("critical system tx CycleTick did not succeed"),
        "unexpected error: {err}"
    );
    assert!(
        executor.receipts().is_empty(),
        "a critical-phase revert must not push a soft receipt"
    );
}

/// A stale reward price at the UTC-day boundary defers only Gem delivery:
/// CycleTick seals the allocation, RewardsGemDelivery leaves the FIFO head
/// pending, and the user-lane feeder vote still executes.
#[test]
fn stale_oracle_cycle_stages_reward_batch_and_allows_later_feeder_vote() {
    const GENESIS_TS: u64 = 1_704_067_200;
    const SECONDS_PER_DAY: u64 = 86_400;
    const PREVIOUS_DAY: u32 = 20_240_101;

    #[derive(Debug, Eq, PartialEq)]
    struct Observation {
        active_day: u32,
        last_executed_at: u64,
        day_settled: bool,
        topup_prepared: bool,
        topup_settled: bool,
        queue_head: u64,
        queue_tail: u64,
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
                cycle.active_utc_day.write(PREVIOUS_DAY).unwrap();
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
                rewards
                    .daily_total_participation
                    .write(&PREVIOUS_DAY, 1)
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
        let config = OutbeEvmConfig::new(test_chain_spec()).with_evm_signer(signer.clone());
        let begin = begin_system_txs_for_test_with_bootstrap(
            &config,
            1,
            B256::ZERO,
            &Bytes::new(),
            None,
            proposer,
            Some(tee_bootstrap.clone()),
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
                .expect("stale reward price must defer only Gem delivery");
        }
        drop(executor);

        let read_ctx = BlockContext::new(1, block_timestamp, CHAIN_ID, proposer, vec![proposer]);
        let mut provider =
            outbe_primitives::storage::direct::DirectStorageProvider::new(&mut state, read_ctx);
        StorageHandle::enter(&mut provider, |storage| {
            let cycle = outbe_cycle::schema::Cycle::new(storage.clone());
            let rewards = outbe_rewards::schema::Rewards::new(storage.clone());
            let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
            let gem = outbe_gem::GemContract::new(storage.clone());
            Observation {
                active_day: cycle.active_utc_day.read().unwrap(),
                last_executed_at: cycle.last_executed_at.read(&emission_trigger).unwrap(),
                day_settled: rewards.daily_settled.read(&PREVIOUS_DAY).unwrap(),
                topup_prepared: rewards.daily_topup_prepared.read(&PREVIOUS_DAY).unwrap(),
                topup_settled: rewards.daily_topup_settled.read(&PREVIOUS_DAY).unwrap(),
                queue_head: rewards.reward_gem_queue_head.read().unwrap(),
                queue_tail: rewards.reward_gem_queue_tail.read().unwrap(),
                voter_gems: u64::from(gem.balance_of(voter).unwrap()),
                feeder_vote_exists: oracle.vote_exists.read(&proposer).unwrap(),
                formation_exists: outbe_metadosis::api::day_limit_formation_receipt(
                    storage,
                    outbe_primitives::time::WorldwideDay::new(PREVIOUS_DAY),
                )
                .unwrap()
                .is_some(),
            }
        })
    }

    let first = run_once();
    assert_eq!(first.active_day, 20_240_102);
    assert_eq!(first.last_executed_at, GENESIS_TS + SECONDS_PER_DAY);
    assert!(first.day_settled);
    assert!(first.topup_prepared);
    assert!(!first.topup_settled);
    assert_eq!(first.queue_head, 0);
    assert_eq!(first.queue_tail, 1);
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
    let signer = test_evm_signer();
    let proposer = signer.address();
    let mut state = state_with_active_proposer(proposer);
    let config = OutbeEvmConfig::new(test_chain_spec()).with_evm_signer(signer.clone());
    let evm = config.evm_with_env(&mut state, test_evm_env(1, REWARDS_ADDRESS));
    let mut executor = config.create_executor(evm, block_one_execution_ctx(Some(1), Bytes::new()));
    executor
        .apply_pre_execution_changes()
        .expect("pre-execution changes should apply");
    let cycle_tx = begin_system_txs_for_test(&config, 1, B256::ZERO, &Bytes::new(), None, proposer)
        .into_iter()
        .next()
        .expect("CycleTick system tx should be present");

    let err = crate::factory::with_forced_outbe_system_call_oog_halt(|| {
        executor.execute_transaction(cycle_tx)
    })
    .expect_err("an OOG halt in the critical CycleTick phase must fail the block");
    assert!(
        err.to_string()
            .contains("critical system tx CycleTick did not succeed"),
        "unexpected error: {err}"
    );
    assert!(
        executor.receipts().is_empty(),
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
    let canonical =
        begin_system_txs_for_test(&config, 1, B256::ZERO, &Bytes::new(), None, proposer);
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
        execution_summary: None,
        consensus_header_artifact: Some(ConsensusHeaderArtifact::BoundaryOutcome(header_artifact)),
        timestamp_millis_part: 0,
        late_finalize_credits: None,
        compressed_entities_root: None,
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
    let rewards_unsigned = build_unsigned_system_tx(
        SystemTxKind::RewardsGemDelivery,
        1,
        1,
        CHAIN_ID,
        SystemTxInputV2::RewardsGemDelivery.encode().unwrap(),
    )
    .unwrap();
    let boundary_unsigned = build_unsigned_system_tx(
        SystemTxKind::BoundaryOutcome,
        2,
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
    let rewards_signed = signer.sign_unsigned(rewards_unsigned).unwrap();
    let boundary_signed = signer.sign_unsigned(boundary_unsigned).unwrap();
    let cycle_recovered = reth_primitives_traits::Recovered::new_unchecked(cycle_signed, proposer);
    let rewards_recovered =
        reth_primitives_traits::Recovered::new_unchecked(rewards_signed, proposer);
    let boundary_recovered =
        reth_primitives_traits::Recovered::new_unchecked(boundary_signed, proposer);
    let mut ctx = execution_ctx(Some(3), extra_data);
    ctx.expected_begin_system_txs = vec![
        cycle_recovered.clone(),
        rewards_recovered,
        boundary_recovered.clone(),
    ];
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

    let canonical =
        begin_system_txs_for_test(&config, 1, B256::ZERO, &Bytes::new(), None, proposer);
    let cycle_input = SystemTxInputV2::CycleTick.encode().unwrap();
    let unsigned = build_unsigned_system_tx_with_gas_limit(
        SystemTxKind::CycleTick,
        0,
        1,
        CHAIN_ID,
        cycle_input,
        canonical[0].tx().gas_limit(),
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
        Err(outbe_primitives::error::PrecompileError::TreeUnavailable(
            "finalized marker advanced past payload parent".into(),
        ))
    })
    .expect_err("tree readiness must abort this payload execution");

    assert!(matches!(
        error
            .as_internal()
            .and_then(|inner| inner.downcast_other::<outbe_primitives::error::PrecompileError>()),
        Some(outbe_primitives::error::PrecompileError::TreeUnavailable(_))
    ));
}
