use super::harness::*;

fn bond_cca(db: &mut CacheDB<EmptyDB>) {
    with_storage(db, |storage| {
        storage
            .increase_balance(CCA_REGISTRY_ADDRESS, BOND_REQUIREMENT)
            .unwrap();
        runtime::bond(storage, CCA, BOND_REQUIREMENT, "Test CCA".into()).unwrap();
    });
}

#[test]
fn a_claim_transaction_issues_the_gem_of_the_day_and_takes_its_weight() {
    for pool in [RewardPool::Waa, RewardPool::Sra, RewardPool::Validator] {
        let mut db = reward_world();
        credit(&mut db, pool, REWARD);
        claim_by_tx(&mut db, pool, REWARD);

        let again = claim_tx(&mut db, pool);
        assert!(again.is_revert(), "{again:?}");
        // The only weight of the day went with the claim, and the day with it.
        assert_eq!(
            again.output.as_ref(),
            IAgentReward::AgentRewardRateMissing {
                pool: pool as u8,
                emissionDay: DAY,
            }
            .abi_encode()
        );
        assert!(again.logs.is_empty());
    }
}

#[test]
fn a_cca_claim_transaction_needs_registration() {
    let mut db = reward_world();
    let unregistered = claim_tx(&mut db, RewardPool::Cca);
    assert!(unregistered.is_revert(), "{unregistered:?}");
    assert!(unregistered.logs.is_empty());

    bond_cca(&mut db);
    credit(&mut db, RewardPool::Cca, REWARD);
    claim_by_tx(&mut db, RewardPool::Cca, REWARD);
}

#[test]
fn an_expiry_transaction_sends_the_reward_of_any_account_to_the_promis_limit() {
    for pool in [
        RewardPool::Waa,
        RewardPool::Sra,
        RewardPool::Cca,
        RewardPool::Validator,
    ] {
        let mut db = reward_world();
        bond_cca(&mut db);
        credit(&mut db, pool, REWARD);
        let deadline = NOW + REWARD_BALANCE_IDLE_SECONDS;
        assert_eq!(reward_deadline(&mut db, pool), deadline);
        let limit_before = promis_limit(&mut db);
        let caller_balance = balance(&db, CCA);
        let data = expiration_call(CCA, pool);

        let early = tx_to(
            &mut db,
            AGENT_REWARD_ADDRESS,
            U256::ZERO,
            data.clone(),
            deadline - 1,
        );
        assert!(early.is_revert(), "{early:?}");
        assert_eq!(
            early.output.as_ref(),
            IAgentReward::AgentRewardNotExpired { deadline }.abi_encode()
        );
        assert!(early.logs.is_empty());
        assert_eq!(promis_limit(&mut db), limit_before);

        let returned = tx_to(
            &mut db,
            AGENT_REWARD_ADDRESS,
            U256::ZERO,
            data.clone(),
            deadline,
        );
        assert!(returned.is_success(), "{returned:?}");
        assert_eq!(
            IAgentReward::expireAgentRewardCall::abi_decode_returns(&returned.output).unwrap(),
            REWARD
        );
        let added = returned
            .logs
            .iter()
            .find(|log| log.address == PROMIS_LIMIT_ADDRESS)
            .and_then(|log| IPromisLimit::PromisLimitAdded::decode_log(log).ok())
            .expect("the expiry credits the Promis Limit")
            .data;
        assert_eq!(added.source, 32);
        assert_eq!(added.amountMinor, REWARD);
        let event = returned
            .logs
            .iter()
            .find_map(|log| IAgentReward::AgentRewardExpired::decode_log(log).ok())
            .expect("the expiry emits AgentRewardExpired")
            .data;
        assert_eq!(event.account, CCA);
        assert_eq!(event.pool, pool as u8);
        assert_eq!(event.emissionDay, DAY);
        assert_eq!(event.promisLoadMinor, REWARD);
        assert_eq!(promis_limit(&mut db), limit_before + REWARD);
        assert_eq!(
            reward_deadline(&mut db, pool),
            0,
            "the last weight deletes the day"
        );
        assert_eq!(balance(&db, CCA), caller_balance);
        assert_eq!(balance(&db, AGENT_REWARD_ADDRESS), U256::ZERO);

        let repeated = tx_to(
            &mut db,
            AGENT_REWARD_ADDRESS,
            U256::ZERO,
            data,
            deadline + 1,
        );
        assert!(repeated.is_revert(), "{repeated:?}");
        assert!(repeated.logs.is_empty());
        assert_eq!(promis_limit(&mut db), limit_before + REWARD);
    }
}

#[test]
fn claim_transactions_revert_at_and_after_the_deadline_without_issuing_gems() {
    for pool in [
        RewardPool::Waa,
        RewardPool::Sra,
        RewardPool::Cca,
        RewardPool::Validator,
    ] {
        let mut db = reward_world();
        bond_cca(&mut db);
        credit(&mut db, pool, REWARD);
        let before = snapshot(&mut db);
        let deadline = NOW + REWARD_BALANCE_IDLE_SECONDS;
        for now in [deadline, deadline + 1] {
            let result = tx_to(
                &mut db,
                AGENT_REWARD_ADDRESS,
                U256::ZERO,
                claim_call(pool, &[DAY]),
                now,
            );
            assert!(result.is_revert(), "{result:?}");
            assert_eq!(
                result.output.as_ref(),
                IAgentReward::AgentRewardDeadlinePassed { deadline }.abi_encode()
            );
            assert!(result.logs.is_empty());
            assert_eq!(snapshot(&mut db), before);
        }
    }
}

#[test]
fn expiry_transactions_reject_value_static_writes_invalid_pools_and_roll_back_out_of_gas() {
    let mut db = reward_world();
    set_balance(&mut db, CCA, U256::from(10));
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
    assert!(funded.logs.is_empty());
    assert_eq!(balance(&db, CCA), U256::from(10));

    let proxy = static_reward_proxy(&mut db);
    let view = tx_to(
        &mut db,
        proxy,
        U256::ZERO,
        IAgentReward::getAgentRewardRateCall {
            pool: RewardPool::Waa as u8,
            emissionDay: DAY,
        }
        .abi_encode(),
        deadline,
    );
    assert!(view.is_success(), "{view:?}");
    let output = &view.output;
    assert_eq!(U256::from_be_slice(&output[..32]), U256::ONE);
    assert_eq!(
        IAgentReward::getAgentRewardRateCall::abi_decode_returns(&output[32..])
            .unwrap()
            .deadline,
        deadline
    );
    let blocked = tx_to(&mut db, proxy, U256::ZERO, data.clone(), deadline);
    assert!(blocked.is_success(), "{blocked:?}");
    assert_eq!(U256::from_be_slice(&blocked.output[..32]), U256::ZERO);
    assert!(blocked.logs.is_empty());
    assert_eq!(reward_deadline(&mut db, RewardPool::Waa), deadline);
    assert_eq!(promis_limit(&mut db), U256::ZERO);

    for data in [
        IAgentReward::expireAgentRewardCall {
            account: Address::ZERO,
            pool: 4,
            emissionDays: vec![DAY],
        }
        .abi_encode(),
        IAgentReward::getAgentRewardRateCall {
            pool: 4,
            emissionDay: DAY,
        }
        .abi_encode(),
    ] {
        let result = tx_to(&mut db, AGENT_REWARD_ADDRESS, U256::ZERO, data, deadline);
        assert!(result.is_revert());
        assert_eq!(
            result.output.as_ref(),
            IAgentReward::UnknownRewardPool { value: 4 }.abi_encode()
        );
        assert!(result.logs.is_empty());
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
    assert!(failed.is_halt(), "{failed:?}");
    assert!(failed.logs.is_empty());
    assert_eq!(promis_limit(&mut db), U256::ZERO);
    assert_eq!(reward_deadline(&mut db, RewardPool::Waa), deadline);
    with_storage(&mut db, |storage| {
        assert_eq!(
            AgentRewardContract::new(storage)
                .weight(RewardPool::Waa, DAY, CCA)
                .unwrap(),
            U256::ONE
        );
    });
    let retry = tx_to(&mut db, AGENT_REWARD_ADDRESS, U256::ZERO, data, deadline);
    assert!(retry.is_success(), "{retry:?}");
    assert_eq!(promis_limit(&mut db), REWARD);
}
