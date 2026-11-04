//! Historical CCA eligibility and bounded four-pool settlement regressions.
use super::*;
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};

const DAY: WorldwideDay = WorldwideDay::new(20231113);

fn ctx<'a>(storage: StorageHandle<'a>, height: u64) -> BlockRuntimeContext<'a> {
    BlockRuntimeContext::new(
        BlockContext {
            block_number: height,
            timestamp: 2_000_000_000,
            chain_id: CHAIN_ID,
            genesis_hash: alloy_primitives::B256::ZERO,
            genesis_timestamp: 2_000_000_000,
            proposer: alloy_primitives::Address::ZERO,
            validators: Vec::new(),
        },
        storage,
    )
}

fn originate(storage: &StorageHandle<'_>, address: Address, day: WorldwideDay, weight: U256) {
    let bond = outbe_ccaregistry::constants::BOND_REQUIREMENT;
    storage
        .increase_balance(outbe_primitives::addresses::CCA_REGISTRY_ADDRESS, bond)
        .unwrap();
    outbe_ccaregistry::runtime::bond(storage.clone(), address, bond, "CCA".into()).unwrap();
    outbe_ccaregistry::api::credis_issued(storage, address, day.value(), weight).unwrap();
}

fn voters(storage: &StorageHandle<'_>, day: WorldwideDay, count: u32) {
    let rewards = outbe_rewards::schema::Rewards::new(storage.clone());
    rewards.day_voter_count.write(&day.value(), count).unwrap();
    rewards
        .day_total_votes
        .write(&day.value(), U256::from(count))
        .unwrap();
    for n in 0..count {
        let address = numbered_address(100_000 + u64::from(n));
        rewards
            .day_voter_at
            .get_nested(&day.value())
            .write(&n, address)
            .unwrap();
        rewards
            .day_votes
            .get_nested(&day.value())
            .write(&address, 1)
            .unwrap();
    }
}

#[test]
fn gap_ed27_cca_that_left_gets_its_share() {
    with_contract_mut(|storage, contract| {
        seed_oracle(&storage, ONE_COEN);
        let alice = numbered_address(1);
        let bob = numbered_address(2);
        originate(&storage, alice, DAY, U256::from(80));
        originate(&storage, bob, DAY, U256::from(20));
        outbe_ccaregistry::runtime::unbond(storage.clone(), alice).unwrap();
        let context = ctx(storage.clone(), 0);
        allocate_fixture(&context, DAY, &[(PoolKind::Cca, U256::from(100))]).unwrap();
        assert_eq!(contract.get_claimable_reward(alice).unwrap(), U256::ZERO);
        crate::api::process_agent_rewards(&context).unwrap();
        assert_eq!(
            contract
                .get_pool_claimable_reward(RewardPool::Cca, alice)
                .unwrap(),
            U256::from(80)
        );
        assert_eq!(
            contract
                .get_pool_claimable_reward(RewardPool::Cca, bob)
                .unwrap(),
            U256::from(20)
        );
        contract
            .claim_reward(RewardPool::Cca, alice, U256::ZERO, (USD, USD))
            .unwrap();
        assert_eq!(
            gem_of(&storage, alice).item.promis_load_minor,
            U256::from(80)
        );
        assert_eq!(
            storage
                .balance(outbe_primitives::addresses::AGENT_REWARD_ADDRESS)
                .unwrap(),
            U256::ZERO
        );
    });
}

#[test]
fn gap_ed26_reward_jobs_pay_256_receivers_per_block() {
    with_contract_mut(|storage, contract| {
        voters(&storage, DAY, 100);
        for n in 0..100 {
            contract
                .increment_waa_tribute(DAY, numbered_address(1_000 + n))
                .unwrap();
            contract
                .increment_sra_tribute(DAY, numbered_address(2_000 + n))
                .unwrap();
            originate(&storage, numbered_address(3_000 + n), DAY, U256::ONE);
        }
        let context = ctx(storage.clone(), 0);
        let pool = U256::from(100);
        allocate_fixture(
            &context,
            DAY,
            &[
                (PoolKind::Validator, pool),
                (PoolKind::Waa, pool),
                (PoolKind::Sra, pool),
                (PoolKind::Cca, pool),
            ],
        )
        .unwrap();
        assert_eq!(contract.accrual_tail.read().unwrap(), 4);
        assert_eq!(
            contract
                .get_claimable_reward(numbered_address(1_000))
                .unwrap(),
            U256::ZERO
        );
        crate::api::process_agent_rewards(&context).unwrap();
        assert_eq!(
            contract
                .pool_claimable_total(RewardPool::Validator)
                .unwrap(),
            pool
        );
        assert_eq!(
            contract.pool_claimable_total(RewardPool::Waa).unwrap(),
            pool
        );
        assert_eq!(
            contract.pool_claimable_total(RewardPool::Sra).unwrap(),
            U256::from(56)
        );
        assert_eq!(
            contract.pool_claimable_total(RewardPool::Cca).unwrap(),
            U256::ZERO
        );
        crate::api::process_agent_rewards(&context).unwrap();
        assert_eq!(
            contract.pool_claimable_total(RewardPool::Sra).unwrap(),
            U256::from(56)
        );
        assert!(crate::distribution::has_pending_accruals_for_day(&storage, DAY.value()).unwrap());
        crate::api::process_agent_rewards(&ctx(storage.clone(), 1)).unwrap();
        assert_eq!(
            contract.pool_claimable_total(RewardPool::Sra).unwrap(),
            pool
        );
        assert_eq!(
            contract.pool_claimable_total(RewardPool::Cca).unwrap(),
            pool
        );
        assert!(!crate::distribution::has_pending_accruals_for_day(&storage, DAY.value()).unwrap());
    });
}

/// Asserts that the Rewards record, the WAA and SRA day lists and the CCA day
/// list of `day` are deleted.
fn assert_day_records_deleted(context: &BlockRuntimeContext<'_>, day: WorldwideDay) {
    let rewards = outbe_rewards::schema::Rewards::new(context.storage.clone());
    assert_eq!(rewards.day_voter_count.read(&day.value()).unwrap(), 0);
    assert_eq!(
        rewards.day_total_votes.read(&day.value()).unwrap(),
        U256::ZERO
    );
    assert_eq!(
        rewards.day_fee_sum_native.read(&day.value()).unwrap(),
        U256::ZERO
    );
    assert!(outbe_rewards::api::get_day_voters(context, day.value())
        .unwrap()
        .is_empty());
    let contract = AgentRewardContract::new(context.storage.clone());
    assert_eq!(contract.waa_address_count.read(&day).unwrap(), 0);
    assert_eq!(contract.sra_address_count.read(&day).unwrap(), 0);
    assert_eq!(
        outbe_ccaregistry::api::reward_day_inputs(&context.storage, day.value()).unwrap(),
        (0, U256::ZERO)
    );
}

#[test]
fn gap_ed28_day_records_deleted_after_their_job() {
    with_contract_mut(|storage, contract| {
        let failed_day = WorldwideDay::new(20231114);
        let later_day = WorldwideDay::new(20231115);
        let rewards = outbe_rewards::schema::Rewards::new(storage.clone());
        for (day, first) in [(DAY, 1), (failed_day, 10)] {
            voters(&storage, day, 3);
            rewards
                .day_fee_sum_native
                .write(&day.value(), U256::from(7))
                .unwrap();
            for n in first..first + 3 {
                contract
                    .increment_waa_tribute(day, numbered_address(n))
                    .unwrap();
                contract
                    .increment_sra_tribute(day, numbered_address(n))
                    .unwrap();
                originate(&storage, numbered_address(n), day, U256::ONE);
            }
        }

        // The votes of the last block of DAY are sealed at block 50.
        rewards
            .day_votes_sealed_at_block
            .write(&DAY.value(), 50)
            .unwrap();
        assert!(
            !outbe_rewards::api::is_voting_complete(&ctx(storage.clone(), 49), DAY.value())
                .unwrap()
        );
        assert!(
            !outbe_rewards::api::is_voting_complete(&ctx(storage.clone(), 50), DAY.value())
                .unwrap()
        );
        assert!(
            outbe_rewards::api::is_voting_complete(&ctx(storage.clone(), 51), DAY.value()).unwrap()
        );

        // One last_sealed_day replaces the flags of each day, and it moves one day at a time.
        assert_eq!(
            outbe_rewards::api::get_last_sealed_day(&storage).unwrap(),
            0
        );
        let context = ctx(storage.clone(), 100);
        let pool = U256::from(30);
        allocate_fixture(
            &context,
            DAY,
            &[
                (PoolKind::Validator, pool),
                (PoolKind::Waa, pool),
                (PoolKind::Sra, pool),
                (PoolKind::Cca, pool),
            ],
        )
        .unwrap();
        assert_eq!(
            outbe_rewards::api::get_last_sealed_day(&storage).unwrap(),
            DAY.value()
        );
        assert!(contract
            .increment_waa_tribute(DAY, numbered_address(99))
            .is_err());
        // The records stay until the jobs of DAY end.
        assert_eq!(rewards.day_voter_count.read(&DAY.value()).unwrap(), 3);
        assert_eq!(contract.waa_address_count.read(&DAY).unwrap(), 3);

        // The four jobs end in this block, then each owner deletes its day list.
        crate::api::process_agent_rewards(&context).unwrap();
        assert!(!crate::distribution::has_pending_accruals_for_day(&storage, DAY.value()).unwrap());
        assert!(outbe_rewards::api::is_day_cleanup_scheduled(&storage, DAY.value()).unwrap());
        assert_day_records_deleted(&context, DAY);
        // A second allocation of a finished day is refused.
        assert!(crate::api::allocate_day_agent_rewards(
            &context,
            DAY,
            crate::api::DayAgentPools::default(),
        )
        .is_err());

        // The allocation of the next day fails. The Cycle seals it and abandons its jobs.
        let failed = ctx(storage.clone(), 101);
        outbe_rewards::api::seal_votes_and_fees(&failed, failed_day.value()).unwrap();
        assert_eq!(
            outbe_rewards::api::get_last_sealed_day(&storage).unwrap(),
            failed_day.value()
        );
        assert!(outbe_rewards::api::seal_votes_and_fees(&failed, failed_day.value()).is_err());
        assert!(outbe_rewards::api::seal_votes_and_fees(&failed, DAY.value()).is_err());
        crate::api::abandon_day_agent_rewards(&failed, failed_day).unwrap();
        assert_eq!(
            rewards.day_voter_count.read(&failed_day.value()).unwrap(),
            3
        );
        crate::api::process_agent_rewards(&failed).unwrap();
        assert_day_records_deleted(&failed, failed_day);

        outbe_rewards::api::seal_votes_and_fees(&failed, later_day.value()).unwrap();
        assert_eq!(
            outbe_rewards::api::get_last_sealed_day(&storage).unwrap(),
            later_day.value()
        );
    });
}

#[test]
fn a_thousand_positive_ccas_finish_in_four_blocks_and_survive_exit_during_processing() {
    with_contract_mut(|storage, contract| {
        for n in 1..=1_000 {
            originate(&storage, numbered_address(n), DAY, U256::ONE);
        }
        let context = ctx(storage.clone(), 0);
        allocate_fixture(&context, DAY, &[(PoolKind::Cca, U256::from(1_000))]).unwrap();
        for height in 0..4 {
            crate::api::process_agent_rewards(&ctx(storage.clone(), height)).unwrap();
            assert_eq!(
                contract.pool_claimable_total(RewardPool::Cca).unwrap(),
                U256::from(((height + 1) * 256).min(1_000))
            );
            if height == 0 {
                outbe_ccaregistry::runtime::unbond(storage.clone(), numbered_address(900)).unwrap();
                storage
                    .set_block_timestamp(U256::from(
                        T_NOW + outbe_ccaregistry::constants::UNBOND_COOLDOWN_SECONDS,
                    ))
                    .unwrap();
                outbe_ccaregistry::runtime::claim_unbonded(storage.clone(), numbered_address(900))
                    .unwrap();
                let bond = outbe_ccaregistry::constants::BOND_REQUIREMENT;
                storage
                    .increase_balance(outbe_primitives::addresses::CCA_REGISTRY_ADDRESS, bond)
                    .unwrap();
                outbe_ccaregistry::runtime::bond(
                    storage.clone(),
                    numbered_address(900),
                    bond,
                    "Returning CCA".into(),
                )
                .unwrap();
            }
        }
        assert_eq!(
            contract
                .get_pool_claimable_reward(RewardPool::Cca, numbered_address(900))
                .unwrap(),
            U256::ONE
        );
        assert!(!crate::distribution::has_pending_accruals_for_day(&storage, DAY.value()).unwrap());
    });
}

#[test]
fn gap_ed29_job_rounding_rest_goes_to_promis_limit_once() {
    with_contract_mut(|storage, contract| {
        voters(&storage, DAY, 3);
        for n in 1..=3 {
            contract
                .increment_waa_tribute(DAY, numbered_address(n))
                .unwrap();
            contract
                .increment_sra_tribute(DAY, numbered_address(n))
                .unwrap();
            originate(&storage, numbered_address(n), DAY, U256::ONE);
        }
        let context = ctx(storage.clone(), 0);
        let amount = U256::from(10);
        allocate_fixture(
            &context,
            DAY,
            &[
                (PoolKind::Validator, amount),
                (PoolKind::Waa, amount),
                (PoolKind::Sra, amount),
                (PoolKind::Cca, amount),
            ],
        )
        .unwrap();
        crate::api::process_agent_rewards(&context).unwrap();
        let ledger = outbe_emission::api::emission_day(storage.clone(), DAY.value())
            .unwrap()
            .unwrap();
        assert_eq!(
            (
                ledger.validators,
                ledger.waa,
                ledger.sra,
                ledger.cca,
                ledger.promis_limit
            ),
            (9, 9, 9, 9, 4)
        );
        assert_eq!(
            outbe_promislimit::PromisLimitContract::new(storage.clone())
                .promis_limit()
                .unwrap(),
            U256::from(4)
        );
        crate::api::process_agent_rewards(&context).unwrap();
        crate::api::process_agent_rewards(&ctx(storage.clone(), 1)).unwrap();
        assert_eq!(
            outbe_promislimit::PromisLimitContract::new(storage.clone())
                .promis_limit()
                .unwrap(),
            U256::from(4)
        );
    });
}

#[test]
fn cleanup_is_bounded_sealed_and_independent_of_monetary_pending() {
    with_contract_mut(|storage, contract| {
        for n in 1..=600 {
            contract
                .increment_waa_tribute(DAY, numbered_address(n))
                .unwrap();
        }
        let context = ctx(storage.clone(), 0);
        allocate_fixture(&context, DAY, &[(PoolKind::Waa, U256::ZERO)]).unwrap();
        crate::api::process_agent_rewards(&context).unwrap();
        assert_eq!(
            contract
                .waa_tribute_counts
                .read(&AgentRewardContract::tribute_count_key(
                    DAY,
                    numbered_address(1)
                ))
                .unwrap(),
            1
        );
        assert!(contract
            .increment_waa_tribute(DAY, numbered_address(999))
            .is_err());
        crate::api::process_agent_rewards(&ctx(storage.clone(), 1)).unwrap();
        crate::api::process_agent_rewards(&ctx(storage.clone(), 2)).unwrap();
        assert!(!crate::distribution::has_pending_accruals_for_day(&storage, DAY.value()).unwrap());
        assert_eq!(contract.waa_address_count.read(&DAY).unwrap(), 600);
        assert_eq!(
            contract
                .waa_tribute_counts
                .read(&AgentRewardContract::tribute_count_key(
                    DAY,
                    numbered_address(256)
                ))
                .unwrap(),
            0
        );
        assert_eq!(
            contract
                .waa_tribute_counts
                .read(&AgentRewardContract::tribute_count_key(
                    DAY,
                    numbered_address(257)
                ))
                .unwrap(),
            1
        );
        crate::api::process_agent_rewards(&ctx(storage.clone(), 3)).unwrap();
        crate::api::process_agent_rewards(&ctx(storage.clone(), 4)).unwrap();
        assert_eq!(contract.waa_address_count.read(&DAY).unwrap(), 0);
        assert!(contract
            .increment_waa_tribute(DAY, numbered_address(1))
            .is_err());
    });
}

#[test]
fn allocation_prevalidates_queue_and_freezes_immutable_inputs() {
    with_contract_mut(|storage, contract| {
        let context = ctx(storage.clone(), 0);
        outbe_rewards::api::seal_votes_and_fees(&context, DAY.value()).unwrap();
        contract.accrual_tail.write(u64::MAX - 3).unwrap();
        assert!(crate::api::allocate_day_agent_rewards(
            &context,
            DAY,
            crate::api::DayAgentPools::default(),
        )
        .is_err());
        assert_eq!(contract.pending_jobs_by_day.read(&DAY.value()).unwrap(), 0);
        contract.accrual_tail.write(0).unwrap();
        allocate_fixture(&context, DAY, &[]).unwrap();
        assert!(crate::api::allocate_day_agent_rewards(
            &context,
            DAY,
            crate::api::DayAgentPools::default(),
        )
        .is_err());
        assert!(contract
            .increment_sra_tribute(DAY, numbered_address(1))
            .is_err());
        assert!(crate::api::abandon_day_agent_rewards(&context, DAY).is_err());
    });
}

#[test]
fn missing_ledger_rolls_back_credits_and_worker_height_and_is_retryable() {
    with_contract_mut(|storage, contract| {
        contract
            .increment_waa_tribute(DAY, numbered_address(1))
            .unwrap();
        let context = ctx(storage.clone(), 0);
        allocate_fixture(&context, DAY, &[(PoolKind::Waa, U256::from(10))]).unwrap();
        let emission = outbe_emission::schema::EmissionContract::new(storage.clone());
        let ledger = emission.emission_days.get(DAY.value()).unwrap().unwrap();
        emission.emission_days.delete(DAY.value()).unwrap();
        assert!(crate::api::process_agent_rewards(&context).is_err());
        assert_eq!(
            contract.get_claimable_reward(numbered_address(1)).unwrap(),
            U256::ZERO
        );
        assert!(!contract.worker_initialized.read().unwrap());
        assert_eq!(contract.accrual_head.read().unwrap(), 0);
        emission.emission_days.create(&ledger).unwrap();
        crate::api::process_agent_rewards(&context).unwrap();
        assert_eq!(
            contract.get_claimable_reward(numbered_address(1)).unwrap(),
            U256::from(10)
        );
    });
}

#[test]
fn failed_day_abandonment_queues_once_and_permanently_seals_all_owners() {
    with_contract_mut(|storage, contract| {
        for n in 1..=300 {
            contract
                .increment_sra_tribute(DAY, numbered_address(n))
                .unwrap();
        }
        let context = ctx(storage.clone(), 0);
        outbe_rewards::api::seal_votes_and_fees(&context, DAY.value()).unwrap();
        crate::api::abandon_day_agent_rewards(&context, DAY).unwrap();
        crate::api::abandon_day_agent_rewards(&context, DAY).unwrap();
        assert_eq!(contract.cleanup_tail.read().unwrap(), 2);
        crate::api::process_agent_rewards(&context).unwrap();
        assert_eq!(contract.sra_address_count.read(&DAY).unwrap(), 300);
        crate::api::process_agent_rewards(&ctx(storage.clone(), 1)).unwrap();
        assert_eq!(contract.sra_address_count.read(&DAY).unwrap(), 0);
        assert!(contract
            .increment_sra_tribute(DAY, numbered_address(1))
            .is_err());
        assert!(!crate::distribution::has_pending_accruals_for_day(&storage, DAY.value()).unwrap());
    });
}

#[test]
fn metered_accrual_and_three_owner_cleanup_fit_the_existing_block_envelope() {
    let next_day = WorldwideDay::new(20231114);
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.set_timestamp(U256::from(T_NOW));
    StorageHandle::enter(&mut provider, |storage| {
        voters(&storage, DAY, 256);
        let mut contract = AgentRewardContract::new(storage.clone());
        for n in 1..=257 {
            let address = numbered_address(n);
            originate(&storage, address, DAY, U256::ONE);
            outbe_ccaregistry::api::credis_issued(&storage, address, next_day.value(), U256::ONE)
                .unwrap();
            if n <= 256 {
                contract.increment_waa_tribute(DAY, address).unwrap();
            }
        }
        let context = ctx(storage.clone(), 0);
        outbe_rewards::api::seal_votes_and_fees(&context, DAY.value()).unwrap();
        crate::api::abandon_day_agent_rewards(&context, DAY).unwrap();
        allocate_fixture(&context, next_day, &[(PoolKind::Cca, U256::from(257))]).unwrap();
    });
    provider.set_gas_limit(30_000_000);
    provider.enable_production_storage_gas_metering();
    StorageHandle::enter(&mut provider, |storage| {
        crate::api::process_agent_rewards(&ctx(storage.clone(), 0)).unwrap();
        let contract = AgentRewardContract::new(storage.clone());
        assert_eq!(
            contract.pool_claimable_total(RewardPool::Cca).unwrap(),
            U256::from(256)
        );
        assert_eq!(contract.waa_address_count.read(&DAY).unwrap(), 0);
        assert_eq!(
            outbe_rewards::schema::Rewards::new(storage.clone())
                .day_voter_count
                .read(&DAY.value())
                .unwrap(),
            0
        );
        let gas = storage.gas_used().unwrap();
        eprintln!("256 accrual + 256 entries per cleanup owner: {gas} storage gas");
        assert!(gas < 30_000_000);
    });
}

#[test]
fn empty_headers_are_bounded_and_corrupt_pending_counts_fail_before_credit() {
    with_contract_mut(|storage, contract| {
        let context = ctx(storage.clone(), 0);
        allocate_fixture(&context, DAY, &[]).unwrap();
        allocate_fixture(&context, WorldwideDay::new(20231114), &[]).unwrap();
        crate::api::process_agent_rewards(&context).unwrap();
        assert_eq!(contract.accrual_head.read().unwrap(), 4);
        assert_eq!(contract.pending_jobs_by_day.read(&20231114).unwrap(), 4);
        contract.pending_jobs_by_day.write(&20231114, 0).unwrap();
        assert!(crate::api::process_agent_rewards(&ctx(storage.clone(), 1)).is_err());
        assert_eq!(contract.accrual_head.read().unwrap(), 4);
        assert_eq!(contract.worker_height.read().unwrap(), 0);
        contract.pending_jobs_by_day.write(&20231114, 4).unwrap();
        crate::api::process_agent_rewards(&ctx(storage.clone(), 1)).unwrap();
        assert_eq!(contract.accrual_head.read().unwrap(), 8);
    });
}

#[test]
fn each_worker_mutation_failure_preserves_committed_obligations_and_retries_once() {
    let prepare = || {
        let mut provider = every_pool_day();
        StorageHandle::enter(&mut provider, |storage| {
            let context = ctx(storage, 0);
            let day = WorldwideDay::new(timestamp_to_date_key(T_NOW));
            let amount = U256::from(10);
            allocate_fixture(
                &context,
                day,
                &[
                    (PoolKind::Validator, amount),
                    (PoolKind::Waa, amount),
                    (PoolKind::Sra, amount),
                    (PoolKind::Cca, amount),
                ],
            )
            .unwrap();
        });
        provider.clear_mutation_failure();
        provider
    };
    let run = |provider: &mut HashMapStorageProvider| {
        StorageHandle::enter(provider, |storage| {
            crate::api::process_agent_rewards(&ctx(storage, 0))
        })
    };
    let mut successful = prepare();
    run(&mut successful).unwrap();
    let writes = successful.clear_mutation_failure();
    let final_state = successful.storage.clone();
    for failure in 0..writes {
        let mut provider = prepare();
        let committed = provider.storage.clone();
        let events = provider
            .get_events(outbe_primitives::addresses::AGENT_REWARD_ADDRESS)
            .clone();
        provider.fail_after_mutation_at(failure);
        assert!(run(&mut provider).is_err(), "mutation {failure}");
        assert_eq!(provider.storage, committed, "mutation {failure}");
        assert_eq!(
            provider.get_events(outbe_primitives::addresses::AGENT_REWARD_ADDRESS),
            &events
        );
        provider.clear_mutation_failure();
        run(&mut provider).unwrap();
        assert_eq!(provider.storage, final_state, "mutation {failure}");
        let once = provider.storage.clone();
        run(&mut provider).unwrap();
        assert_eq!(provider.storage, once);
    }
}

#[test]
fn missing_tribute_index_or_positive_count_never_becomes_residue() {
    for pool in [RewardPool::Waa, RewardPool::Sra] {
        for clear_address in [false, true] {
            with_contract_mut(|storage, contract| {
                let address = numbered_address(1);
                increment_tribute_counter(contract, pool, DAY, address).unwrap();
                if clear_address {
                    // A valid zero-address recipient must not mask another missing index.
                    increment_tribute_counter(contract, pool, DAY, Address::ZERO).unwrap();
                }
                let context = ctx(storage.clone(), 0);
                allocate_fixture(&context, DAY, &[(pool, U256::from(10))]).unwrap();
                let index = contract.day_index(pool).unwrap();
                let address_key = AgentRewardContract::address_index_key(DAY, 0);
                let count_key = AgentRewardContract::tribute_count_key(DAY, address);
                if clear_address {
                    index.addresses.write(&address_key, Address::ZERO).unwrap();
                } else {
                    index.counts.write(&count_key, 0).unwrap();
                }
                assert!(crate::api::process_agent_rewards(&context).is_err());
                assert_eq!(contract.accrual_head.read().unwrap(), 0);
                assert_eq!(contract.pending_jobs_by_day.read(&DAY.value()).unwrap(), 4);
                assert_eq!(contract.get_claimable_reward(address).unwrap(), U256::ZERO);
                assert_eq!(
                    outbe_promislimit::PromisLimitContract::new(storage.clone())
                        .promis_limit()
                        .unwrap(),
                    U256::ZERO
                );
                assert!(!contract.worker_initialized.read().unwrap());
                index.addresses.write(&address_key, address).unwrap();
                index.counts.write(&count_key, 1).unwrap();
                crate::api::process_agent_rewards(&context).unwrap();
                assert_eq!(
                    contract.get_pool_claimable_reward(pool, address).unwrap(),
                    U256::from(if clear_address { 5 } else { 10 })
                );
            });
        }
    }
}

#[test]
fn exact_256_entry_boundary_does_not_credit_the_next_pool_in_the_same_block() {
    with_contract_mut(|storage, contract| {
        for n in 1..=256 {
            contract
                .increment_waa_tribute(DAY, numbered_address(n))
                .unwrap();
        }
        contract
            .increment_sra_tribute(DAY, numbered_address(999))
            .unwrap();
        let context = ctx(storage.clone(), 0);
        allocate_fixture(
            &context,
            DAY,
            &[(PoolKind::Waa, U256::from(256)), (PoolKind::Sra, U256::ONE)],
        )
        .unwrap();
        crate::api::process_agent_rewards(&context).unwrap();
        assert_eq!(contract.accrual_head.read().unwrap(), 2);
        assert_eq!(
            contract.pool_claimable_total(RewardPool::Waa).unwrap(),
            U256::from(256)
        );
        assert_eq!(
            contract.pool_claimable_total(RewardPool::Sra).unwrap(),
            U256::ZERO
        );
        crate::api::process_agent_rewards(&context).unwrap();
        assert_eq!(
            contract.pool_claimable_total(RewardPool::Sra).unwrap(),
            U256::ZERO
        );
        crate::api::process_agent_rewards(&ctx(storage.clone(), 1)).unwrap();
        assert_eq!(
            contract.pool_claimable_total(RewardPool::Sra).unwrap(),
            U256::ONE
        );
        assert_eq!(contract.accrual_head.read().unwrap(), 4);
    });
}
