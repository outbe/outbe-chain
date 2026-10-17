use alloy_primitives::{address, B256, U256};
use alloy_sol_types::SolEvent;
use outbe_primitives::addresses::TRIBUTE_ADDRESS;
use outbe_primitives::error::{PrecompileError, Result as PrecompileResult};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::wwd_entity_id::{derive_poseidon_entity_id, WwdEntityId};

use crate::{TributeContract, TributeData};

fn with_tribute<R>(f: impl FnOnce(&mut TributeContract<'_>) -> R) -> R {
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let _enclave = crate::enclave_client::test_enclave::scope();
        f(&mut TributeContract::new(storage))
    })
}

fn with_provider<R>(f: impl FnOnce(&mut HashMapStorageProvider) -> R) -> R {
    f(&mut HashMapStorageProvider::new(1))
}

fn sample_tribute() -> TributeData {
    let worldwide_day = 20241220u32.into();
    let owner = address!("0x1111111111111111111111111111111111111111");
    TributeData {
        tribute_id: derive_poseidon_entity_id(owner, worldwide_day).unwrap(),
        owner,
        worldwide_day,
        issuance_amount_minor: U256::from(1_000_000u64),
        issuance_currency: 840,
        nominal_amount_minor: U256::from(500_000u64),
        reference_currency: 840,
        tribute_price_minor: U256::from(2_000_000u64),
    }
}

fn entity_id(seed: u64, day: outbe_primitives::time::WorldwideDay) -> WwdEntityId {
    derive_poseidon_entity_id(alloy_primitives::Address::repeat_byte(seed as u8), day).unwrap()
}

fn set_owner(tribute: &mut TributeData, owner: alloy_primitives::Address) {
    tribute.owner = owner;
    tribute.tribute_id = derive_poseidon_entity_id(owner, tribute.worldwide_day).unwrap();
}

fn set_day(tribute: &mut TributeData, day: outbe_primitives::time::WorldwideDay) {
    tribute.worldwide_day = day;
    tribute.tribute_id = derive_poseidon_entity_id(tribute.owner, day).unwrap();
}

fn open_sample_day(tc: &mut TributeContract) {
    tc.unseal_day(20241220u32.into()).unwrap();
}

#[test]
fn test_initial_state() {
    with_tribute(|tc| {
        assert_eq!(tc.total_supply().unwrap(), 0);
        let totals = tc.get_day_totals(20241220u32.into()).unwrap();
        assert_eq!(totals.tribute_count, 0);
        assert_eq!(totals.tribute_nominal_total_minor, U256::ZERO);
        assert!(!totals.is_sealed);
    });
}

#[test]
fn test_issue_requires_initialized_unsealed_day() {
    with_tribute(|tc| {
        let tribute = sample_tribute();
        assert!(tc.issue(&tribute).is_err());

        open_sample_day(tc);
        tc.issue(&tribute).unwrap();
        assert_eq!(tc.total_supply().unwrap(), 1);
    });
}

#[test]
fn test_issue_duplicate_fails() {
    with_tribute(|tc| {
        let tribute = sample_tribute();
        open_sample_day(tc);
        tc.issue(&tribute).unwrap();
        assert!(tc.issue(&tribute).is_err());
    });
}

#[test]
fn test_day_bucket_tracks_nominal_and_gratis() {
    with_tribute(|tc| {
        let t1 = sample_tribute();
        let mut t2 = sample_tribute();
        set_owner(
            &mut t2,
            address!("0x2222222222222222222222222222222222222222"),
        );
        t2.nominal_amount_minor = U256::from(300_000u64);

        open_sample_day(tc);
        tc.issue(&t1).unwrap();
        tc.issue(&t2).unwrap();

        let totals = tc.get_day_totals(20241220u32.into()).unwrap();
        assert_eq!(totals.tribute_count, 2);
        assert_eq!(
            totals.tribute_nominal_total_minor,
            t1.nominal_amount_minor + t2.nominal_amount_minor
        );
        assert!(!totals.is_sealed);
    });
}

#[test]
fn day_totals_track_exact_incremental_tribute_inputs() {
    with_tribute(|tc| {
        let first = sample_tribute();
        let mut second = sample_tribute();
        set_owner(
            &mut second,
            address!("0x2222222222222222222222222222222222222222"),
        );
        second.reference_currency = 978;
        second.nominal_amount_minor = U256::from(7);

        open_sample_day(tc);
        tc.issue(&first).unwrap();
        tc.issue(&second).unwrap();

        let projection = tc.get_day_totals(first.worldwide_day).unwrap();

        assert!(!projection.is_sealed);
        assert_eq!(projection.tribute_count, 2);
        assert_eq!(
            projection.tribute_nominal_total_minor,
            first.nominal_amount_minor + second.nominal_amount_minor
        );
    });
}

#[test]
fn day_totals_remove_burned_tribute_contribution() {
    with_tribute(|tc| {
        let first = sample_tribute();
        let mut second = sample_tribute();
        set_owner(
            &mut second,
            address!("0x2222222222222222222222222222222222222222"),
        );
        second.nominal_amount_minor = U256::from(7);

        open_sample_day(tc);
        tc.issue(&first).unwrap();
        tc.issue(&second).unwrap();
        tc.burn(first.tribute_id).unwrap();

        let projection = tc.get_day_totals(first.worldwide_day).unwrap();

        assert_eq!(projection.tribute_count, 1);
        assert_eq!(
            projection.tribute_nominal_total_minor,
            second.nominal_amount_minor
        );
    });
}

#[test]
fn incremental_day_totals_match_independent_randomized_full_fold() {
    with_tribute(|tc| {
        let day = 20241220u32.into();
        let mut seed = 0x4f43_4d30_3600_0001_u64;
        let mut entries = Vec::<(TributeData, bool)>::new();

        open_sample_day(tc);
        for owner_byte in 1_u8..=32 {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let mut tribute = sample_tribute();
            set_owner(
                &mut tribute,
                alloy_primitives::Address::repeat_byte(owner_byte),
            );
            tribute.reference_currency =
                [840, 978, 826, 392, 756][usize::try_from(seed % 5).unwrap()];
            tribute.nominal_amount_minor = U256::from((seed % 10_000) + 1);
            tc.issue(&tribute).unwrap();
            entries.push((tribute, true));
            assert_day_totals_match_fold(tc, day, &entries);
        }

        for index in (0..entries.len()).step_by(3) {
            tc.burn(entries[index].0.tribute_id).unwrap();
            entries[index].1 = false;
            assert_day_totals_match_fold(tc, day, &entries);
        }
    });
}

#[test]
fn tribute_population_crosses_multiple_work_shards_without_a_total_cap() {
    with_tribute(|tc| {
        let day = 20241220u32.into();
        open_sample_day(tc);

        for seed in 1_u16..=257 {
            let mut tribute = sample_tribute();
            let mut owner = [0_u8; 20];
            owner[18..].copy_from_slice(&seed.to_be_bytes());
            set_owner(&mut tribute, alloy_primitives::Address::from(owner));
            tribute.reference_currency = 840 + (seed % 3);
            tc.issue(&tribute).unwrap();
        }

        let second_shard = tc.get_day_totals(day).unwrap();
        assert_eq!(second_shard.tribute_count, 257);
        assert_eq!(tc.total_supply().unwrap(), 257);

        for seed in 258_u16..=513 {
            let mut tribute = sample_tribute();
            let mut owner = [0_u8; 20];
            owner[18..].copy_from_slice(&seed.to_be_bytes());
            set_owner(&mut tribute, alloy_primitives::Address::from(owner));
            tribute.reference_currency = 840 + (seed % 3);
            tc.issue(&tribute).unwrap();
        }

        let third_shard = tc.get_day_totals(day).unwrap();
        assert_eq!(third_shard.tribute_count, 513);
        assert_eq!(tc.total_supply().unwrap(), 513);

        let last = {
            let mut tribute = sample_tribute();
            let mut owner = [0_u8; 20];
            owner[18..].copy_from_slice(&513_u16.to_be_bytes());
            set_owner(&mut tribute, alloy_primitives::Address::from(owner));
            tribute
        };
        tc.burn(last.tribute_id).unwrap();

        let after_burn = tc.get_day_totals(day).unwrap();
        assert_eq!(after_burn.tribute_count, 512);
    });
}

fn assert_day_totals_match_fold(
    tc: &TributeContract<'_>,
    day: outbe_primitives::time::WorldwideDay,
    entries: &[(TributeData, bool)],
) {
    let mut count = 0_u32;
    let mut nominal = U256::ZERO;
    for (tribute, is_live) in entries {
        if !is_live {
            continue;
        }
        count = count.checked_add(1).unwrap();
        nominal = nominal.checked_add(tribute.nominal_amount_minor).unwrap();
    }

    let projection = tc.get_day_totals(day).unwrap();
    assert_eq!(projection.tribute_count, count);
    assert_eq!(projection.tribute_nominal_total_minor, nominal);
}

#[test]
fn test_burn_tribute() {
    with_tribute(|tc| {
        let tribute = sample_tribute();
        open_sample_day(tc);
        tc.issue(&tribute).unwrap();

        tc.burn(tribute.tribute_id).unwrap();

        assert_eq!(tc.total_supply().unwrap(), 0);
        assert!(tc.get_tribute(tribute.tribute_id).unwrap().is_none());

        let totals = tc.get_day_totals(20241220u32.into()).unwrap();
        assert_eq!(totals.tribute_count, 0);
        assert_eq!(totals.tribute_nominal_total_minor, U256::ZERO);
    });
}

#[test]
fn test_burn_nonexistent_fails() {
    with_tribute(|tc| {
        let tribute_id = entity_id(0x99, 20241220u32.into());
        assert!(tc.burn(tribute_id).is_err());
    });
}

#[test]
fn test_seal_day() {
    with_tribute(|tc| {
        let day = 20241220u32.into();
        open_sample_day(tc);

        let tribute = sample_tribute();
        tc.issue(&tribute).unwrap();
        let totals_before_seal = tc.get_day_totals(day).unwrap();

        tc.seal_day(day).unwrap();
        assert!(tc.is_day_sealed(day).unwrap());

        let mut second_tribute = sample_tribute();
        set_owner(
            &mut second_tribute,
            address!("0x2222222222222222222222222222222222222222"),
        );
        assert!(tc.issue(&second_tribute).is_err());
        assert!(tc.burn(tribute.tribute_id).is_err());
        assert!(tc.get_tribute(tribute.tribute_id).unwrap().is_some());
        let totals_after_rejected_mutations = tc.get_day_totals(day).unwrap();
        assert_eq!(
            totals_after_rejected_mutations.tribute_count,
            totals_before_seal.tribute_count
        );
        assert_eq!(
            totals_after_rejected_mutations.tribute_nominal_total_minor,
            totals_before_seal.tribute_nominal_total_minor
        );

        tc.unseal_day(day).unwrap();
        assert!(!tc.is_day_sealed(day).unwrap());
        tc.burn(tribute.tribute_id).unwrap();
    });
}

#[test]
fn lysis_bulk_accounting_rejects_count_and_nominal_mismatch_before_zeroing() {
    with_tribute(|tc| {
        let tribute = sample_tribute();
        open_sample_day(tc);
        tc.issue(&tribute).unwrap();
        tc.seal_day(tribute.worldwide_day).unwrap();

        assert!(tc
            .consume_lysis_partition(tribute.worldwide_day, 2, tribute.nominal_amount_minor,)
            .is_err());
        assert!(tc
            .consume_lysis_partition(
                tribute.worldwide_day,
                1,
                tribute.nominal_amount_minor + U256::from(1),
            )
            .is_err());
        let unchanged = tc.get_day_totals(tribute.worldwide_day).unwrap();
        assert_eq!(unchanged.tribute_count, 1);
        assert_eq!(
            unchanged.tribute_nominal_total_minor,
            tribute.nominal_amount_minor
        );
        assert_eq!(tc.total_supply().unwrap(), 1);

        tc.consume_lysis_partition(tribute.worldwide_day, 1, tribute.nominal_amount_minor)
            .unwrap();
        let completed = tc.get_day_totals(tribute.worldwide_day).unwrap();
        assert!(completed.initialized);
        assert!(completed.is_sealed);
        assert_eq!(completed.tribute_count, 0);
        assert_eq!(completed.tribute_nominal_total_minor, U256::ZERO);
        assert_eq!(tc.total_supply().unwrap(), 0);
    });
}

#[test]
fn test_balance_of_tracks_live_owner_tributes() {
    with_tribute(|tc| {
        let alice = address!("0x1111111111111111111111111111111111111111");

        let mut t1 = sample_tribute();
        set_owner(&mut t1, alice);

        let mut t2 = sample_tribute();
        set_owner(&mut t2, alice);
        set_day(&mut t2, 20241221u32.into());

        open_sample_day(tc);
        tc.unseal_day(t2.worldwide_day).unwrap();
        tc.issue(&t1).unwrap();
        tc.issue(&t2).unwrap();
        assert_eq!(tc.balance_of(alice).unwrap(), 2);

        tc.burn(t1.tribute_id).unwrap();
        assert_eq!(tc.balance_of(alice).unwrap(), 1);
    });
}

#[test]
fn test_token_uri_returns_metadata_json() {
    with_tribute(|tc| {
        let tribute = sample_tribute();
        open_sample_day(tc);
        tc.issue(&tribute).unwrap();

        let token_uri = tc.token_uri(tribute.tribute_id).unwrap();
        assert!(token_uri.starts_with("data:application/json;utf8,"));
        assert!(token_uri.contains("Outbe Tribute"));
        assert!(token_uri.contains("worldwide_day"));
        assert!(token_uri.contains("issuance_amount_minor"));
        assert!(token_uri.contains("\"trait_type\":\"reference_currency\""));
    });
}

#[test]
fn test_get_tribute_ids_by_owner() {
    with_tribute(|tc| {
        let alice = address!("0x1111111111111111111111111111111111111111");
        let bob = address!("0x2222222222222222222222222222222222222222");

        let mut t1 = sample_tribute();
        set_owner(&mut t1, alice);

        let mut t2 = sample_tribute();
        set_owner(&mut t2, alice);
        set_day(&mut t2, 20241221u32.into());

        let mut t3 = sample_tribute();
        set_owner(&mut t3, bob);

        open_sample_day(tc);
        tc.unseal_day(t2.worldwide_day).unwrap();
        tc.issue(&t1).unwrap();
        tc.issue(&t2).unwrap();
        tc.issue(&t3).unwrap();

        let alice_ids = tc.get_tribute_ids_by_owner(alice).unwrap();
        assert_eq!(alice_ids, vec![t1.tribute_id, t2.tribute_id]);

        let bob_ids = tc.get_tribute_ids_by_owner(bob).unwrap();
        assert_eq!(bob_ids, vec![t3.tribute_id]);
    });
}

#[test]
fn test_get_tribute_ids_by_day() {
    with_tribute(|tc| {
        let t1 = sample_tribute();
        let mut t2 = sample_tribute();
        set_owner(
            &mut t2,
            address!("0x2222222222222222222222222222222222222222"),
        );

        open_sample_day(tc);
        tc.issue(&t1).unwrap();
        tc.issue(&t2).unwrap();

        let day_ids = tc.get_tribute_ids_by_day(20241220u32.into()).unwrap();
        let mut expected = vec![t1.tribute_id, t2.tribute_id];
        expected.sort_unstable();
        assert_eq!(day_ids, expected);
    });
}

#[test]
fn test_get_tributes_by_owner_sparse_after_burn() {
    with_tribute(|tc| {
        let alice = address!("0x1111111111111111111111111111111111111111");

        let mut t1 = sample_tribute();
        set_owner(&mut t1, alice);

        let mut t2 = sample_tribute();
        set_owner(&mut t2, alice);
        set_day(&mut t2, 20241221u32.into());

        open_sample_day(tc);
        tc.unseal_day(t2.worldwide_day).unwrap();
        tc.issue(&t1).unwrap();
        tc.issue(&t2).unwrap();
        tc.burn(t1.tribute_id).unwrap();

        let tributes = tc.read_all_by_owner(alice).unwrap();
        assert_eq!(tributes.len(), 1);
        assert_eq!(tributes[0].tribute_id, t2.tribute_id);
    });
}

#[test]
fn test_get_all_day_tributes_sparse_after_burn() {
    with_tribute(|tc| {
        let t1 = sample_tribute();
        let mut t2 = sample_tribute();
        set_owner(
            &mut t2,
            address!("0x2222222222222222222222222222222222222222"),
        );

        open_sample_day(tc);
        tc.issue(&t1).unwrap();
        tc.issue(&t2).unwrap();
        tc.burn(t1.tribute_id).unwrap();

        let tributes = tc.read_all_by_day(20241220u32.into()).unwrap();
        assert_eq!(tributes.len(), 1);
        assert_eq!(tributes[0].tribute_id, t2.tribute_id);
    });
}

#[test]
fn test_burn_all_by_wwd() {
    with_tribute(|tc| {
        let t1 = sample_tribute();
        let mut t2 = sample_tribute();
        set_owner(
            &mut t2,
            address!("0x2222222222222222222222222222222222222222"),
        );

        open_sample_day(tc);
        tc.issue(&t1).unwrap();
        tc.issue(&t2).unwrap();

        tc.burn_all_by_wwd(20241220u32.into()).unwrap();

        assert_eq!(tc.total_supply().unwrap(), 0);
        assert!(tc.get_tribute(t1.tribute_id).unwrap().is_none());
        assert!(tc.get_tribute(t2.tribute_id).unwrap().is_none());
        assert!(tc
            .get_tribute_ids_by_day(20241220u32.into())
            .unwrap()
            .is_empty());

        let totals = tc.get_day_totals(20241220u32.into()).unwrap();
        assert_eq!(totals.tribute_count, 0);
        assert_eq!(totals.tribute_nominal_total_minor, U256::ZERO);
    });
}

#[test]
fn sealed_day_rejects_burn_all_by_wwd_without_partial_mutation() {
    with_tribute(|tc| {
        let day = 20241220u32.into();
        let t1 = sample_tribute();
        let mut t2 = sample_tribute();
        set_owner(
            &mut t2,
            address!("0x2222222222222222222222222222222222222222"),
        );

        open_sample_day(tc);
        tc.issue(&t1).unwrap();
        tc.issue(&t2).unwrap();
        tc.seal_day(day).unwrap();

        assert!(tc.burn_all_by_wwd(day).is_err());
        assert_eq!(tc.total_supply().unwrap(), 2);
        assert!(tc.get_tribute(t1.tribute_id).unwrap().is_some());
        assert!(tc.get_tribute(t2.tribute_id).unwrap().is_some());
        assert_eq!(tc.get_day_totals(day).unwrap().tribute_count, 2);
    });
}

#[test]
fn test_events_emitted_for_issue_and_burn() {
    with_provider(|provider| {
        let tribute = sample_tribute();
        StorageHandle::enter(&mut *provider, |storage| {
            let _enclave = crate::enclave_client::test_enclave::scope();
            let mut contract = TributeContract::new(storage);
            contract.unseal_day(tribute.worldwide_day).unwrap();
            contract.issue(&tribute).unwrap();
        });
        let events = provider.get_events(TRIBUTE_ADDRESS);
        assert_eq!(events.len(), 2);
        let issued =
            crate::precompile::ITribute::TributeIssued::decode_log_data(&events[1]).unwrap();
        assert_eq!(issued.tributeId, tribute.tribute_id.to_u256());
        assert_eq!(issued.owner, tribute.owner);
        assert_eq!(issued.worldwideDay, tribute.worldwide_day.value());
        assert!(!issued.nominalAmountMinor.is_empty());
        StorageHandle::enter(&mut *provider, |storage| {
            let _enclave = crate::enclave_client::test_enclave::scope();
            TributeContract::new(storage)
                .burn(tribute.tribute_id)
                .unwrap();
        });
        let events = provider.get_events(TRIBUTE_ADDRESS);
        assert_eq!(events.len(), 3);
        let burned =
            crate::precompile::ITribute::TributeBurned::decode_log_data(&events[2]).unwrap();
        assert_eq!(burned.tributeId, tribute.tribute_id.to_u256());
        assert_eq!(burned.owner, tribute.owner);
    });
}

#[test]
fn failed_mutations_emit_no_events_and_sealing_emits_only_its_business_event() {
    with_provider(|provider| {
        let tribute = sample_tribute();
        StorageHandle::enter(&mut *provider, |storage| {
            let _enclave = crate::enclave_client::test_enclave::scope();
            TributeContract::new(storage)
                .unseal_day(tribute.worldwide_day)
                .unwrap();
        });
        provider.clear_events(TRIBUTE_ADDRESS);

        StorageHandle::enter(&mut *provider, |storage| {
            let _enclave = crate::enclave_client::test_enclave::scope();
            let reverted: PrecompileResult<()> = storage.with_checkpoint(|| {
                let contract = TributeContract::new(storage.clone());
                TributeContract::new(storage.clone()).issue(&tribute)?;
                assert!(contract.get_tribute(tribute.tribute_id)?.is_some());
                Err(PrecompileError::Revert("nested caller reverted".into()))
            });
            assert!(reverted.is_err());
            let contract = TributeContract::new(storage);
            assert!(contract.get_tribute(tribute.tribute_id).unwrap().is_none());
            assert_eq!(contract.total_supply().unwrap(), 0);
            assert_eq!(
                contract
                    .get_day_totals(tribute.worldwide_day)
                    .unwrap()
                    .tribute_count,
                0
            );
        });
        assert!(provider.get_events(TRIBUTE_ADDRESS).is_empty());

        let mut oog_tribute = sample_tribute();
        set_owner(
            &mut oog_tribute,
            address!("0x2222222222222222222222222222222222222222"),
        );
        StorageHandle::enter(&mut *provider, |storage| {
            let _enclave = crate::enclave_client::test_enclave::scope();
            let out_of_gas: PrecompileResult<()> = storage.with_checkpoint(|| {
                let contract = TributeContract::new(storage.clone());
                TributeContract::new(storage.clone()).issue(&oog_tribute)?;
                assert!(contract.get_tribute(oog_tribute.tribute_id)?.is_some());
                Err(PrecompileError::OutOfGas)
            });
            assert!(matches!(out_of_gas, Err(PrecompileError::OutOfGas)));
            let contract = TributeContract::new(storage);
            assert!(contract
                .get_tribute(oog_tribute.tribute_id)
                .unwrap()
                .is_none());
            assert_eq!(contract.total_supply().unwrap(), 0);
            assert_eq!(
                contract
                    .get_day_totals(oog_tribute.worldwide_day)
                    .unwrap()
                    .tribute_count,
                0
            );
        });
        assert!(provider.get_events(TRIBUTE_ADDRESS).is_empty());

        StorageHandle::enter(&mut *provider, |storage| {
            let _enclave = crate::enclave_client::test_enclave::scope();
            TributeContract::new(storage).issue(&tribute).unwrap();
        });
        provider.clear_events(TRIBUTE_ADDRESS);
        StorageHandle::enter(&mut *provider, |storage| {
            let _enclave = crate::enclave_client::test_enclave::scope();
            assert!(TributeContract::new(storage).issue(&tribute).is_err());
        });
        assert!(provider.get_events(TRIBUTE_ADDRESS).is_empty());

        StorageHandle::enter(&mut *provider, |storage| {
            let _enclave = crate::enclave_client::test_enclave::scope();
            TributeContract::new(storage)
                .burn(tribute.tribute_id)
                .unwrap();
        });
        provider.clear_events(TRIBUTE_ADDRESS);
        StorageHandle::enter(&mut *provider, |storage| {
            let _enclave = crate::enclave_client::test_enclave::scope();
            assert!(TributeContract::new(storage)
                .burn(tribute.tribute_id)
                .is_err());
        });
        assert!(provider.get_events(TRIBUTE_ADDRESS).is_empty());

        StorageHandle::enter(&mut *provider, |storage| {
            let _enclave = crate::enclave_client::test_enclave::scope();
            TributeContract::new(storage)
                .seal_day(tribute.worldwide_day)
                .unwrap();
        });
        let events = provider.get_events(TRIBUTE_ADDRESS);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].topics()[0],
            crate::precompile::ITribute::TributeWorldwideDaySealed::SIGNATURE_HASH
        );
    });
}

#[test]
fn failed_burn_transaction_restores_body_compact_state_and_events() {
    with_provider(|provider| {
        let tribute = sample_tribute();
        StorageHandle::enter(&mut *provider, |storage| {
            let _enclave = crate::enclave_client::test_enclave::scope();
            let mut contract = TributeContract::new(storage);
            contract.unseal_day(tribute.worldwide_day).unwrap();
            contract.issue(&tribute).unwrap();
        });
        provider.clear_events(TRIBUTE_ADDRESS);

        for out_of_gas in [false, true] {
            StorageHandle::enter(&mut *provider, |storage| {
                let _enclave = crate::enclave_client::test_enclave::scope();
                let failed: PrecompileResult<()> = storage.with_checkpoint(|| {
                    let contract = TributeContract::new(storage.clone());
                    TributeContract::new(storage.clone()).burn(tribute.tribute_id)?;
                    assert!(contract.get_tribute(tribute.tribute_id)?.is_none());
                    if out_of_gas {
                        Err(PrecompileError::OutOfGas)
                    } else {
                        Err(PrecompileError::Revert("transaction reverted".into()))
                    }
                });
                assert!(failed.is_err());
                let contract = TributeContract::new(storage);
                let restored = contract
                    .get_tribute(tribute.tribute_id)
                    .unwrap()
                    .expect("failed burn must restore the body");
                assert_eq!(restored.tribute_id, tribute.tribute_id);
                assert_eq!(restored.owner, tribute.owner);
                assert_eq!(restored.nominal_amount_minor, tribute.nominal_amount_minor);
                assert_eq!(contract.total_supply().unwrap(), 1);
                assert_eq!(
                    contract
                        .get_day_totals(tribute.worldwide_day)
                        .unwrap()
                        .tribute_count,
                    1
                );
            });
            assert!(provider.get_events(TRIBUTE_ADDRESS).is_empty());
        }
    });
}

#[test]
fn encrypted_retirement_preserves_frozen_nominal_after_live_total_is_zero() {
    with_tribute(|tc| {
        let plain = sample_tribute();
        let context = outbe_primitives::tribute_encryption::TributeContextV2 {
            chain_id: 1,
            tribute_id: plain.tribute_id,
            owner: plain.owner,
            worldwide_day: plain.worldwide_day,
            issuance_currency: plain.issuance_currency,
            reference_currency: plain.reference_currency,
            tribute_price_minor: plain.tribute_price_minor,
            offer_input_hash: B256::repeat_byte(0x78),
        };
        let encrypted = outbe_tee_enclave::tribute_encryption::encrypt_tribute(
            &crate::enclave_client::test_enclave::NETWORK_SECRET,
            &outbe_tee_enclave::crypto::x25519_public(&[0x79; 32]),
            context,
            &outbe_primitives::tribute_encryption::TributeAmountsV2 {
                issuance_amount_minor: plain.issuance_amount_minor,
                nominal_amount_minor: plain.nominal_amount_minor,
            },
        )
        .unwrap();
        tc.unseal_day(plain.worldwide_day).unwrap();
        tc.issue_encrypted(&encrypted).unwrap();
        tc.seal_day(plain.worldwide_day).unwrap();
        tc.freeze_day_nominal(&tc.get_day_totals(plain.worldwide_day).unwrap())
            .unwrap();
        let frozen = tc.encrypted_day_nominal(plain.worldwide_day, true).unwrap();
        tc.consume_lysis_partition_inner(plain.worldwide_day, 1, plain.nominal_amount_minor)
            .unwrap();
        assert_eq!(
            tc.get_day_totals(plain.worldwide_day)
                .unwrap()
                .tribute_nominal_total_minor,
            U256::ZERO
        );
        assert_eq!(
            tc.day_nominal_amount(plain.worldwide_day, true).unwrap(),
            plain.nominal_amount_minor
        );
        assert_eq!(
            tc.encrypted_day_nominal(plain.worldwide_day, true).unwrap(),
            frozen
        );
        assert!(tc
            .consume_lysis_partition_inner(plain.worldwide_day, 1, plain.nominal_amount_minor)
            .is_err());
    });
}

#[test]
fn old_digest_is_rejected_without_issuing_a_tribute() {
    with_tribute(|tc| {
        let mut tribute = sample_tribute();
        open_sample_day(tc);
        // Pinned legacy digest for owner 0x11…11 and day 20241220.
        let digest = alloy_primitives::b256!(
            "0e775956064fb921159d3f5c322eb19d85d4006e4fb52ea3f84a8583c63f4c9a"
        );
        tribute.tribute_id = WwdEntityId::from_day_and_digest(tribute.worldwide_day, digest);
        assert!(tc.issue(&tribute).is_err());
        assert_eq!(tc.total_supply().unwrap(), 0);
        tribute.tribute_id =
            derive_poseidon_entity_id(tribute.owner, tribute.worldwide_day).unwrap();
        tc.issue(&tribute).unwrap();
        assert_eq!(tc.total_supply().unwrap(), 1);
    });
}

#[test]
fn v1_mechanics_fi_groups_fractions_and_create_nod_within_7_days_burns_tribute() {
    use alloy_sol_types::SolCall;

    let mut provider = HashMapStorageProvider::new(1);
    let base_ts = 1_750_000_000u64;
    provider.set_timestamp(U256::from(base_ts));

    let day: outbe_primitives::time::WorldwideDay = 20241220u32.into();
    let owner1 = address!("0x1111111111111111111111111111111111111111");
    let owner2 = address!("0x2222222222222222222222222222222222222222");
    let stranger = address!("0x9999999999999999999999999999999999999999");

    let mut t1 = sample_tribute();
    set_owner(&mut t1, owner1);
    t1.nominal_amount_minor = U256::from(400_000_000u64); // 400 USD

    let mut t2 = sample_tribute();
    set_owner(&mut t2, owner2);
    t2.nominal_amount_minor = U256::from(600_000_000u64); // 600 USD

    StorageHandle::enter(&mut provider, |storage| {
        let _enclave = crate::enclave_client::test_enclave::scope();
        let mut tc = TributeContract::new(storage.clone());
        tc.unseal_day(day).unwrap();

        // Stage 4: Issue Tributes in leagues 100 and 4000
        tc.issue_with_league(&t1, Some(100)).unwrap();
        tc.issue_with_league(&t2, Some(4000)).unwrap();
        assert_eq!(tc.get_league(t1.tribute_id).unwrap(), 100);
        assert_eq!(tc.get_league(t2.tribute_id).unwrap(), 4000);

        // Stage 2: Verify fi_groups[wwd]
        let groups = tc.get_fi_groups(day).unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups.get(&100), Some(&(1, t1.nominal_amount_minor)));
        assert_eq!(groups.get(&4000), Some(&(1, t2.nominal_amount_minor)));

        // Cannot createNod before Metadosis has completed
        assert!(tc.create_nod(owner1, t1.tribute_id).is_err());

        // Stage 3: Seal day and compute Metadosis fractions for WWD
        tc.seal_day(day).unwrap();
        let lysis_limit = U256::from(500_000_000_000u64); // large enough so nod_max = lysis_limit / 1024 >= loads
        let (fractions, allocated) = tc
            .compute_and_record_metadosis_fractions(day, lysis_limit, base_ts)
            .unwrap();
        assert!(fractions.get(&4000).copied().unwrap_or(0) > 0);
        assert!(fractions.get(&100).copied().unwrap_or(0) > 0);
        assert!(allocated > U256::ZERO);

        // Stage 5: Stranger cannot createNod for owner1's Tribute
        assert!(tc.create_nod(stranger, t1.tribute_id).is_err());

        // Owner1 creates Nod via precompile ABI dispatch within 7 days (day + 3 days)
        storage
            .set_block_timestamp(U256::from(
                base_ts + 3 * outbe_primitives::time::SECONDS_PER_DAY,
            ))
            .unwrap();
        let call_bytes = crate::precompile::ITribute::createNodCall {
            tributeId: t1.tribute_id.to_u256(),
        }
        .abi_encode();
        let ret =
            crate::precompile::dispatch(storage.clone(), &call_bytes, owner1, U256::ZERO).unwrap();
        let nod_id_u256 =
            crate::precompile::ITribute::createNodCall::abi_decode_returns(&ret).unwrap();
        assert_eq!(WwdEntityId::from(nod_id_u256), t1.tribute_id);

        // Verify Nod exists in outbe-nod and Tribute is burned!
        let nod_item = outbe_nod::api::get_item(&storage, t1.tribute_id)
            .unwrap()
            .expect("Nod must be created");
        assert_eq!(nod_item.owner, owner1);
        assert_eq!(nod_item.league_id, 100);
        assert!(tc.get_record(t1.tribute_id).unwrap().is_none());
        assert_eq!(tc.total_supply().unwrap(), 1);

        // Calling createNod a second time for the burned Tribute must fail
        assert!(
            crate::precompile::dispatch(storage.clone(), &call_bytes, owner1, U256::ZERO).is_err()
        );

        // Advance past 7 days (7 days + 1 second) -> owner2's createNod expires!
        storage
            .set_block_timestamp(U256::from(
                base_ts + 7 * outbe_primitives::time::SECONDS_PER_DAY + 1,
            ))
            .unwrap();
        assert!(tc.create_nod(owner2, t2.tribute_id).is_err());
        // Tribute 2 remains unburned when window expired
        assert!(tc.get_record(t2.tribute_id).unwrap().is_some());
    });

    // Verify TributeBurned event was emitted when t1's Nod was created
    let events = provider.get_events(TRIBUTE_ADDRESS);
    let burned_events: Vec<_> = events
        .iter()
        .filter_map(|log| crate::precompile::ITribute::TributeBurned::decode_log_data(log).ok())
        .collect();
    assert_eq!(burned_events.len(), 1);
    assert_eq!(burned_events[0].tributeId, t1.tribute_id.to_u256());
    assert_eq!(burned_events[0].owner, owner1);
}
