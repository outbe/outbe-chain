use crate::{
    api,
    constants::{BOND_REQUIREMENT, UNBOND_COOLDOWN_SECONDS},
    precompile::{dispatch, ICcaRegistry},
    runtime,
    schema::{address_day_key, CcaContract, CcaRecordEntryExt},
};
use alloy_primitives::{address, Address, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::{
    addresses::CCA_REGISTRY_ADDRESS,
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
};

const ALICE: Address = address!("00000000000000000000000000000000000000a1");
const BOB: Address = address!("00000000000000000000000000000000000000b1");
const NOW: u64 = 1_700_000_000;
const DAY: u32 = 20231115;
fn run(f: impl FnOnce(StorageHandle<'_>)) {
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_timestamp(U256::from(NOW));
    StorageHandle::enter(&mut provider, f);
}
fn cca_at(id: u32) -> Address {
    Address::from_word(U256::from(id).into())
}

fn seal(storage: &StorageHandle<'_>, day: u32) {
    let ctx = outbe_primitives::block::BlockRuntimeContext::new(
        outbe_primitives::block::BlockContext {
            block_gas_limit: 0,
            user_gas_used: 0,
            block_number: 10_000,
            timestamp: 2_000_000_000,
            chain_id: 1,
            genesis_hash: alloy_primitives::B256::ZERO,
            genesis_timestamp: 1,
            proposer: Address::ZERO,
            validators: Vec::new(),
        },
        storage.clone(),
    );
    outbe_rewards::api::seal_votes_and_fees(&ctx, day).unwrap();
}

fn bond(storage: &StorageHandle<'_>, who: Address, amount: U256) {
    // Simulate the real payable boundary's already-credited value.
    storage
        .increase_balance(CCA_REGISTRY_ADDRESS, amount)
        .unwrap();
    dispatch(
        storage.clone(),
        &ICcaRegistry::bondCall {
            name: "Test CCA".into(),
        }
        .abi_encode(),
        who,
        amount,
    )
    .unwrap();
}
#[test]
fn the_bonded_total_follows_bonds_and_claims_and_needs_the_registry_balance() {
    run(|storage| {
        bond(&storage, ALICE, BOND_REQUIREMENT);
        bond(&storage, BOB, U256::from(7u64));
        assert_eq!(
            api::total_bonded(&storage).unwrap(),
            BOND_REQUIREMENT + U256::from(7u64)
        );

        let shortfall = runtime::bond(storage.clone(), BOB, U256::ONE, "Test CCA".into());
        assert_eq!(
            shortfall.unwrap_err().to_string(),
            outbe_primitives::error::PrecompileError::from(
                crate::errors::CcaError::CustodyShortfall
            )
            .to_string()
        );
        assert_eq!(
            api::total_bonded(&storage).unwrap(),
            BOND_REQUIREMENT + U256::from(7u64)
        );
        assert_eq!(
            api::get_cca(&storage, BOB).unwrap().bondedMinor,
            U256::from(7u64)
        );

        runtime::unbond(storage.clone(), ALICE).unwrap();
        storage
            .set_block_timestamp(U256::from(NOW + UNBOND_COOLDOWN_SECONDS))
            .unwrap();
        runtime::claim_unbonded(storage.clone(), ALICE).unwrap();
        assert_eq!(api::total_bonded(&storage).unwrap(), U256::from(7u64));
        assert_eq!(
            storage.balance(CCA_REGISTRY_ADDRESS).unwrap(),
            U256::from(7u64)
        );
    });
}

#[test]
fn an_unbond_claim_that_leaves_less_than_the_bonded_total_reverts() {
    run(|storage| {
        bond(&storage, ALICE, BOND_REQUIREMENT);
        bond(&storage, BOB, U256::from(7u64));
        storage
            .decrease_balance(CCA_REGISTRY_ADDRESS, U256::from(7u64))
            .unwrap();
        runtime::unbond(storage.clone(), ALICE).unwrap();
        storage
            .set_block_timestamp(U256::from(NOW + UNBOND_COOLDOWN_SECONDS))
            .unwrap();

        let error = runtime::claim_unbonded(storage.clone(), ALICE).unwrap_err();
        assert_eq!(
            error.to_string(),
            outbe_primitives::error::PrecompileError::from(
                crate::errors::CcaError::CustodyShortfall
            )
            .to_string()
        );
        assert_eq!(
            api::get_cca(&storage, ALICE).unwrap().bondedMinor,
            BOND_REQUIREMENT
        );
        assert_eq!(
            api::total_bonded(&storage).unwrap(),
            BOND_REQUIREMENT + U256::from(7u64)
        );
        assert_eq!(
            storage.balance(CCA_REGISTRY_ADDRESS).unwrap(),
            BOND_REQUIREMENT
        );
        assert_eq!(storage.balance(ALICE).unwrap(), U256::ZERO);
    });
}

#[test]
fn active_set_accepts_more_than_128_and_preserves_topup_exit_and_reentry() {
    run(|storage| {
        for id in 1..=1000 {
            bond(&storage, cca_at(id), BOND_REQUIREMENT);
        }
        let contract = CcaContract::new(storage.clone());
        assert_eq!(contract.active.len().unwrap(), 1000);
        let partial = cca_at(1001);
        bond(&storage, partial, BOND_REQUIREMENT - U256::ONE);
        assert_eq!(
            api::cca_state(&storage, partial).unwrap(),
            ICcaRegistry::State::Bonding
        );
        assert_eq!(contract.active.len().unwrap(), 1000);
        bond(&storage, partial, U256::ONE);
        assert!(api::is_active(&storage, partial).unwrap());

        let incumbent = cca_at(1);
        bond(&storage, incumbent, U256::ONE);
        assert_eq!(
            api::get_cca(&storage, incumbent).unwrap().bondedMinor,
            BOND_REQUIREMENT + U256::ONE
        );
        assert!(api::is_active(&storage, incumbent).unwrap());
        assert_eq!(contract.active.len().unwrap(), 1001);

        runtime::unbond(storage.clone(), incumbent).unwrap();
        storage
            .set_block_timestamp(U256::from(NOW + UNBOND_COOLDOWN_SECONDS))
            .unwrap();
        runtime::claim_unbonded(storage.clone(), incumbent).unwrap();
        assert_eq!(contract.active.len().unwrap(), 1000);
        assert_eq!(
            api::cca_state(&storage, incumbent).unwrap(),
            ICcaRegistry::State::Deregistered
        );

        bond(&storage, incumbent, BOND_REQUIREMENT);
        assert_eq!(contract.active.len().unwrap(), 1001);
    });
}

#[test]
fn day_index_is_stable_through_zero_weights_deficits_exit_and_sealing() {
    run(|storage| {
        bond(&storage, ALICE, BOND_REQUIREMENT);
        bond(&storage, BOB, BOND_REQUIREMENT);
        runtime::credis_issued(&storage, ALICE, DAY, U256::from(10)).unwrap();
        runtime::credis_forfeited(&storage, ALICE, DAY, U256::from(15)).unwrap();
        runtime::credis_forfeited(&storage, BOB, DAY, U256::from(7)).unwrap();
        runtime::credis_issued(&storage, ALICE, DAY, U256::from(9)).unwrap();
        runtime::credis_issued(&storage, ALICE, DAY + 1, U256::from(20)).unwrap();
        runtime::unbond(storage.clone(), ALICE).unwrap();
        assert!(api::reward_day_inputs(&storage, DAY).is_err());
        seal(&storage, DAY);
        assert_eq!(
            api::reward_day_inputs(&storage, DAY).unwrap(),
            (2, U256::from(4))
        );
        assert_eq!(
            api::reward_day_entry(&storage, DAY, 0).unwrap(),
            (ALICE, U256::from(4))
        );
        assert_eq!(
            api::reward_day_entry(&storage, DAY, 1).unwrap(),
            (BOB, U256::ZERO)
        );
        assert!(api::reward_day_entry(&storage, DAY, 2).is_err());
        assert!(runtime::credis_forfeited(&storage, BOB, DAY, U256::ONE).is_err());
        assert!(runtime::credis_issued(&storage, BOB, DAY - 1, U256::ONE).is_err());
        assert_eq!(
            api::reward_weight(&storage, ALICE, DAY + 1).unwrap(),
            U256::from(20)
        );
    });
}

#[test]
fn day_count_total_and_cleanup_queue_overflow_roll_back() {
    run(|storage| {
        bond(&storage, ALICE, BOND_REQUIREMENT);
        bond(&storage, BOB, BOND_REQUIREMENT);
        let contract = CcaContract::new(storage.clone());
        contract.reward_day_count.write(&DAY, u32::MAX).unwrap();
        assert!(runtime::credis_forfeited(&storage, ALICE, DAY, U256::ONE).is_err());
        assert_eq!(
            contract
                .gratis_deficit_minor_per_utc_day
                .read(&address_day_key(ALICE, DAY))
                .unwrap(),
            U256::ZERO
        );
        contract.reward_day_count.clear(&DAY).unwrap();
        runtime::credis_issued(&storage, ALICE, DAY, U256::MAX).unwrap();
        assert!(runtime::credis_issued(&storage, BOB, DAY, U256::ONE).is_err());
        assert_eq!(contract.reward_day_count.read(&DAY).unwrap(), 1);
        assert_eq!(api::reward_weight(&storage, BOB, DAY).unwrap(), U256::ZERO);
        seal(&storage, DAY);
        contract.day_cleanup_tail.write(u64::MAX).unwrap();
        let ctx = outbe_primitives::block::BlockRuntimeContext::new(
            outbe_primitives::block::BlockContext {
                block_gas_limit: 0,
                user_gas_used: 0,
                block_number: 0,
                timestamp: 0,
                chain_id: 0,
                genesis_hash: alloy_primitives::B256::ZERO,
                genesis_timestamp: 0,
                proposer: Address::ZERO,
                validators: Vec::new(),
            },
            storage.clone(),
        );
        assert!(api::enqueue_day_cleanup(&ctx, DAY).is_err());
        assert!(!contract.day_cleanup_scheduled.read(&DAY).unwrap());
    });
}

#[test]
fn active_index_capacity_is_checked_before_insertion() {
    run(|storage| {
        let contract = CcaContract::new(storage.clone());
        // The records map reserves its record footprint before the active set.
        storage
            .sstore(
                CCA_REGISTRY_ADDRESS,
                U256::from(<crate::schema::CcaRecord as outbe_primitives::storage::types::StorableType>::SLOTS),
                U256::from(u32::MAX),
            )
            .unwrap();
        storage
            .increase_balance(CCA_REGISTRY_ADDRESS, BOND_REQUIREMENT)
            .unwrap();
        assert!(
            runtime::bond(storage.clone(), ALICE, BOND_REQUIREMENT, "Test CCA".into()).is_err()
        );
        assert!(!contract.records.exists(ALICE).unwrap());
        assert_eq!(contract.active.len().unwrap(), u32::MAX);
        assert_eq!(api::total_bonded(&storage).unwrap(), U256::ZERO);
    });
}

#[test]
fn names_round_trip_across_storage_lengths_and_empty_names_preserve_registration() {
    run(|storage| {
        let empty_name = ICcaRegistry::bondCall {
            name: String::new(),
        }
        .abi_encode();
        assert!(dispatch(storage.clone(), &empty_name, ALICE, U256::ONE).is_err());
        assert!(api::get_cca(&storage, ALICE).is_err());

        for (index, name) in [
            "A".to_owned(),
            "a".repeat(31),
            "b".repeat(32),
            "名".repeat(22),
            "B".to_owned(),
        ]
        .into_iter()
        .enumerate()
        {
            storage
                .increase_balance(CCA_REGISTRY_ADDRESS, U256::ONE)
                .unwrap();
            let call = ICcaRegistry::bondCall { name: name.clone() }.abi_encode();
            dispatch(storage.clone(), &call, ALICE, U256::ONE).unwrap();
            let query = ICcaRegistry::getCcaCall { cca: ALICE }.abi_encode();
            let output = dispatch(storage.clone(), &query, BOB, U256::ZERO).unwrap();
            let record = ICcaRegistry::getCcaCall::abi_decode_returns(&output).unwrap();
            assert_eq!(record.cca, ALICE);
            assert_eq!(record.name, name);
            assert_eq!(record.state, ICcaRegistry::State::Bonding);
            assert_eq!(record.bondedMinor, U256::from(index + 1));

            assert!(dispatch(storage.clone(), &empty_name, ALICE, U256::ONE).is_err());
            assert_eq!(api::get_cca(&storage, ALICE).unwrap(), record);
        }

        runtime::unbond(storage.clone(), ALICE).unwrap();
        storage
            .set_block_timestamp(U256::from(NOW + UNBOND_COOLDOWN_SECONDS))
            .unwrap();
        runtime::claim_unbonded(storage.clone(), ALICE).unwrap();
        let record = api::get_cca(&storage, ALICE).unwrap();
        assert_eq!(record.name, "B");
        assert_eq!(record.state, ICcaRegistry::State::Deregistered);
        assert_eq!(record.bondedMinor, U256::ZERO);
    });
}

#[test]
fn incremental_registration_exit_and_reregistration_preserve_history() {
    run(|storage| {
        assert!(api::cca_state(&storage, ALICE).is_err());
        assert!(api::get_cca(&storage, ALICE).is_err());
        assert!(!api::is_active(&storage, ALICE).unwrap());
        assert_eq!(
            api::require_active_cca(&storage, ALICE)
                .unwrap_err()
                .to_string(),
            outbe_primitives::error::PrecompileError::from(crate::errors::CcaError::NotRegistered)
                .to_string()
        );
        let first = BOND_REQUIREMENT - U256::ONE;
        bond(&storage, ALICE, first);
        assert_eq!(
            api::cca_state(&storage, ALICE).unwrap(),
            ICcaRegistry::State::Bonding
        );
        assert_eq!(
            api::require_active_cca(&storage, ALICE)
                .unwrap_err()
                .to_string(),
            outbe_primitives::error::PrecompileError::from(crate::errors::CcaError::CcaNotActive {
                cca: ALICE,
                state: ICcaRegistry::State::Bonding,
            })
            .to_string()
        );
        assert!(runtime::credis_issued(&storage, ALICE, DAY, U256::ONE).is_err());
        bond(&storage, ALICE, U256::ONE);
        assert!(api::is_active(&storage, ALICE).unwrap());
        api::require_active_cca(&storage, ALICE).unwrap();
        assert!(runtime::claim_unbonded(storage.clone(), ALICE).is_err());
        bond(&storage, ALICE, U256::from(7));
        runtime::credis_issued(&storage, ALICE, DAY, U256::from(100)).unwrap();
        runtime::unbond(storage.clone(), ALICE).unwrap();
        let record = api::get_cca(&storage, ALICE).unwrap();
        assert_eq!(record.cca, ALICE);
        assert_eq!(record.state, ICcaRegistry::State::Deregistering);
        assert_eq!(record.bondedMinor, BOND_REQUIREMENT + U256::from(7));
        assert_eq!(record.unbondUnlocksAfter, NOW + UNBOND_COOLDOWN_SECONDS);
        assert!(!api::is_active(&storage, ALICE).unwrap());
        assert_eq!(
            api::require_active_cca(&storage, ALICE)
                .unwrap_err()
                .to_string(),
            outbe_primitives::error::PrecompileError::from(crate::errors::CcaError::CcaNotActive {
                cca: ALICE,
                state: ICcaRegistry::State::Deregistering,
            })
            .to_string()
        );
        assert!(runtime::unbond(storage.clone(), ALICE).is_err());
        assert!(runtime::bond(storage.clone(), ALICE, U256::ONE, "Test CCA".into()).is_err());
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
            ICcaRegistry::State::Deregistered
        );
        assert!(runtime::claim_unbonded(storage.clone(), ALICE).is_err());
        assert_eq!(
            api::get_cca(&storage, ALICE).unwrap().bondedMinor,
            U256::ZERO
        );
        assert_eq!(storage.balance(CCA_REGISTRY_ADDRESS).unwrap(), U256::ZERO);
        assert_eq!(
            storage.balance(ALICE).unwrap(),
            BOND_REQUIREMENT + U256::from(7)
        );
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
        assert!(runtime::bond(storage.clone(), ALICE, U256::ZERO, "Test CCA".into()).is_err());
        assert!(
            runtime::bond(storage.clone(), Address::ZERO, U256::ONE, "Test CCA".into()).is_err()
        );
        assert!(runtime::unbond(storage.clone(), ALICE).is_err());
        bond(&storage, ALICE, U256::ONE);
        runtime::unbond(storage.clone(), ALICE).unwrap();
        assert_eq!(
            api::get_cca(&storage, ALICE).unwrap().bondedMinor,
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
fn forfeit_subtracts_only_burned_gratis_even_after_exit() {
    run(|storage| {
        bond(&storage, ALICE, BOND_REQUIREMENT);
        runtime::credis_issued(&storage, ALICE, DAY, U256::from(100)).unwrap();
        runtime::unbond(storage.clone(), ALICE).unwrap();
        runtime::credis_forfeited(&storage, ALICE, DAY, U256::from(50)).unwrap();
        runtime::credis_forfeited(&storage, ALICE, DAY, U256::ZERO).unwrap();
        assert_eq!(
            api::reward_weight(&storage, ALICE, DAY).unwrap(),
            U256::from(50)
        );
        runtime::credis_forfeited(&storage, ALICE, DAY, U256::from(51)).unwrap();
        assert_eq!(
            api::reward_weight(&storage, ALICE, DAY).unwrap(),
            U256::ZERO
        );
        runtime::credis_forfeited(&storage, ALICE, DAY, U256::ONE).unwrap();
        assert_eq!(
            api::reward_weight(&storage, ALICE, DAY).unwrap(),
            U256::ZERO
        );
    });
}

#[test]
fn failed_unbond_claim_preserves_record_and_balance() {
    run(|storage| {
        bond(&storage, ALICE, BOND_REQUIREMENT);
        runtime::credis_issued(&storage, ALICE, DAY, U256::ONE).unwrap();
        let balance = storage.balance(CCA_REGISTRY_ADDRESS).unwrap();
        storage
            .decrease_balance(CCA_REGISTRY_ADDRESS, balance)
            .unwrap();
        runtime::unbond(storage.clone(), ALICE).unwrap();
        storage
            .set_block_timestamp(U256::from(NOW + UNBOND_COOLDOWN_SECONDS))
            .unwrap();
        assert!(runtime::claim_unbonded(storage.clone(), ALICE).is_err());
        assert_eq!(
            api::get_cca(&storage, ALICE).unwrap().bondedMinor,
            BOND_REQUIREMENT
        );
        assert_eq!(
            api::cca_state(&storage, ALICE).unwrap(),
            ICcaRegistry::State::Deregistering
        );
        assert_eq!(storage.balance(ALICE).unwrap(), U256::ZERO);
    });
}

#[test]
fn timestamp_conversion_and_deadline_overflow_are_checked() {
    run(|storage| {
        bond(&storage, ALICE, BOND_REQUIREMENT);
        storage.set_block_timestamp(U256::from(u64::MAX)).unwrap();
        assert_eq!(
            runtime::unbond(storage.clone(), ALICE)
                .unwrap_err()
                .to_string(),
            outbe_primitives::error::PrecompileError::from(crate::errors::CcaError::Arithmetic)
                .to_string()
        );
        assert!(api::is_active(&storage, ALICE).unwrap());
    });
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_timestamp(U256::MAX);
    StorageHandle::enter(&mut provider, |storage| {
        bond(&storage, ALICE, BOND_REQUIREMENT);
        assert_eq!(
            runtime::unbond(storage.clone(), ALICE)
                .unwrap_err()
                .to_string(),
            outbe_primitives::error::PrecompileError::Revert("timestamp exceeds u64".into())
                .to_string()
        );
        assert!(api::is_active(&storage, ALICE).unwrap());
    });
}

#[test]
fn abi_reads_and_nonpayable_selectors() {
    run(|storage| {
        bond(&storage, ALICE, BOND_REQUIREMENT);
        let out = dispatch(
            storage.clone(),
            &ICcaRegistry::getCcaCall { cca: ALICE }.abi_encode(),
            BOB,
            U256::ZERO,
        )
        .unwrap();
        assert_eq!(
            ICcaRegistry::getCcaCall::abi_decode_returns(&out)
                .unwrap()
                .bondedMinor,
            BOND_REQUIREMENT
        );
        for data in [
            ICcaRegistry::getCcaCall { cca: ALICE }.abi_encode(),
            ICcaRegistry::getCcaStateCall { cca: ALICE }.abi_encode(),
            ICcaRegistry::unbondCall {}.abi_encode(),
            ICcaRegistry::claimUnbondedCall {}.abi_encode(),
            ICcaRegistry::supportsInterfaceCall {
                interfaceId: [0x01, 0xff, 0xc9, 0xa7].into(),
            }
            .abi_encode(),
        ] {
            assert!(dispatch(storage.clone(), &data, ALICE, U256::ONE).is_err());
        }
        for signature in ["claimRewards()", "claimRewards(uint256)"] {
            let mut data = alloy_primitives::keccak256(signature)[..4].to_vec();
            if signature.contains("uint256") {
                data.extend_from_slice(&[0; 32]);
            }
            assert!(dispatch(storage.clone(), &data, ALICE, U256::ZERO).is_err());
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
            &ICcaRegistry::bondCall {
                name: "Test CCA".into()
            }
            .abi_encode(),
            ALICE,
            U256::ONE
        )
        .is_err());
        assert!(api::cca_state(&storage, ALICE).is_err());
    });
    assert!(provider.get_ordered_events().is_empty());
}

#[test]
fn deficits_offset_later_openings_only_in_the_same_cca_day() {
    run(|storage| {
        let next = 20231116;
        bond(&storage, ALICE, BOND_REQUIREMENT);
        bond(&storage, BOB, BOND_REQUIREMENT);
        runtime::credis_issued(&storage, ALICE, DAY, U256::from(20)).unwrap();
        runtime::credis_forfeited(&storage, ALICE, DAY, U256::from(50)).unwrap();
        for amount in [10, 20] {
            runtime::credis_issued(&storage, ALICE, DAY, U256::from(amount)).unwrap();
            assert_eq!(
                api::reward_weight(&storage, ALICE, DAY).unwrap(),
                U256::ZERO
            );
        }
        runtime::credis_issued(&storage, ALICE, DAY, U256::from(15)).unwrap();
        // Reordering the same day's credits and debits gives the same net weight.
        runtime::credis_issued(&storage, BOB, DAY, U256::from(65)).unwrap();
        runtime::credis_forfeited(&storage, BOB, DAY, U256::from(50)).unwrap();
        assert_eq!(
            api::reward_weight(&storage, ALICE, DAY).unwrap(),
            U256::from(15)
        );
        assert_eq!(
            api::reward_weight(&storage, BOB, DAY).unwrap(),
            U256::from(15)
        );
        runtime::credis_forfeited(&storage, ALICE, DAY, U256::from(25)).unwrap();
        runtime::credis_issued(&storage, ALICE, next, U256::from(7)).unwrap();
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
        runtime::credis_forfeited(&storage, ALICE, DAY, U256::MAX).unwrap();
        assert!(runtime::credis_forfeited(&storage, ALICE, DAY, U256::ONE).is_err());
        let contract = CcaContract::new(storage.clone());
        let key = address_day_key(ALICE, DAY);
        assert_eq!(
            contract
                .gratis_deficit_minor_per_utc_day
                .read(&key)
                .unwrap(),
            U256::MAX
        );
        assert_eq!(
            api::reward_weight(&storage, ALICE, DAY).unwrap(),
            U256::ZERO
        );
        runtime::credis_issued(&storage, ALICE, DAY, U256::MAX).unwrap();
        assert_eq!(
            contract
                .gratis_deficit_minor_per_utc_day
                .read(&key)
                .unwrap(),
            U256::ZERO
        );
        assert_eq!(
            api::reward_weight(&storage, ALICE, DAY).unwrap(),
            U256::ZERO
        );
        runtime::credis_issued(&storage, ALICE, DAY, U256::ONE).unwrap();
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
            (0, ICcaRegistry::State::Bonding),
            (1, ICcaRegistry::State::Active),
            (2, ICcaRegistry::State::Deregistering),
            (3, ICcaRegistry::State::Deregistered),
        ] {
            storage
                .sstore(CCA_REGISTRY_ADDRESS, slot.slot(), U256::from(word))
                .unwrap();
            assert_eq!(contract.load(ALICE).unwrap().state, state);
            assert_eq!(api::cca_state(&storage, ALICE).unwrap(), state);
            slot.write(state).unwrap();
            assert_eq!(
                storage.sload(CCA_REGISTRY_ADDRESS, slot.slot()).unwrap(),
                U256::from(word)
            );
        }
        for word in [U256::from(4), U256::from(255), U256::from(256), U256::MAX] {
            storage
                .sstore(CCA_REGISTRY_ADDRESS, slot.slot(), word)
                .unwrap();
            assert!(contract.load(ALICE).is_err());
            assert!(api::get_cca(&storage, ALICE).is_err());
            assert!(runtime::bond(storage.clone(), ALICE, U256::ONE, "Test CCA".into()).is_err());
            assert_eq!(
                storage.sload(CCA_REGISTRY_ADDRESS, slot.slot()).unwrap(),
                word
            );
        }
    });
}

#[test]
fn cleanup_is_bounded_resumable_and_atomic_including_deficit_only_entries() {
    use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
    run(|storage| {
        for id in 1..=300 {
            let address = cca_at(id);
            bond(&storage, address, BOND_REQUIREMENT);
            if id % 2 == 0 {
                runtime::credis_forfeited(&storage, address, DAY, U256::ONE).unwrap();
            } else {
                runtime::credis_issued(&storage, address, DAY, U256::ONE).unwrap();
            }
        }
        let contract = CcaContract::new(storage.clone());
        let mut ctx = BlockRuntimeContext::new(
            BlockContext {
                block_gas_limit: 0,
                user_gas_used: 0,
                block_number: 0,
                timestamp: NOW,
                chain_id: 1,
                genesis_hash: alloy_primitives::B256::ZERO,
                genesis_timestamp: u64::max(NOW, 1),
                proposer: alloy_primitives::Address::ZERO,
                validators: Vec::new(),
            },
            storage.clone(),
        );
        assert!(api::enqueue_day_cleanup(&ctx, DAY).is_err());
        seal(&storage, DAY);
        api::enqueue_day_cleanup(&ctx, DAY).unwrap();
        api::enqueue_day_cleanup(&ctx, DAY).unwrap();
        assert_eq!(contract.day_cleanup_tail.read().unwrap(), 1);
        // No source is deleted merely by scheduling.
        assert_eq!(
            api::reward_day_inputs(&storage, DAY).unwrap(),
            (300, U256::from(150))
        );
        api::cleanup(&ctx).unwrap();
        assert_eq!(contract.day_cleanup_cursor.read(&DAY).unwrap(), 256);
        assert!(!contract
            .reward_day_members
            .read(&address_day_key(cca_at(256), DAY))
            .unwrap());
        assert_eq!(
            contract
                .gratis_deficit_minor_per_utc_day
                .read(&address_day_key(cca_at(256), DAY))
                .unwrap(),
            U256::ZERO
        );
        assert_eq!(
            api::reward_day_entry(&storage, DAY, 256).unwrap(),
            (cca_at(257), U256::ONE)
        );
        api::cleanup(&ctx).unwrap();
        assert_eq!(contract.day_cleanup_cursor.read(&DAY).unwrap(), 256);
        // Fail after some successful deletions, then retry the same height.
        let key = crate::schema::day_index_key(DAY, 280);
        contract.reward_day_addresses.clear(&key).unwrap();
        ctx.block.block_number = 1;
        assert!(api::cleanup(&ctx).is_err());
        assert_eq!(contract.cleanup_processed_block.read().unwrap(), 0);
        assert_eq!(contract.day_cleanup_cursor.read(&DAY).unwrap(), 256);
        assert_eq!(
            api::reward_day_entry(&storage, DAY, 256).unwrap().0,
            cca_at(257)
        );
        contract
            .reward_day_addresses
            .write(&key, cca_at(281))
            .unwrap();
        api::cleanup(&ctx).unwrap();
        assert_eq!(contract.day_cleanup_head.read().unwrap(), 1);
        assert_eq!(
            api::reward_day_inputs(&storage, DAY).unwrap(),
            (0, U256::ZERO)
        );
        for id in 1..=300 {
            let key = address_day_key(cca_at(id), DAY);
            assert!(!contract.reward_day_members.read(&key).unwrap());
            assert_eq!(
                contract.gratis_minor_per_utc_day.read(&key).unwrap(),
                U256::ZERO
            );
            assert_eq!(
                contract
                    .gratis_deficit_minor_per_utc_day
                    .read(&key)
                    .unwrap(),
                U256::ZERO
            );
        }
        api::enqueue_day_cleanup(&ctx, DAY).unwrap();
        assert_eq!(contract.day_cleanup_tail.read().unwrap(), 1);
        assert!(runtime::credis_issued(&storage, cca_at(1), DAY, U256::ONE).is_err());
        assert!(runtime::credis_forfeited(&storage, cca_at(2), DAY, U256::ONE).is_err());
    });
}

#[test]
fn cleanup_empty_headers_are_bounded_and_sealing_can_skip_days() {
    use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
    run(|storage| {
        let mut ctx = BlockRuntimeContext::new(
            BlockContext {
                block_gas_limit: 0,
                user_gas_used: 0,
                block_number: 0,
                timestamp: NOW,
                chain_id: 1,
                genesis_hash: alloy_primitives::B256::ZERO,
                genesis_timestamp: u64::max(NOW, 1),
                proposer: alloy_primitives::Address::ZERO,
                validators: Vec::new(),
            },
            storage.clone(),
        );
        seal(&storage, DAY + 10);
        for day in DAY..DAY + 6 {
            api::enqueue_day_cleanup(&ctx, day).unwrap();
        }
        let contract = CcaContract::new(storage.clone());
        api::cleanup(&ctx).unwrap();
        assert_eq!(contract.day_cleanup_head.read().unwrap(), 4);
        api::cleanup(&ctx).unwrap();
        assert_eq!(contract.day_cleanup_head.read().unwrap(), 4);
        ctx.block.block_number = 1;
        api::cleanup(&ctx).unwrap();
        assert_eq!(contract.day_cleanup_head.read().unwrap(), 6);
        contract.day_cleanup_head.write(7).unwrap();
        ctx.block.block_number = 2;
        assert!(api::cleanup(&ctx).is_err());
        assert_eq!(contract.cleanup_processed_block.read().unwrap(), 1);
    });
}
