// CCA custody and registration through real EVM transactions.
pub(crate) use alloy_evm::{Evm as _, EvmFactory as _};
pub(crate) use alloy_primitives::{Address, Bytes, U256};
pub(crate) use alloy_sol_types::{SolCall, SolError, SolEvent};
pub(crate) use outbe_agentreward::{
    constants::REWARD_BALANCE_IDLE_SECONDS, schema::RewardPool, AgentRewardContract,
};
pub(crate) use outbe_ccaregistry::{
    api,
    constants::{BOND_REQUIREMENT, UNBOND_COOLDOWN_SECONDS},
    precompile::ICcaRegistry,
    runtime,
    schema::CcaContract,
};
pub(crate) use outbe_evm::OutbeEvmFactory;
pub(crate) use outbe_gemfactory::precompile::IGemFactory;
pub(crate) use outbe_primitives::{
    block::{BlockContext, BlockRuntimeContext},
    storage::{direct::DirectStorageProvider, StorageHandle},
};
pub(crate) use outbe_system_common::addresses::{
    AGENT_REWARD_ADDRESS, CCA_REGISTRY_ADDRESS, GEM_ADDRESS, GEM_FACTORY_ADDRESS, ORACLE_ADDRESS,
    PROMIS_LIMIT_ADDRESS,
};
pub(crate) use outbe_system_common::time::SECONDS_PER_DAY;
pub(crate) use outbe_system_common::time::{previous_date_key, timestamp_to_date_key};
pub(crate) use reth_ethereum::evm::primitives::EvmEnv;
pub(crate) use revm::{
    context::{result::ExecutionResult, BlockEnv, CfgEnv, TxEnv},
    database::{CacheDB, EmptyDB},
    primitives::{hardfork::SpecId, TxKind},
    state::{AccountInfo, Bytecode},
    DatabaseCommit,
};

pub(super) const CCA: Address = Address::repeat_byte(0xc1);
pub(super) const NOW: u64 = 1_700_000_000;

pub(super) fn tx(
    db: &mut CacheDB<EmptyDB>,
    value: U256,
    data: Vec<u8>,
    now: u64,
) -> ExecutionResult {
    tx_to(db, CCA_REGISTRY_ADDRESS, value, data, now)
}

pub(super) fn tx_to(
    db: &mut CacheDB<EmptyDB>,
    target: Address,
    value: U256,
    data: Vec<u8>,
    now: u64,
) -> ExecutionResult {
    try_tx_to(db, target, value, data, now).unwrap()
}

pub(super) fn try_tx_to(
    db: &mut CacheDB<EmptyDB>,
    target: Address,
    value: U256,
    data: Vec<u8>,
    now: u64,
) -> Result<ExecutionResult, String> {
    try_tx_to_with_gas(db, target, value, data, now, 3_000_000)
}

pub(super) fn try_tx_to_with_gas(
    db: &mut CacheDB<EmptyDB>,
    target: Address,
    value: U256,
    data: Vec<u8>,
    now: u64,
    gas_limit: u64,
) -> Result<ExecutionResult, String> {
    let nonce = db.cache.accounts.get(&CCA).unwrap().info.nonce;
    let env = EvmEnv {
        cfg_env: CfgEnv::new()
            .with_chain_id(1)
            .with_spec_and_mainnet_gas_params(SpecId::PRAGUE),
        block_env: BlockEnv {
            timestamp: U256::from(now),
            gas_limit: 30_000_000,
            ..Default::default()
        },
    };
    let outcome = {
        let mut evm = OutbeEvmFactory::new(std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()))
            .create_evm(&mut *db, env);
        evm.transact_raw(
            TxEnv::builder()
                .caller(CCA)
                .nonce(nonce)
                .kind(TxKind::Call(target))
                .value(value)
                .data(Bytes::from(data))
                .gas_limit(gas_limit)
                .build()
                .unwrap(),
        )
        .map_err(|error| error.to_string())?
    };
    db.commit(outcome.state);
    Ok(outcome.result)
}

pub(super) fn with_storage<R>(
    db: &mut CacheDB<EmptyDB>,
    f: impl FnOnce(StorageHandle<'_>) -> R,
) -> R {
    with_storage_at(db, NOW, f)
}

pub(super) fn with_storage_at<R>(
    db: &mut CacheDB<EmptyDB>,
    now: u64,
    f: impl FnOnce(StorageHandle<'_>) -> R,
) -> R {
    let mut provider = DirectStorageProvider::new(
        db,
        BlockContext {
            block_gas_limit: 0,
            user_gas_used: 0,
            block_number: 1,
            timestamp: now,
            chain_id: 1,
            genesis_hash: alloy_primitives::B256::ZERO,
            genesis_timestamp: u64::max(now, 1),
            proposer: alloy_primitives::Address::ZERO,
            validators: Vec::new(),
        },
    );
    let result = StorageHandle::enter(&mut provider, f);
    provider.flush().unwrap();
    result
}

/// Dispatches a reward claim with the EVM storage provider.
/// Returns the ABI output and the logs the claim emitted, or the revert reason.
pub(super) fn claim_reward(
    db: &mut CacheDB<EmptyDB>,
    data: Vec<u8>,
    now: u64,
) -> Result<(Bytes, Vec<alloy_primitives::Log>), String> {
    claim_reward_in_block(db, data, now, 1)
}

/// [`claim_reward`] in block `block`: one owner gets one Gem of a series in each block.
pub(super) fn claim_reward_in_block(
    db: &mut CacheDB<EmptyDB>,
    data: Vec<u8>,
    now: u64,
    block: u64,
) -> Result<(Bytes, Vec<alloy_primitives::Log>), String> {
    let mut provider = DirectStorageProvider::new(
        db,
        BlockContext {
            block_gas_limit: 0,
            user_gas_used: 0,
            block_number: block,
            timestamp: now,
            chain_id: 1,
            genesis_hash: alloy_primitives::B256::ZERO,
            genesis_timestamp: u64::max(now, 1),
            proposer: alloy_primitives::Address::ZERO,
            validators: Vec::new(),
        },
    );
    let result = StorageHandle::enter(&mut provider, |storage| {
        outbe_agentreward::precompile::dispatch(storage, &data, CCA, U256::ZERO)
            .map_err(|error| error.to_string())
    });
    let logs = provider.take_events();
    provider.flush().unwrap();
    result.map(|output| (output, logs))
}

alloy_sol_types::sol!("../../contracts/precompiles/src/IAgentReward.sol");
alloy_sol_types::sol!("../../contracts/precompiles/src/IPromisLimit.sol");

pub(super) const PRICE: U256 = U256::from_limbs([2_000_000, 0, 0, 0]);
pub(super) const REWARD: U256 = U256::from_limbs([250, 0, 0, 0]);

/// CCA can claim Gems: marker code on the touched precompiles and a previous-day
/// COEN/USD VWAP of [`PRICE`].
pub(super) fn reward_world() -> CacheDB<EmptyDB> {
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(CCA, AccountInfo::default());
    let code = Bytecode::new_raw(Bytes::from_static(&[0xef]));
    for address in [
        AGENT_REWARD_ADDRESS,
        CCA_REGISTRY_ADDRESS,
        GEM_ADDRESS,
        GEM_FACTORY_ADDRESS,
        ORACLE_ADDRESS,
        PROMIS_LIMIT_ADDRESS,
    ] {
        db.insert_account_info(
            address,
            AccountInfo {
                code_hash: code.hash_slow(),
                code: Some(code.clone()),
                ..Default::default()
            },
        );
    }
    with_storage(&mut db, |storage| {
        seed_oracle(&storage);
        seed_vwap(
            &storage,
            previous_date_key(timestamp_to_date_key(NOW)).expect("date step"),
            PRICE,
        );
    });
    db
}

pub(super) fn credit(db: &mut CacheDB<EmptyDB>, pool: RewardPool, amount: U256) {
    with_storage(db, |storage| {
        AgentRewardContract::new(storage)
            .add_claimable_reward(pool, CCA, amount)
            .unwrap();
    });
}

pub(super) fn claim_tx(
    db: &mut CacheDB<EmptyDB>,
    pool: RewardPool,
    requested: U256,
) -> ExecutionResult {
    tx_to(
        db,
        AGENT_REWARD_ADDRESS,
        U256::ZERO,
        IAgentReward::claimRewardCall {
            pool: pool as u8,
            promisLoadMinor: requested,
            issuanceCurrency: 840,
            referenceCurrency: 840,
        }
        .abi_encode(),
        NOW,
    )
}

/// Claims `requested` of CCA's `held` balance in `pool` by transaction, zero for all
/// of it, and checks the Gem, both events and what stays claimable.
pub(super) fn claim_by_tx(
    db: &mut CacheDB<EmptyDB>,
    pool: RewardPool,
    held: U256,
    requested: U256,
) -> U256 {
    let load = if requested.is_zero() { held } else { requested };
    let result = claim_tx(db, pool, requested);
    assert!(result.is_success(), "{result:?}");
    let gem_id =
        IAgentReward::claimRewardCall::abi_decode_returns(result.output().unwrap()).unwrap();
    let claimed = result
        .logs()
        .iter()
        .find_map(|log| IAgentReward::RewardsClaimed::decode_log(log).ok())
        .expect("the claim emits RewardsClaimed")
        .data;
    assert_eq!(claimed.account, CCA);
    assert_eq!(claimed.pool, pool as u8);
    assert_eq!(claimed.promisLoadMinor, load);
    assert_eq!(claimed.gemId, gem_id);
    let issued = result
        .logs()
        .iter()
        .find_map(|log| IGemFactory::GemIssued::decode_log(log).ok())
        .expect("the claim emits GemIssued")
        .data;
    assert_eq!(issued.gemId, gem_id);
    assert_eq!(issued.payload.as_ref(), &load.to_be_bytes::<32>());
    with_storage(db, |storage| {
        let gem = outbe_gem::api::get_view(&storage, gem_id).unwrap().unwrap();
        assert_eq!(gem.item.owner, CCA);
        assert_eq!(gem.item.promis_load_minor, load);
        assert_eq!(gem.item.entry_price_minor, discounted(PRICE));
        assert_eq!(gem.item.issued_at, NOW);
        assert_eq!(
            AgentRewardContract::new(storage)
                .get_pool_claimable_reward(pool, CCA)
                .unwrap(),
            held - load
        );
    });
    assert_eq!(
        db.cache.accounts[&AGENT_REWARD_ADDRESS].info.balance,
        U256::ZERO
    );
    gem_id
}

pub(super) fn expiration_call(account: Address, pool: RewardPool) -> Vec<u8> {
    IAgentReward::returnExpiredRewardCall {
        account,
        pool: pool as u8,
    }
    .abi_encode()
}

pub(super) fn balance_deadline(
    db: &mut CacheDB<EmptyDB>,
    account: Address,
    pool: RewardPool,
) -> u64 {
    let result = tx_to(
        db,
        AGENT_REWARD_ADDRESS,
        U256::ZERO,
        IAgentReward::getPoolBalanceDeadlineCall {
            account,
            pool: pool as u8,
        }
        .abi_encode(),
        NOW,
    );
    assert!(result.is_success(), "{result:?}");
    IAgentReward::getPoolBalanceDeadlineCall::abi_decode_returns(result.output().unwrap()).unwrap()
}

pub(super) fn promis_limit(db: &mut CacheDB<EmptyDB>) -> U256 {
    let result = tx_to(
        db,
        PROMIS_LIMIT_ADDRESS,
        U256::ZERO,
        IPromisLimit::promisLimitCall {}.abi_encode(),
        NOW,
    );
    assert!(result.is_success(), "{result:?}");
    IPromisLimit::promisLimitCall::abi_decode_returns(result.output().unwrap()).unwrap()
}

/// Forward calldata with STATICCALL and return its success word followed by returndata.
pub(super) fn static_reward_proxy(db: &mut CacheDB<EmptyDB>) -> Address {
    let address = Address::repeat_byte(0xe7);
    let mut code = vec![0x36, 0x5f, 0x5f, 0x37, 0x5f, 0x5f, 0x36, 0x5f, 0x73];
    code.extend_from_slice(AGENT_REWARD_ADDRESS.as_slice());
    code.extend_from_slice(&[
        0x5a, 0xfa, 0x5f, 0x52, 0x3d, 0x5f, 0x60, 0x20, 0x3e, 0x3d, 0x60, 0x20, 0x01, 0x5f, 0xf3,
    ]);
    let code = Bytecode::new_raw(Bytes::from(code));
    db.insert_account_info(
        address,
        AccountInfo {
            code_hash: code.hash_slow(),
            code: Some(code),
            ..Default::default()
        },
    );
    address
}

/// `price` less the agent discount, the entry of an agent reward Gem.
pub(super) fn discounted(price: U256) -> U256 {
    outbe_core_common::markup::marked_down(price, outbe_agentreward::constants::AGENT_DISCOUNT_PCT)
        .unwrap()
}

pub(super) fn seed_oracle(storage: &StorageHandle<'_>) {
    outbe_oracle::test_support::publish_day_type_rate(storage, U256::from(3_000_000), 1, NOW)
        .unwrap();
}

pub(super) fn seed_vwap(storage: &StorageHandle<'_>, day: u32, price: U256) {
    let index = outbe_oracle::api::coen_pair_index_opt(storage.clone(), 840)
        .unwrap()
        .unwrap();
    outbe_oracle::schema::OracleContract::new(storage.clone())
        .record_utc_day_vwap(day, index, price)
        .unwrap();
}

// Snapshot all mutated contract storage, including Gem owner indexes and factory counters.
type ContractState = (Address, U256, Vec<(U256, U256)>);

pub(super) fn snapshot(db: &mut CacheDB<EmptyDB>) -> (ICcaRegistry::Cca, Vec<ContractState>) {
    let record = with_storage(db, |s| api::get_cca(&s, CCA).unwrap());
    let mut contracts = Vec::new();
    for address in [
        CCA_REGISTRY_ADDRESS,
        AGENT_REWARD_ADDRESS,
        GEM_ADDRESS,
        GEM_FACTORY_ADDRESS,
    ] {
        let mut slots = db
            .cache
            .accounts
            .get(&address)
            .map(|account| {
                account
                    .storage
                    .iter()
                    .filter(|(_, value)| !value.is_zero())
                    .map(|(key, value)| (*key, *value))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        slots.sort();
        let balance = db
            .cache
            .accounts
            .get(&address)
            .map(|a| a.info.balance)
            .unwrap_or_default();
        contracts.push((address, balance, slots));
    }
    (record, contracts)
}
