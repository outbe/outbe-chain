use crate::{
    api, emission_sink,
    precompile::{dispatch, ICca},
    runtime::{self, BOND_REQUIREMENT, UNBOND_COOLDOWN_SECONDS},
    schema::CcaContract,
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
    emission_sink::distribute_daily(&ctx, WorldwideDay::from(20231114), amount).unwrap()
}
fn native(amount: u64) -> U256 {
    checked_protocol_to_native(U256::from(amount)).unwrap()
}

#[test]
fn incremental_registration_exit_and_reregistration_preserve_history() {
    run(|storage| {
        assert_eq!(api::cca_state(&storage, ALICE).unwrap() as u8, 0);
        let first = BOND_REQUIREMENT - U256::ONE;
        bond(&storage, ALICE, first);
        assert_eq!(api::cca_state(&storage, ALICE).unwrap() as u8, 2);
        assert!(runtime::position_opened(&storage, ALICE, U256::ONE).is_err());
        bond(&storage, ALICE, U256::ONE);
        assert!(api::is_active(&storage, ALICE).unwrap());
        bond(&storage, ALICE, U256::from(7));
        runtime::position_opened(&storage, ALICE, U256::from(100)).unwrap();
        assert_eq!(reward(&storage, U256::from(20)), U256::ZERO);
        runtime::unbond(storage.clone(), ALICE).unwrap();
        let record = api::get_cca(&storage, ALICE).unwrap();
        assert_eq!(record.selfBond, U256::ZERO);
        assert_eq!(record.unbondAmount, BOND_REQUIREMENT + U256::from(7));
        assert_eq!(record.unbondCompleteTime, NOW + UNBOND_COOLDOWN_SECONDS);
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
        assert_eq!(api::cca_state(&storage, ALICE).unwrap() as u8, 3);
        assert!(runtime::claim_unbonded(storage.clone(), ALICE).is_err());
        runtime::claim_rewards(storage.clone(), ALICE).unwrap();
        assert_eq!(storage.balance(CCA_ADDRESS).unwrap(), U256::ZERO);
        assert_eq!(
            storage.balance(ALICE).unwrap(),
            BOND_REQUIREMENT + U256::from(7) + native(20)
        );
        assert!(runtime::claim_rewards(storage.clone(), ALICE).is_err());
        bond(&storage, ALICE, BOND_REQUIREMENT);
        assert_eq!(
            api::get_cca(&storage, ALICE).unwrap().rewardWeight,
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
            api::get_cca(&storage, ALICE).unwrap().unbondAmount,
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
        runtime::position_opened(&storage, ALICE, U256::from(1)).unwrap();
        runtime::position_opened(&storage, BOB, U256::from(3)).unwrap();
        assert_eq!(reward(&storage, U256::from(11)), U256::ONE);
        assert_eq!(
            api::get_cca(&storage, ALICE).unwrap().claimableRewards,
            native(2)
        );
        assert_eq!(
            api::get_cca(&storage, BOB).unwrap().claimableRewards,
            native(8)
        );
        runtime::unbond(storage.clone(), BOB).unwrap();
        assert_eq!(reward(&storage, U256::from(11)), U256::ZERO);
        assert_eq!(
            api::get_cca(&storage, ALICE).unwrap().claimableRewards,
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
        runtime::position_opened(&storage, ALICE, U256::from(100)).unwrap();
        runtime::unbond(storage.clone(), ALICE).unwrap();
        runtime::position_voided(&storage, ALICE, U256::from(50)).unwrap();
        runtime::position_voided(&storage, ALICE, U256::ZERO).unwrap();
        assert_eq!(
            api::get_cca(&storage, ALICE).unwrap().rewardWeight,
            U256::from(50)
        );
        assert!(runtime::position_voided(&storage, ALICE, U256::from(51)).is_err());
        assert_eq!(
            api::get_cca(&storage, ALICE).unwrap().rewardWeight,
            U256::from(50)
        );
    });
}

#[test]
fn wide_reward_products_do_not_overflow_and_conversion_failure_rolls_back() {
    run(|storage| {
        bond(&storage, ALICE, BOND_REQUIREMENT);
        runtime::position_opened(&storage, ALICE, U256::MAX).unwrap();
        assert_eq!(reward(&storage, U256::from(100)), U256::ZERO);
        let ctx = BlockRuntimeContext::new(BlockContext::default(), storage.clone());
        let before = storage.balance(CCA_ADDRESS).unwrap();
        assert!(emission_sink::distribute_daily(&ctx, 20231115.into(), U256::MAX).is_err());
        assert_eq!(storage.balance(CCA_ADDRESS).unwrap(), before);
        assert_eq!(
            api::get_cca(&storage, ALICE).unwrap().claimableRewards,
            native(100)
        );
        assert!(runtime::position_opened(&storage, ALICE, U256::ONE).is_err());
        assert_eq!(
            api::get_cca(&storage, ALICE).unwrap().rewardWeight,
            U256::MAX
        );
    });
}

#[test]
fn failed_claim_preserves_record_and_balance() {
    run(|storage| {
        bond(&storage, ALICE, BOND_REQUIREMENT);
        runtime::position_opened(&storage, ALICE, U256::ONE).unwrap();
        reward(&storage, U256::from(9));
        let balance = storage.balance(CCA_ADDRESS).unwrap();
        storage.decrease_balance(CCA_ADDRESS, balance).unwrap();
        assert!(runtime::claim_rewards(storage.clone(), ALICE).is_err());
        assert_eq!(
            api::get_cca(&storage, ALICE).unwrap().claimableRewards,
            native(9)
        );
        runtime::unbond(storage.clone(), ALICE).unwrap();
        storage
            .set_block_timestamp(U256::from(NOW + UNBOND_COOLDOWN_SECONDS))
            .unwrap();
        assert!(runtime::claim_unbonded(storage.clone(), ALICE).is_err());
        assert_eq!(
            api::get_cca(&storage, ALICE).unwrap().unbondAmount,
            BOND_REQUIREMENT
        );
        assert_eq!(api::cca_state(&storage, ALICE).unwrap() as u8, 2);
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
            ICca::getCcaCall::abi_decode_returns(&out).unwrap().selfBond,
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
        assert_eq!(api::cca_state(&storage, ALICE).unwrap() as u8, 0);
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
            runtime::position_opened(&storage, cca, U256::ONE).unwrap();
        }
    });
    provider.set_gas_limit(30_000_000);
    provider.enable_production_storage_gas_metering();
    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(reward(&storage, U256::from(128)), U256::ZERO);
        assert!(storage.gas_used().unwrap() < 30_000_000);
    });
}
