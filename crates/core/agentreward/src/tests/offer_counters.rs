use super::*;

fn increment(
    contract: &mut AgentRewardContract<'_>,
    pool: RewardPool,
    address: Address,
) -> outbe_primitives::error::Result<()> {
    match pool {
        RewardPool::Waa => contract.increment_waa_offer(DAY, address),
        _ => contract.increment_sra_offer(DAY, address),
    }
}

/// The count of `address`, the number of the addresses and the total of the day.
fn counters(
    contract: &AgentRewardContract<'_>,
    pool: RewardPool,
    address: Address,
) -> (u64, u32, u64) {
    let index = contract.offer_index(pool).unwrap();
    (
        index
            .counts
            .read(&AgentRewardContract::offer_count_key(DAY, address))
            .unwrap(),
        index.address_count.read(&DAY).unwrap(),
        index.total.read(&DAY).unwrap(),
    )
}

fn assert_overflow_writes_nothing(
    contract: &mut AgentRewardContract<'_>,
    pool: RewardPool,
    address: Address,
) {
    let before = counters(contract, pool, address);
    assert_reverts(
        increment(contract, pool, address),
        AgentRewardError::OfferCounterOverflow,
    );
    assert_eq!(counters(contract, pool, address), before);
}

#[test]
fn a_full_address_count_refuses_a_new_address_but_not_a_counted_one() {
    for pool in [RewardPool::Waa, RewardPool::Sra] {
        with_storage(|storage| {
            let mut contract = AgentRewardContract::new(storage.clone());
            let index = contract.offer_index(pool).unwrap();
            index.address_count.write(&DAY, u32::MAX - 1).unwrap();
            increment(&mut contract, pool, agent(1)).unwrap();
            assert_eq!(counters(&contract, pool, agent(1)), (1, u32::MAX, 1));
            assert_overflow_writes_nothing(&mut contract, pool, agent(2));
            increment(&mut contract, pool, agent(1)).unwrap();
            assert_eq!(counters(&contract, pool, agent(1)), (2, u32::MAX, 2));
        });
    }
}

#[test]
fn a_full_day_total_refuses_the_next_offer() {
    for pool in [RewardPool::Waa, RewardPool::Sra] {
        with_storage(|storage| {
            let mut contract = AgentRewardContract::new(storage.clone());
            let index = contract.offer_index(pool).unwrap();
            index.total.write(&DAY, u64::MAX - 1).unwrap();
            increment(&mut contract, pool, agent(1)).unwrap();
            assert_eq!(counters(&contract, pool, agent(1)), (1, 1, u64::MAX));
            assert_overflow_writes_nothing(&mut contract, pool, agent(2));
            assert_overflow_writes_nothing(&mut contract, pool, agent(1));
        });
    }
}

#[test]
fn a_full_count_of_one_address_refuses_its_next_offer() {
    for pool in [RewardPool::Waa, RewardPool::Sra] {
        with_storage(|storage| {
            let mut contract = AgentRewardContract::new(storage.clone());
            let index = contract.offer_index(pool).unwrap();
            index
                .counts
                .write(
                    &AgentRewardContract::offer_count_key(DAY, agent(1)),
                    u64::MAX - 1,
                )
                .unwrap();
            index.address_count.write(&DAY, 1).unwrap();
            index.total.write(&DAY, 1).unwrap();
            increment(&mut contract, pool, agent(1)).unwrap();
            assert_eq!(counters(&contract, pool, agent(1)), (u64::MAX, 1, 2));
            assert_overflow_writes_nothing(&mut contract, pool, agent(1));
        });
    }
}

#[test]
fn each_offer_publishes_the_account_and_its_weight_of_the_day() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.set_timestamp(U256::from(T_NOW));
    StorageHandle::enter(&mut provider, |storage| {
        let mut contract = AgentRewardContract::new(storage);
        for (pool, address) in [
            (RewardPool::Waa, agent(1)),
            (RewardPool::Waa, agent(1)),
            (RewardPool::Sra, agent(2)),
        ] {
            increment(&mut contract, pool, address).unwrap();
        }
    });
    let added: Vec<_> = provider
        .get_ordered_events()
        .iter()
        .filter_map(|log| IAgentReward::AgentRewardWeightAdded::decode_log(log).ok())
        .map(|log| {
            (
                log.data.account,
                log.data.pool,
                log.data.emissionDay,
                log.data.weight,
            )
        })
        .collect();
    assert_eq!(
        added,
        vec![
            (agent(1), RewardPool::Waa as u8, DAY, U256::from(1)),
            (agent(1), RewardPool::Waa as u8, DAY, U256::from(2)),
            (agent(2), RewardPool::Sra as u8, DAY, U256::from(1)),
        ]
    );
}
