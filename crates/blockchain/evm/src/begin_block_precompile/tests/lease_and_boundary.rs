use super::super::cycle::enforce_tee_lease_deadlines;
use super::*;
use alloy_primitives::address;
use outbe_primitives::addresses::STAKING_ADDRESS;
use outbe_primitives::time::SECONDS_PER_DAY;

/// Same funded base the felony path burns by the admitted 8% (`1_000_000 * 8 / 100`).
const FUNDED_STAKE: u64 = 1_000_000;
const STAKE_AFTER_EIGHT_PERCENT: u64 = 920_000;

fn fund_staking_escrow(storage: StorageHandle<'_>, validators: &[Address], escrow: u64) {
    let staking = outbe_staking::contract::Staking::new(storage.clone());
    let amount = U256::from(FUNDED_STAKE);
    for validator in validators {
        staking.stake_amount.write(validator, amount).unwrap();
    }
    staking
        .total_staked
        .write(amount * U256::from(validators.len()))
        .unwrap();
    storage
        .increase_balance(STAKING_ADDRESS, U256::from(escrow))
        .unwrap();
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
        fund_staking_escrow(storage.clone(), &[VALIDATOR], FUNDED_STAKE);
        let ctx = runtime_ctx(storage.clone());
        assert_eq!(enforce_tee_lease_deadlines(&ctx).unwrap(), 1);
        let vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        let jailed = vs.get_validator(VALIDATOR).unwrap().unwrap();
        assert_eq!(jailed.status, outbe_validatorset::runtime::status::JAILED);
        let staking = outbe_staking::contract::Staking::new(storage.clone());
        assert_eq!(
            staking.get_stake(VALIDATOR).unwrap(),
            U256::from(STAKE_AFTER_EIGHT_PERCENT)
        );
        assert_eq!(jailed.slash_count, 1);
        let limit = outbe_promislimit::PromisLimitContract::new(storage.clone());
        assert_eq!(limit.promis_limit().unwrap(), U256::ZERO);
        assert_eq!(limit.native_rest().unwrap(), U256::from(80_000u64));
        assert_eq!(
            staking.get_total_staked().unwrap(),
            U256::from(STAKE_AFTER_EIGHT_PERCENT)
        );
        assert_eq!(
            storage.balance(STAKING_ADDRESS).unwrap(),
            U256::from(STAKE_AFTER_EIGHT_PERCENT)
        );
        assert_eq!(enforce_tee_lease_deadlines(&ctx).unwrap(), 0);
        assert_eq!(vs.get_validator(VALIDATOR).unwrap().unwrap(), jailed);
        assert_eq!(limit.native_rest().unwrap(), U256::from(80_000u64));
        assert_eq!(
            staking.get_stake(VALIDATOR).unwrap(),
            U256::from(STAKE_AFTER_EIGHT_PERCENT)
        );
        assert_eq!(
            storage.balance(STAKING_ADDRESS).unwrap(),
            U256::from(STAKE_AFTER_EIGHT_PERCENT)
        );
    });
}

#[test]
fn tee_lease_sweep_treats_missing_post_bootstrap_binding_as_overdue() {
    let mut provider = configured_storage(2, 1_700_000_200);
    provider.enter(|storage| {
        fund_staking_escrow(storage.clone(), &[VALIDATOR], FUNDED_STAKE);
        let ctx = runtime_ctx(storage.clone());
        assert_eq!(enforce_tee_lease_deadlines(&ctx).unwrap(), 1);
        let vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        let jailed = vs.get_validator(VALIDATOR).unwrap().unwrap();
        assert_eq!(jailed.status, outbe_validatorset::runtime::status::JAILED);
        assert_eq!(jailed.slash_count, 1);
        assert_eq!(
            outbe_staking::contract::Staking::new(storage.clone())
                .get_stake(VALIDATOR)
                .unwrap(),
            U256::from(STAKE_AFTER_EIGHT_PERCENT)
        );
        assert_eq!(
            storage.balance(STAKING_ADDRESS).unwrap(),
            U256::from(STAKE_AFTER_EIGHT_PERCENT)
        );
    });
}

#[test]
fn tee_lease_sweep_reexecution_and_timestamp_jump_are_deterministic() {
    let deadline = 1_700_000_100;
    let jumped_timestamp = deadline + 10 * 14 * SECONDS_PER_DAY;
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
fn tee_lease_sweep_later_failure_rolls_back_whole_sweep() {
    let deadline = 1_700_000_100;
    let later = address!("0x2222222222222222222222222222222222222222");
    let mut provider = configured_storage(2, deadline);
    install_validator_lease(&mut provider, deadline);
    provider.enter(|storage| {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.register_validator(OWNER, later, &[8u8; 48]).unwrap();
        vs.mark_pending(later).unwrap();
        vs.confirm_validator_ready(later).unwrap();
        vs.activate_validator_via_boundary_for_test(later).unwrap();
        // One 8% burn is 80_000. The second overdue validator cannot be paid.
        fund_staking_escrow(storage, &[VALIDATOR, later], 80_000);
    });

    let storage_before = provider.storage.clone();
    let logs_before = provider.get_ordered_events().to_vec();
    let swept = provider.enter(|storage| {
        let ctx = runtime_ctx(storage);
        enforce_tee_lease_deadlines(&ctx)
    });
    assert!(swept.is_err());
    assert_eq!(provider.storage, storage_before);
    assert_eq!(provider.get_ordered_events(), logs_before.as_slice());

    provider.enter(|storage| {
        let vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        let staking = outbe_staking::contract::Staking::new(storage.clone());
        for validator in [VALIDATOR, later] {
            let record = vs.get_validator(validator).unwrap().unwrap();
            assert_eq!(record.status, outbe_validatorset::runtime::status::ACTIVE);
            assert_eq!(record.slash_count, 0);
            assert_eq!(
                staking.get_stake(validator).unwrap(),
                U256::from(FUNDED_STAKE)
            );
        }
        assert_eq!(
            storage.balance(STAKING_ADDRESS).unwrap(),
            U256::from(80_000u64)
        );
    });
}

/// The bridge sync at the boundary is best effort: with no sub-call driver
/// (this provider stubs nothing) the controller call fails, and the boundary
/// still commits epoch 1, its active-set hash, and the epoch-1 committee snapshot.
#[test]
fn boundary_outcome_survives_unavailable_hyperlane_sync() {
    let mut provider = configured_storage(2, 2);
    let artifact = boundary_noop();
    let expected_committee_hash = artifact.committee_set_hash;
    let expected_active_set_hash = artifact.reshare.active_set_hash;

    provider.enter(|storage| {
        assert!(
            storage
                .call(Address::ZERO, U256::ZERO, Bytes::new())
                .is_err(),
            "HashMapStorageProvider has no sub-call driver, so Hyperlane sync sub-calls fail"
        );
        let vs_before = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        assert_eq!(vs_before.epoch_snapshot().unwrap().number, U256::ZERO);

        let input = SystemTxInputV2::BoundaryOutcome { artifact }
            .encode()
            .unwrap();
        dispatch(storage, &input, SYSTEM_ADDRESS, U256::ZERO).unwrap();
    });

    provider.enter(|storage| {
        let vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        assert_eq!(vs.epoch_snapshot().unwrap().number, U256::from(1u64));
        assert_eq!(
            vs.active_consensus_set_hash().unwrap(),
            expected_active_set_hash
        );
        let snapshot_key = outbe_validatorset::committee_snapshot_key(1, expected_committee_hash);
        let snapshot = outbe_validatorset::read_committee_snapshot(storage, snapshot_key)
            .unwrap()
            .expect("epoch-1 committee snapshot must be committed despite unavailable sub-call");
        assert_eq!(snapshot.vrf_material_version, 1);
        assert_eq!(snapshot.vrf_group_public_key_bytes, vec![0x42u8; 96]);
    });
}
