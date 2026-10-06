use super::*;

mod factory_boundary;
mod nod_bodies;
mod proposer_parity;
mod withdrawals;

#[test]
fn compressed_entities_header_semantics_reject_missing_wrong_scheme_and_wrong_root() {
    let root = B256::repeat_byte(0xA1);
    assert!(validate_compressed_entities_root_scheme(None)
        .unwrap_err()
        .to_string()
        .contains("missing compressed-entities root artifact"));
    assert!(
        validate_compressed_entities_root_scheme(Some(CompressedEntitiesRootArtifact {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME + 1,
            r_sealed: root,
        }))
        .unwrap_err()
        .to_string()
        .contains("scheme mismatch")
    );
    assert!(validate_compressed_entities_root_after_seal(
        Some(CompressedEntitiesRootArtifact {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            r_sealed: B256::ZERO,
        }),
        root,
    )
    .unwrap_err()
    .to_string()
    .contains("header/SealOutput root mismatch"));
    assert_eq!(
        validate_compressed_entities_root_after_seal(
            Some(CompressedEntitiesRootArtifact {
                commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                r_sealed: root,
            }),
            root,
        )
        .unwrap()
        .r_sealed,
        root
    );
}

fn state_with_active_proposer_and_funded_account_without_ocomp(
    proposer: Address,
    funded: Address,
) -> State<CacheDB<EmptyDBTyped<ProviderError>>> {
    state_with_active_proposer_and_funded_account_fixture(proposer, funded, false)
}

#[test]
fn pending_rpc_context_opens_ce_scope_but_skips_consensus_hooks() {
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
    drop(
        executor
            .compressed_entities_scope
            .begin_explicit_gas_window(0)
            .expect("pending RPC env must open the CE lifecycle"),
    );
    executor
        .execute_transaction(user_tx)
        .expect("pending RPC env must execute txpool transactions inside a CE scope");
    assert_eq!(executor.receipts().len(), 1);
}

#[test]
fn finish_uses_final_extra_data_setter_for_summary_validation() {
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
    let evm = config.evm_with_env(&mut state, evm_env);
    let ctx = execution_ctx(Some(0), Bytes::new());
    let mut executor = OutbeBlockExecutor::new(
        EthBlockExecutor::new(evm, ctx.inner.clone(), &chain_spec, &receipt_builder),
        fixtures::executor_inputs_from_ctx(&ctx, None, true),
    );
    let final_extra_data = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        execution_summary: Some(ExecutionSummaryArtifact {
            validator_fee_sum: U256::ZERO,
        }),
        consensus_header_artifact: None,
        timestamp_millis_part: 0,
        late_finalize_credits: None,
        compressed_entities_root: None,
    })
    .expect("final extra_data must encode");

    executor.set_final_extra_data(final_extra_data);
    let artifacts = outbe_primitives::reshare_artifact::decode_outbe_block_artifacts(
        executor.final_extra_data().as_ref(),
    )
    .unwrap();
    super::validate_execution_summary_artifact(
        true,
        1,
        artifacts.execution_summary,
        executor.current_execution_summary(),
    )
    .expect("final extra_data summary must validate");
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
    let evm = config.evm_with_env(&mut state, evm_env);
    let ctx = execution_ctx(Some(0), Bytes::new());
    let executor = OutbeBlockExecutor::new(
        EthBlockExecutor::new(evm, ctx.inner.clone(), &chain_spec, &receipt_builder),
        fixtures::executor_inputs_from_ctx(&ctx, None, true),
    );

    let artifacts = outbe_primitives::reshare_artifact::decode_outbe_block_artifacts(
        executor.final_extra_data().as_ref(),
    )
    .unwrap();
    let err = super::validate_execution_summary_artifact(
        true,
        1,
        artifacts.execution_summary,
        executor.current_execution_summary(),
    )
    .expect_err("stale pre-summary extra_data must not validate");

    assert!(err
        .to_string()
        .contains("missing execution summary artifact in block extra_data"));
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
