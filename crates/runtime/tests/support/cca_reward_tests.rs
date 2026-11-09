#[test]
fn reward_claim_transaction_issues_the_gem_and_clears_the_pool() {
    let mut db = reward_world();
    credit(&mut db, RewardPool::Waa, REWARD);
    claim_by_tx(&mut db, RewardPool::Waa, REWARD, U256::ZERO);
}
#[test]
fn an_sra_claim_transaction_issues_the_gem() {
    let mut db = reward_world();
    credit(&mut db, RewardPool::Sra, REWARD);
    claim_by_tx(&mut db, RewardPool::Sra, REWARD, U256::ZERO);
}
#[test]
fn a_cca_claim_transaction_needs_registration_and_may_claim_part() {
    let mut db = reward_world();
    credit(&mut db, RewardPool::Cca, REWARD);
    assert!(!claim_tx(&mut db, RewardPool::Cca, U256::ZERO).is_success());
    with_storage(&mut db, |storage| {
        storage
            .increase_balance(CCA_REGISTRY_ADDRESS, BOND_REQUIREMENT)
            .unwrap();
        runtime::bond(storage.clone(), CCA, BOND_REQUIREMENT, "Test CCA".into()).unwrap();
        assert_eq!(
            AgentRewardContract::new(storage)
                .get_pool_claimable_reward(RewardPool::Cca, CCA)
                .unwrap(),
            REWARD
        );
    });
    claim_by_tx(&mut db, RewardPool::Cca, REWARD, U256::from(100));
}
#[test]
fn a_validator_claim_transaction_issues_the_gem_and_an_empty_pool_is_a_no_op() {
    let mut db = reward_world();
    credit(&mut db, RewardPool::Validator, REWARD);
    claim_by_tx(&mut db, RewardPool::Validator, REWARD, U256::ZERO);

    let result = claim_tx(&mut db, RewardPool::Validator, U256::ZERO);
    assert!(result.is_success(), "{result:?}");
    assert_eq!(
        IAgentReward::claimRewardCall::abi_decode_returns(result.output().unwrap()).unwrap(),
        U256::ZERO
    );
    assert!(result.logs().is_empty());
}
#[test]
fn expired_reward_transactions_return_unrelated_and_zero_accounts_to_the_real_promis_limit() {
    for pool in [
        RewardPool::Waa,
        RewardPool::Sra,
        RewardPool::Cca,
        RewardPool::Validator,
    ] {
        let deregistered = Address::repeat_byte(0xa8);
        for account in [Address::repeat_byte(0xa7), Address::ZERO, deregistered] {
            let mut db = reward_world();
            assert_ne!(account, CCA);
            if account == deregistered {
                with_storage_at(&mut db, NOW - UNBOND_COOLDOWN_SECONDS, |storage| {
                    storage
                        .increase_balance(CCA_REGISTRY_ADDRESS, BOND_REQUIREMENT)
                        .unwrap();
                    runtime::bond(
                        storage.clone(),
                        account,
                        BOND_REQUIREMENT,
                        "Former CCA".into(),
                    )
                    .unwrap();
                    runtime::unbond(storage, account).unwrap();
                });
                with_storage_at(&mut db, NOW, |storage| {
                    runtime::claim_unbonded(storage.clone(), account).unwrap();
                    assert_eq!(
                        api::get_cca(&storage, account).unwrap().state,
                        ICcaRegistry::State::Deregistered
                    );
                });
            }
            with_storage_at(&mut db, NOW, |storage| {
                assert!(!api::is_active(&storage, account).unwrap());
                let mut rewards = AgentRewardContract::new(storage);
                rewards.add_claimable_reward(pool, account, REWARD).unwrap();
                // Another account and another pool must not be consumed by the return.
                rewards.add_claimable_reward(pool, CCA, U256::ONE).unwrap();
                let other = if pool == RewardPool::Waa {
                    RewardPool::Sra
                } else {
                    RewardPool::Waa
                };
                rewards
                    .add_claimable_reward(other, account, U256::from(7))
                    .unwrap();
            });
            let deadline = NOW + REWARD_BALANCE_IDLE_SECONDS;
            assert_eq!(balance_deadline(&mut db, account, pool), deadline);
            let limit_before = promis_limit(&mut db);
            let caller_balance = db.cache.accounts[&CCA].info.balance;
            let data = expiration_call(account, pool);
            let early = tx_to(
                &mut db,
                AGENT_REWARD_ADDRESS,
                U256::ZERO,
                data.clone(),
                deadline - 1,
            );
            assert!(matches!(early, ExecutionResult::Revert { .. }), "{early:?}");
            assert_eq!(
                early.output().unwrap().as_ref(),
                IAgentReward::RewardBalanceNotExpired { deadline }.abi_encode()
            );
            assert!(early.logs().is_empty());
            assert_eq!(balance_deadline(&mut db, account, pool), deadline);
            assert_eq!(promis_limit(&mut db), limit_before);

            // Credits to an already-expired balance retain its original lifetime.
            with_storage_at(&mut db, deadline, |storage| {
                AgentRewardContract::new(storage)
                    .add_claimable_reward(pool, account, U256::ONE)
                    .unwrap();
            });
            assert_eq!(balance_deadline(&mut db, account, pool), deadline);
            let returned = tx_to(
                &mut db,
                AGENT_REWARD_ADDRESS,
                U256::ZERO,
                data.clone(),
                deadline,
            );
            assert!(returned.is_success(), "{returned:?}");
            let amount = REWARD + U256::ONE;
            assert_eq!(
                IAgentReward::returnExpiredRewardCall::abi_decode_returns(
                    returned.output().unwrap()
                )
                .unwrap(),
                amount
            );
            assert_eq!(returned.logs().len(), 1);
            let log = &returned.logs()[0];
            assert_eq!(log.address, AGENT_REWARD_ADDRESS);
            assert_eq!(log.data.topics().len(), 3);
            assert_eq!(
                log.data.topics()[0],
                IAgentReward::RewardBalanceExpired::SIGNATURE_HASH
            );
            assert_eq!(log.data.topics()[1], account.into_word());
            assert_eq!(
                log.data.topics()[2],
                alloy_primitives::B256::from(U256::from(pool as u8))
            );
            let event = IAgentReward::RewardBalanceExpired::decode_log(log)
                .unwrap()
                .data;
            assert_eq!(event.account, account);
            assert_eq!(event.pool, pool as u8);
            assert_eq!(event.promisLoadMinor, amount);
            assert_eq!(promis_limit(&mut db), limit_before + amount);
            assert_eq!(balance_deadline(&mut db, account, pool), 0);
            with_storage(&mut db, |storage| {
                let rewards = AgentRewardContract::new(storage);
                assert_eq!(
                    rewards.get_pool_claimable_reward(pool, account).unwrap(),
                    U256::ZERO
                );
                assert_eq!(
                    rewards.get_pool_claimable_reward(pool, CCA).unwrap(),
                    U256::ONE
                );
                assert_eq!(
                    rewards.claimable_total_minor.read(&(pool as u8)).unwrap(),
                    U256::ONE
                );
                let other = if pool == RewardPool::Waa {
                    RewardPool::Sra
                } else {
                    RewardPool::Waa
                };
                assert_eq!(
                    rewards.get_pool_claimable_reward(other, account).unwrap(),
                    U256::from(7)
                );
            });
            assert_eq!(db.cache.accounts[&CCA].info.balance, caller_balance);
            assert_eq!(
                db.cache.accounts[&AGENT_REWARD_ADDRESS].info.balance,
                U256::ZERO
            );
            assert_eq!(
                db.cache.accounts[&PROMIS_LIMIT_ADDRESS].info.balance,
                U256::ZERO
            );
            let repeated = tx_to(
                &mut db,
                AGENT_REWARD_ADDRESS,
                U256::ZERO,
                data.clone(),
                deadline + 1,
            );
            assert!(repeated.is_success(), "{repeated:?}");
            assert_eq!(
                IAgentReward::returnExpiredRewardCall::abi_decode_returns(
                    repeated.output().unwrap()
                )
                .unwrap(),
                U256::ZERO
            );
            assert!(repeated.logs().is_empty());
            assert_eq!(promis_limit(&mut db), limit_before + amount);

            with_storage_at(&mut db, deadline + 1, |storage| {
                AgentRewardContract::new(storage)
                    .add_claimable_reward(pool, account, REWARD)
                    .unwrap();
            });
            let renewed = deadline + 1 + REWARD_BALANCE_IDLE_SECONDS;
            assert_eq!(balance_deadline(&mut db, account, pool), renewed);
            let stale = tx_to(
                &mut db,
                AGENT_REWARD_ADDRESS,
                U256::ZERO,
                data,
                deadline + 1,
            );
            assert!(matches!(stale, ExecutionResult::Revert { .. }));
            assert_eq!(
                stale.output().unwrap().as_ref(),
                IAgentReward::RewardBalanceNotExpired { deadline: renewed }.abi_encode()
            );
            assert!(stale.logs().is_empty());
            assert_eq!(promis_limit(&mut db), limit_before + amount);
        }
    }
}
#[test]
fn expired_claim_transactions_revert_at_and_after_the_deadline_without_issuing_gems() {
    for pool in [
        RewardPool::Waa,
        RewardPool::Sra,
        RewardPool::Cca,
        RewardPool::Validator,
    ] {
        let mut db = reward_world();
        with_storage(&mut db, |storage| {
            storage
                .increase_balance(CCA_REGISTRY_ADDRESS, BOND_REQUIREMENT)
                .unwrap();
            runtime::bond(storage, CCA, BOND_REQUIREMENT, "Test CCA".into()).unwrap();
        });
        credit(&mut db, pool, REWARD);
        let before = snapshot(&mut db);
        let deadline = NOW + REWARD_BALANCE_IDLE_SECONDS;
        for now in [deadline, deadline + 1] {
            let result = tx_to(
                &mut db,
                AGENT_REWARD_ADDRESS,
                U256::ZERO,
                IAgentReward::claimRewardCall {
                    pool: pool as u8,
                    promisLoadMinor: U256::ZERO,
                    issuanceCurrency: 840,
                    referenceCurrency: 840,
                }
                .abi_encode(),
                now,
            );
            assert!(
                matches!(result, ExecutionResult::Revert { .. }),
                "{result:?}"
            );
            assert_eq!(
                result.output().unwrap().as_ref(),
                IAgentReward::ClaimDeadlinePassed { deadline }.abi_encode()
            );
            assert!(result.logs().is_empty());
            assert_eq!(snapshot(&mut db), before);
        }
    }
}
#[test]
fn expired_reward_transactions_reject_value_static_writes_invalid_pools_and_roll_back_out_of_gas() {
    let mut db = reward_world();
    db.cache.accounts.get_mut(&CCA).unwrap().info.balance = U256::from(10);
    credit(&mut db, RewardPool::Waa, REWARD);
    let deadline = NOW + REWARD_BALANCE_IDLE_SECONDS;
    let data = expiration_call(CCA, RewardPool::Waa);
    let funded = tx_to(
        &mut db,
        AGENT_REWARD_ADDRESS,
        U256::ONE,
        data.clone(),
        deadline,
    );
    assert!(!funded.is_success());
    assert!(funded.logs().is_empty());
    assert_eq!(db.cache.accounts[&CCA].info.balance, U256::from(10));

    let proxy = static_reward_proxy(&mut db);
    let view = tx_to(
        &mut db,
        proxy,
        U256::ZERO,
        IAgentReward::getPoolBalanceDeadlineCall {
            account: CCA,
            pool: 0,
        }
        .abi_encode(),
        deadline,
    );
    assert!(view.is_success(), "{view:?}");
    let output = view.output().unwrap();
    assert_eq!(U256::from_be_slice(&output[..32]), U256::ONE);
    assert_eq!(
        IAgentReward::getPoolBalanceDeadlineCall::abi_decode_returns(&output[32..]).unwrap(),
        deadline
    );
    let blocked = tx_to(&mut db, proxy, U256::ZERO, data.clone(), deadline);
    assert!(blocked.is_success(), "{blocked:?}");
    assert_eq!(
        U256::from_be_slice(&blocked.output().unwrap()[..32]),
        U256::ZERO
    );
    assert!(blocked.logs().is_empty());
    assert_eq!(balance_deadline(&mut db, CCA, RewardPool::Waa), deadline);
    assert_eq!(promis_limit(&mut db), U256::ZERO);

    for data in [
        IAgentReward::returnExpiredRewardCall {
            account: Address::ZERO,
            pool: 4,
        }
        .abi_encode(),
        IAgentReward::getPoolBalanceDeadlineCall {
            account: Address::ZERO,
            pool: 4,
        }
        .abi_encode(),
    ] {
        let result = tx_to(&mut db, AGENT_REWARD_ADDRESS, U256::ZERO, data, deadline);
        assert!(matches!(result, ExecutionResult::Revert { .. }));
        assert_eq!(
            result.output().unwrap().as_ref(),
            IAgentReward::UnknownRewardPool { value: 4 }.abi_encode()
        );
        assert!(result.logs().is_empty());
    }

    let mut successful_db = db.clone();
    let success = tx_to(
        &mut successful_db,
        AGENT_REWARD_ADDRESS,
        U256::ZERO,
        data.clone(),
        deadline,
    );
    assert!(success.is_success(), "{success:?}");
    // Less than the successful transaction's charged gas cannot complete the return.
    let failed = try_tx_to_with_gas(
        &mut db,
        AGENT_REWARD_ADDRESS,
        U256::ZERO,
        data.clone(),
        deadline,
        success.tx_gas_used() - 1,
    )
    .unwrap();
    assert!(matches!(failed, ExecutionResult::Halt { .. }), "{failed:?}");
    assert!(failed.logs().is_empty());
    assert_eq!(promis_limit(&mut db), U256::ZERO);
    assert_eq!(balance_deadline(&mut db, CCA, RewardPool::Waa), deadline);
    with_storage(&mut db, |storage| {
        let rewards = AgentRewardContract::new(storage);
        assert_eq!(
            rewards
                .get_pool_claimable_reward(RewardPool::Waa, CCA)
                .unwrap(),
            REWARD
        );
        assert_eq!(rewards.claimable_total_minor.read(&0).unwrap(), REWARD);
    });
    let retry = tx_to(&mut db, AGENT_REWARD_ADDRESS, U256::ZERO, data, deadline);
    assert!(retry.is_success(), "{retry:?}");
    assert_eq!(promis_limit(&mut db), REWARD);
}
