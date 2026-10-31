//! CCA reward distribution and claims owned by AgentReward.
use super::*;
use crate::distribution::calculate_proportional_distribution;
use outbe_ccaregistry::{
    api,
    constants::{BOND_REQUIREMENT, MAX_ACTIVE_CCAS, UNBOND_COOLDOWN_SECONDS},
    precompile::ICcaRegistry,
    runtime,
};
use outbe_primitives::{
    addresses::{AGENT_REWARD_ADDRESS, CCA_REGISTRY_ADDRESS},
    block::{BlockContext, BlockRuntimeContext},
};

const ALICE: Address = Address::repeat_byte(1);
const BOB: Address = Address::repeat_byte(2);
const NOW: u64 = T_NOW;
const DAY: u32 = 20231115;
fn run(f: impl FnOnce(StorageHandle<'_>)) {
    with_contract_mut(|storage, _| {
        seed_oracle(&storage, ONE_COEN);
        f(storage);
    });
}
fn bond(storage: &StorageHandle<'_>, who: Address, amount: U256) {
    storage
        .increase_balance(CCA_REGISTRY_ADDRESS, amount)
        .unwrap();
    runtime::bond(storage.clone(), who, amount, "Test CCA".into()).unwrap();
}
fn reward(storage: &StorageHandle<'_>, amount: U256) -> U256 {
    let ctx = BlockRuntimeContext::new(BlockContext::empty_for_tests(1, NOW, 1), storage.clone());
    distribute_daily(&ctx, DAY.into(), &[(PoolKind::Cca, amount)]).unwrap()
}
fn claimable(storage: &StorageHandle<'_>, who: Address) -> U256 {
    AgentRewardContract::new(storage.clone())
        .get_pool_claimable_reward(RewardPool::Cca, who)
        .unwrap()
}
#[test]
fn inactive_weights_are_excluded_and_residue_stays_excess() {
    run(|storage| {
        assert_eq!(reward(&storage, U256::from(11)), U256::from(11));
        bond(&storage, ALICE, BOND_REQUIREMENT);
        bond(&storage, BOB, BOND_REQUIREMENT);
        assert_eq!(reward(&storage, U256::from(11)), U256::from(11));
        runtime::credis_issued(&storage, ALICE, DAY, U256::from(1)).unwrap();
        runtime::credis_issued(&storage, BOB, DAY, U256::from(3)).unwrap();
        assert_eq!(reward(&storage, U256::from(11)), U256::ONE);
        assert_eq!(claimable(&storage, ALICE), U256::from(2));
        assert_eq!(claimable(&storage, BOB), U256::from(8));
        runtime::unbond(storage.clone(), BOB).unwrap();
        assert_eq!(reward(&storage, U256::from(11)), U256::ZERO);
        assert_eq!(claimable(&storage, ALICE), U256::from(13));
        AgentRewardContract::new(storage.clone())
            .claim_reward(RewardPool::Cca, BOB, U256::ZERO, (USD, USD))
            .unwrap();
        assert_eq!(storage.balance(BOB).unwrap(), U256::ZERO);
        assert_eq!(
            storage.balance(CCA_REGISTRY_ADDRESS).unwrap(),
            BOND_REQUIREMENT * U256::from(2)
        );
        assert_eq!(storage.balance(AGENT_REWARD_ADDRESS).unwrap(), U256::ZERO);
    });
}

#[test]
fn wide_reward_products_do_not_overflow_and_a_failed_credit_rolls_back() {
    run(|storage| {
        bond(&storage, ALICE, BOND_REQUIREMENT);
        runtime::credis_issued(&storage, ALICE, DAY, U256::MAX).unwrap();
        assert_eq!(reward(&storage, U256::from(100)), U256::ZERO);
        let ctx = BlockRuntimeContext::new(BlockContext::default(), storage.clone());
        assert!(distribute_daily(&ctx, 20231115.into(), &[(PoolKind::Cca, U256::MAX)]).is_err());
        assert_eq!(claimable(&storage, ALICE), U256::from(100));
        assert!(runtime::credis_issued(&storage, ALICE, DAY, U256::ONE).is_err());
        assert_eq!(api::reward_weight(&storage, ALICE, DAY).unwrap(), U256::MAX);
    });
}

#[test]
fn reward_map_overflow_rolls_back_earlier_credits() {
    run(|storage| {
        for cca in [ALICE, BOB] {
            bond(&storage, cca, BOND_REQUIREMENT);
            runtime::credis_issued(&storage, cca, DAY, U256::ONE).unwrap();
        }
        let contract = AgentRewardContract::new(storage.clone());
        let active = [ALICE, BOB];
        // Fail on the second recipient, after the first credit.
        contract
            .cca_claimable_minor
            .write(&active[1], U256::MAX)
            .unwrap();
        let ctx = BlockRuntimeContext::new(BlockContext::default(), storage.clone());
        assert!(distribute_daily(&ctx, DAY.into(), &[(PoolKind::Cca, U256::from(10))]).is_err());
        assert_eq!(
            contract.cca_claimable_minor.read(&active[0]).unwrap(),
            U256::ZERO
        );
        assert_eq!(
            contract.cca_claimable_minor.read(&active[1]).unwrap(),
            U256::MAX
        );
    });
}

#[test]
fn daily_distribution_at_the_active_cap_matches_the_proportional_reference() {
    // Engineering reserve inside the 30_000_000 steady block gas limit.
    const CCA_DISTRIBUTION_GAS_BUDGET: u64 = 5_000_000;
    let population = u64::from(MAX_ACTIVE_CCAS);
    let mut weights = Vec::new();
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        for id in 1..=population {
            let cca = Address::from_word(U256::from(id).into());
            bond(&storage, cca, BOND_REQUIREMENT);
            runtime::credis_issued(&storage, cca, DAY, U256::ONE).unwrap();
            weights.push((cca, U256::ONE));
        }
    });
    provider.set_gas_limit(30_000_000);
    provider.enable_production_storage_gas_metering();
    StorageHandle::enter(&mut provider, |storage| {
        let pool = U256::from(population);
        let excess = reward(&storage, pool);
        let (expected, expected_excess) =
            calculate_proportional_distribution(pool, &weights).unwrap();
        assert_eq!(excess, expected_excess);
        assert_eq!(excess, U256::ZERO);
        for share in expected {
            assert_eq!(claimable(&storage, share.address), share.reward_amount);
        }
        let gas = storage.gas_used().unwrap();
        assert!(
            gas <= CCA_DISTRIBUTION_GAS_BUDGET,
            "CCA distribution used {gas} gas, budget is {CCA_DISTRIBUTION_GAS_BUDGET}"
        );
    });
}

#[test]
fn daily_buckets_isolate_delayed_settlement_and_cross_day_voids() {
    run(|storage| {
        let next = 20231116;
        bond(&storage, ALICE, BOND_REQUIREMENT);
        bond(&storage, BOB, BOND_REQUIREMENT);
        runtime::credis_issued(&storage, ALICE, DAY, U256::from(60)).unwrap();
        runtime::credis_issued(&storage, ALICE, DAY, U256::from(40)).unwrap();
        runtime::credis_issued(&storage, BOB, DAY, U256::from(100)).unwrap();
        storage
            .set_block_timestamp(U256::from(
                outbe_primitives::time::date_key_to_utc_timestamp(next).unwrap(),
            ))
            .unwrap();
        runtime::credis_issued(&storage, ALICE, next, U256::from(100)).unwrap();
        runtime::credis_issued(&storage, BOB, next, U256::from(150)).unwrap();
        runtime::credis_forfeited(&storage, ALICE, next, U256::from(50)).unwrap();
        assert_eq!(
            api::reward_weight(&storage, ALICE, DAY).unwrap(),
            U256::from(100)
        );
        assert_eq!(
            api::reward_weight(&storage, ALICE, next).unwrap(),
            U256::from(50)
        );
        let ctx = BlockRuntimeContext::new(BlockContext::default(), storage.clone());
        assert_eq!(
            distribute_daily(&ctx, DAY.into(), &[(PoolKind::Cca, U256::from(120))]).unwrap(),
            U256::ZERO
        );
        assert_eq!(claimable(&storage, ALICE), U256::from(60));
        assert_eq!(claimable(&storage, BOB), U256::from(60));
        assert_eq!(
            distribute_daily(&ctx, next.into(), &[(PoolKind::Cca, U256::from(120))]).unwrap(),
            U256::ZERO
        );
        assert_eq!(claimable(&storage, ALICE), U256::from(90));
        assert_eq!(claimable(&storage, BOB), U256::from(150));
        // Historical GRATIS does not carry into an empty day.
        assert_eq!(
            distribute_daily(&ctx, 20231117.into(), &[(PoolKind::Cca, U256::from(120))]).unwrap(),
            U256::from(120)
        );
        // A later void cannot claw back already accrued rewards.
        runtime::credis_forfeited(&storage, ALICE, next, U256::from(50)).unwrap();
        assert_eq!(claimable(&storage, ALICE), U256::from(90));
    });
}

#[test]
fn cca_pool_queries_and_claims_are_isolated() {
    use crate::precompile::{dispatch, IAgentReward};
    use alloy_sol_types::SolCall;
    run(|storage| {
        bond(&storage, ALICE, BOND_REQUIREMENT);
        bond(&storage, BOB, BOND_REQUIREMENT);
        runtime::credis_issued(&storage, ALICE, DAY, U256::ONE).unwrap();
        assert_eq!(reward(&storage, U256::from(1000)), U256::ZERO);
        let mut contract = AgentRewardContract::new(storage.clone());
        for (pool, amount) in [(RewardPool::Waa, 100), (RewardPool::Sra, 200)] {
            contract
                .add_claimable_reward(pool, ALICE, U256::from(amount))
                .unwrap();
        }
        let query = IAgentReward::getClaimableBalanceCall { account: ALICE };
        let output = dispatch(storage.clone(), &query.abi_encode(), BOB, U256::ZERO).unwrap();
        assert_eq!(
            IAgentReward::getClaimableBalanceCall::abi_decode_returns(&output).unwrap(),
            U256::from(1300)
        );
        for (pool, amount) in [(0, 100), (1, 200), (2, 1000)] {
            let query = IAgentReward::getPoolClaimableBalanceCall {
                account: ALICE,
                pool,
            };
            let output = dispatch(storage.clone(), &query.abi_encode(), BOB, U256::ZERO).unwrap();
            assert_eq!(
                IAgentReward::getPoolClaimableBalanceCall::abi_decode_returns(&output).unwrap(),
                U256::from(amount)
            );
        }
        assert_eq!(contract.get_claimable_reward(BOB).unwrap(), U256::ZERO);
        let claim = IAgentReward::claimRewardCall {
            pool: 2,
            promisLoadMinor: U256::ZERO,
            issuanceCurrency: USD,
            referenceCurrency: USD,
        }
        .abi_encode();
        // A registered caller cannot spend another account's rewards. Unregistered calls also fail.
        for caller in [BOB, Address::repeat_byte(3)] {
            assert!(dispatch(storage.clone(), &claim, caller, U256::ZERO).is_err());
        }
        assert!(dispatch(storage.clone(), &claim, ALICE, U256::ONE).is_err());
        assert_eq!(claimable(&storage, ALICE), U256::from(1000));
        for pool in [4, 255] {
            assert!(dispatch(
                storage.clone(),
                &IAgentReward::claimRewardCall {
                    pool,
                    promisLoadMinor: U256::ZERO,
                    issuanceCurrency: USD,
                    referenceCurrency: USD,
                }
                .abi_encode(),
                ALICE,
                U256::ZERO
            )
            .is_err());
            assert!(dispatch(
                storage.clone(),
                &IAgentReward::getPoolClaimableBalanceCall {
                    account: ALICE,
                    pool
                }
                .abi_encode(),
                ALICE,
                U256::ZERO
            )
            .is_err());
        }
        for pool in [0, 1] {
            let output = dispatch(
                storage.clone(),
                // An empty pool is a no-op whatever currencies the call names.
                &IAgentReward::claimRewardCall {
                    pool,
                    promisLoadMinor: U256::ZERO,
                    issuanceCurrency: 0,
                    referenceCurrency: 0,
                }
                .abi_encode(),
                BOB,
                U256::ZERO,
            )
            .unwrap();
            assert_eq!(
                IAgentReward::claimRewardCall::abi_decode_returns(&output).unwrap(),
                U256::ZERO
            );
        }
        let output = dispatch(storage.clone(), &claim, ALICE, U256::ZERO).unwrap();
        let id = IAgentReward::claimRewardCall::abi_decode_returns(&output).unwrap();
        assert!(outbe_gem::api::get_item(&storage, id).unwrap().is_some());
        assert_eq!(
            contract
                .get_pool_claimable_reward(RewardPool::Cca, ALICE)
                .unwrap(),
            U256::ZERO
        );
        assert_eq!(
            contract
                .get_pool_claimable_reward(RewardPool::Waa, ALICE)
                .unwrap(),
            U256::from(100)
        );
        assert_eq!(
            contract
                .get_pool_claimable_reward(RewardPool::Sra, ALICE)
                .unwrap(),
            U256::from(200)
        );
        assert_eq!(storage.balance(AGENT_REWARD_ADDRESS).unwrap(), U256::ZERO);
        contract
            .add_claimable_reward(RewardPool::Cca, ALICE, U256::MAX)
            .unwrap();
        assert!(contract.get_claimable_reward(ALICE).is_err());
    });
}

#[test]
fn deregistered_cca_claims_from_agentreward_after_withdrawing_bond() {
    run(|storage| {
        bond(&storage, ALICE, BOND_REQUIREMENT);
        runtime::credis_issued(&storage, ALICE, DAY, U256::ONE).unwrap();
        reward(&storage, U256::from(1000));
        runtime::unbond(storage.clone(), ALICE).unwrap();
        let now = NOW + UNBOND_COOLDOWN_SECONDS;
        storage.set_block_timestamp(U256::from(now)).unwrap();
        runtime::claim_unbonded(storage.clone(), ALICE).unwrap();
        assert_eq!(
            api::cca_state(&storage, ALICE).unwrap(),
            ICcaRegistry::State::Deregistered
        );
        assert_eq!(storage.balance(CCA_REGISTRY_ADDRESS).unwrap(), U256::ZERO);
        assert_eq!(storage.balance(AGENT_REWARD_ADDRESS).unwrap(), U256::ZERO);
        let index = outbe_oracle::api::coen_pair_index_opt(storage.clone(), 840)
            .unwrap()
            .unwrap();
        outbe_oracle::schema::OracleContract::new(storage.clone())
            .record_utc_day_vwap(
                previous_date_key(timestamp_to_date_key(now)).expect("date step"),
                index,
                ONE_COEN,
            )
            .unwrap();
        AgentRewardContract::new(storage.clone())
            .claim_reward(RewardPool::Cca, ALICE, U256::ZERO, (USD, USD))
            .unwrap();
        assert_eq!(claimable(&storage, ALICE), U256::ZERO);
        assert_eq!(storage.balance(AGENT_REWARD_ADDRESS).unwrap(), U256::ZERO);
        assert_eq!(storage.balance(ALICE).unwrap(), BOND_REQUIREMENT);
        assert_eq!(
            gem_of(&storage, ALICE).item.promis_load_minor,
            U256::from(1000)
        );
    });
}

const CAROL: Address = Address::repeat_byte(3);

fn originate(storage: &StorageHandle<'_>, who: Address, weight: u64) {
    bond(storage, who, BOND_REQUIREMENT);
    runtime::credis_issued(storage, who, DAY, U256::from(weight)).unwrap();
}

#[test]
fn one_eligible_cca_receives_the_whole_pool() {
    run(|storage| {
        originate(&storage, ALICE, 1);
        assert_eq!(reward(&storage, U256::from(100)), U256::ZERO);
        assert_eq!(claimable(&storage, ALICE), U256::from(100));
        assert_eq!(storage.balance(AGENT_REWARD_ADDRESS).unwrap(), U256::ZERO);
        assert_eq!(
            storage.balance(CCA_REGISTRY_ADDRESS).unwrap(),
            BOND_REQUIREMENT
        );
    });
}

#[test]
fn weights_eighty_and_twenty_split_a_hundred_with_no_excess() {
    run(|storage| {
        originate(&storage, ALICE, 80);
        originate(&storage, BOB, 20);
        assert_eq!(reward(&storage, U256::from(100)), U256::ZERO);
        assert_eq!(claimable(&storage, ALICE), U256::from(80));
        assert_eq!(claimable(&storage, BOB), U256::from(20));
        assert_eq!(storage.balance(AGENT_REWARD_ADDRESS).unwrap(), U256::ZERO);
        assert_eq!(
            storage.balance(CCA_REGISTRY_ADDRESS).unwrap(),
            BOND_REQUIREMENT * U256::from(2)
        );
    });
}

#[test]
fn concentrated_weights_are_not_capped() {
    run(|storage| {
        originate(&storage, ALICE, 90);
        originate(&storage, BOB, 10);
        assert_eq!(reward(&storage, U256::from(1000)), U256::ZERO);
        assert_eq!(claimable(&storage, ALICE), U256::from(900));
        assert_eq!(claimable(&storage, BOB), U256::from(100));
    });
}

#[test]
fn equal_weights_leave_indivisible_residue_as_excess() {
    run(|storage| {
        originate(&storage, ALICE, 1);
        originate(&storage, BOB, 1);
        originate(&storage, CAROL, 1);
        assert_eq!(reward(&storage, U256::from(10)), U256::ONE);
        assert_eq!(claimable(&storage, ALICE), U256::from(3));
        assert_eq!(claimable(&storage, BOB), U256::from(3));
        assert_eq!(claimable(&storage, CAROL), U256::from(3));
        assert_eq!(storage.balance(AGENT_REWARD_ADDRESS).unwrap(), U256::ZERO);
    });
}

#[test]
fn zero_cca_pool_credits_nothing() {
    run(|storage| {
        originate(&storage, ALICE, 80);
        assert_eq!(reward(&storage, U256::ZERO), U256::ZERO);
        assert_eq!(claimable(&storage, ALICE), U256::ZERO);
        assert_eq!(storage.balance(AGENT_REWARD_ADDRESS).unwrap(), U256::ZERO);
        assert_eq!(
            storage.balance(CCA_REGISTRY_ADDRESS).unwrap(),
            BOND_REQUIREMENT
        );
    });
}

#[test]
fn a_second_claim_does_not_mint_or_move_the_bond() {
    run(|storage| {
        originate(&storage, ALICE, 1);
        assert_eq!(reward(&storage, U256::from(100)), U256::ZERO);
        let gem_id = AgentRewardContract::new(storage.clone())
            .claim_reward(RewardPool::Cca, ALICE, U256::ZERO, (USD, USD))
            .unwrap();
        assert!(AgentRewardContract::new(storage.clone())
            .claim_reward(RewardPool::Cca, ALICE, U256::ZERO, (USD, USD))
            .is_err());
        assert_eq!(claimable(&storage, ALICE), U256::ZERO);
        assert_eq!(storage.balance(AGENT_REWARD_ADDRESS).unwrap(), U256::ZERO);
        assert_eq!(storage.balance(ALICE).unwrap(), U256::ZERO);
        assert_eq!(
            storage.balance(CCA_REGISTRY_ADDRESS).unwrap(),
            BOND_REQUIREMENT
        );
        assert_eq!(
            outbe_gem::api::list_by_owner(&storage, ALICE)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            outbe_gem::api::get_item(&storage, gem_id)
                .unwrap()
                .unwrap()
                .promis_load_minor,
            U256::from(100)
        );
    });
}
