use super::*;
use outbe_primitives::time::SECONDS_PER_DAY;

mod factory_boundary;
mod nod_bodies;
mod proposer_parity;
mod withdrawals;

#[test]
fn proposer_and_validator_state_matches_after_a_gem_issue() {
    use outbe_gem::precompile::IGem;
    use outbe_gemfactory::{precompile::IGemFactory, schema::GemPosition, GemFactoryContract};
    use outbe_primitives::addresses::GEM_FACTORY_ADDRESS;
    use outbe_primitives::reshare_artifact::decode_outbe_block_artifacts;

    outbe_chain_constants::initialize(None).unwrap();
    // The minimal parity variant issues a Gem from a seeded merchant position.
    let block_ts = fixtures::TEST_BLOCK_TIMESTAMP_BASE + 1;
    let position_id = U256::from(1);
    let user: reth_ethereum::TransactionSigned = TxEip1559 {
        chain_id: CHAIN_ID,
        nonce: 0,
        gas_limit: 3_000_000,
        max_fee_per_gas: MIN_PROTOCOL_BASE_FEE as u128,
        max_priority_fee_per_gas: 0,
        to: TxKind::Call(GEM_FACTORY_ADDRESS),
        value: U256::ZERO,
        input: IGemFactory::issueGemCall {
            positionId: position_id,
            owner: OWNER,
            promisLoadMinor: U256::from(1_000_000),
            issuanceCurrency: 840,
            referenceCurrency: 840,
        }
        .abi_encode()
        .into(),
        access_list: Default::default(),
    }
    .into_signed(Signature::test_signature())
    .into();
    let user = user.try_into_recovered().unwrap();
    let mut signed_body = Vec::new();
    let mut header_extra_data = Bytes::new();
    let mut observations = Vec::new();
    for replay in [false, true] {
        let signer = test_evm_signer();
        let proposer = signer.address();
        let chain_spec = test_chain_spec();
        let parent_hash = chain_spec.genesis_hash();
        let mut state =
            state_with_active_validators_seeded(&[(proposer, dummy_pubkey(0xA2))], |storage| {
                GemFactoryContract::new(storage.clone())
                    .positions
                    .create(&GemPosition {
                        position_id,
                        merchant: Address(*user.signer()),
                        // Dated by the priced day, which floors the merchant gem.
                        source_intex_id: outbe_intex::SeriesId::pack(
                            outbe_primitives::time::previous_date_key(
                                outbe_primitives::time::timestamp_to_date_key(block_ts),
                            )
                            .unwrap(),
                            *b"USD",
                            b'U',
                        )
                        .unwrap(),
                        remaining_capacity_minor: U256::from(2_000_000),
                        issued_at: block_ts - 1,
                        expires_at: block_ts + SECONDS_PER_DAY,
                    })
                    .unwrap();
                // The capacity the position's expiry hour holds, as opening it records.
                GemFactoryContract::new(storage.clone())
                    .hour_capacity_minor
                    .write(
                        &((block_ts + SECONDS_PER_DAY) / 3_600),
                        U256::from(2_000_000),
                    )
                    .unwrap();
                seed_previous_day_vwap(&storage, block_ts, U256::from(1_000_000u64));
                outbe_oracle::api::set_exchange_rate(
                    storage,
                    Address::ZERO,
                    outbe_oracle::api::DAY_TYPE_PAIR,
                    outbe_oracle::api::RateObservation {
                        rate: U256::from(1_000_000u64),
                        block_number: 1,
                        timestamp: block_ts,
                    },
                )
                .unwrap();
            });
        state.database.insert_account_info(
            Address(*user.signer()),
            AccountInfo {
                balance: U256::from(10u64).pow(U256::from(18)),
                ..Default::default()
            },
        );
        let marker_code = Bytecode::new_legacy([0xef].into());
        state.database.insert_account_info(
            GEM_FACTORY_ADDRESS,
            AccountInfo {
                code_hash: marker_code.hash_slow(),
                code: Some(marker_code),
                ..Default::default()
            },
        );
        let config = OutbeEvmConfig::new(chain_spec).with_evm_signer(signer);
        if !replay {
            signed_body = begin_system_txs_for_test(
                &config,
                BeginBlockFixture {
                    block_number: 1,
                    parent_hash,
                    extra_data: &Bytes::new(),
                    parent_consensus_metadata: None,
                    proposer,
                    bootstrap: BootstrapFixture::StandardForBlock,
                },
            );
        }
        let mut ctx =
            block_one_execution_ctx(Some(signed_body.len() + 1), header_extra_data.clone());
        ctx.inner.parent_hash = parent_hash;
        ctx.proposer_evm_address = Some(proposer);
        if replay {
            ctx.expected_begin_system_txs = signed_body.clone();
        }
        let evm = config.evm_with_env(&mut state, test_evm_env(1, REWARDS_ADDRESS));
        let mut executor = config.create_executor(evm, ctx);
        executor.apply_pre_execution_changes().unwrap();
        for (index, tx) in signed_body.clone().into_iter().enumerate() {
            executor
                .execute_transaction(tx)
                .unwrap_or_else(|error| panic!("system transaction {index} failed: {error}"));
        }
        executor.execute_transaction(user.clone()).unwrap();
        if !replay {
            executor.prepare_final_header_artifacts(0).unwrap();
            header_extra_data = executor.final_extra_data().clone();
        }
        let artifact = decode_outbe_block_artifacts(executor.final_extra_data().as_ref())
            .unwrap()
            .execution_summary;
        let (evm, result) = executor.finish().expect("Gem block header must validate");
        drop(evm);
        state.merge_transitions(BundleRetention::Reverts);
        assert!(state
            .bundle_state
            .state()
            .contains_key(&outbe_primitives::addresses::GEM_ADDRESS));
        assert_eq!(result.receipts.len(), signed_body.len() + 1);
        assert!(result.receipts.iter().all(|receipt| receipt.success));
        let logs = || {
            result
                .receipts
                .iter()
                .flat_map(|receipt| &receipt.logs)
                .filter(|log| log.address == outbe_primitives::addresses::GEM_ADDRESS)
        };
        let gem = logs()
            .find_map(|log| IGem::Transfer::decode_log_data(&log.data).ok())
            .expect("user transaction must mint a Gem");
        assert_eq!(gem.from, Address::ZERO);
        let mut provider = outbe_primitives::storage::direct::DirectStorageProvider::new(
            &mut state,
            BlockContext::empty_for_tests(1, block_ts, CHAIN_ID),
        );
        StorageHandle::enter(&mut provider, |storage| {
            let view = outbe_gem::api::get_view(&storage, gem.tokenId)
                .unwrap()
                .unwrap();
            assert_eq!(view.item.owner, OWNER);
            assert_eq!(view.item.promis_load_minor, U256::from(1_000_000));
            assert_eq!(
                outbe_gem::api::list_by_owner(&storage, OWNER)
                    .unwrap()
                    .len(),
                1
            );
        });
        drop(provider);
        observations.push((
            post_state_root(&state.bundle_state),
            artifact,
            result.receipts,
            result.gas_used,
        ));
    }
    assert_eq!(observations[0], observations[1]);
}

#[test]
fn pending_rpc_context_skips_consensus_hooks_and_executes_user_transactions() {
    let user_tx = test_regular_tx()
        .try_into_recovered()
        .expect("regular tx signer should recover");
    let mut state =
        state_with_active_proposer_and_funded_account(REWARDS_ADDRESS, user_tx.signer());
    let evm_env = test_evm_env(2, REWARDS_ADDRESS);
    let config = OutbeEvmConfig::new(test_chain_spec());
    let evm = config.evm_with_env(&mut state, evm_env);
    let mut ctx = execution_ctx(None, Bytes::new());
    ctx.execute_outbe_block_hooks = false;
    let mut executor = config.create_executor(evm, ctx);

    executor
        .apply_pre_execution_changes()
        .expect("pending RPC env should skip consensus-only Outbe hooks");
    assert!(executor.receipts().is_empty());
    executor
        .execute_transaction(user_tx)
        .expect("pending RPC env must execute txpool transactions");
    assert_eq!(executor.receipts().len(), 1);
}

#[test]
fn finish_uses_final_extra_data_setter_for_summary_validation() {
    let chain_spec = test_chain_spec();
    let receipt_builder = reth_ethereum::evm::RethReceiptBuilder::default();
    let config = OutbeEvmConfig::new(chain_spec.clone());
    let expected_fee = U256::from(1_234u64);

    let db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
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
    let ctx = execution_ctx(Some(0), Bytes::new());
    let mut executor = OutbeBlockExecutor::new(
        EthBlockExecutor::new(evm, ctx.inner.clone(), &chain_spec, &receipt_builder),
        fixtures::executor_inputs_from_ctx(&ctx, None, true),
    );
    executor.current_block_validator_fees = expected_fee;
    let final_extra_data = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        radicle_publication_enabled: false,
        radicle_publication: None,
        execution_summary: Some(ExecutionSummaryArtifact {
            validator_fee_sum: expected_fee,
        }),
        consensus_header_artifact: None,
        timestamp_millis_part: 0,
        late_finalize_credits: None,
        enclave_witness: None,
    })
    .expect("final extra_data must encode");

    executor.set_final_extra_data(final_extra_data);
    executor
        .finish()
        .expect("final extra_data summary must validate through finish()");

    // Genesis block (number == 0) is exempt from execution-summary validation even without extra_data.
    let db_genesis = CacheDB::<EmptyDBTyped<ProviderError>>::default();
    let mut state_genesis = State::builder()
        .with_database(db_genesis)
        .with_bundle_update()
        .build();
    let evm_env_genesis = EvmEnv {
        cfg_env: CfgEnv::new()
            .with_chain_id(CHAIN_ID)
            .with_spec_and_mainnet_gas_params(SpecId::SHANGHAI),
        block_env: BlockEnv {
            number: U256::ZERO,
            gas_limit: 30_000_000,
            basefee: 1_000_000_000,
            beneficiary: OWNER,
            timestamp: U256::from(1u64),
            ..Default::default()
        },
    };
    let evm_genesis = config.evm_with_env(&mut state_genesis, evm_env_genesis);
    let genesis_executor = OutbeBlockExecutor::new(
        EthBlockExecutor::new(
            evm_genesis,
            ctx.inner.clone(),
            &chain_spec,
            &receipt_builder,
        ),
        fixtures::executor_inputs_from_ctx(&ctx, None, true),
    );
    genesis_executor
        .finish()
        .expect("genesis block must bypass execution summary requirement in finish()");
}

#[test]
fn finish_without_final_extra_data_setter_rejects_missing_summary() {
    let chain_spec = test_chain_spec();
    let receipt_builder = reth_ethereum::evm::RethReceiptBuilder::default();
    let config = OutbeEvmConfig::new(chain_spec.clone());
    let db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
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
    let evm = config.evm_with_env(&mut state, evm_env.clone());
    let ctx = execution_ctx(Some(0), Bytes::new());
    let executor = OutbeBlockExecutor::new(
        EthBlockExecutor::new(evm, ctx.inner.clone(), &chain_spec, &receipt_builder),
        fixtures::executor_inputs_from_ctx(&ctx, None, true),
    );

    let err = match executor.finish() {
        Ok(_) => panic!("stale pre-summary extra_data must not validate in finish()"),
        Err(err) => err,
    };
    assert!(
        err.to_string()
            .contains("missing execution summary artifact in block extra_data"),
        "unexpected error: {err}"
    );

    // Deliberately non-zero mismatched validator_fee_sum must also fail finish().
    let db_mismatch = CacheDB::<EmptyDBTyped<ProviderError>>::default();
    let mut state_mismatch = State::builder()
        .with_database(db_mismatch)
        .with_bundle_update()
        .build();
    let evm_mismatch = config.evm_with_env(&mut state_mismatch, evm_env);
    let mut mismatch_executor = OutbeBlockExecutor::new(
        EthBlockExecutor::new(
            evm_mismatch,
            ctx.inner.clone(),
            &chain_spec,
            &receipt_builder,
        ),
        fixtures::executor_inputs_from_ctx(&ctx, None, true),
    );
    mismatch_executor.current_block_validator_fees = U256::from(1_000u64);
    let mismatched_extra_data = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        radicle_publication_enabled: false,
        radicle_publication: None,
        execution_summary: Some(ExecutionSummaryArtifact {
            validator_fee_sum: U256::from(2_000u64),
        }),
        consensus_header_artifact: None,
        timestamp_millis_part: 0,
        late_finalize_credits: None,
        enclave_witness: None,
    })
    .expect("mismatched final extra_data must encode");
    mismatch_executor.set_final_extra_data(mismatched_extra_data);

    let mismatch_err = match mismatch_executor.finish() {
        Ok(_) => panic!("mismatched validator_fee_sum must fail finish()"),
        Err(err) => err,
    };
    assert!(
        mismatch_err
            .to_string()
            .contains("execution summary artifact mismatch"),
        "unexpected error: {mismatch_err}"
    );
}

#[test]
fn finish_rejects_non_artifact_header_extra_data() {
    let config = OutbeEvmConfig::new(test_chain_spec());
    let db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
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
    let ctx = execution_ctx(Some(0), Bytes::from_static(b"reth/vtest/macos"));
    let executor = config.create_executor(evm, ctx);

    let err = match executor.finish() {
        Ok(_) => {
            panic!("non-artifact extra_data must currently reproduce the payload-builder failure")
        }
        Err(err) => err,
    };

    assert!(err
        .to_string()
        .contains("unknown non-empty extra_data block artifact"));
}
