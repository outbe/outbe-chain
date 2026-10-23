use super::{sample_tribute, set_owner};
use crate::TributeContract;
use alloy_primitives::{address, U256};
use alloy_sol_types::SolEvent;
use outbe_primitives::addresses::TRIBUTE_ADDRESS;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::storage::{
    metadosis_process_ready_binding, MetadosisCycleLifecycle, MetadosisMutationPurposeTag,
};
use outbe_primitives::wwd_entity_id::WwdEntityId;

fn seed_entry_price(storage: &StorageHandle<'_>, iso: u16, utc_day: u32, vwap: U256) {
    let index = outbe_oracle::api::register_pair(
        storage.clone(),
        outbe_oracle::api::AddressPair::new_coen_to(iso),
    )
    .unwrap();
    outbe_oracle::schema::OracleContract::new(storage.clone())
        .record_utc_day_vwap(utc_day, index, vwap)
        .unwrap();
}

#[test]
fn v1_mechanics_fi_groups_fractions_and_late_create_nod_burns_tribute() {
    use alloy_sol_types::SolCall;

    let mut provider = HashMapStorageProvider::new(1);
    let base_ts = 1_750_000_000u64;
    provider.set_timestamp(U256::from(base_ts));
    provider.enable_metadosis_mutation_frame(MetadosisMutationPurposeTag::CycleLifecycle);

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
        let binding = metadosis_process_ready_binding(
            storage.chain_id().unwrap(),
            storage.block_number().unwrap(),
            storage.now_secs().unwrap(),
        );
        let (fractions, allocated) = storage
            .with_metadosis_mutation::<MetadosisCycleLifecycle, _>(binding, || {
                tc.compute_and_record_metadosis_fractions(day, lysis_limit, base_ts)
            })
            .unwrap();
        assert!(fractions.get(&4000).copied().unwrap_or(0) > 0);
        assert!(fractions.get(&100).copied().unwrap_or(0) > 0);
        assert!(allocated > U256::ZERO);
        seed_entry_price(
            &storage,
            840,
            outbe_nod::openings::entry_price_day(base_ts).expect("date step"),
            U256::from(1_234_567u64),
        );

        storage
            .set_block_timestamp(U256::from(base_ts - 1))
            .unwrap();
        let early = tc.create_nod(owner1, t1.tribute_id).unwrap_err();
        let expected_early: PrecompileError =
            crate::errors::TributeError::NodCreationBeforeCompletion.into();
        assert_eq!(early.to_string(), expected_early.to_string());
        assert!(tc.get_record(t1.tribute_id).unwrap().is_some());
        assert!(outbe_nod::api::get_item(&storage, t1.tribute_id)
            .unwrap()
            .is_none());
        assert_eq!(tc.total_supply().unwrap(), 2);
        storage.set_block_timestamp(U256::from(base_ts)).unwrap();

        // A non-owner claim preserves the Tribute and issues no Nod.
        let stranger_err = tc.create_nod(stranger, t1.tribute_id).unwrap_err();
        let expected_owner: PrecompileError =
            crate::errors::TributeError::CallerNotTributeOwner.into();
        assert_eq!(stranger_err.to_string(), expected_owner.to_string());
        assert!(tc.get_record(t1.tribute_id).unwrap().is_some());
        assert!(outbe_nod::api::get_item(&storage, t1.tribute_id)
            .unwrap()
            .is_none());

        // Owner1 creates a Nod three days after completion.
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

        // Verify Nod exists in outbe-nod and Tribute is burned with consistent partition counters!
        let nod_item = outbe_nod::api::get_item(&storage, t1.tribute_id)
            .unwrap()
            .expect("Nod must be created");
        assert_eq!(nod_item.owner, owner1);
        assert_eq!(nod_item.league_id, 100);
        assert_eq!(
            nod_item.issued_at,
            base_ts + 3 * outbe_primitives::time::SECONDS_PER_DAY
        );
        assert_eq!(
            nod_item.encrypted.terms.entry_price_minor,
            U256::from(1_234_567u64)
        );
        assert!(tc.get_record(t1.tribute_id).unwrap().is_none());
        assert_eq!(tc.total_supply().unwrap(), 1);
        let totals_after_t1 = tc.get_day_totals(day).unwrap();
        assert_eq!(totals_after_t1.tribute_count, 1);
        assert_eq!(
            totals_after_t1.tribute_nominal_total_minor,
            t2.nominal_amount_minor
        );
        let groups_after_t1 = tc.get_fi_groups(day).unwrap();
        assert_eq!(groups_after_t1.len(), 1);
        assert_eq!(
            groups_after_t1.get(&4000),
            Some(&(1, t2.nominal_amount_minor))
        );

        // Calling createNod a second time for the burned Tribute must fail
        assert!(
            crate::precompile::dispatch(storage.clone(), &call_bytes, owner1, U256::ZERO).is_err()
        );

        // A claim after seven days still creates the Nod and deletes only that Tribute.
        let late = base_ts + 7 * outbe_primitives::time::SECONDS_PER_DAY + 1;
        storage.set_block_timestamp(U256::from(late)).unwrap();
        assert_eq!(tc.create_nod(owner2, t2.tribute_id).unwrap(), t2.tribute_id);
        assert!(tc.get_record(t2.tribute_id).unwrap().is_none());
        assert_eq!(tc.total_supply().unwrap(), 0);
        let late_nod = outbe_nod::api::get_item(&storage, t2.tribute_id)
            .unwrap()
            .expect("late Nod");
        assert_eq!(late_nod.owner, owner2);
        assert_eq!(late_nod.league_id, 4000);
        assert_eq!(late_nod.issued_at, late);
        let totals_after_t2 = tc.get_day_totals(day).unwrap();
        assert_eq!(totals_after_t2.tribute_count, 0);
        assert!(totals_after_t2.tribute_nominal_total_minor.is_zero());
        assert!(tc.get_fi_groups(day).unwrap().is_empty());
        assert!(outbe_nod::api::get_item(&storage, t1.tribute_id)
            .unwrap()
            .is_some());
    });

    let events = provider.get_events(TRIBUTE_ADDRESS);
    let burned_events: Vec<_> = events
        .iter()
        .filter_map(|log| crate::precompile::ITribute::TributeBurned::decode_log_data(log).ok())
        .collect();
    assert_eq!(burned_events.len(), 2);
    assert_eq!(burned_events[0].tributeId, t1.tribute_id.to_u256());
    assert_eq!(burned_events[0].owner, owner1);
    assert_eq!(burned_events[1].tributeId, t2.tribute_id.to_u256());
    assert_eq!(burned_events[1].owner, owner2);
}

#[test]
fn create_nod_reverts_without_a_completion_day_price() {
    use alloy_sol_types::SolCall;

    let mut provider = HashMapStorageProvider::new(1);
    let completed_at = 1_750_000_000u64;
    provider.set_timestamp(U256::from(completed_at));
    provider.enable_metadosis_mutation_frame(MetadosisMutationPurposeTag::CycleLifecycle);
    let tribute = sample_tribute();
    let day = tribute.worldwide_day;

    StorageHandle::enter(&mut provider, |storage| {
        let _enclave = crate::enclave_client::test_enclave::scope();
        let mut tc = TributeContract::new(storage.clone());
        tc.unseal_day(day).unwrap();
        tc.issue_with_league(&tribute, Some(100)).unwrap();
        tc.seal_day(day).unwrap();
        let binding = metadosis_process_ready_binding(
            storage.chain_id().unwrap(),
            storage.block_number().unwrap(),
            storage.now_secs().unwrap(),
        );
        storage
            .with_metadosis_mutation::<MetadosisCycleLifecycle, _>(binding, || {
                tc.compute_and_record_metadosis_fractions(
                    day,
                    U256::from(500_000_000_000u64),
                    completed_at,
                )
            })
            .unwrap();
        storage
            .set_block_timestamp(U256::from(
                completed_at + outbe_primitives::time::SECONDS_PER_DAY,
            ))
            .unwrap();

        let call = crate::precompile::ITribute::createNodCall {
            tributeId: tribute.tribute_id.to_u256(),
        }
        .abi_encode();
        let unpriced =
            crate::precompile::dispatch(storage.clone(), &call, tribute.owner, U256::ZERO);
        let expected_err: PrecompileError = crate::errors::TributeError::EntryPriceUnavailable {
            price_day: outbe_nod::openings::entry_price_day(completed_at).expect("date step"),
            currency: 840,
        }
        .into();
        assert_eq!(unpriced.unwrap_err().to_string(), expected_err.to_string());
        assert!(tc.get_record(tribute.tribute_id).unwrap().is_some());
        assert_eq!(tc.total_supply().unwrap(), 1);
        assert!(outbe_nod::api::get_item(&storage, tribute.tribute_id)
            .unwrap()
            .is_none());

        seed_entry_price(
            &storage,
            840,
            outbe_nod::openings::entry_price_day(completed_at).expect("date step"),
            U256::from(1_234_567u64),
        );
        crate::precompile::dispatch(storage.clone(), &call, tribute.owner, U256::ZERO).unwrap();
        assert!(tc.get_record(tribute.tribute_id).unwrap().is_none());
    });

    let burned = provider
        .get_events(TRIBUTE_ADDRESS)
        .iter()
        .filter(|log| crate::precompile::ITribute::TributeBurned::decode_log_data(log).is_ok())
        .count();
    assert_eq!(burned, 1);
}
