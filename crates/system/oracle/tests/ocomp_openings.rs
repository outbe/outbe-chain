use alloy_primitives::{B256, U256};
use outbe_common::WorldwideDay;
use outbe_oracle::api::{coen_iso_pair, AddressPair};
use outbe_oracle::schema::OracleContract;
use outbe_oracle::{
    evaluate_oracle_opening_v1, oracle_count_slot_plan_v1, oracle_opening_slot_plan_v1,
    OracleOpeningEvaluationError, OracleOpeningPlanError, MAX_OCOMP_ACTIVE_SCURVE_ENTRIES,
    MAX_OCOMP_REFERENCE_CURRENCIES, MAX_OCOMP_WWD_PAIR_ENTRIES,
};
use outbe_primitives::{
    addresses::ORACLE_ADDRESS,
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
};

fn slot_word(storage: &StorageHandle<'_>, slot: B256) -> U256 {
    storage
        .sload(ORACLE_ADDRESS, U256::from_be_bytes(slot.0))
        .unwrap()
}

/// Seeds the Oracle state both plan rounds are derived against and returns the
/// two derived settlement pair hashes.
fn seed_oracle(storage: &StorageHandle<'_>, day: WorldwideDay) -> (AddressPair, AddressPair) {
    let oracle = OracleContract::new(storage.clone());
    // The settlement pair is derived from the ISO code, not stored.
    let usd_pair = coen_iso_pair(840);
    let eur_pair = coen_iso_pair(978);
    oracle.pair_ordinal.write(&usd_pair, 1).unwrap();
    oracle.pair_ordinal.write(&eur_pair, 2).unwrap();
    oracle.reference_currencies.push(840).unwrap();
    oracle.reference_currencies.push(978).unwrap();
    oracle.worldwide_day_vwap_exists.write(&day, true).unwrap();
    oracle.worldwide_day_vwap_pair_count.write(&day, 2).unwrap();
    oracle
        .worldwide_day_vwap_pair_id
        .get_nested(&day)
        .write(&0, 1)
        .unwrap();
    oracle
        .worldwide_day_vwap_value
        .get_nested(&day)
        .write(&0, U256::from(100))
        .unwrap();
    oracle
        .worldwide_day_vwap_pair_id
        .get_nested(&day)
        .write(&1, 2)
        .unwrap();
    oracle
        .worldwide_day_vwap_value
        .get_nested(&day)
        .write(&1, U256::from(200))
        .unwrap();
    oracle.scurve_count.write(4).unwrap();
    oracle.scurve_oldest_idx.write(2).unwrap();
    let target_day = outbe_oracle::scurve::truncate_to_day(day.to_timestamp_utc());
    oracle.scurve_pair_id.write(&2, 1).unwrap();
    oracle.scurve_peak_day.write(&2, target_day).unwrap();
    oracle.scurve_peak_price.write(&2, U256::from(300)).unwrap();
    oracle.scurve_pair_id.write(&3, 2).unwrap();
    oracle.scurve_peak_day.write(&3, target_day).unwrap();
    oracle.scurve_peak_price.write(&3, U256::from(400)).unwrap();
    (usd_pair, eur_pair)
}

#[test]
fn oracle_opening_plan_reads_the_exact_raw_slots_used_by_runtime_semantics() {
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let day = WorldwideDay::new(20260715);
        let (_usd_pair, _eur_pair) = seed_oracle(&storage, day);
        let oracle = OracleContract::new(storage.clone());
        let target_day = outbe_oracle::scurve::truncate_to_day(day.to_timestamp_utc());

        // Round one is a fixed five-word probe, independent of the ISO list.
        let counts = oracle_count_slot_plan_v1(day, &[840, 978]).unwrap();
        assert_eq!(
            counts
                .slots
                .iter()
                .copied()
                .map(|slot| slot_word(&storage, slot))
                .collect::<Vec<_>>(),
            vec![
                U256::from(2), // reference_currencies length
                U256::from(1), // wwd_vwap_exists
                U256::from(2), // wwd_vwap_pair_count
                U256::from(4), // scurve_count
                U256::from(2), // scurve_oldest
            ]
        );

        let plan = oracle_opening_slot_plan_v1(day, &[840, 978], 2, 2, 4, 2).unwrap();
        let raw_slots = plan
            .slots
            .iter()
            .copied()
            .map(|slot| (slot, slot_word(&storage, slot)))
            .collect::<Vec<_>>();
        assert_eq!(
            raw_slots
                .iter()
                .map(|(_, value)| *value)
                .collect::<Vec<_>>(),
            vec![
                U256::from(2),   // reference_currencies length
                U256::from(840), // reference_currencies[0]
                U256::from(978), // reference_currencies[1]
                U256::from(1),   // pair_ordinal[COEN/840]
                U256::from(2),   // pair_ordinal[COEN/978]
                U256::from(1),   // wwd_vwap_exists
                U256::from(2),   // wwd_vwap_pair_count
                U256::from(1),
                U256::from(100),
                U256::from(2),
                U256::from(200),
                U256::from(4), // scurve_count
                U256::from(2), // scurve_oldest
                U256::from(1),
                U256::from(target_day),
                U256::from(300),
                U256::from(2),
                U256::from(target_day),
                U256::from(400),
            ]
        );

        let evaluated = evaluate_oracle_opening_v1(day, &[840, 978], &raw_slots).unwrap();
        for (iso, pair_id) in [(840, 1), (978, 2)] {
            let runtime_vwap = oracle
                .get_worldwide_day_vwap_for_pair_id(day, pair_id)
                .unwrap()
                .unwrap_or(U256::ZERO);
            let runtime_scurve = outbe_oracle::scurve::get_max_active_scurve_value(
                &oracle,
                pair_id,
                day.to_timestamp_utc(),
            )
            .unwrap();
            assert_eq!(
                evaluated.entry_price(iso),
                Some(runtime_vwap.max(runtime_scurve))
            );
        }

        let mut reordered = raw_slots;
        reordered.swap(0, 1);
        assert!(evaluate_oracle_opening_v1(day, &[840, 978], &reordered).is_err());
    });
}

/// An ISO the chain does not list as a reference currency must not evaluate,
/// even when its derived pair happens to be registered.
#[test]
fn oracle_opening_rejects_an_iso_outside_the_on_chain_reference_list() {
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let day = WorldwideDay::new(20260715);
        seed_oracle(&storage, day);
        // 826 (GBP) has a registered pair but is absent from slot 55.
        let oracle = OracleContract::new(storage.clone());
        oracle.pair_ordinal.write(&coen_iso_pair(826), 3).unwrap();

        let plan = oracle_opening_slot_plan_v1(day, &[826, 840, 978], 2, 2, 4, 2).unwrap();
        let raw_slots = plan
            .slots
            .iter()
            .copied()
            .map(|slot| (slot, slot_word(&storage, slot)))
            .collect::<Vec<_>>();

        assert_eq!(
            evaluate_oracle_opening_v1(day, &[826, 840, 978], &raw_slots),
            Err(OracleOpeningEvaluationError::IsoNotAReferenceCurrency { iso: 826 })
        );
    });
}

#[test]
fn oracle_opening_plan_checks_every_cap_before_detail_allocation() {
    let day = WorldwideDay::new(20260715);
    let isos = [840u16];
    assert!(oracle_opening_slot_plan_v1(
        day,
        &isos,
        MAX_OCOMP_REFERENCE_CURRENCIES,
        MAX_OCOMP_WWD_PAIR_ENTRIES,
        MAX_OCOMP_ACTIVE_SCURVE_ENTRIES,
        0,
    )
    .is_ok());
    assert_eq!(
        oracle_opening_slot_plan_v1(day, &isos, MAX_OCOMP_REFERENCE_CURRENCIES + 1, 0, 0, 0),
        Err(OracleOpeningPlanError::ReferenceCurrencyCountExceedsCap {
            actual: 257,
            cap: 256,
        })
    );
    assert_eq!(
        oracle_opening_slot_plan_v1(day, &isos, 1, MAX_OCOMP_WWD_PAIR_ENTRIES + 1, 0, 0),
        Err(OracleOpeningPlanError::WorldwideDayPairCountExceedsCap {
            actual: 257,
            cap: 256,
        })
    );
    assert_eq!(
        oracle_opening_slot_plan_v1(day, &isos, 1, 0, MAX_OCOMP_ACTIVE_SCURVE_ENTRIES + 1, 0),
        Err(OracleOpeningPlanError::ActiveScurveCountExceedsCap {
            actual: 257,
            cap: 256,
        })
    );
    assert_eq!(
        oracle_opening_slot_plan_v1(day, &isos, 1, 0, 2, 3),
        Err(OracleOpeningPlanError::ScurveOldestExceedsCount {
            oldest: 3,
            count: 2,
        })
    );
}
