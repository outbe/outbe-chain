// Supported-workload samples, not bounds on arbitrary governance-installed code or state.
use super::receipts_and_gas_support::*;
use super::*;

#[test]
fn calibration_supported_committee_and_unbonding_envelope() {
    let validators = committee(test_evm_signer().address());
    for full_exit in [false, true] {
        run_prefix(
            (
                if full_exit {
                    "256 validators x64 claims Oracle slash"
                } else {
                    "256 active x63 claims TEE expiry"
                },
                &validators,
                (150, GENESIS + 120),
            ),
            |storage| seed_committee_unbonding(storage, &validators, full_exit),
            Vec::new(),
            None,
            false,
        );
    }
}

#[test]
fn calibration_supported_liveness_overlap() {
    let validators = committee(test_evm_signer().address());
    for active_count in [255, 256] {
        run_prefix(
            (
                &format!("{active_count} active x256 newcomer domains"),
                &validators[..active_count],
                (150, GENESIS + 120),
            ),
            |storage| {
                let controller =
                    outbe_hyperlanecontroller::schema::HyperlaneControllerContract::new(storage);
                controller
                    .ica_router
                    .write(Address::repeat_byte(0x82))
                    .unwrap();
                for domain in 1..=256 {
                    controller.domains.push(domain).unwrap();
                }
            },
            Vec::new(),
            None,
            false,
        );
    }
}

#[test]
fn calibrated_canonical_envelope_fits_artifact_limit() {
    let handlers = outbe_cycle::triggers::ACTIVE_TRIGGERS
        .iter()
        .try_fold(0u64, |sum, spec| sum.checked_add(spec.work_budget_gas()))
        .unwrap();
    assert_eq!(outbe_cycle::triggers::ACTIVE_TRIGGERS.len(), 7);
    assert_eq!(handlers, 22_093_000);
    assert!(handlers < outbe_primitives::system_tx::SYSTEM_TX_ARTIFACT_GAS_LIMIT);
}

#[test]
fn calibration_bootstrap_and_dense_cleanup_overlap() {
    let proposer = test_evm_signer().address();
    run_prefix(
        (
            "bootstrap +256 accrual +3x256 cleanup +248 bid GC",
            &[(proposer, dummy_pubkey(0xa2))],
            (1, GENESIS + SECONDS_PER_DAY + 60),
        ),
        |storage| {
            let day = 20231231;
            let agent = outbe_agentreward::AgentRewardContract::new(storage.clone());
            let rewards = outbe_rewards::schema::Rewards::new(storage.clone());
            let cca = outbe_ccaregistry::schema::CcaContract::new(storage.clone());
            agent
                .cleanup_jobs
                .create(&outbe_agentreward::schema::AccrualJob {
                    seq: 0,
                    day,
                    pool: 0,
                    pool_minor: U256::ZERO,
                    next_index: 0,
                    credited_minor: U256::ZERO,
                    recipient_count: 256,
                    total_weight: U256::ZERO,
                })
                .unwrap();
            agent.cleanup_tail.write(1).unwrap();
            agent.waa_address_count.write(&day.into(), 256).unwrap();
            agent.waa_total_count.write(&day.into(), 256).unwrap();
            rewards.day_cleanup_tail.write(1).unwrap();
            rewards.day_cleanup_day_at.write(&0, day).unwrap();
            rewards.last_sealed_day.write(day).unwrap();
            rewards.day_cleanup_scheduled.write(&day, true).unwrap();
            rewards.day_voter_count.write(&day, 256).unwrap();
            rewards
                .day_total_votes
                .write(&day, U256::from(256))
                .unwrap();
            cca.day_cleanup_tail.write(1).unwrap();
            cca.day_cleanup_queue.write(&0, day).unwrap();
            cca.day_cleanup_scheduled.write(&day, true).unwrap();
            cca.reward_day_count.write(&day, 256).unwrap();
            for index in 0..256u32 {
                let address = numbered_test_address(0x92, u64::from(index));
                let akey =
                    outbe_agentreward::AgentRewardContract::address_index_key(day.into(), index);
                let ckey =
                    outbe_agentreward::AgentRewardContract::tribute_count_key(day.into(), address);
                agent.waa_addresses.write(&akey, address).unwrap();
                agent.waa_tribute_counts.write(&ckey, 1).unwrap();
                agent.waa_member_index.write(&ckey, index + 1).unwrap();
                rewards
                    .day_voter_at
                    .get_nested(&day)
                    .write(&index, address)
                    .unwrap();
                rewards
                    .day_votes
                    .get_nested(&day)
                    .write(&address, 1)
                    .unwrap();
                let index_key = B256::from(U256::from((u64::from(day) << 32) | u64::from(index)));
                let member = outbe_primitives::storage::keys::address_u32_key(address, day);
                cca.reward_day_addresses.write(&index_key, address).unwrap();
                cca.reward_day_members.write(&member, true).unwrap();
                cca.gratis_minor_per_utc_day
                    .write(&member, U256::ONE)
                    .unwrap();
            }
            seed_cycle_tick_genesis(storage.clone(), GENESIS, proposer).unwrap();
            queue_accepted_cleanup(storage.clone(), 1);
            seed_previous_day_vwap(
                &storage,
                GENESIS + SECONDS_PER_DAY + 60,
                U256::from(1_000_000),
            );
            seed_dense_voter_participation(storage.clone(), 20240101, 256).unwrap();
            seed_dense_agent_recipients(storage, 20240101, 128).unwrap();
        },
        Vec::new(),
        None,
        false,
    );
}

#[test]
fn calibration_boundary_with_nested_ism_calls() {
    use outbe_hyperlanecontroller::sol_ext::{IInterchainAccountRouter, IStorageMultisigIsm};
    let validators = committee(test_evm_signer().address());
    let validators = &validators[..255];
    let ism = Address::repeat_byte(0xb1);
    let router = Address::repeat_byte(0xb2);
    let local = u32::try_from(CHAIN_ID).unwrap();
    let contracts = vec![
        (
            ism,
            returning_contract(vec![(
                IStorageMultisigIsm::validatorsAndThresholdCall::SELECTOR,
                IStorageMultisigIsm::validatorsAndThresholdCall::abi_encode_returns(
                    &IStorageMultisigIsm::validatorsAndThresholdReturn {
                        _0: Vec::new(),
                        _1: 0,
                    },
                ),
            )]),
        ),
        (
            router,
            returning_contract(vec![(
                IInterchainAccountRouter::quoteGasPaymentCall::SELECTOR,
                IInterchainAccountRouter::quoteGasPaymentCall::abi_encode_returns(&U256::ZERO),
            )]),
        ),
        (RELAY, Bytecode::new_legacy(bridge_work(false).into())),
    ];
    let boundary = boundary_with_epoch(1, true, validators.to_vec());
    let (phases, _, logs, _) = run_prefix(
        (
            "boundary255 validators x256 domains nested ISM",
            validators,
            (150, GENESIS + 120),
        ),
        |storage| {
            let controller =
                outbe_hyperlanecontroller::schema::HyperlaneControllerContract::new(storage);
            controller.ica_router.write(router).unwrap();
            controller.domains.push(local).unwrap();
            controller.ism_by_domain.write(&local, ism).unwrap();
            for domain in 1..=255 {
                assert_ne!(domain, local);
                controller.domains.push(domain).unwrap();
                controller.ism_by_domain.write(&domain, ism).unwrap();
            }
        },
        contracts,
        Some(boundary),
        false,
    );
    assert!(phases.iter().any(|(phase, _)| phase == "boundary_outcome"));
    assert!(logs.iter().any(|log| log.data.topics().first()
        == Some(&keccak256("ValidatorsAndThresholdApplied(uint8,uint256)"))));
    assert_eq!(
        logs.iter()
            .filter(|log| log.data.topics().first()
                == Some(&keccak256("RemoteCallDispatched(uint32,bytes32,uint256)")))
            .count(),
        255
    );
}

#[test]
fn calibration_noop_and_calendar_horizon() {
    let proposer = test_evm_signer().address();
    for elapsed_days in [0, 1, 65] {
        let (_, handlers, logs, _) = run_prefix(
            (
                &format!("calendar elapsed_days={elapsed_days}"),
                &[(proposer, dummy_pubkey(0xa2))],
                (1, GENESIS + elapsed_days * SECONDS_PER_DAY + 60),
            ),
            |storage| {
                seed_cycle_tick_genesis(storage.clone(), GENESIS, proposer).unwrap();
                for spec in outbe_cycle::triggers::ACTIVE_TRIGGERS {
                    outbe_cycle::schema::Cycle::new(storage.clone())
                        .last_executed_at
                        .write(&spec.id, GENESIS + 1)
                        .unwrap();
                }
                if elapsed_days == 1 {
                    let mut agent = outbe_agentreward::AgentRewardContract::new(storage);
                    agent
                        .increment_waa_tribute(20240101u32.into(), Address::repeat_byte(0x44))
                        .unwrap();
                    agent
                        .pending_total_minor
                        .write(
                            &(outbe_agentreward::schema::RewardPool::Waa as u8),
                            U256::MAX,
                        )
                        .unwrap();
                }
            },
            Vec::new(),
            None,
            false,
        );
        if elapsed_days == 0 {
            assert!(handlers.is_empty());
        } else {
            assert!(handlers.iter().any(|(id, _)| *id == 0));
            if elapsed_days == 1 {
                assert!(logs.iter().any(|log| log.data.topics().first()
                    == Some(
                        &outbe_cycle::ICycle::DayEmissionLimitAllocationFailed::SIGNATURE_HASH
                    )));
            }
        }
    }
}

fn seed_committee_unbonding(
    storage: StorageHandle<'_>,
    validators: &[(Address, [u8; 48])],
    full_exit: bool,
) {
    let mut staking = outbe_staking::contract::Staking::new(storage.clone());
    staking
        .config_unbonding_period
        .write(SECONDS_PER_DAY)
        .unwrap();
    for (address, _) in validators {
        staking
            .topup(*address, *address, U256::from(63_000))
            .unwrap();
        storage
            .increase_balance(STAKING_ADDRESS, U256::from(63_000))
            .unwrap();
        for _ in 0..63 {
            staking.unstake(*address, U256::from(1_000)).unwrap();
        }
        if full_exit {
            staking.unstake(*address, U256::from(1_000)).unwrap();
        }
        assert_eq!(
            staking.per_val_unbonding_len.read(address).unwrap(),
            if full_exit { 64 } else { 63 }
        );
    }
    if full_exit {
        let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
        oracle.config_is_initialized.write(true).unwrap();
        oracle.config_slash_window.write(150).unwrap();
        oracle
            .config_min_valid_per_window
            .write(outbe_oracle::schema::SCALE_1E18)
            .unwrap();
        oracle
            .config_slash_fraction
            .write(outbe_oracle::schema::SCALE_1E18 / U256::from(20))
            .unwrap();
        for (address, _) in validators {
            oracle.penalty_miss_count.write(address, 1).unwrap();
        }
    } else {
        seed_expiring_tee_nodes(
            storage,
            &validators.iter().map(|v| v.0).collect::<Vec<_>>(),
            GENESIS + 120,
        )
        .unwrap();
    }
}
