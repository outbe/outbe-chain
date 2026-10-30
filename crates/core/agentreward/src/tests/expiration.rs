use super::*;
use crate::{constants::REWARD_BALANCE_IDLE_SECONDS, errors::AgentRewardError};
use outbe_promislimit::PromisLimitContract;

const ALICE: Address = Address::repeat_byte(1);
const BOB: Address = Address::repeat_byte(2);
const POOLS: [RewardPool; 4] = [
    RewardPool::Waa,
    RewardPool::Sra,
    RewardPool::Cca,
    RewardPool::Validator,
];

fn assert_error<T: std::fmt::Debug>(
    result: outbe_primitives::error::Result<T>,
    expected: AgentRewardError,
) {
    assert_eq!(
        format!("{:?}", result.unwrap_err()),
        format!(
            "{:?}",
            outbe_primitives::error::PrecompileError::from(expected)
        )
    );
}

#[test]
fn deadlines_follow_positive_credits_and_are_independent() {
    with_contract_mut(|storage, contract| {
        for pool in POOLS {
            contract
                .add_claimable_reward(pool, ALICE, U256::ZERO)
                .unwrap();
            assert_eq!(contract.get_pool_balance_deadline(pool, ALICE).unwrap(), 0);
            contract
                .add_claimable_reward(pool, ALICE, U256::from(10))
                .unwrap();
            assert_eq!(
                contract.get_pool_balance_deadline(pool, ALICE).unwrap(),
                T_NOW + REWARD_BALANCE_IDLE_SECONDS
            );
        }
        storage.set_block_timestamp(U256::from(T_NOW + 10)).unwrap();
        for pool in POOLS {
            contract
                .add_claimable_reward(pool, ALICE, U256::ONE)
                .unwrap();
            contract.add_claimable_reward(pool, BOB, U256::ONE).unwrap();
            assert_eq!(
                contract.get_pool_balance_deadline(pool, ALICE).unwrap(),
                T_NOW + REWARD_BALANCE_IDLE_SECONDS
            );
            assert_eq!(
                contract.get_pool_balance_deadline(pool, BOB).unwrap(),
                T_NOW + 10 + REWARD_BALANCE_IDLE_SECONDS
            );
        }
    });
}

#[test]
fn gap_ed32_idle_reward_balance_goes_to_promis_limit() {
    for pool in POOLS {
        with_contract_mut(|storage, contract| {
            let load = U256::from(100);
            contract
                .add_claimable_reward(pool, Address::ZERO, load)
                .unwrap();
            contract.add_claimable_reward(pool, BOB, U256::ONE).unwrap();
            contract
                .pending_total_minor
                .write(&(pool as u8), U256::from(77))
                .unwrap();
            let deadline = contract
                .get_pool_balance_deadline(pool, Address::ZERO)
                .unwrap();
            storage
                .set_block_timestamp(U256::from(deadline - 1))
                .unwrap();
            assert_error(
                contract.return_expired_reward(pool, Address::ZERO),
                AgentRewardError::RewardBalanceNotExpired(deadline),
            );
            storage.set_block_timestamp(U256::from(deadline)).unwrap();
            // Credits to an expired aggregate balance retain its deadline.
            contract
                .add_claimable_reward(pool, Address::ZERO, U256::ONE)
                .unwrap();
            assert_eq!(
                contract
                    .get_pool_balance_deadline(pool, Address::ZERO)
                    .unwrap(),
                deadline
            );
            let promis = PromisLimitContract::new(storage.clone());
            assert_eq!(
                contract.return_expired_reward(pool, Address::ZERO).unwrap(),
                load + U256::ONE
            );
            assert_eq!(promis.promis_limit().unwrap(), load + U256::ONE);
            assert_eq!(contract.pool_claimable_total(pool).unwrap(), U256::ONE);
            assert_eq!(contract.pool_pending_total(pool).unwrap(), U256::from(77));
            assert_eq!(
                contract
                    .get_pool_balance_deadline(pool, Address::ZERO)
                    .unwrap(),
                0
            );
            assert_eq!(
                contract.return_expired_reward(pool, Address::ZERO).unwrap(),
                U256::ZERO
            );
            assert_eq!(promis.promis_limit().unwrap(), load + U256::ONE);
            assert_eq!(storage.balance(Address::ZERO).unwrap(), U256::ZERO);
            contract
                .add_claimable_reward(pool, Address::ZERO, U256::ONE)
                .unwrap();
            assert_eq!(
                contract
                    .get_pool_balance_deadline(pool, Address::ZERO)
                    .unwrap(),
                deadline + REWARD_BALANCE_IDLE_SECONDS
            );
            assert_error(
                contract.return_expired_reward(pool, Address::ZERO),
                AgentRewardError::RewardBalanceNotExpired(deadline + REWARD_BALANCE_IDLE_SECONDS),
            );
        });
    }
}

#[test]
fn claims_reject_exact_and_past_deadlines_without_issuing_gems() {
    for pool in POOLS {
        for offset in [0, 1] {
            with_contract_mut(|storage, contract| {
                if pool == RewardPool::Cca {
                    let bond = outbe_ccaregistry::constants::BOND_REQUIREMENT;
                    storage
                        .increase_balance(outbe_primitives::addresses::CCA_REGISTRY_ADDRESS, bond)
                        .unwrap();
                    outbe_ccaregistry::runtime::bond(
                        storage.clone(),
                        ALICE,
                        bond,
                        "Test CCA".into(),
                    )
                    .unwrap();
                }
                contract
                    .add_claimable_reward(pool, ALICE, U256::from(10))
                    .unwrap();
                let deadline = contract.get_pool_balance_deadline(pool, ALICE).unwrap();
                storage
                    .set_block_timestamp(U256::from(deadline + offset))
                    .unwrap();
                assert_error(
                    contract.claim_reward(pool, ALICE, U256::ZERO, (USD, USD)),
                    AgentRewardError::ClaimDeadlinePassed(deadline),
                );
                assert_eq!(
                    contract.get_pool_claimable_reward(pool, ALICE).unwrap(),
                    U256::from(10)
                );
                assert_eq!(
                    contract.get_pool_balance_deadline(pool, ALICE).unwrap(),
                    deadline
                );
                assert!(outbe_gem::api::list_by_owner(&storage, ALICE)
                    .unwrap()
                    .is_empty());
            });
        }
    }
}

#[test]
fn tiny_partial_claim_renews_only_its_pool_and_full_claim_clears_deadline() {
    for pool in POOLS {
        with_contract_mut(|storage, contract| {
            if pool == RewardPool::Cca {
                let bond = outbe_ccaregistry::constants::BOND_REQUIREMENT;
                storage
                    .increase_balance(outbe_primitives::addresses::CCA_REGISTRY_ADDRESS, bond)
                    .unwrap();
                outbe_ccaregistry::runtime::bond(storage.clone(), ALICE, bond, "Test CCA".into())
                    .unwrap();
            }
            for credited_pool in POOLS {
                contract
                    .add_claimable_reward(credited_pool, ALICE, U256::from(10))
                    .unwrap();
            }
            let deadline = T_NOW + REWARD_BALANCE_IDLE_SECONDS;
            storage
                .set_block_timestamp(U256::from(deadline - 1))
                .unwrap();
            seed_oracle(&storage, ONE_COEN);
            let index = outbe_oracle::api::coen_pair_index_opt(storage.clone(), USD)
                .unwrap()
                .unwrap();
            outbe_oracle::schema::OracleContract::new(storage.clone())
                .record_utc_day_vwap(
                    previous_date_key(timestamp_to_date_key(deadline - 1)).unwrap(),
                    index,
                    ONE_COEN,
                )
                .unwrap();
            contract
                .claim_reward(pool, ALICE, U256::ONE, (USD, USD))
                .unwrap();
            assert_eq!(
                contract.get_pool_claimable_reward(pool, ALICE).unwrap(),
                U256::from(9)
            );
            for other_pool in POOLS {
                let expected = if other_pool == pool {
                    deadline - 1 + REWARD_BALANCE_IDLE_SECONDS
                } else {
                    deadline
                };
                assert_eq!(
                    contract
                        .get_pool_balance_deadline(other_pool, ALICE)
                        .unwrap(),
                    expected
                );
            }
            contract
                .claim_reward(pool, ALICE, U256::ZERO, (USD, USD))
                .unwrap();
            assert_eq!(contract.get_pool_balance_deadline(pool, ALICE).unwrap(), 0);
            assert_eq!(
                contract
                    .reward_balance_deadlines
                    .read(&AgentRewardContract::reward_balance_key(pool, ALICE))
                    .unwrap(),
                0
            );
        });
    }
}

#[test]
fn failures_preserve_deadlines_and_return_accounting() {
    for corrupt_total in [false, true] {
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        provider.set_timestamp(U256::from(T_NOW));
        StorageHandle::enter(&mut provider, |storage| {
            let mut contract = AgentRewardContract::new(storage.clone());
            contract
                .add_claimable_reward(RewardPool::Waa, ALICE, U256::from(10))
                .unwrap();
            let deadline = contract
                .get_pool_balance_deadline(RewardPool::Waa, ALICE)
                .unwrap();
            // A failed Gem issuance must not renew the remaining reward.
            storage.set_block_timestamp(U256::from(T_NOW + 10)).unwrap();
            assert!(contract
                .claim_reward(RewardPool::Waa, ALICE, U256::ONE, (USD, USD))
                .is_err());
            assert_eq!(
                contract
                    .get_pool_balance_deadline(RewardPool::Waa, ALICE)
                    .unwrap(),
                deadline
            );
            if corrupt_total {
                contract
                    .claimable_total_minor
                    .write(&(RewardPool::Waa as u8), U256::ZERO)
                    .unwrap();
            } else {
                PromisLimitContract::new(storage.clone())
                    .checked_credit(U256::MAX)
                    .unwrap();
            }
            storage
                .set_block_timestamp(U256::from(deadline + 1))
                .unwrap();
        });
        let before = provider.storage.clone();
        StorageHandle::enter(&mut provider, |storage| {
            assert!(AgentRewardContract::new(storage)
                .return_expired_reward(RewardPool::Waa, ALICE)
                .is_err());
        });
        assert_eq!(provider.storage, before);
        assert!(provider
            .get_events(outbe_primitives::addresses::AGENT_REWARD_ADDRESS)
            .is_empty());
    }
}

#[test]
fn missing_deadlines_and_time_overflow_fail_without_partial_credits() {
    with_contract_mut(|storage, contract| {
        let pool = RewardPool::Waa;
        contract
            .add_claimable_reward(pool, ALICE, U256::ONE)
            .unwrap();
        contract
            .reward_balance_deadlines
            .write(&AgentRewardContract::reward_balance_key(pool, ALICE), 0)
            .unwrap();
        assert_error(
            contract.get_pool_balance_deadline(pool, ALICE),
            AgentRewardError::RewardDeadlineMissing,
        );
        assert_error(
            contract.add_claimable_reward(pool, ALICE, U256::ONE),
            AgentRewardError::RewardDeadlineMissing,
        );
        assert_error(
            contract.return_expired_reward(pool, ALICE),
            AgentRewardError::RewardDeadlineMissing,
        );
        assert_error(
            contract.claim_reward(pool, ALICE, U256::ONE, (USD, USD)),
            AgentRewardError::RewardDeadlineMissing,
        );
        storage.set_block_timestamp(U256::from(u64::MAX)).unwrap();
        assert_error(
            contract.add_claimable_reward(pool, BOB, U256::ONE),
            AgentRewardError::RewardDeadlineOverflow,
        );
        storage.set_block_timestamp(U256::MAX).unwrap();
        assert!(contract.add_claimable_reward(pool, BOB, U256::ONE).is_err());
        assert_eq!(contract.get_pool_balance_deadline(pool, BOB).unwrap(), 0);
        assert_eq!(
            contract.get_pool_claimable_reward(pool, BOB).unwrap(),
            U256::ZERO
        );
        assert_eq!(contract.pool_claimable_total(pool).unwrap(), U256::ONE);
    });
}

#[test]
fn zero_address_waa_and_sra_distribution_establishes_deadlines() {
    with_contract_mut(|storage, contract| {
        let day = WorldwideDay::new(timestamp_to_date_key(T_NOW));
        contract.increment_waa_tribute(day, Address::ZERO).unwrap();
        contract.increment_sra_tribute(day, Address::ZERO).unwrap();
        let ctx = outbe_primitives::block::BlockRuntimeContext::new(
            outbe_primitives::block::BlockContext::empty_for_tests(1, T_NOW, CHAIN_ID),
            storage,
        );
        distribute_and_credit(
            &ctx,
            day,
            &[
                (PoolKind::Waa, U256::from(10)),
                (PoolKind::Sra, U256::from(20)),
            ],
        )
        .unwrap();
        for pool in [RewardPool::Waa, RewardPool::Sra] {
            assert_eq!(
                contract
                    .get_pool_balance_deadline(pool, Address::ZERO)
                    .unwrap(),
                T_NOW + REWARD_BALANCE_IDLE_SECONDS
            );
        }
        assert_eq!(
            contract.get_claimable_reward(Address::ZERO).unwrap(),
            U256::from(30)
        );
    });
}

#[test]
fn a_partial_claim_with_deadline_overflow_preserves_the_balance() {
    with_contract_mut(|storage, contract| {
        storage
            .set_block_timestamp(U256::from(u64::MAX - REWARD_BALANCE_IDLE_SECONDS))
            .unwrap();
        contract
            .add_claimable_reward(RewardPool::Waa, ALICE, U256::from(10))
            .unwrap();
        storage
            .set_block_timestamp(U256::from(u64::MAX - 1))
            .unwrap();
        assert_error(
            contract.claim_reward(RewardPool::Waa, ALICE, U256::ONE, (USD, USD)),
            AgentRewardError::RewardDeadlineOverflow,
        );
        assert_eq!(
            contract
                .get_pool_claimable_reward(RewardPool::Waa, ALICE)
                .unwrap(),
            U256::from(10)
        );
        assert_eq!(
            contract
                .get_pool_balance_deadline(RewardPool::Waa, ALICE)
                .unwrap(),
            u64::MAX
        );
        assert_eq!(
            contract.pool_claimable_total(RewardPool::Waa).unwrap(),
            U256::from(10)
        );
    });
}
