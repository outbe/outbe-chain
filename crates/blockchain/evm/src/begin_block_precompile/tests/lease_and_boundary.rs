use super::super::cycle::{enforce_tee_lease_deadlines, run_ocomp_recovery_sweep};
use super::*;

#[test]
fn mandatory_cycle_tick_sweep_closes_due_ocomp_recovery_when_lifecycle_is_disabled() {
    let mut provider = HashMapStorageProvider::new_with_chain_identity(CHAIN_ID, GENESIS_HASH);
    provider.set_block_number(1);
    StorageHandle::enter(&mut provider, |storage| {
        let mut validators = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        validators.config_owner.write(OWNER).unwrap();
        validators.set_config_max_validators(1).unwrap();
        validators
            .register_validator(OWNER, VALIDATOR, &[0x41; 48])
            .unwrap();
        validators
            .test_activate_validator_canonically(
                VALIDATOR,
                outbe_validatorset::StakeProjection::new(U256::from(1_000), None),
                U256::from(1_000),
            )
            .unwrap();

        let mut staking = outbe_staking::contract::Staking::new(storage.clone());
        staking.config_min_stake.write(U256::from(1_000)).unwrap();
        staking
            .stake_amount
            .write(&VALIDATOR, U256::from(1_000))
            .unwrap();
        staking.total_staked.write(U256::from(1_000)).unwrap();
        storage
            .set_balance(
                outbe_primitives::addresses::STAKING_ADDRESS,
                U256::from(1_000),
            )
            .unwrap();
        staking.record_ocomp_miss(VALIDATOR).unwrap();
    });

    provider.set_block_number(43_201);
    StorageHandle::enter(&mut provider, |storage| {
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(43_201, 1_000, CHAIN_ID),
            storage.clone(),
        );
        run_ocomp_recovery_sweep(&ctx).unwrap();
        assert!(matches!(
            outbe_validatorset::contract::ValidatorSet::new(storage)
                .validator_lifecycle(VALIDATOR)
                .unwrap(),
            outbe_validatorset::ValidatorLifecycle::JailRetained(_)
        ));
    });
}

fn install_validator_lease(provider: &mut HashMapStorageProvider, valid_until: u64) {
    provider.enter(|storage| {
        let registry = outbe_teeregistry::TeeRegistry::new(storage);
        let node_hash = B256::repeat_byte(0x61);
        registry
            .validator_v1_node_hash
            .write(&VALIDATOR, node_hash)
            .unwrap();
        registry
            .v1_node_enclave_id
            .write(&node_hash, B256::repeat_byte(0x62))
            .unwrap();
        registry
            .v1_node_binding_id
            .write(&node_hash, B256::repeat_byte(0x63))
            .unwrap();
        registry
            .v1_node_intent_hash
            .write(&node_hash, B256::repeat_byte(0x64))
            .unwrap();
        registry
            .v1_node_valid_until
            .write(&node_hash, valid_until)
            .unwrap();
    });
}

#[test]
fn tee_lease_sweep_skips_bootstrap_then_jails_at_exact_deadline_once() {
    let deadline = 1_700_000_100;
    let mut bootstrap = configured_storage(1, deadline);
    bootstrap.enter(|storage| {
        let ctx = runtime_ctx(storage.clone());
        assert_eq!(enforce_tee_lease_deadlines(&ctx).unwrap(), 0);
        let vs = outbe_validatorset::contract::ValidatorSet::new(storage);
        assert_eq!(
            vs.get_validator(VALIDATOR).unwrap().unwrap().status,
            outbe_validatorset::runtime::status::ACTIVE
        );
    });

    let mut before = configured_storage(2, deadline - 1);
    install_validator_lease(&mut before, deadline);
    before.enter(|storage| {
        let ctx = runtime_ctx(storage.clone());
        assert_eq!(enforce_tee_lease_deadlines(&ctx).unwrap(), 0);
        let vs = outbe_validatorset::contract::ValidatorSet::new(storage);
        assert_eq!(
            vs.get_validator(VALIDATOR).unwrap().unwrap().status,
            outbe_validatorset::runtime::status::ACTIVE
        );
    });

    let mut at_deadline = configured_storage(2, deadline);
    install_validator_lease(&mut at_deadline, deadline);
    at_deadline.enter(|storage| {
        let ctx = runtime_ctx(storage.clone());
        assert_eq!(enforce_tee_lease_deadlines(&ctx).unwrap(), 1);
        let vs = outbe_validatorset::contract::ValidatorSet::new(storage);
        let jailed = vs.get_validator(VALIDATOR).unwrap().unwrap();
        assert_eq!(jailed.status, outbe_validatorset::runtime::status::JAILED);
        assert_eq!(jailed.slash_count, 0);
        assert_eq!(enforce_tee_lease_deadlines(&ctx).unwrap(), 0);
        assert_eq!(vs.get_validator(VALIDATOR).unwrap().unwrap(), jailed);
    });
}

#[test]
fn tee_lease_sweep_treats_missing_post_bootstrap_binding_as_overdue() {
    let mut provider = configured_storage(2, 1_700_000_200);
    provider.enter(|storage| {
        let ctx = runtime_ctx(storage.clone());
        assert_eq!(enforce_tee_lease_deadlines(&ctx).unwrap(), 1);
        let vs = outbe_validatorset::contract::ValidatorSet::new(storage);
        let jailed = vs.get_validator(VALIDATOR).unwrap().unwrap();
        assert_eq!(jailed.status, outbe_validatorset::runtime::status::JAILED);
        assert_eq!(jailed.slash_count, 0);
    });
}

#[test]
fn tee_lease_sweep_reexecution_and_timestamp_jump_are_deterministic() {
    let deadline = 1_700_000_100;
    let jumped_timestamp = deadline + 10 * 1_209_600;
    let mut base = configured_storage(12, jumped_timestamp);
    install_validator_lease(&mut base, deadline);
    let parent = base.storage.clone();

    let run = || {
        let mut provider = provider_from_storage(12, jumped_timestamp, parent.clone());
        let jailed = provider.enter(|storage| {
            let ctx = runtime_ctx(storage);
            enforce_tee_lease_deadlines(&ctx).unwrap()
        });
        (jailed, provider.storage)
    };

    let first = run();
    let replay = run();
    assert_eq!(first.0, 1);
    assert_eq!(first, replay);
}

#[test]
fn dispatch_boundary_outcome_roundtrip_noop() {
    let mut provider = configured_storage(2, 2);
    provider.enter(|storage| {
        let input = SystemTxInputV2::BoundaryOutcome {
            artifact: boundary_noop(),
        }
        .encode()
        .unwrap();
        dispatch(storage, &input, SYSTEM_ADDRESS, U256::ZERO).unwrap();
    });
}

#[test]
fn boundary_outcome_records_announced_tee_recipient_pubkeys() {
    let mut provider = configured_storage(2, 2);
    let recipient = B256::repeat_byte(0x7A);
    let offer_public_key = B256::repeat_byte(0x7B);
    let node_hash = B256::repeat_byte(0x7C);

    provider.enter(|storage| {
        let reg = outbe_teeregistry::TeeRegistry::new(storage.clone());
        reg.tribute_offer_public_key
            .write(offer_public_key)
            .unwrap();
        reg.validator_v1_node_hash
            .write(&VALIDATOR, node_hash)
            .unwrap();
        assert_eq!(reg.announced_recipient_key(VALIDATOR).unwrap(), B256::ZERO);

        let mut artifact = boundary_noop();
        artifact.tee_recipient_pubkeys = vec![(VALIDATOR, recipient)];
        let input = SystemTxInputV2::BoundaryOutcome { artifact }
            .encode()
            .unwrap();
        dispatch(storage, &input, SYSTEM_ADDRESS, U256::ZERO).unwrap();
    });

    // The boundary-announced recipient key is now readable from the registry.
    provider.enter(|storage| {
        let reg = outbe_teeregistry::TeeRegistry::new(storage);
        assert_eq!(reg.announced_recipient_key(VALIDATOR).unwrap(), recipient);
        assert_eq!(reg.offer_public_key().unwrap(), offer_public_key);
        assert_eq!(
            reg.validator_v1_node_hash.read(&VALIDATOR).unwrap(),
            node_hash
        );
    });
}

fn strict_upgrade_penalty_fixture(updated: bool) -> HashMapStorageProvider {
    let mut provider = configured_storage(49, 10_000);
    install_validator_lease(&mut provider, 100_000);
    provider.enter(|storage| {
        let registry = outbe_teeregistry::TeeRegistry::new(storage.clone());
        registry
            .strict_upgrade_proposal
            .write(U256::from(7))
            .unwrap();
        registry.strict_upgrade_height.write(50).unwrap();
        registry
            .strict_upgrade_mrenclave
            .write(B256::repeat_byte(0x71))
            .unwrap();
        registry
            .strict_upgrade_successor
            .write(B256::repeat_byte(0x72))
            .unwrap();
        registry
            .strict_upgrade_predecessor
            .write(B256::repeat_byte(0x73))
            .unwrap();
        registry
            .active_v1_policy_proposal_id
            .write(U256::from(7))
            .unwrap();
        if updated {
            registry
                .v1_node_policy_hash
                .write(&B256::repeat_byte(0x61), B256::repeat_byte(0x72))
                .unwrap();
            registry
                .v1_node_mrenclave
                .write(&B256::repeat_byte(0x61), B256::repeat_byte(0x71))
                .unwrap();
        }
        let staking = outbe_staking::contract::Staking::new(storage.clone());
        staking.config_min_stake.write(U256::from(1_000)).unwrap();
        staking
            .stake_amount
            .write(&VALIDATOR, U256::from(8_000))
            .unwrap();
        staking.total_staked.write(U256::from(8_000)).unwrap();
        staking.unbonding_count.write(1).unwrap();
        staking.per_val_unbonding_head.write(&VALIDATOR, 1).unwrap();
        staking.unbonding_validator.write(&0, VALIDATOR).unwrap();
        staking
            .unbonding_amount
            .write(&0, U256::from(2_000))
            .unwrap();
        staking.unbonding_complete_time.write(&0, 10_000).unwrap();
        storage
            .set_balance(
                outbe_primitives::addresses::STAKING_ADDRESS,
                U256::from(10_000),
            )
            .unwrap();
    });
    provider
}

#[test]
fn strict_upgrade_penalty_is_exact_height_once_and_includes_matured_unbonding() {
    let mut provider = strict_upgrade_penalty_fixture(false);
    let original = provider.storage.clone();
    provider.enter(|storage| {
        crate::executor::enforce_enclave_upgrade_deadline(&runtime_ctx(storage)).unwrap()
    });
    assert_eq!(provider.storage, original, "no penalty before H");
    provider.set_block_number(50);
    provider.enter(|storage| {
        let ctx = runtime_ctx(storage.clone());
        crate::executor::enforce_enclave_upgrade_deadline(&ctx).unwrap();
        let mut staking = outbe_staking::contract::Staking::new(storage.clone());
        assert_eq!(staking.get_stake(VALIDATOR).unwrap(), U256::from(7_200));
        assert_eq!(
            staking.unbonding_amount.read(&0).unwrap(),
            U256::from(1_800)
        );
        assert_eq!(
            storage
                .balance(outbe_primitives::addresses::STAKING_ADDRESS)
                .unwrap(),
            U256::from(9_000)
        );
        staking.process_unbonding(10_000).unwrap();
        assert_eq!(
            staking.unbonding_amount.read(&0).unwrap(),
            U256::from(1_800),
            "slash postpones a matured claim before withdrawals run"
        );
        let validators = outbe_validatorset::contract::ValidatorSet::new(storage);
        assert_eq!(
            validators.get_validator(VALIDATOR).unwrap().unwrap().status,
            outbe_validatorset::runtime::status::JAILED
        );
    });
    let once = provider.storage.clone();
    provider.enter(|storage| {
        crate::executor::enforce_enclave_upgrade_deadline(&runtime_ctx(storage)).unwrap()
    });
    assert_eq!(
        provider.storage, once,
        "deadline reexecution must not slash twice"
    );
}

#[test]
fn strict_upgrade_penalty_exempts_updated_validator_and_rolls_back_failed_burn() {
    let mut updated = strict_upgrade_penalty_fixture(true);
    updated.set_block_number(50);
    updated.enter(|storage| {
        crate::executor::enforce_enclave_upgrade_deadline(&runtime_ctx(storage.clone())).unwrap();
        let validators = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        assert_eq!(
            validators.get_validator(VALIDATOR).unwrap().unwrap().status,
            outbe_validatorset::runtime::status::ACTIVE
        );
        assert_eq!(
            outbe_staking::contract::Staking::new(storage)
                .get_stake(VALIDATOR)
                .unwrap(),
            U256::from(8_000)
        );
    });
    let mut failed = strict_upgrade_penalty_fixture(false);
    failed.set_block_number(50);
    failed.enter(|storage| {
        storage
            .set_balance(outbe_primitives::addresses::STAKING_ADDRESS, U256::ZERO)
            .unwrap()
    });
    let before = failed.storage.clone();
    let events = failed.events.clone();
    failed.enter(|storage| {
        assert!(crate::executor::enforce_enclave_upgrade_deadline(&runtime_ctx(storage)).is_err())
    });
    assert_eq!(failed.storage, before);
    assert_eq!(
        failed.events, events,
        "failed sweep must roll back jail, slash and idempotency marker"
    );
}
