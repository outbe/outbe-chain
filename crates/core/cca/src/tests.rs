use crate::{
    api, emission_sink,
    precompile::{dispatch, ICca},
    runtime::{self, BOND_REQUIREMENT, UNBOND_COOLDOWN_SECONDS},
    schema::{CcaContract, CcaRecordEntryExt},
};
use alloy_primitives::{address, Address, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::{
    addresses::CCA_ADDRESS,
    block::{BlockContext, BlockRuntimeContext},
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
    time::WorldwideDay,
    units::checked_protocol_to_native,
};

const ALICE: Address = address!("00000000000000000000000000000000000000a1");
const BOB: Address = address!("00000000000000000000000000000000000000b1");
const NOW: u64 = 1_700_000_000;
const DAY: WorldwideDay = WorldwideDay::new(20231115);
fn run(f: impl FnOnce(StorageHandle<'_>)) {
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_timestamp(U256::from(NOW));
    StorageHandle::enter(&mut provider, f);
}
fn bond(storage: &StorageHandle<'_>, who: Address, amount: U256) {
    // Simulate the real payable boundary's already-credited value.
    storage.increase_balance(CCA_ADDRESS, amount).unwrap();
    dispatch(
        storage.clone(),
        &ICca::bondCall {}.abi_encode(),
        who,
        amount,
    )
    .unwrap();
}
fn reward(storage: &StorageHandle<'_>, amount: U256) -> U256 {
    let ctx = BlockRuntimeContext::new(BlockContext::empty_for_tests(1, NOW, 1), storage.clone());
    emission_sink::distribute_daily(&ctx, DAY, amount).unwrap()
}
fn native(amount: u64) -> U256 {
    checked_protocol_to_native(U256::from(amount)).unwrap()
}

#[test]
fn incremental_registration_exit_and_reregistration_preserve_history() {
    run(|storage| {
        assert!(api::cca_state(&storage, ALICE).is_err());
        assert!(api::get_cca(&storage, ALICE).is_err());
        assert!(!api::is_active(&storage, ALICE).unwrap());
        let first = BOND_REQUIREMENT - U256::ONE;
        bond(&storage, ALICE, first);
        assert_eq!(
            api::cca_state(&storage, ALICE).unwrap(),
            ICca::State::Bonding
        );
        assert!(runtime::position_opened(&storage, ALICE, DAY, U256::ONE).is_err());
        bond(&storage, ALICE, U256::ONE);
        assert!(api::is_active(&storage, ALICE).unwrap());
        assert!(runtime::claim_unbonded(storage.clone(), ALICE).is_err());
        bond(&storage, ALICE, U256::from(7));
        runtime::position_opened(&storage, ALICE, DAY, U256::from(100)).unwrap();
        assert_eq!(reward(&storage, U256::from(20)), U256::ZERO);
        runtime::unbond(storage.clone(), ALICE).unwrap();
        let record = api::get_cca(&storage, ALICE).unwrap();
        assert_eq!(record.cca, ALICE);
        assert_eq!(record.state, ICca::State::Deregistering);
        assert_eq!(record.bondedAmount, BOND_REQUIREMENT + U256::from(7));
        assert_eq!(record.unbondUnlocksAfter, NOW + UNBOND_COOLDOWN_SECONDS);
        assert!(!api::is_active(&storage, ALICE).unwrap());
        assert!(runtime::unbond(storage.clone(), ALICE).is_err());
        assert!(runtime::bond(storage.clone(), ALICE, U256::ONE).is_err());
        storage
            .set_block_timestamp(U256::from(NOW + UNBOND_COOLDOWN_SECONDS - 1))
            .unwrap();
        assert!(runtime::claim_unbonded(storage.clone(), ALICE).is_err());
        storage
            .set_block_timestamp(U256::from(NOW + UNBOND_COOLDOWN_SECONDS))
            .unwrap();
        runtime::claim_unbonded(storage.clone(), ALICE).unwrap();
        assert_eq!(
            storage.balance(ALICE).unwrap(),
            BOND_REQUIREMENT + U256::from(7)
        );
        assert_eq!(
            api::cca_state(&storage, ALICE).unwrap(),
            ICca::State::Deregistered
        );
        assert!(runtime::claim_unbonded(storage.clone(), ALICE).is_err());
        assert_eq!(
            api::get_cca(&storage, ALICE).unwrap().bondedAmount,
            U256::ZERO
        );
        runtime::claim_rewards(storage.clone(), ALICE).unwrap();
        assert_eq!(storage.balance(CCA_ADDRESS).unwrap(), U256::ZERO);
        assert_eq!(
            storage.balance(ALICE).unwrap(),
            BOND_REQUIREMENT + U256::from(7) + native(20)
        );
        assert!(runtime::claim_rewards(storage.clone(), ALICE).is_err());
        bond(&storage, ALICE, BOND_REQUIREMENT);
        assert_eq!(
            api::reward_weight(&storage, ALICE, DAY).unwrap(),
            U256::from(100)
        );
        assert_eq!(
            CcaContract::new(storage.clone()).active.read_all().unwrap(),
            vec![ALICE]
        );
    });
}

#[test]
fn partial_registration_can_exit_and_rejected_calls_do_not_mutate() {
    run(|storage| {
        assert!(runtime::bond(storage.clone(), ALICE, U256::ZERO).is_err());
        assert!(runtime::bond(storage.clone(), Address::ZERO, U256::ONE).is_err());
        assert!(runtime::unbond(storage.clone(), ALICE).is_err());
        bond(&storage, ALICE, U256::ONE);
        runtime::unbond(storage.clone(), ALICE).unwrap();
        assert_eq!(
            api::get_cca(&storage, ALICE).unwrap().bondedAmount,
            U256::ONE
        );
        assert!(CcaContract::new(storage)
            .active
            .read_all()
            .unwrap()
            .is_empty());
    });
}

#[test]
fn proportional_rewards_exclude_inactive_weights_and_recycle_dust() {
    run(|storage| {
        assert_eq!(reward(&storage, U256::from(11)), U256::from(11));
        bond(&storage, ALICE, BOND_REQUIREMENT);
        bond(&storage, BOB, BOND_REQUIREMENT);
        assert_eq!(reward(&storage, U256::from(11)), U256::from(11));
        runtime::position_opened(&storage, ALICE, DAY, U256::from(1)).unwrap();
        runtime::position_opened(&storage, BOB, DAY, U256::from(3)).unwrap();
        assert_eq!(reward(&storage, U256::from(11)), U256::ONE);
        assert_eq!(
            api::get_cca(&storage, ALICE).unwrap().rewardAmount,
            native(2)
        );
        assert_eq!(api::get_cca(&storage, BOB).unwrap().rewardAmount, native(8));
        runtime::unbond(storage.clone(), BOB).unwrap();
        assert_eq!(reward(&storage, U256::from(11)), U256::ZERO);
        assert_eq!(
            api::get_cca(&storage, ALICE).unwrap().rewardAmount,
            native(13)
        );
        runtime::claim_rewards(storage.clone(), BOB).unwrap();
        assert_eq!(storage.balance(BOB).unwrap(), native(8));
        assert_eq!(
            storage.balance(CCA_ADDRESS).unwrap(),
            BOND_REQUIREMENT * U256::from(2) + native(13)
        );
    });
}

#[test]
fn void_subtracts_only_burned_gratis_even_after_exit() {
    run(|storage| {
        bond(&storage, ALICE, BOND_REQUIREMENT);
        runtime::position_opened(&storage, ALICE, DAY, U256::from(100)).unwrap();
        runtime::unbond(storage.clone(), ALICE).unwrap();
        runtime::position_voided(&storage, ALICE, DAY, U256::from(50)).unwrap();
        runtime::position_voided(&storage, ALICE, DAY, U256::ZERO).unwrap();
        assert_eq!(
            api::reward_weight(&storage, ALICE, DAY).unwrap(),
            U256::from(50)
        );
        runtime::position_voided(&storage, ALICE, DAY, U256::from(51)).unwrap();
        assert_eq!(
            api::reward_weight(&storage, ALICE, DAY).unwrap(),
            U256::ZERO
        );
        runtime::position_voided(&storage, ALICE, DAY, U256::ONE).unwrap();
        assert_eq!(
            api::reward_weight(&storage, ALICE, DAY).unwrap(),
            U256::ZERO
        );
    });
}

#[test]
fn wide_reward_products_do_not_overflow_and_conversion_failure_rolls_back() {
    run(|storage| {
        bond(&storage, ALICE, BOND_REQUIREMENT);
        runtime::position_opened(&storage, ALICE, DAY, U256::MAX).unwrap();
        assert_eq!(reward(&storage, U256::from(100)), U256::ZERO);
        let ctx = BlockRuntimeContext::new(BlockContext::default(), storage.clone());
        let before = storage.balance(CCA_ADDRESS).unwrap();
        assert!(emission_sink::distribute_daily(&ctx, 20231115.into(), U256::MAX).is_err());
        assert_eq!(storage.balance(CCA_ADDRESS).unwrap(), before);
        assert_eq!(
            api::get_cca(&storage, ALICE).unwrap().rewardAmount,
            native(100)
        );
        assert!(runtime::position_opened(&storage, ALICE, DAY, U256::ONE).is_err());
        assert_eq!(api::reward_weight(&storage, ALICE, DAY).unwrap(), U256::MAX);
    });
}

#[test]
fn failed_claim_preserves_record_and_balance() {
    run(|storage| {
        bond(&storage, ALICE, BOND_REQUIREMENT);
        runtime::position_opened(&storage, ALICE, DAY, U256::ONE).unwrap();
        reward(&storage, U256::from(9));
        let balance = storage.balance(CCA_ADDRESS).unwrap();
        storage.decrease_balance(CCA_ADDRESS, balance).unwrap();
        assert!(runtime::claim_rewards(storage.clone(), ALICE).is_err());
        assert_eq!(
            api::get_cca(&storage, ALICE).unwrap().rewardAmount,
            native(9)
        );
        runtime::unbond(storage.clone(), ALICE).unwrap();
        storage
            .set_block_timestamp(U256::from(NOW + UNBOND_COOLDOWN_SECONDS))
            .unwrap();
        assert!(runtime::claim_unbonded(storage.clone(), ALICE).is_err());
        assert_eq!(
            api::get_cca(&storage, ALICE).unwrap().bondedAmount,
            BOND_REQUIREMENT
        );
        assert_eq!(
            api::cca_state(&storage, ALICE).unwrap(),
            ICca::State::Deregistering
        );
        assert_eq!(storage.balance(ALICE).unwrap(), U256::ZERO);
    });
}

#[test]
fn timestamp_conversion_and_deadline_overflow_are_checked() {
    run(|storage| {
        bond(&storage, ALICE, BOND_REQUIREMENT);
        storage.set_block_timestamp(U256::from(u64::MAX)).unwrap();
        assert!(runtime::unbond(storage.clone(), ALICE).is_err());
        assert!(api::is_active(&storage, ALICE).unwrap());
    });
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_timestamp(U256::MAX);
    StorageHandle::enter(&mut provider, |storage| {
        bond(&storage, ALICE, BOND_REQUIREMENT);
        assert!(runtime::unbond(storage.clone(), ALICE).is_err());
        assert!(api::is_active(&storage, ALICE).unwrap());
    });
}

#[test]
fn abi_reads_and_nonpayable_selectors() {
    run(|storage| {
        bond(&storage, ALICE, BOND_REQUIREMENT);
        let out = dispatch(
            storage.clone(),
            &ICca::getCcaCall { cca: ALICE }.abi_encode(),
            BOB,
            U256::ZERO,
        )
        .unwrap();
        assert_eq!(
            ICca::getCcaCall::abi_decode_returns(&out)
                .unwrap()
                .bondedAmount,
            BOND_REQUIREMENT
        );
        for data in [
            ICca::getCcaCall { cca: ALICE }.abi_encode(),
            ICca::getCcaStateCall { cca: ALICE }.abi_encode(),
            ICca::unbondCall {}.abi_encode(),
            ICca::claimUnbondedCall {}.abi_encode(),
            ICca::claimRewardsCall {}.abi_encode(),
            ICca::supportsInterfaceCall {
                interfaceId: [0x01, 0xff, 0xc9, 0xa7].into(),
            }
            .abi_encode(),
        ] {
            assert!(dispatch(storage.clone(), &data, ALICE, U256::ONE).is_err());
        }
        assert!(dispatch(storage.clone(), &[], ALICE, U256::ONE).is_err());
        assert!(dispatch(storage, &[0, 1, 2, 3], ALICE, U256::ZERO).is_err());
    });
}

#[test]
fn static_registration_is_rejected_without_state_or_events() {
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_static(true);
    StorageHandle::enter(&mut provider, |storage| {
        assert!(dispatch(
            storage.clone(),
            &ICca::bondCall {}.abi_encode(),
            ALICE,
            U256::ONE
        )
        .is_err());
        assert!(api::cca_state(&storage, ALICE).is_err());
    });
    assert!(provider.get_ordered_events().is_empty());
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
        let next = WorldwideDay::new(20231116);
        bond(&storage, ALICE, BOND_REQUIREMENT);
        bond(&storage, BOB, BOND_REQUIREMENT);
        runtime::position_opened(&storage, ALICE, DAY, U256::from(60)).unwrap();
        runtime::position_opened(&storage, ALICE, DAY, U256::from(40)).unwrap();
        runtime::position_opened(&storage, BOB, DAY, U256::from(100)).unwrap();
        storage
            .set_block_timestamp(U256::from(next.start_timestamp()))
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
            emission_sink::distribute_daily(&ctx, DAY, U256::from(120)).unwrap(),
            U256::ZERO
        );
        assert_eq!(
            api::get_cca(&storage, ALICE).unwrap().rewardAmount,
            native(60)
        );
        assert_eq!(
            api::get_cca(&storage, BOB).unwrap().rewardAmount,
            native(60)
        );
        assert_eq!(
            emission_sink::distribute_daily(&ctx, next, U256::from(120)).unwrap(),
            U256::ZERO
        );
        assert_eq!(
            api::get_cca(&storage, ALICE).unwrap().rewardAmount,
            native(90)
        );
        assert_eq!(
            api::get_cca(&storage, BOB).unwrap().rewardAmount,
            native(150)
        );
        // Historical GRATIS does not carry into an empty day.
        assert_eq!(
            emission_sink::distribute_daily(&ctx, 20231117.into(), U256::from(120)).unwrap(),
            U256::from(120)
        );
        // A later void cannot claw back already accrued rewards.
        runtime::position_voided(&storage, ALICE, next, U256::from(50)).unwrap();
        assert_eq!(
            api::get_cca(&storage, ALICE).unwrap().rewardAmount,
            native(90)
        );
    });
}

#[test]
fn deficits_offset_later_openings_only_in_the_same_cca_day() {
    run(|storage| {
        let next = WorldwideDay::new(20231116);
        bond(&storage, ALICE, BOND_REQUIREMENT);
        bond(&storage, BOB, BOND_REQUIREMENT);
        runtime::position_opened(&storage, ALICE, DAY, U256::from(20)).unwrap();
        runtime::position_voided(&storage, ALICE, DAY, U256::from(50)).unwrap();
        for amount in [10, 20] {
            runtime::position_opened(&storage, ALICE, DAY, U256::from(amount)).unwrap();
            assert_eq!(
                api::reward_weight(&storage, ALICE, DAY).unwrap(),
                U256::ZERO
            );
        }
        runtime::position_opened(&storage, ALICE, DAY, U256::from(15)).unwrap();
        // Reordering the same day's credits and debits gives the same net weight.
        runtime::position_opened(&storage, BOB, DAY, U256::from(65)).unwrap();
        runtime::position_voided(&storage, BOB, DAY, U256::from(50)).unwrap();
        assert_eq!(
            api::reward_weight(&storage, ALICE, DAY).unwrap(),
            U256::from(15)
        );
        assert_eq!(
            api::reward_weight(&storage, BOB, DAY).unwrap(),
            U256::from(15)
        );
        assert_eq!(reward(&storage, U256::from(100)), U256::ZERO);
        assert_eq!(
            api::get_cca(&storage, ALICE).unwrap().rewardAmount,
            native(50)
        );
        assert_eq!(
            api::get_cca(&storage, BOB).unwrap().rewardAmount,
            native(50)
        );
        runtime::position_voided(&storage, ALICE, DAY, U256::from(25)).unwrap();
        runtime::position_opened(&storage, ALICE, next, U256::from(7)).unwrap();
        assert_eq!(
            api::reward_weight(&storage, ALICE, next).unwrap(),
            U256::from(7)
        );
        assert_eq!(
            api::reward_weight(&storage, ALICE, DAY).unwrap(),
            U256::ZERO
        );
        assert_eq!(
            api::reward_weight(&storage, BOB, DAY).unwrap(),
            U256::from(15)
        );
    });
}

#[test]
fn deficit_overflow_rolls_back_and_full_range_can_be_offset() {
    run(|storage| {
        bond(&storage, ALICE, BOND_REQUIREMENT);
        runtime::position_voided(&storage, ALICE, DAY, U256::MAX).unwrap();
        assert!(runtime::position_voided(&storage, ALICE, DAY, U256::ONE).is_err());
        let contract = CcaContract::new(storage.clone());
        let key = CcaContract::reward_weight_key(ALICE, DAY);
        assert_eq!(contract.reward_deficits.read(&key).unwrap(), U256::MAX);
        assert_eq!(
            api::reward_weight(&storage, ALICE, DAY).unwrap(),
            U256::ZERO
        );
        runtime::position_opened(&storage, ALICE, DAY, U256::MAX).unwrap();
        assert_eq!(contract.reward_deficits.read(&key).unwrap(), U256::ZERO);
        assert_eq!(
            api::reward_weight(&storage, ALICE, DAY).unwrap(),
            U256::ZERO
        );
        runtime::position_opened(&storage, ALICE, DAY, U256::ONE).unwrap();
        assert_eq!(api::reward_weight(&storage, ALICE, DAY).unwrap(), U256::ONE);
    });
}

#[test]
fn typed_states_preserve_storage_encoding_and_reject_invalid_words() {
    run(|storage| {
        bond(&storage, ALICE, BOND_REQUIREMENT);
        let contract = CcaContract::new(storage.clone());
        let slot = contract.records.entry(ALICE).state();
        for (word, state) in [
            (0, ICca::State::Bonding),
            (1, ICca::State::Active),
            (2, ICca::State::Deregistering),
            (3, ICca::State::Deregistered),
        ] {
            storage
                .sstore(CCA_ADDRESS, slot.slot(), U256::from(word))
                .unwrap();
            assert_eq!(contract.load(ALICE).unwrap().state, state);
            assert_eq!(api::cca_state(&storage, ALICE).unwrap(), state);
            slot.write(state).unwrap();
            assert_eq!(
                storage.sload(CCA_ADDRESS, slot.slot()).unwrap(),
                U256::from(word)
            );
        }
        for word in [U256::from(4), U256::from(255), U256::from(256), U256::MAX] {
            storage.sstore(CCA_ADDRESS, slot.slot(), word).unwrap();
            assert!(contract.load(ALICE).is_err());
            assert!(api::get_cca(&storage, ALICE).is_err());
            assert!(runtime::bond(storage.clone(), ALICE, U256::ONE).is_err());
            assert_eq!(storage.sload(CCA_ADDRESS, slot.slot()).unwrap(), word);
        }
    });
}
