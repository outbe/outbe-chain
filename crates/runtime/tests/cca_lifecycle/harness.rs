// CCA custody and registration through real EVM transactions.
pub(crate) use alloy_consensus::TxLegacy;
pub(crate) use alloy_primitives::{Address, Bytes, TxKind, U256};
pub(crate) use alloy_sol_types::{SolCall, SolError, SolEvent};
pub(crate) use evm2::{
    bytecode::Bytecode,
    env::BlockEnvExt,
    ethereum::TxEnvelope,
    evm::{AccountInfo, CacheDB, EmptyDB},
    SpecId, TxResult,
};
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
pub(crate) use outbe_evm::{factory::OutbeEvmTypes, OutbeEvmFactory};
pub(crate) use outbe_gem::precompile::IGem;
pub(crate) use outbe_primitives::{
    block::{BlockContext, BlockRuntimeContext},
    storage::{direct::DirectStorageProvider, StorageHandle},
};
pub(crate) use outbe_system_common::addresses::{
    AGENT_REWARD_ADDRESS, CCA_REGISTRY_ADDRESS, GEM_ADDRESS, GEM_FACTORY_ADDRESS, ORACLE_ADDRESS,
    PROMIS_LIMIT_ADDRESS,
};
pub(crate) use outbe_system_common::time::SECONDS_PER_DAY;
pub(crate) use reth_ethereum::evm::EthEvmEnv;
pub(crate) use reth_primitives_traits::Recovered;

pub(super) const CCA: Address = Address::repeat_byte(0xc1);
pub(super) const NOW: u64 = 1_700_000_000;

pub(super) fn balance(db: &CacheDB<EmptyDB>, address: Address) -> U256 {
    db.account_info(&address)
        .map_or(U256::ZERO, |info| info.balance)
}

pub(super) fn set_balance(db: &mut CacheDB<EmptyDB>, address: Address, amount: U256) {
    let mut info = db.account_info(&address).cloned().unwrap_or_default();
    info.balance = amount;
    db.insert_account_info(&address, info);
}

pub(super) fn tx(
    db: &mut CacheDB<EmptyDB>,
    value: U256,
    data: Vec<u8>,
    now: u64,
) -> TxResult<OutbeEvmTypes> {
    tx_to(db, CCA_REGISTRY_ADDRESS, value, data, now)
}

pub(super) fn tx_to(
    db: &mut CacheDB<EmptyDB>,
    target: Address,
    value: U256,
    data: Vec<u8>,
    now: u64,
) -> TxResult<OutbeEvmTypes> {
    try_tx_to(db, target, value, data, now).unwrap()
}

pub(super) fn try_tx_to(
    db: &mut CacheDB<EmptyDB>,
    target: Address,
    value: U256,
    data: Vec<u8>,
    now: u64,
) -> Result<TxResult<OutbeEvmTypes>, String> {
    try_tx_to_with_gas(db, target, value, data, now, 3_000_000)
}

pub(super) fn try_tx_to_with_gas(
    db: &mut CacheDB<EmptyDB>,
    target: Address,
    value: U256,
    data: Vec<u8>,
    now: u64,
    gas_limit: u64,
) -> Result<TxResult<OutbeEvmTypes>, String> {
    let nonce = db.account_info(&CCA).map_or(0, |info| info.nonce);
    let env = EthEvmEnv::new(
        SpecId::PRAGUE,
        BlockEnvExt {
            timestamp: U256::from(now),
            gas_limit: U256::from(30_000_000_u64),
            ..Default::default()
        },
        1,
    );
    let tx = Recovered::new_unchecked(
        TxEnvelope::Legacy(TxLegacy {
            chain_id: Some(1),
            nonce,
            gas_price: 0,
            gas_limit,
            to: TxKind::Call(target),
            value,
            input: Bytes::from(data),
        }),
        CCA,
    );
    let outcome = {
        let mut evm = OutbeEvmFactory::new(std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()))
            .create_evm(&mut *db, env);
        let detached = evm
            .transact(&tx)
            .map_err(|error| error.to_string())?
            .detach();
        detached
    };
    db.commit_pending(&outcome.pending_state);
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
pub(super) fn redeem_agent_reward(
    db: &mut CacheDB<EmptyDB>,
    data: Vec<u8>,
    now: u64,
) -> Result<(Bytes, Vec<alloy_primitives::Log>), String> {
    redeem_agent_reward_in_block(db, data, now, 1)
}

/// [`redeem_agent_reward`] in block `block`: one owner gets one Gem of a series in each block.
pub(super) fn redeem_agent_reward_in_block(
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

/// CCA can claim Gems: marker code on the touched precompiles and a 24-hour
/// COEN/USD VWAP of [`PRICE`] at [`NOW`].
pub(super) fn reward_world() -> CacheDB<EmptyDB> {
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(&CCA, AccountInfo::default());
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
            &address,
            AccountInfo {
                code_hash: code.hash_slow(),
                code: Some(code.clone()),
                ..Default::default()
            },
        );
    }
    with_storage(&mut db, |storage| {
        seed_oracle(&storage);
        seed_hour_vwap(&storage, PRICE);
    });
    db
}

/// The emission day of the rewards of the tests.
pub(super) const DAY: u32 = 20_231_113;

/// Gives `account` the whole `amount` of `pool` in emission day `day`: `account` is the only
/// receiver of the pool, and the rate of the pool is set.
pub(super) fn credit_to(
    db: &mut CacheDB<EmptyDB>,
    account: Address,
    pool: RewardPool,
    day: u32,
    amount: U256,
) {
    with_storage(db, |storage| {
        let mut agent = AgentRewardContract::new(storage.clone());
        let rewards = outbe_rewards::schema::Rewards::new(storage.clone());
        match pool {
            RewardPool::Waa => agent.increment_waa_offer(day, account).unwrap(),
            RewardPool::Sra => agent.increment_sra_offer(day, account).unwrap(),
            RewardPool::Validator => {
                rewards
                    .day_votes
                    .get_nested(&day)
                    .write(&account, 1)
                    .unwrap();
                rewards.day_voter_count.write(&day, 1).unwrap();
                rewards.day_total_votes.write(&day, U256::ONE).unwrap();
            }
            RewardPool::Cca => {
                outbe_ccaregistry::api::credis_issued(&storage, account, day, U256::ONE).unwrap();
            }
        }
        if rewards.last_sealed_day.read().unwrap() < day {
            rewards.last_sealed_day.write(day).unwrap();
        }
        let limit = u128::try_from(amount).unwrap();
        let mut limits = [0u128; 5];
        limits[match pool {
            RewardPool::Validator => 0,
            RewardPool::Cca => 1,
            RewardPool::Waa => 2,
            RewardPool::Sra => 3,
        }] = limit;
        let mut record = outbe_emission::EmissionDay::new(
            day,
            outbe_system_common::time::WorldwideDay::new(0),
            outbe_emission::api::compute_metadosis_start(day).unwrap(),
            limit,
            limits,
        );
        if matches!(pool, RewardPool::Waa | RewardPool::Sra) {
            record.state = outbe_emission::schema::emission_day_state::PROCESSING;
        }
        outbe_emission::api::put_emission_day(storage.clone(), &record).unwrap();
        agent.set_agent_reward_rate(pool, day).unwrap();
    });
}

/// Gives CCA the whole `amount` of `pool` in [`DAY`].
pub(super) fn credit(db: &mut CacheDB<EmptyDB>, pool: RewardPool, amount: U256) {
    credit_to(db, CCA, pool, DAY, amount);
}

pub(super) fn claim_call(pool: RewardPool, days: &[u32]) -> Vec<u8> {
    IAgentReward::redeemAgentRewardCall {
        pool: pool as u8,
        emissionDays: days.to_vec(),
        issuanceCurrency: 840,
        referenceCurrency: 840,
    }
    .abi_encode()
}

pub(super) fn claim_tx(db: &mut CacheDB<EmptyDB>, pool: RewardPool) -> TxResult<OutbeEvmTypes> {
    tx_to(
        db,
        AGENT_REWARD_ADDRESS,
        U256::ZERO,
        claim_call(pool, &[DAY]),
        NOW,
    )
}

/// Claims the reward `amount` of CCA in `pool` and [`DAY`] by transaction, and checks the Gem,
/// both events and that the weight is gone.
pub(super) fn claim_by_tx(db: &mut CacheDB<EmptyDB>, pool: RewardPool, amount: U256) -> U256 {
    let result = claim_tx(db, pool);
    assert!(result.is_success(), "{result:?}");
    let gem_id = IAgentReward::redeemAgentRewardCall::abi_decode_returns(&result.output).unwrap();
    let claimed = result
        .logs
        .iter()
        .find_map(|log| IAgentReward::AgentRewardRedeemed::decode_log(log).ok())
        .expect("the claim emits AgentRewardRedeemed")
        .data;
    assert_eq!(claimed.account, CCA);
    assert_eq!(claimed.pool, pool as u8);
    assert_eq!(claimed.promisLoadMinor, amount);
    assert_eq!(claimed.gemId, gem_id);
    let issued = result
        .logs
        .iter()
        .find_map(|log| IGem::GemIssued::decode_log(log).ok())
        .expect("the claim emits GemIssued")
        .data;
    assert_eq!(issued.gemId, gem_id);
    assert_eq!(issued.payload.as_ref(), &amount.to_be_bytes::<32>());
    with_storage(db, |storage| {
        let gem = outbe_gem::api::get_view(&storage, gem_id).unwrap().unwrap();
        assert_eq!(gem.item.owner, CCA);
        assert_eq!(gem.item.promis_load_minor, amount);
        assert_eq!(gem.item.entry_price_minor, discounted(PRICE));
        assert_eq!(gem.item.issued_at, NOW);
        assert_eq!(
            AgentRewardContract::new(storage)
                .weight(pool, DAY, CCA)
                .unwrap(),
            U256::ZERO
        );
    });
    assert_eq!(balance(db, AGENT_REWARD_ADDRESS), U256::ZERO);
    gem_id
}

pub(super) fn expiration_call(account: Address, pool: RewardPool) -> Vec<u8> {
    IAgentReward::expireAgentRewardCall {
        account,
        pool: pool as u8,
        emissionDays: vec![DAY],
    }
    .abi_encode()
}

/// The deadline of the reward of `pool` in [`DAY`]; zero when the day has no rate left.
pub(super) fn reward_deadline(db: &mut CacheDB<EmptyDB>, pool: RewardPool) -> u64 {
    let result = tx_to(
        db,
        AGENT_REWARD_ADDRESS,
        U256::ZERO,
        IAgentReward::getAgentRewardRateCall {
            pool: pool as u8,
            emissionDay: DAY,
        }
        .abi_encode(),
        NOW,
    );
    assert!(result.is_success(), "{result:?}");
    IAgentReward::getAgentRewardRateCall::abi_decode_returns(&result.output)
        .unwrap()
        .deadline
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
    IPromisLimit::promisLimitCall::abi_decode_returns(&result.output).unwrap()
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
        &address,
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

/// Covers the 24-hour COEN/USD window of the storage's hour at `price`.
pub(super) fn seed_hour_vwap(storage: &StorageHandle<'_>, price: U256) {
    outbe_oracle::test_support::seed_covered_trailing_window(
        storage,
        outbe_oracle::api::AddressPair::new_coen_to(840),
        price,
        U256::from(1_000_000u64),
    )
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
            .storage
            .get(&address)
            .map(|storage| {
                storage
                    .slots
                    .iter()
                    .filter(|(_, value)| !value.is_zero())
                    .map(|(key, value)| (*key, *value))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        slots.sort();
        contracts.push((address, balance(db, address), slots));
    }
    (record, contracts)
}
