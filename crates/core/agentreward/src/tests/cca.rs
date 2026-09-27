//! CCA reward distribution and claims owned by AgentReward.
use super::*;
use outbe_ccaregistry::{
    api,
    constants::{BOND_REQUIREMENT, UNBOND_COOLDOWN_SECONDS},
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
fn capped_rewards_exclude_inactive_weights_and_recycle_dust() {
    run(|storage| {
        assert_eq!(reward(&storage, U256::from(11)), U256::from(11));
        bond(&storage, ALICE, BOND_REQUIREMENT);
        bond(&storage, BOB, BOND_REQUIREMENT);
        assert_eq!(reward(&storage, U256::from(11)), U256::from(11));
        runtime::position_opened(&storage, ALICE, DAY, U256::from(1)).unwrap();
        runtime::position_opened(&storage, BOB, DAY, U256::from(3)).unwrap();
        assert_eq!(reward(&storage, U256::from(11)), U256::from(5));
        assert_eq!(claimable(&storage, ALICE), native(3));
        assert_eq!(claimable(&storage, BOB), native(3));
        runtime::unbond(storage.clone(), BOB).unwrap();
        assert_eq!(reward(&storage, U256::from(11)), U256::from(8));
        assert_eq!(claimable(&storage, ALICE), native(6));
        AgentRewardContract::new(storage.clone())
            .claim_reward(RewardPool::Cca, BOB, U256::ZERO)
            .unwrap();
        assert_eq!(storage.balance(BOB).unwrap(), U256::ZERO);
        assert_eq!(
            storage.balance(CCA_REGISTRY_ADDRESS).unwrap(),
            BOND_REQUIREMENT * U256::from(2)
        );
        assert_eq!(storage.balance(AGENT_REWARD_ADDRESS).unwrap(), native(6));
    });
}

#[test]
fn wide_reward_products_do_not_overflow_and_conversion_failure_rolls_back() {
    run(|storage| {
        bond(&storage, ALICE, BOND_REQUIREMENT);
        runtime::position_opened(&storage, ALICE, DAY, U256::MAX).unwrap();
        assert_eq!(reward(&storage, U256::from(100)), U256::from(68));
        let ctx = BlockRuntimeContext::new(BlockContext::default(), storage.clone());
        let before = storage.balance(AGENT_REWARD_ADDRESS).unwrap();
        assert!(distribute_daily(&ctx, 20231115.into(), &[(PoolKind::Cca, U256::MAX)]).is_err());
        assert_eq!(storage.balance(AGENT_REWARD_ADDRESS).unwrap(), before);
        assert_eq!(claimable(&storage, ALICE), native(32));
        assert!(runtime::position_opened(&storage, ALICE, DAY, U256::ONE).is_err());
        assert_eq!(api::reward_weight(&storage, ALICE, DAY).unwrap(), U256::MAX);
    });
}

#[test]
fn reward_map_overflow_rolls_back_earlier_credits_and_minting() {
    run(|storage| {
        for cca in [ALICE, BOB] {
            bond(&storage, cca, BOND_REQUIREMENT);
            runtime::position_opened(&storage, cca, DAY, U256::ONE).unwrap();
        }
        let contract = AgentRewardContract::new(storage.clone());
        let active = [ALICE, BOB];
        // Fail on the second recipient, after the first credit and mint.
        contract
            .cca_claimable_rewards
            .write(&active[1], U256::MAX)
            .unwrap();
        let balance = storage.balance(AGENT_REWARD_ADDRESS).unwrap();
        let ctx = BlockRuntimeContext::new(BlockContext::default(), storage.clone());
        assert!(distribute_daily(&ctx, DAY.into(), &[(PoolKind::Cca, U256::from(10))]).is_err());
        assert_eq!(
            contract.cca_claimable_rewards.read(&active[0]).unwrap(),
            U256::ZERO
        );
        assert_eq!(
            contract.cca_claimable_rewards.read(&active[1]).unwrap(),
            U256::MAX
        );
        assert_eq!(storage.balance(AGENT_REWARD_ADDRESS).unwrap(), balance);
    });
}

#[test]
fn daily_distribution_fits_a_representative_active_population() {
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        for id in 1..=128u64 {
            let cca = Address::from_word(U256::from(id).into());
            bond(&storage, cca, BOND_REQUIREMENT);
            runtime::position_opened(&storage, cca, DAY, U256::ONE).unwrap();
        }
    });
    provider.set_gas_limit(30_000_000);
    provider.enable_production_storage_gas_metering();
    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(reward(&storage, U256::from(128)), U256::ZERO);
        assert!(storage.gas_used().unwrap() < 30_000_000);
    });
}

#[test]
fn daily_buckets_isolate_delayed_settlement_and_cross_day_voids() {
    run(|storage| {
        let next = 20231116;
        bond(&storage, ALICE, BOND_REQUIREMENT);
        bond(&storage, BOB, BOND_REQUIREMENT);
        runtime::position_opened(&storage, ALICE, DAY, U256::from(60)).unwrap();
        runtime::position_opened(&storage, ALICE, DAY, U256::from(40)).unwrap();
        runtime::position_opened(&storage, BOB, DAY, U256::from(100)).unwrap();
        storage
            .set_block_timestamp(U256::from(
                outbe_primitives::time::date_key_to_utc_timestamp(next),
            ))
            .unwrap();
        runtime::position_opened(&storage, ALICE, next, U256::from(100)).unwrap();
        runtime::position_opened(&storage, BOB, next, U256::from(150)).unwrap();
        runtime::position_voided(&storage, ALICE, next, U256::from(50)).unwrap();
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
            U256::from(44)
        );
        assert_eq!(claimable(&storage, ALICE), native(38));
        assert_eq!(claimable(&storage, BOB), native(38));
        assert_eq!(
            distribute_daily(&ctx, next.into(), &[(PoolKind::Cca, U256::from(120))]).unwrap(),
            U256::from(44)
        );
        assert_eq!(claimable(&storage, ALICE), native(76));
        assert_eq!(claimable(&storage, BOB), native(76));
        // Historical GRATIS does not carry into an empty day.
        assert_eq!(
            distribute_daily(&ctx, 20231117.into(), &[(PoolKind::Cca, U256::from(120))]).unwrap(),
            U256::from(120)
        );
        // A later void cannot claw back already accrued rewards.
        runtime::position_voided(&storage, ALICE, next, U256::from(50)).unwrap();
        assert_eq!(claimable(&storage, ALICE), native(76));
    });
}

#[test]
fn cca_pool_queries_and_claims_are_isolated() {
    use crate::precompile::{dispatch, IAgentReward};
    use alloy_sol_types::SolCall;
    run(|storage| {
        bond(&storage, ALICE, BOND_REQUIREMENT);
        bond(&storage, BOB, BOND_REQUIREMENT);
        runtime::position_opened(&storage, ALICE, DAY, U256::ONE).unwrap();
        assert_eq!(reward(&storage, U256::from(1000)), U256::from(680));
        let mut contract = AgentRewardContract::new(storage.clone());
        for (pool, amount) in [(RewardPool::Waa, 100), (RewardPool::Sra, 200)] {
            contract
                .add_claimable_reward(pool, ALICE, native(amount))
                .unwrap();
            storage
                .increase_balance(AGENT_REWARD_ADDRESS, native(amount))
                .unwrap();
        }
        let query = IAgentReward::getClaimableBalanceCall { account: ALICE };
        let output = dispatch(storage.clone(), &query.abi_encode(), BOB, U256::ZERO).unwrap();
        assert_eq!(
            IAgentReward::getClaimableBalanceCall::abi_decode_returns(&output).unwrap(),
            native(620)
        );
        for (pool, amount) in [(0, 100), (1, 200), (2, 320)] {
            let query = IAgentReward::getPoolClaimableBalanceCall {
                account: ALICE,
                pool,
            };
            let output = dispatch(storage.clone(), &query.abi_encode(), BOB, U256::ZERO).unwrap();
            assert_eq!(
                IAgentReward::getPoolClaimableBalanceCall::abi_decode_returns(&output).unwrap(),
                native(amount)
            );
        }
        assert_eq!(contract.get_claimable_reward(BOB).unwrap(), U256::ZERO);
        let claim = IAgentReward::claimRewardCall {
            pool: 2,
            amount: U256::ZERO,
        }
        .abi_encode();
        // A registered caller cannot spend another account's rewards; unregistered calls also fail.
        for caller in [BOB, Address::repeat_byte(3)] {
            assert!(dispatch(storage.clone(), &claim, caller, U256::ZERO).is_err());
        }
        assert!(dispatch(storage.clone(), &claim, ALICE, U256::ONE).is_err());
        assert_eq!(claimable(&storage, ALICE), native(320));
        for pool in [3, 255] {
            assert!(dispatch(
                storage.clone(),
                &IAgentReward::claimRewardCall {
                    pool,
                    amount: U256::ZERO
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
                &IAgentReward::claimRewardCall {
                    pool,
                    amount: U256::ZERO,
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
        assert_eq!(
            outbe_gem::api::get_gem(&storage, id)
                .unwrap()
                .unwrap()
                .gem_type,
            GemTypes::Cca as u8
        );
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
            native(100)
        );
        assert_eq!(
            contract
                .get_pool_claimable_reward(RewardPool::Sra, ALICE)
                .unwrap(),
            native(200)
        );
        assert_eq!(storage.balance(AGENT_REWARD_ADDRESS).unwrap(), native(300));
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
        runtime::position_opened(&storage, ALICE, DAY, U256::ONE).unwrap();
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
        assert_eq!(storage.balance(AGENT_REWARD_ADDRESS).unwrap(), native(320));
        let index = outbe_oracle::api::coen_pair_index_opt(storage.clone(), 840)
            .unwrap()
            .unwrap();
        outbe_oracle::schema::OracleContract::new(storage.clone())
            .utc_day_vwap_value
            .get_nested(&previous_date_key(timestamp_to_date_key(now)))
            .write(&index, ONE_COEN)
            .unwrap();
        AgentRewardContract::new(storage.clone())
            .claim_reward(RewardPool::Cca, ALICE, U256::ZERO)
            .unwrap();
        assert_eq!(claimable(&storage, ALICE), U256::ZERO);
        assert_eq!(storage.balance(AGENT_REWARD_ADDRESS).unwrap(), U256::ZERO);
        assert_eq!(storage.balance(ALICE).unwrap(), BOND_REQUIREMENT);
        assert_eq!(gem_of(&storage, ALICE).promis_load_minor, U256::from(320));
    });
}
