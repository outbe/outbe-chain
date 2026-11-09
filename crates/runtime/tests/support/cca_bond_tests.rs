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
    for address in [CCA_REGISTRY_ADDRESS, AGENT_REWARD_ADDRESS] {
        db.insert_account_info(
            address,
            AccountInfo {
                code_hash: code.hash_slow(),
                code: Some(code.clone()),
                ..Default::default()
            },
        );
    }

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
        assert_eq!(record.bondedMinor, BOND_REQUIREMENT - U256::ONE);
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
    assert_eq!(event.amountMinor, U256::ONE);
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
        api::credis_issued(&s, CCA, 20231115, U256::from(100)).unwrap();
        let ctx = BlockRuntimeContext::new(
            BlockContext {
                block_gas_limit: 0,
                user_gas_used: 0,
                block_number: 2,
                timestamp: NOW + 2 * SECONDS_PER_DAY,
                chain_id: 1,
                genesis_hash: alloy_primitives::B256::ZERO,
                genesis_timestamp: 1,
                proposer: alloy_primitives::Address::ZERO,
                validators: Vec::new(),
            },
            s.clone(),
        );
        outbe_rewards::api::seal_votes_and_fees(&ctx, 20231115).unwrap();
        let allocation = outbe_agentreward::api::allocate_day_agent_rewards(
            &ctx,
            20231115.into(),
            outbe_agentreward::api::DayAgentPools {
                cca: U256::from(1000),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(allocation.total_excess, U256::ZERO);
        assert_eq!(
            AgentRewardContract::new(s.clone())
                .get_pool_claimable_reward(RewardPool::Cca, CCA)
                .unwrap(),
            U256::ZERO
        );
        outbe_emission::api::record_day_allocation(
            s.clone(),
            20231115,
            outbe_emission::api::DayAllocationAmounts {
                emission_limit: U256::from(1000),
                metadosis_limit: U256::ZERO,
                validators: U256::ZERO,
                waa: U256::ZERO,
                sra: U256::ZERO,
                cca: allocation.cca_allocated,
                promis_limit: U256::ZERO,
            },
        )
        .unwrap();
        outbe_agentreward::api::process_agent_rewards(&ctx).unwrap();
    });
    with_storage(&mut db, |s| {
        assert_eq!(
            AgentRewardContract::new(s)
                .get_pool_claimable_reward(RewardPool::Cca, CCA)
                .unwrap(),
            U256::from(1000)
        )
    });
    assert_eq!(
        db.cache.accounts[&AGENT_REWARD_ADDRESS].info.balance,
        U256::ZERO
    );
    // Claim cases use a fixed balance independent of the pool split.
    let reward = U256::from(320);
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
        assert_eq!(record.bondedMinor, BOND_REQUIREMENT);
        assert_eq!(record.unbondUnlocksAfter, NOW + UNBOND_COOLDOWN_SECONDS);
        assert!(!api::is_active(&s, CCA).unwrap());
        assert!(api::credis_issued(&s, CCA, 20231115, U256::ONE).is_err());
    });
    let claim = |amount| {
        IAgentReward::claimRewardCall {
            pool: 2,
            promisLoadMinor: amount,
            issuanceCurrency: 840,
            referenceCurrency: 840,
        }
        .abi_encode()
    };
    let before = snapshot(&mut db);
    // Both spot and an older daily VWAP are available. Neither may replace yesterday.
    with_storage(&mut db, |storage| {
        seed_oracle(&storage);
        seed_vwap(
            &storage,
            previous_date_key(previous_date_key(timestamp_to_date_key(NOW)).expect("date step"))
                .expect("date step"),
            U256::from(9_000_000),
        );
    });
    for amount in [U256::ZERO, reward + U256::ONE, U256::ONE] {
        assert!(claim_reward(&mut db, claim(amount), NOW).is_err());
        assert_eq!(snapshot(&mut db), before);
    }
    // Retry tomorrow against today's finalized VWAP, with a price distinct from spot.
    let price = U256::from(2_000_000);
    let claim_time = NOW + SECONDS_PER_DAY;
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
        if target == AGENT_REWARD_ADDRESS {
            // The readers route is the live AgentReward entry; an unknown selector is
            // refused there as well.
            assert!(claim_reward(&mut db, data, claim_time).is_err());
        } else {
            let removed = tx_to(&mut db, target, U256::ZERO, data, claim_time);
            assert!(!removed.is_success());
            assert!(removed.logs().is_empty());
        }
        assert_eq!(snapshot(&mut db), before);
    }

    // Force a failure inside issuance after the Gem is stored.
    with_storage(&mut db, |storage| {
        outbe_gemfactory::schema::GemFactoryContract::new(storage)
            .total_gems_issued
            .write(U256::MAX)
            .unwrap();
    });
    let before_failure = snapshot(&mut db);
    assert!(claim_reward(&mut db, claim(U256::ZERO), claim_time).is_err());
    assert_eq!(snapshot(&mut db), before_failure);
    with_storage(&mut db, |storage| {
        outbe_gemfactory::schema::GemFactoryContract::new(storage)
            .total_gems_issued
            .write(U256::ZERO)
            .unwrap();
    });
    with_storage(&mut db, |storage| {
        AgentRewardContract::new(storage)
            .cca_claimable_minor
            .write(&CCA, reward)
            .unwrap();
    });
    for (block, (load, requested)) in (1..).zip([(100, U256::from(100)), (220, U256::ZERO)]) {
        let (output, logs) =
            claim_reward_in_block(&mut db, claim(requested), claim_time, block).unwrap();
        let gem_id = IAgentReward::claimRewardCall::abi_decode_returns(&output).unwrap();
        let claimed = logs
            .iter()
            .find_map(|log| IAgentReward::RewardsClaimed::decode_log(log).ok())
            .unwrap()
            .data;
        assert_eq!(claimed.account, CCA);
        assert_eq!(claimed.pool, 2);
        assert!(logs.iter().any(|log| log.address == AGENT_REWARD_ADDRESS
            && IAgentReward::RewardsClaimed::decode_log(log).is_ok()));
        assert_eq!(claimed.promisLoadMinor, U256::from(load));
        let issued = logs
            .iter()
            .find_map(|log| {
                outbe_gemfactory::precompile::IGemFactory::GemIssued::decode_log(log).ok()
            })
            .unwrap()
            .data;
        assert_eq!(issued.gemId, gem_id);
        assert_eq!(claimed.gemId, gem_id);
        with_storage(&mut db, |storage| {
            let gem = outbe_gem::api::get_view(&storage, gem_id).unwrap().unwrap();
            assert_eq!(gem.item.owner, CCA);
            assert_eq!(gem.item.promis_load_minor, U256::from(load));
            assert_eq!(gem.item.entry_price_minor, discounted(price));
            assert_eq!(gem.item.issuance_currency, 840);
            assert_eq!(gem.item.reference_currency, 840);
            assert_eq!(gem.item.issued_at, claim_time);
            assert_eq!(
                api::get_cca(&storage, CCA).unwrap().bondedMinor,
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
            U256::ZERO
        )
    });
    assert!(claim_reward(&mut db, claim(U256::ZERO), claim_time).is_err());
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
        assert_eq!(record.bondedMinor, U256::ZERO);
        assert_eq!(record.name, "Test CCA");
        // The terminal CCA job permits bounded deletion; its seal remains permanent.
        assert_eq!(api::reward_weight(&s, CCA, 20231115).unwrap(), U256::ZERO);
        assert!(api::credis_forfeited(&s, CCA, 20231115, U256::ONE).is_err());
    });
    assert!(!tx(
        &mut db,
        U256::ZERO,
        ICcaRegistry::claimUnbondedCall {}.abi_encode(),
        NOW + UNBOND_COOLDOWN_SECONDS
    )
    .is_success());
}
#[test]
fn bond_past_128_active_ccas_accepts_value_and_updates_the_set() {
    let mut db = CacheDB::new(EmptyDB::default());
    let code = Bytecode::new_raw(Bytes::from_static(&[0xef]));
    db.insert_account_info(
        CCA_REGISTRY_ADDRESS,
        AccountInfo {
            code_hash: code.hash_slow(),
            code: Some(code),
            ..Default::default()
        },
    );
    db.insert_account_info(
        CCA,
        AccountInfo {
            balance: BOND_REQUIREMENT,
            ..Default::default()
        },
    );
    with_storage(&mut db, |storage| {
        for id in 1..=128u64 {
            let cca = Address::from_word(U256::from(id).into());
            storage
                .increase_balance(CCA_REGISTRY_ADDRESS, BOND_REQUIREMENT)
                .unwrap();
            runtime::bond(storage.clone(), cca, BOND_REQUIREMENT, "Test CCA".into()).unwrap();
        }
        assert_eq!(CcaContract::new(storage).active.len().unwrap(), 128);
    });
    let registry_before = db.cache.accounts[&CCA_REGISTRY_ADDRESS].info.balance;
    let caller_before = db.cache.accounts[&CCA].info.balance;
    let outcome = tx(
        &mut db,
        BOND_REQUIREMENT,
        ICcaRegistry::bondCall {
            name: "Overflow CCA".into(),
        }
        .abi_encode(),
        NOW,
    );
    assert!(outcome.is_success());
    assert_eq!(outcome.logs().len(), 1);
    assert_eq!(
        db.cache.accounts[&CCA].info.balance,
        caller_before - BOND_REQUIREMENT
    );
    assert_eq!(
        db.cache.accounts[&CCA_REGISTRY_ADDRESS].info.balance,
        registry_before + BOND_REQUIREMENT
    );
    with_storage(&mut db, |storage| {
        assert!(api::is_active(&storage, CCA).unwrap());
        assert_eq!(CcaContract::new(storage).active.len().unwrap(), 129);
    });
}
