use super::*;

#[test]
fn the_daily_cycle_tick_burns_the_burn_addresses_and_adds_the_base_fee_count() {
    const GENESIS_TS: u64 = 1_704_067_200;
    use alloy_sol_types::SolEvent;
    use outbe_promislimit::{precompile::IPromisLimit, DEAD_ADDRESS};
    use outbe_system_common::time::SECONDS_PER_DAY;
    use outbe_system_common::units::UNITS_PER_PROTOCOL_MINOR as MINOR;

    let signer = test_evm_signer();
    let proposer = signer.address();
    let mut state =
        state_with_active_validators_seeded(&[(proposer, dummy_pubkey(0xA2))], |storage| {
            seed_cycle_tick_genesis(storage.clone(), GENESIS_TS, proposer)
                .expect("cycle-tick genesis fixture");
            PromisLimitContract::new(storage)
                .count_base_fees(MINOR * U256::from(5) / U256::from(2))
                .unwrap();
        });
    for (account, balance) in [
        (Address::ZERO, MINOR * U256::from(3) + U256::from(5)),
        (DEAD_ADDRESS, MINOR),
    ] {
        state.insert_account(
            account,
            AccountInfo {
                balance,
                ..Default::default()
            },
        );
    }
    let mut evm_env = test_evm_env(1, REWARDS_ADDRESS);
    let block_timestamp = GENESIS_TS + SECONDS_PER_DAY + 60;
    let tee_bootstrap = sample_tee_bootstrap_payload_at(1, block_timestamp);
    evm_env.block_env.timestamp = U256::from(block_timestamp);
    let config = OutbeEvmConfig::new(
        test_chain_spec_with_genesis_timestamp(GENESIS_TS),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(signer.clone());
    let evm = config.evm_with_env(&mut state, evm_env);
    let mut executor = config.create_executor(
        evm,
        execution_ctx_with_tee_bootstrap(Some(0), Bytes::new(), tee_bootstrap.clone()),
    );
    executor
        .apply_pre_execution_changes()
        .expect("pre-execution changes should apply before begin-zone system txs");
    let system_txs = begin_system_txs_for_test(
        &config,
        BeginBlockFixture {
            block_number: 1,
            parent_hash: B256::ZERO,
            extra_data: &Bytes::new(),
            parent_consensus_metadata: None,
            proposer,
            bootstrap: BootstrapFixture::explicit(Some(tee_bootstrap)),
        },
    );
    for tx in system_txs {
        executor
            .execute_transaction(tx)
            .expect("begin-zone system tx should execute in tx loop");
    }
    let adds: Vec<_> = executor
        .receipts()
        .iter()
        .flat_map(|receipt| receipt.logs.iter())
        .filter(|log| log.address == PROMIS_LIMIT_ADDRESS)
        .filter_map(|log| IPromisLimit::PromisLimitAdded::decode_log(log).ok())
        .map(|log| (log.data.source, log.data.amountMinor))
        .filter(|(source, _)| matches!(source, 41 | 46))
        .collect();
    assert_eq!(adds, vec![(41, U256::from(2)), (46, U256::from(4))]);

    let read_ctx = BlockContext {
        block_gas_limit: 0,
        user_gas_used: 0,
        block_number: 1,
        timestamp: block_timestamp,
        chain_id: CHAIN_ID,
        genesis_hash: alloy_primitives::B256::ZERO,
        genesis_timestamp: GENESIS_TS,
        proposer: REWARDS_ADDRESS,
        validators: vec![REWARDS_ADDRESS],
    };
    let mut provider = DirectStorageProvider::new(executor.evm_mut().db_mut(), read_ctx);
    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(storage.balance(Address::ZERO).unwrap(), U256::from(5));
        assert_eq!(storage.balance(DEAD_ADDRESS).unwrap(), U256::ZERO);
        assert_eq!(
            PromisLimitContract::new(storage).base_fee_count().unwrap(),
            MINOR / U256::from(2)
        );
        Ok::<_, PrecompileError>(())
    })
    .unwrap();
}
