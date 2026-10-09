use super::*;

const GENESIS_TS: u64 = 1_704_067_200;
const SECONDS_PER_DAY: u64 = 86_400;
const VALIDATOR_COUNT: u32 = outbe_consensus::bls::MAX_VALIDATORS;

#[derive(Clone, Copy)]
struct RewardDays {
    block_ts: u64,
    reward_utc_day: u32,
    backlog_utc_day: u32,
}

fn seed_backlog(storage: StorageHandle<'_>, proposer: Address, days: RewardDays) {
    let RewardDays {
        block_ts,
        reward_utc_day,
        backlog_utc_day,
    } = days;
    let emission_trigger = outbe_cycle::triggers::TriggerId::ProtocolCycle.as_u32();
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
}

fn seed_next_reward_day(storage: StorageHandle<'_>, reward_utc_day: u32) {
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
}

fn prepare_reward_state(
    proposer: Address,
    days: RewardDays,
) -> State<CacheDB<EmptyDBTyped<ProviderError>>> {
    state_with_active_validators_seeded(&[(proposer, dummy_pubkey(0xA2))], |storage| {
        seed_backlog(storage.clone(), proposer, days);
        seed_next_reward_day(storage.clone(), days.reward_utc_day);
        seed_previous_day_vwap(&storage, days.block_ts, U256::from(1_000_000u64));
        outbe_oracle::api::set_exchange_rate(
            storage,
            Address::ZERO,
            outbe_oracle::api::DAY_TYPE_PAIR,
            outbe_oracle::api::RateObservation {
                rate: U256::from(1_000_000u64),
                block_number: 1,
                timestamp: days.block_ts,
            },
        )
        .unwrap();
    })
}

fn execute_reward_delivery(
    state: &mut State<CacheDB<EmptyDBTyped<ProviderError>>>,
    signer: Arc<OutbeEvmSigner>,
    proposer: Address,
    block_ts: u64,
) {
    let mut evm_env = test_evm_env(1, REWARDS_ADDRESS);
    evm_env.block_env.timestamp = U256::from(block_ts);
    let config = OutbeEvmConfig::new(test_chain_spec()).with_evm_signer(signer);
    let evm = config.evm_with_env(state, evm_env);
    let mut executor = config.create_executor(evm, block_one_execution_ctx(Some(0), Bytes::new()));
    executor.apply_pre_execution_changes().unwrap();
    let mut system_txs = begin_system_txs_for_test(
        &config,
        BeginBlockFixture {
            block_number: 1,
            parent_hash: B256::ZERO,
            extra_data: &Bytes::new(),
            parent_consensus_metadata: None,
            proposer,
            bootstrap: BootstrapFixture::StandardForBlock,
        },
    );
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
}

fn assert_reward_queues(
    state: &mut State<CacheDB<EmptyDBTyped<ProviderError>>>,
    proposer: Address,
    days: RewardDays,
) {
    let RewardDays {
        block_ts,
        reward_utc_day,
        backlog_utc_day,
    } = days;
    let read_ctx = BlockContext::new(1, block_ts, CHAIN_ID, proposer, vec![proposer]);
    let mut provider =
        outbe_primitives::storage::direct::DirectStorageProvider::new(state, read_ctx);
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

pub(super) fn run() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let block_ts = GENESIS_TS + 2 * SECONDS_PER_DAY + 60;
    let current_utc_day = outbe_primitives::time::timestamp_to_date_key(block_ts);
    let reward_utc_day = outbe_primitives::time::previous_date_key(current_utc_day);
    let backlog_utc_day = outbe_primitives::time::previous_date_key(reward_utc_day);

    let days = RewardDays {
        block_ts,
        reward_utc_day,
        backlog_utc_day,
    };
    let mut state = prepare_reward_state(proposer, days);
    execute_reward_delivery(&mut state, signer, proposer, block_ts);
    assert_reward_queues(&mut state, proposer, days);
}
