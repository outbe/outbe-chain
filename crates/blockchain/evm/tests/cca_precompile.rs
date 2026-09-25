//! CCA custody and registration through real EVM transactions.
use alloy_evm::{Evm as _, EvmFactory as _};
use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{SolCall, SolEvent};
use outbe_agentreward::{
    distribution::{distribute_daily, PoolKind},
    schema::RewardPool,
    AgentRewardContract,
};
use outbe_ccaregistry::{
    api,
    constants::{BOND_REQUIREMENT, UNBOND_COOLDOWN_SECONDS},
    precompile::ICcaRegistry,
};
use outbe_evm::OutbeEvmFactory;
use outbe_primitives::{
    addresses::{AGENT_REWARD_ADDRESS, CCA_REGISTRY_ADDRESS},
    block::{BlockContext, BlockRuntimeContext},
    storage::{direct::DirectStorageProvider, StorageHandle},
    time::{previous_date_key, timestamp_to_date_key},
    units::checked_protocol_to_native,
};
use reth_ethereum::evm::primitives::EvmEnv;
use revm::{
    context::{result::ExecutionResult, BlockEnv, CfgEnv, TxEnv},
    database::{CacheDB, EmptyDB},
    primitives::{hardfork::SpecId, TxKind},
    state::{AccountInfo, Bytecode},
    DatabaseCommit,
};

const CCA: Address = Address::repeat_byte(0xc1);
const NOW: u64 = 1_700_000_000;

fn tx(db: &mut CacheDB<EmptyDB>, value: U256, data: Vec<u8>, now: u64) -> ExecutionResult {
    tx_to(db, CCA_REGISTRY_ADDRESS, value, data, now)
}

fn tx_to(
    db: &mut CacheDB<EmptyDB>,
    target: Address,
    value: U256,
    data: Vec<u8>,
    now: u64,
) -> ExecutionResult {
    try_tx_to(db, target, value, data, now).unwrap()
}

fn try_tx_to(
    db: &mut CacheDB<EmptyDB>,
    target: Address,
    value: U256,
    data: Vec<u8>,
    now: u64,
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
        let mut evm = OutbeEvmFactory::new().create_evm(&mut *db, env);
        evm.transact_raw(
            TxEnv::builder()
                .caller(CCA)
                .nonce(nonce)
                .kind(TxKind::Call(target))
                .value(value)
                .data(Bytes::from(data))
                .gas_limit(3_000_000)
                .build()
                .unwrap(),
        )
        .map_err(|error| error.to_string())?
    };
    db.commit(outcome.state);
    Ok(outcome.result)
}

fn with_storage<R>(db: &mut CacheDB<EmptyDB>, f: impl FnOnce(StorageHandle<'_>) -> R) -> R {
    let mut provider = DirectStorageProvider::new(db, BlockContext::empty_for_tests(1, NOW, 1));
    let result = StorageHandle::enter(&mut provider, f);
    provider.flush().unwrap();
    result
}

#[test]
fn evm_bond_rewards_and_exit_preserve_custody_and_history() {
    let mut db = CacheDB::new(EmptyDB::default());
    let initial = BOND_REQUIREMENT * U256::from(2);
    db.insert_account_info(
        CCA,
        AccountInfo {
            balance: initial,
            ..Default::default()
        },
    );
    // Same marker as fresh genesis and the block executor, preserving empty-balance state.
    let code = Bytecode::new_raw(Bytes::from_static(&[0xef]));
    db.insert_account_info(
        CCA_REGISTRY_ADDRESS,
        AccountInfo {
            code_hash: code.hash_slow(),
            code: Some(code),
            ..Default::default()
        },
    );

    assert!(!tx(
        &mut db,
        U256::ZERO,
        ICcaRegistry::getCcaCall { cca: CCA }.abi_encode(),
        NOW
    )
    .is_success());
    assert!(!tx(
        &mut db,
        U256::ONE,
        ICcaRegistry::bondCall {
            name: String::new()
        }
        .abi_encode(),
        NOW
    )
    .is_success());
    assert_eq!(db.cache.accounts[&CCA].info.balance, initial);
    assert!(tx(
        &mut db,
        BOND_REQUIREMENT - U256::ONE,
        ICcaRegistry::bondCall {
            name: "Test CCA".into()
        }
        .abi_encode(),
        NOW
    )
    .is_success());
    with_storage(&mut db, |s| {
        assert!(!api::is_active(&s, CCA).unwrap());
        let record = api::get_cca(&s, CCA).unwrap();
        assert_eq!(record.state, ICcaRegistry::State::Bonding);
        assert_eq!(record.name, "Test CCA");
        assert_eq!(record.bondedAmount, BOND_REQUIREMENT - U256::ONE);
    });
    let bonded = tx(
        &mut db,
        U256::ONE,
        ICcaRegistry::bondCall {
            name: "Test CCA".into(),
        }
        .abi_encode(),
        NOW,
    );
    assert!(bonded.is_success());
    assert_eq!(bonded.logs().len(), 1);
    let log = &bonded.logs()[0];
    assert_eq!(log.address, CCA_REGISTRY_ADDRESS);
    let event = ICcaRegistry::Bonded::decode_log(log).unwrap().data;
    assert_eq!(event.cca, CCA);
    assert_eq!(event.amount, U256::ONE);
    assert_eq!(event.state, ICcaRegistry::State::Active);
    assert_eq!(
        db.cache.accounts[&CCA].info.balance,
        initial - BOND_REQUIREMENT
    );
    assert_eq!(
        db.cache.accounts[&CCA_REGISTRY_ADDRESS].info.balance,
        BOND_REQUIREMENT
    );
    // Malformed and nonpayable funded calls refund value and preserve registration.
    for data in [
        vec![],
        vec![0, 1, 2, 3],
        ICcaRegistry::unbondCall {}.abi_encode(),
    ] {
        assert!(!tx(&mut db, U256::ONE, data, NOW).is_success());
        assert_eq!(
            db.cache.accounts[&CCA_REGISTRY_ADDRESS].info.balance,
            BOND_REQUIREMENT
        );
    }
    with_storage(&mut db, |s| {
        api::position_opened(&s, CCA, 20231115, U256::from(100)).unwrap();
        let ctx = BlockRuntimeContext::new(BlockContext::empty_for_tests(2, NOW, 1), s.clone());
        assert_eq!(
            distribute_daily(&ctx, 20231115.into(), &[(PoolKind::Cca, U256::from(1000))]).unwrap(),
            U256::from(680)
        );
    });
    let reward = checked_protocol_to_native(U256::from(320)).unwrap();
    assert_eq!(
        db.cache.accounts[&AGENT_REWARD_ADDRESS].info.balance,
        reward
    );
    assert_eq!(
        db.cache.accounts[&CCA_REGISTRY_ADDRESS].info.balance,
        BOND_REQUIREMENT
    );
    assert!(tx(
        &mut db,
        U256::ZERO,
        ICcaRegistry::unbondCall {}.abi_encode(),
        NOW
    )
    .is_success());
    assert!(!tx(
        &mut db,
        U256::ONE,
        ICcaRegistry::bondCall {
            name: "Test CCA".into()
        }
        .abi_encode(),
        NOW
    )
    .is_success());
    with_storage(&mut db, |s| {
        let record = api::get_cca(&s, CCA).unwrap();
        assert_eq!(record.state as u8, ICcaRegistry::State::Deregistering as u8);
        assert_eq!(record.bondedAmount, BOND_REQUIREMENT);
        assert_eq!(record.unbondUnlocksAfter, NOW + UNBOND_COOLDOWN_SECONDS);
        assert!(!api::is_active(&s, CCA).unwrap());
        assert!(api::position_opened(&s, CCA, 20231115, U256::ONE).is_err());
    });
    let claim = |amount| IAgentReward::claimRewardCall { pool: 2, amount }.abi_encode();
    let before = snapshot(&mut db);
    // Both spot and an older daily VWAP are available; neither may replace yesterday.
    with_storage(&mut db, |storage| {
        seed_oracle(&storage);
        seed_vwap(
            &storage,
            previous_date_key(previous_date_key(timestamp_to_date_key(NOW))),
            U256::from(9_000_000),
        );
    });
    for amount in [U256::ZERO, reward + U256::ONE, U256::ONE] {
        let failed = tx_to(
            &mut db,
            AGENT_REWARD_ADDRESS,
            U256::ZERO,
            claim(amount),
            NOW,
        );
        assert!(!failed.is_success());
        assert!(failed.logs().is_empty());
        assert_eq!(snapshot(&mut db), before);
    }
    // Retry tomorrow against today's finalized VWAP, with a price distinct from spot.
    let price = U256::from(2_000_000);
    let claim_time = NOW + 86400;
    with_storage(&mut db, |storage| {
        seed_vwap(&storage, timestamp_to_date_key(NOW), price)
    });
    for (target, signature, args) in [
        (AGENT_REWARD_ADDRESS, "issueCcaReward(address,uint256)", 64),
        (CCA_REGISTRY_ADDRESS, "claimRewards(uint256)", 32),
        (CCA_REGISTRY_ADDRESS, "claimRewards()", 0),
    ] {
        let mut data = alloy_primitives::keccak256(signature)[..4].to_vec();
        data.resize(4 + args, 0);
        let removed = tx_to(&mut db, target, U256::ZERO, data, claim_time);
        assert!(!removed.is_success());
        assert!(removed.logs().is_empty());
        assert_eq!(snapshot(&mut db), before);
    }

    // Force a failure inside issuance after the Gem has been stored.
    with_storage(&mut db, |storage| {
        outbe_gemfactory::schema::GemFactoryContract::new(storage)
            .total_gems_issued
            .write(U256::MAX)
            .unwrap();
    });
    let before_failure = snapshot(&mut db);
    let failed = tx_to(
        &mut db,
        AGENT_REWARD_ADDRESS,
        U256::ZERO,
        claim(U256::ZERO),
        claim_time,
    );
    assert!(!failed.is_success());
    assert!(failed.logs().is_empty());
    assert_eq!(snapshot(&mut db), before_failure);
    with_storage(&mut db, |storage| {
        outbe_gemfactory::schema::GemFactoryContract::new(storage)
            .total_gems_issued
            .write(U256::ZERO)
            .unwrap();
    });
    // Force failure after successful issuance, when burning its backing.
    with_storage(&mut db, |storage| {
        let backing = storage.balance(AGENT_REWARD_ADDRESS).unwrap();
        storage
            .decrease_balance(AGENT_REWARD_ADDRESS, backing)
            .unwrap();
    });
    let before_failure = snapshot(&mut db);
    let failed = try_tx_to(
        &mut db,
        AGENT_REWARD_ADDRESS,
        U256::ZERO,
        claim(U256::ZERO),
        claim_time,
    )
    .unwrap_err();
    assert!(failed.contains("insufficient balance for burn"));
    assert_eq!(snapshot(&mut db), before_failure);
    with_storage(&mut db, |storage| {
        storage
            .increase_balance(AGENT_REWARD_ADDRESS, reward)
            .unwrap();
        // Preserve an existing native remainder too.
        storage
            .increase_balance(AGENT_REWARD_ADDRESS, U256::from(7))
            .unwrap();
        AgentRewardContract::new(storage)
            .cca_claimable_rewards
            .write(&CCA, reward + U256::from(7))
            .unwrap();
    });
    for (load, requested) in [
        (
            100,
            checked_protocol_to_native(U256::from(100)).unwrap() + U256::from(3),
        ),
        (220, U256::ZERO),
    ] {
        let result = tx_to(
            &mut db,
            AGENT_REWARD_ADDRESS,
            U256::ZERO,
            claim(requested),
            claim_time,
        );
        assert!(result.is_success(), "{result:?}");
        let gem_id =
            IAgentReward::claimRewardCall::abi_decode_returns(result.output().unwrap()).unwrap();
        let claimed = result
            .logs()
            .iter()
            .find_map(|log| IAgentReward::RewardsClaimed::decode_log(log).ok())
            .unwrap()
            .data;
        assert_eq!(claimed.cca, CCA);
        assert!(result
            .logs()
            .iter()
            .any(|log| log.address == AGENT_REWARD_ADDRESS
                && IAgentReward::RewardsClaimed::decode_log(log).is_ok()));
        assert_eq!(
            claimed.amount,
            checked_protocol_to_native(U256::from(load)).unwrap()
        );
        let issued = result
            .logs()
            .iter()
            .find_map(|log| {
                outbe_gemfactory::precompile::IGemFactory::GemIssued::decode_log(log).ok()
            })
            .unwrap()
            .data;
        assert_eq!(issued.gemId, gem_id);
        with_storage(&mut db, |storage| {
            let gem = outbe_gem::api::get_gem(&storage, gem_id).unwrap().unwrap();
            assert_eq!(gem.owner, CCA);
            assert_eq!(gem.gem_type, outbe_gemfactory::schema::GemTypes::Cca as u8);
            assert_eq!(gem.promis_load_minor, U256::from(load));
            assert_eq!(gem.entry_price_minor, price);
            assert_eq!(gem.issuance_currency, 840);
            assert_eq!(gem.reference_currency, 840);
            assert_eq!(gem.issued_at, claim_time);
            assert_eq!(
                api::get_cca(&storage, CCA).unwrap().bondedAmount,
                BOND_REQUIREMENT
            );
        });
        assert_eq!(
            db.cache.accounts[&CCA].info.balance,
            initial - BOND_REQUIREMENT
        );
    }
    with_storage(&mut db, |storage| {
        assert_eq!(
            AgentRewardContract::new(storage)
                .get_pool_claimable_reward(RewardPool::Cca, CCA)
                .unwrap(),
            U256::from(7)
        )
    });
    assert!(!tx_to(
        &mut db,
        AGENT_REWARD_ADDRESS,
        U256::ZERO,
        claim(U256::ZERO),
        claim_time
    )
    .is_success());
    assert_eq!(
        db.cache.accounts[&CCA_REGISTRY_ADDRESS].info.balance,
        BOND_REQUIREMENT
    );
    assert!(!tx(
        &mut db,
        U256::ZERO,
        ICcaRegistry::claimUnbondedCall {}.abi_encode(),
        NOW + UNBOND_COOLDOWN_SECONDS - 1
    )
    .is_success());
    assert!(tx(
        &mut db,
        U256::ZERO,
        ICcaRegistry::claimUnbondedCall {}.abi_encode(),
        NOW + UNBOND_COOLDOWN_SECONDS
    )
    .is_success());
    assert_eq!(
        db.cache.accounts[&CCA_REGISTRY_ADDRESS].info.balance,
        U256::ZERO
    );
    assert_eq!(db.cache.accounts[&CCA].info.balance, initial);
    with_storage(&mut db, |s| {
        let record = api::get_cca(&s, CCA).unwrap();
        assert_eq!(record.state as u8, ICcaRegistry::State::Deregistered as u8);
        assert_eq!(record.bondedAmount, U256::ZERO);
        assert_eq!(record.name, "Test CCA");
        assert_eq!(
            api::reward_weight(&s, CCA, 20231115).unwrap(),
            U256::from(100)
        );
    });
    assert!(!tx(
        &mut db,
        U256::ZERO,
        ICcaRegistry::claimUnbondedCall {}.abi_encode(),
        NOW + UNBOND_COOLDOWN_SECONDS
    )
    .is_success());
}

alloy_sol_types::sol!("../../../contracts/precompiles/src/IAgentReward.sol");

fn seed_oracle(storage: &StorageHandle<'_>) {
    outbe_oracle::api::register_pair(storage.clone(), outbe_oracle::api::DAY_TYPE_PAIR).unwrap();
    outbe_oracle::api::set_exchange_rate(
        storage.clone(),
        Address::ZERO,
        outbe_oracle::api::DAY_TYPE_PAIR,
        U256::from(3_000_000),
        1,
        NOW,
    )
    .unwrap();
    outbe_oracle::schema::OracleContract::new(storage.clone())
        .reference_currencies
        .push(840)
        .unwrap();
}

fn seed_vwap(storage: &StorageHandle<'_>, day: u32, price: U256) {
    let index = outbe_oracle::api::coen_pair_index_opt(storage.clone(), 840)
        .unwrap()
        .unwrap();
    outbe_oracle::schema::OracleContract::new(storage.clone())
        .utc_day_vwap_value
        .get_nested(&day)
        .write(&index, price)
        .unwrap();
}

// Snapshot all mutated contract storage, including Gem owner indexes and factory counters.
type ContractState = (Address, U256, Vec<(U256, U256)>);

fn snapshot(db: &mut CacheDB<EmptyDB>) -> (ICcaRegistry::Cca, Vec<ContractState>) {
    let record = with_storage(db, |s| api::get_cca(&s, CCA).unwrap());
    let mut contracts = Vec::new();
    for address in [
        CCA_REGISTRY_ADDRESS,
        AGENT_REWARD_ADDRESS,
        outbe_primitives::addresses::GEM_ADDRESS,
        outbe_primitives::addresses::GEM_FACTORY_ADDRESS,
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
