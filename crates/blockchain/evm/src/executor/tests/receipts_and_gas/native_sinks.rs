//! The EIP-1559 base fee is a native burn: the sender pays base plus tip, the
//! rewards escrow receives only the tip, and no capacity is credited.

use super::*;
use outbe_primitives::addresses::PROMIS_LIMIT_ADDRESS;

#[test]
fn base_fee_is_burned_tip_reaches_rewards_and_promis_limit_is_untouched() {
    let chain_spec = test_chain_spec();
    let receipt_builder = reth_ethereum::evm::RethReceiptBuilder::default();
    let config = OutbeEvmConfig::new(chain_spec.clone());
    let tx = test_priority_fee_tx();
    let recovered = tx
        .clone()
        .try_into_recovered()
        .expect("priority-fee tx signer should recover");
    let sender = recovered.signer();
    let sender_before = U256::from(1_000_000u64);

    let mut db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
    db.insert_account_info(
        sender,
        AccountInfo {
            balance: sender_before,
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
        fixtures::executor_inputs_from_ctx(&ctx, None, false),
    );
    executor
        .execute_transaction(recovered)
        .expect("priority-fee tx should execute");
    let gas_used = U256::from(executor.receipts()[0].cumulative_gas_used);
    drop(executor);

    let base = U256::from(MIN_PROTOCOL_BASE_FEE);
    // max_fee = 2 x base and tip = base, so the effective price is 2 x base.
    let tip = U256::from(tx.max_priority_fee_per_gas().unwrap_or_default());
    let balance = |state: &mut State<CacheDB<EmptyDBTyped<ProviderError>>>, account: Address| {
        state
            .basic(account)
            .expect("account read")
            .map(|info| info.balance)
            .unwrap_or_default()
    };
    let sender_after = balance(&mut state, sender);
    let rewards_after = balance(&mut state, REWARDS_ADDRESS);
    let recipient_after = balance(&mut state, Address::ZERO);

    assert_eq!(sender_before - sender_after, (base + tip) * gas_used);
    assert_eq!(
        rewards_after,
        tip * gas_used,
        "only the tip reaches the rewards escrow"
    );
    assert_eq!(
        recipient_after,
        U256::ZERO,
        "a zero-value call credits nothing"
    );
    assert_eq!(
        (sender_before - sender_after) - rewards_after,
        base * gas_used,
        "the base fee component is credited to no account"
    );

    let read_ctx = BlockContext::new(1, 1, CHAIN_ID, REWARDS_ADDRESS, vec![REWARDS_ADDRESS]);
    let mut provider =
        outbe_primitives::storage::direct::DirectStorageProvider::new(&mut state, read_ctx);
    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(
            outbe_promislimit::PromisLimitContract::new(storage.clone())
                .get_total_unallocated()
                .unwrap(),
            U256::ZERO,
            "fee handling must not credit Promis Limit capacity"
        );
        assert_eq!(storage.balance(PROMIS_LIMIT_ADDRESS).unwrap(), U256::ZERO);
        Ok::<_, outbe_primitives::error::PrecompileError>(())
    })
    .unwrap();
}
