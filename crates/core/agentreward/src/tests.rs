use alloy_primitives::{Address, U256};
use alloy_sol_types::{SolCall, SolEvent};
use outbe_emission::schema::emission_day_state;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_promislimit::PromisLimitContract;

use crate::constants::{MAX_REWARD_DAYS_PER_CALL, REWARD_BALANCE_IDLE_SECONDS};
use crate::errors::AgentRewardError;
use crate::precompile::IAgentReward;
use crate::schema::{AgentRewardContract, RewardPool};

const CHAIN_ID: u64 = 1;
const T_NOW: u64 = 1_700_000_000;
const DAY: u32 = 20_231_113;
const NEXT_DAY: u32 = 20_231_114;
/// A COEN price, in the six-decimal protocol scale prices are quoted in.
const ONE_COEN: U256 = U256::from_limbs([1_000_000, 0, 0, 0]);
const USD: u16 = 840;

fn assert_reverts<T: std::fmt::Debug>(
    result: outbe_primitives::error::Result<T>,
    expected: AgentRewardError,
) {
    let actual = result.unwrap_err();
    let expected = outbe_primitives::error::PrecompileError::from(expected);
    assert_eq!(format!("{actual:?}"), format!("{expected:?}"));
}

fn agent(n: u8) -> Address {
    Address::repeat_byte(n)
}

fn with_storage<R>(f: impl FnOnce(StorageHandle) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(T_NOW));
    StorageHandle::enter(&mut storage, |storage| f(storage.clone()))
}

/// The record of `day` with `limit` for each pool, in `state`, before its Metadosis.
fn record_day(storage: &StorageHandle<'_>, day: u32, limit: u128, state: u8) {
    let mut record = outbe_emission::EmissionDay::new(
        day,
        outbe_system_common::time::WorldwideDay::new(0),
        outbe_emission::api::compute_metadosis_start(day).unwrap(),
        4 * limit,
        [limit, limit, limit, limit, 0],
    );
    record.state = state;
    outbe_emission::api::put_emission_day(storage.clone(), &record).unwrap();
}

fn record_of(storage: &StorageHandle<'_>, day: u32) -> outbe_emission::EmissionDay {
    outbe_emission::api::get_emission_day(storage.clone(), day)
        .unwrap()
        .unwrap()
}

fn offers(contract: &mut AgentRewardContract<'_>, day: u32, weights: &[(Address, u64)]) {
    for &(address, weight) in weights {
        for _ in 0..weight {
            contract.increment_waa_offer(day, address).unwrap();
        }
    }
}

fn promis_limit(storage: &StorageHandle<'_>) -> U256 {
    PromisLimitContract::new(storage.clone())
        .promis_limit()
        .unwrap()
}

/// Registers COEN/840 with `price` as its spot and the 24-hour VWAP of the current hour.
fn seed_oracle(storage: &StorageHandle<'_>, price: U256) {
    outbe_oracle::test_support::publish_day_type_rate(storage, price, 1, T_NOW).unwrap();
    outbe_oracle::test_support::seed_covered_trailing_window(
        storage,
        outbe_oracle::api::AddressPair::new_coen_to(USD),
        price,
        U256::from(1_000_000u64),
    )
    .unwrap();
}

fn set_time(storage: &StorageHandle<'_>, timestamp: u64) {
    storage.set_block_timestamp(U256::from(timestamp)).unwrap();
}

/// A WAA pool of `limit` with the `weights` of `DAY`, its rate set.
fn waa_pool(storage: &StorageHandle<'_>, limit: u128, weights: &[(Address, u64)]) {
    record_day(storage, DAY, limit, emission_day_state::PROCESSING);
    let mut contract = AgentRewardContract::new(storage.clone());
    offers(&mut contract, DAY, weights);
    contract
        .set_agent_reward_rate(RewardPool::Waa, DAY)
        .unwrap();
}

#[test]
fn one_rate_pays_each_receiver_its_share_and_the_rounding_goes_to_the_promis_limit() {
    with_storage(|storage| {
        let before = promis_limit(&storage);
        waa_pool(
            &storage,
            100,
            &[(agent(1), 1), (agent(2), 2), (agent(4), 4)],
        );
        let contract = AgentRewardContract::new(storage.clone());
        let amounts: Vec<U256> = [1, 2, 4]
            .map(|n| {
                contract
                    .agent_reward(RewardPool::Waa, DAY, agent(n))
                    .unwrap()
                    .1
            })
            .to_vec();
        assert_eq!(amounts, [14u64, 28, 57].map(U256::from).to_vec());
        let record = record_of(&storage, DAY);
        assert_eq!(record.waa, 99);
        assert_eq!(record.promis_limit, 1);
        assert_eq!(promis_limit(&storage), before + U256::from(1u64));
        let (rate, deadline, unclaimed) = contract.agent_reward_rate(RewardPool::Waa, DAY).unwrap();
        assert_eq!(
            rate,
            U256::from(100u64) * U256::from(10u64).pow(U256::from(18u64)) / U256::from(7u64)
        );
        assert_eq!(deadline, T_NOW + REWARD_BALANCE_IDLE_SECONDS);
        assert_eq!(unclaimed, U256::from(99u64));
    });
}

#[test]
fn the_rate_has_decimal_places_so_a_large_total_weight_does_not_round_it_to_zero() {
    with_storage(|storage| {
        record_day(&storage, DAY, 100, emission_day_state::PROCESSING);
        let contract = AgentRewardContract::new(storage.clone());
        let weight = 1_000_000_000_000u64;
        for (address, count) in [(agent(1), weight), (agent(2), 3 * weight)] {
            contract
                .waa_offer_counts
                .write(&AgentRewardContract::offer_count_key(DAY, address), count)
                .unwrap();
        }
        contract.waa_address_count.write(&DAY, 2).unwrap();
        contract.waa_total_count.write(&DAY, 4 * weight).unwrap();
        let mut contract = AgentRewardContract::new(storage.clone());
        contract
            .set_agent_reward_rate(RewardPool::Waa, DAY)
            .unwrap();
        let (rate, _, _) = contract.agent_reward_rate(RewardPool::Waa, DAY).unwrap();
        assert!(!rate.is_zero());
        assert_eq!(
            contract
                .agent_reward(RewardPool::Waa, DAY, agent(1))
                .unwrap()
                .1,
            U256::from(25u64)
        );
        assert_eq!(
            contract
                .agent_reward(RewardPool::Waa, DAY, agent(2))
                .unwrap()
                .1,
            U256::from(75u64)
        );
    });
}

#[test]
fn a_pool_with_no_receiver_sends_its_whole_limit_to_the_promis_limit() {
    with_storage(|storage| {
        let before = promis_limit(&storage);
        waa_pool(&storage, 100, &[]);
        let record = record_of(&storage, DAY);
        assert_eq!(record.waa, 0);
        assert_eq!(record.promis_limit, 100);
        assert_eq!(promis_limit(&storage), before + U256::from(100u64));
        assert_eq!(
            AgentRewardContract::new(storage)
                .agent_reward_rate(RewardPool::Waa, DAY)
                .unwrap(),
            (U256::ZERO, 0, U256::ZERO)
        );
    });
}

#[test]
fn a_rate_is_set_once_and_closes_the_weights_of_its_day() {
    with_storage(|storage| {
        waa_pool(&storage, 100, &[(agent(1), 1)]);
        let mut contract = AgentRewardContract::new(storage.clone());
        assert_reverts(
            contract.increment_waa_offer(DAY, agent(2)),
            AgentRewardError::AgentRewardRateAlreadySet {
                pool: RewardPool::Waa as u8,
                day: DAY,
            },
        );
        assert!(matches!(
            contract.set_agent_reward_rate(RewardPool::Waa, DAY),
            Err(outbe_primitives::error::PrecompileError::FatalWithCause(_))
        ));
        // The Validator and CCA rates come before the record of the day starts.
        assert!(matches!(
            contract.set_agent_reward_rate(RewardPool::Validator, DAY),
            Err(outbe_primitives::error::PrecompileError::FatalWithCause(_))
        ));
    });
}

#[test]
fn a_failed_rate_sends_the_whole_pool_limit_to_the_promis_limit() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.set_timestamp(U256::from(T_NOW));
    StorageHandle::enter(&mut provider, |storage| {
        // The votes of the day are not sealed, so the Validator rate cannot read its weights.
        record_day(&storage, DAY, 100, emission_day_state::PENDING);
        let before = promis_limit(&storage);
        AgentRewardContract::new(storage.clone())
            .set_agent_reward_rate(RewardPool::Validator, DAY)
            .unwrap();
        let record = record_of(&storage, DAY);
        assert_eq!(record.validators, 0);
        assert_eq!(record.promis_limit, 100);
        assert_eq!(promis_limit(&storage), before + U256::from(100u64));
    });
    let failed: Vec<_> = provider
        .get_ordered_events()
        .iter()
        .filter_map(|log| IAgentReward::AgentRewardRateFailed::decode_log(log).ok())
        .map(|log| (log.data.emissionDay, log.data.pool, log.data.amountMinor))
        .collect();
    assert_eq!(
        failed,
        vec![(DAY, RewardPool::Validator as u8, U256::from(100u64))]
    );
}

#[test]
fn a_claim_of_days_issues_one_gem_for_their_sum() {
    with_storage(|storage| {
        seed_oracle(&storage, ONE_COEN);
        waa_pool(&storage, 100, &[(agent(1), 1), (agent(2), 1)]);
        record_day(&storage, NEXT_DAY, 60, emission_day_state::PROCESSING);
        let mut contract = AgentRewardContract::new(storage.clone());
        offers(&mut contract, NEXT_DAY, &[(agent(1), 1)]);
        contract
            .set_agent_reward_rate(RewardPool::Waa, NEXT_DAY)
            .unwrap();

        let gem_id = contract
            .redeem_agent_reward(RewardPool::Waa, agent(1), &[DAY, NEXT_DAY], (USD, USD))
            .unwrap();
        assert!(!gem_id.is_zero());
        let owned = outbe_gem::api::list_by_owner(&storage, agent(1)).unwrap();
        assert_eq!(owned.len(), 1, "one Gem for the days");
        let gem = outbe_gem::api::get_view(&storage, owned[0].gem_id)
            .unwrap()
            .unwrap();
        assert_eq!(gem.item.promis_load_minor, U256::from(110u64));
        assert_eq!(
            contract.weight(RewardPool::Waa, DAY, agent(1)).unwrap(),
            U256::ZERO
        );
        let (_, _, unclaimed) = contract.agent_reward_rate(RewardPool::Waa, DAY).unwrap();
        assert_eq!(unclaimed, U256::from(50u64));
    });
}

#[test]
fn a_claim_reverts_on_a_bad_list_a_day_with_no_rate_or_a_day_claimed_already() {
    with_storage(|storage| {
        seed_oracle(&storage, ONE_COEN);
        waa_pool(&storage, 100, &[(agent(1), 1), (agent(2), 1)]);
        let mut contract = AgentRewardContract::new(storage.clone());
        let claim = |contract: &mut AgentRewardContract<'_>, days: &[u32]| {
            contract.redeem_agent_reward(RewardPool::Waa, agent(1), days, (USD, USD))
        };
        assert_reverts(
            claim(&mut contract, &[]),
            AgentRewardError::InvalidRewardDays(0),
        );
        let too_many = vec![DAY; MAX_REWARD_DAYS_PER_CALL + 1];
        assert_reverts(
            claim(&mut contract, &too_many),
            AgentRewardError::InvalidRewardDays(MAX_REWARD_DAYS_PER_CALL + 1),
        );
        assert_reverts(
            claim(&mut contract, &[NEXT_DAY]),
            AgentRewardError::AgentRewardRateMissing {
                pool: RewardPool::Waa as u8,
                day: NEXT_DAY,
            },
        );
        assert_reverts(
            claim(&mut contract, &[DAY, DAY]),
            AgentRewardError::NoAgentRewardWeight(DAY),
        );
        claim(&mut contract, &[DAY]).unwrap();
        assert_reverts(
            claim(&mut contract, &[DAY]),
            AgentRewardError::NoAgentRewardWeight(DAY),
        );
    });
}

#[test]
fn the_deadline_ends_claims_and_any_account_expires_the_rest() {
    with_storage(|storage| {
        seed_oracle(&storage, ONE_COEN);
        waa_pool(&storage, 100, &[(agent(1), 1), (agent(2), 2)]);
        let mut contract = AgentRewardContract::new(storage.clone());
        let deadline = T_NOW + REWARD_BALANCE_IDLE_SECONDS;
        assert_reverts(
            contract.expire_agent_reward(RewardPool::Waa, agent(1), &[DAY]),
            AgentRewardError::AgentRewardNotExpired(deadline),
        );
        set_time(&storage, deadline);
        assert_reverts(
            contract.redeem_agent_reward(RewardPool::Waa, agent(1), &[DAY], (USD, USD)),
            AgentRewardError::AgentRewardDeadlinePassed(deadline),
        );
        let before = promis_limit(&storage);
        assert_eq!(
            contract
                .expire_agent_reward(RewardPool::Waa, agent(1), &[DAY])
                .unwrap(),
            U256::from(33u64)
        );
        assert_eq!(promis_limit(&storage), before + U256::from(33u64));
        assert_reverts(
            contract.expire_agent_reward(RewardPool::Waa, agent(1), &[DAY]),
            AgentRewardError::NoAgentRewardWeight(DAY),
        );
    });
}

#[test]
fn the_last_weight_of_a_day_deletes_its_values_and_sends_the_rounding_on() {
    with_storage(|storage| {
        seed_oracle(&storage, ONE_COEN);
        waa_pool(
            &storage,
            100,
            &[(agent(1), 1), (agent(2), 1), (agent(3), 1)],
        );
        let mut contract = AgentRewardContract::new(storage.clone());
        contract
            .redeem_agent_reward(RewardPool::Waa, agent(1), &[DAY], (USD, USD))
            .unwrap();
        set_time(&storage, T_NOW + REWARD_BALANCE_IDLE_SECONDS);
        contract
            .expire_agent_reward(RewardPool::Waa, agent(2), &[DAY])
            .unwrap();
        let before = promis_limit(&storage);
        contract
            .expire_agent_reward(RewardPool::Waa, agent(3), &[DAY])
            .unwrap();
        // 100 / 3 leaves 1 at the rate and 0 after the three claims of 33.
        assert_eq!(promis_limit(&storage), before + U256::from(33u64));
        assert_eq!(
            contract.agent_reward_rate(RewardPool::Waa, DAY).unwrap(),
            (U256::ZERO, 0, U256::ZERO)
        );
        assert_eq!(contract.waa_address_count.read(&DAY).unwrap(), 0);
        assert_eq!(contract.waa_total_count.read(&DAY).unwrap(), 0);
    });
}

#[test]
fn the_readers_give_the_rate_and_the_reward_of_an_account() {
    with_storage(|storage| {
        waa_pool(&storage, 100, &[(agent(1), 1), (agent(2), 3)]);
        let rate = crate::precompile::dispatch(
            storage.clone(),
            &IAgentReward::getAgentRewardRateCall {
                pool: RewardPool::Waa as u8,
                emissionDay: DAY,
            }
            .abi_encode(),
            Address::ZERO,
            U256::ZERO,
        )
        .unwrap();
        let rate = IAgentReward::getAgentRewardRateCall::abi_decode_returns(&rate).unwrap();
        assert_eq!(rate.deadline, T_NOW + REWARD_BALANCE_IDLE_SECONDS);
        assert_eq!(rate.unclaimedMinor, U256::from(100u64));
        let reward = crate::precompile::dispatch(
            storage,
            &IAgentReward::getAgentRewardCall {
                account: agent(2),
                pool: RewardPool::Waa as u8,
                emissionDay: DAY,
            }
            .abi_encode(),
            Address::ZERO,
            U256::ZERO,
        )
        .unwrap();
        let reward = IAgentReward::getAgentRewardCall::abi_decode_returns(&reward).unwrap();
        assert_eq!(reward.weight, U256::from(3u64));
        assert_eq!(reward.promisLoadMinor, U256::from(75u64));
    });
}

mod claim_price;
mod offer_counters;
