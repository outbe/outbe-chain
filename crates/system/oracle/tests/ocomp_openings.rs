use alloy_primitives::{keccak256, B256, U256};
use outbe_common::WorldwideDay;
use outbe_oracle::contract::OracleContract;
use outbe_oracle::{
    evaluate_oracle_opening_v1, oracle_count_slot_plan_v1, oracle_opening_slot_plan_v1,
    OracleOpeningPlanError, MAX_OCOMP_ACTIVE_SCURVE_ENTRIES, MAX_OCOMP_WWD_PAIR_ENTRIES,
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

#[test]
fn oracle_opening_plan_reads_the_exact_raw_slots_used_by_runtime_semantics() {
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let oracle = OracleContract::new(storage.clone());
        let day = WorldwideDay::new(20260715);
        let usd_pair = keccak256("USD/COEN");
        let eur_pair = keccak256("EUR/COEN");
        let usd_denom = keccak256("USD");
        let eur_denom = keccak256("EUR");

        oracle
            .settlement_iso_to_denom
            .write(&840, usd_denom)
            .unwrap();
        oracle.settlement_iso_to_pair.write(&840, usd_pair).unwrap();
        oracle
            .settlement_iso_to_denom
            .write(&978, eur_denom)
            .unwrap();
        oracle.settlement_iso_to_pair.write(&978, eur_pair).unwrap();
        oracle.pair_hash_to_id.write(&usd_pair, 1).unwrap();
        oracle.pair_hash_to_id.write(&eur_pair, 2).unwrap();
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

        let counts = oracle_count_slot_plan_v1(day, &[840, 978]).unwrap();
        assert_eq!(
            counts
                .slots
                .iter()
                .copied()
                .map(|slot| slot_word(&storage, slot))
                .collect::<Vec<_>>(),
            vec![
                U256::from_be_bytes(usd_denom.0),
                U256::from_be_bytes(usd_pair.0),
                U256::from_be_bytes(eur_denom.0),
                U256::from_be_bytes(eur_pair.0),
                U256::from(1),
                U256::from(2),
                U256::from(4),
                U256::from(2),
            ]
        );

        let plan =
            oracle_opening_slot_plan_v1(day, &[(840, usd_pair), (978, eur_pair)], 2, 4, 2).unwrap();
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
                U256::from_be_bytes(usd_denom.0),
                U256::from_be_bytes(usd_pair.0),
                U256::from(1),
                U256::from_be_bytes(eur_denom.0),
                U256::from_be_bytes(eur_pair.0),
                U256::from(2),
                U256::from(1),
                U256::from(2),
                U256::from(1),
                U256::from(100),
                U256::from(2),
                U256::from(200),
                U256::from(4),
                U256::from(2),
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

#[test]
fn oracle_opening_plan_checks_both_caps_before_detail_allocation() {
    let day = WorldwideDay::new(20260715);
    let pairs = [(840, B256::repeat_byte(1))];
    assert!(oracle_opening_slot_plan_v1(
        day,
        &pairs,
        MAX_OCOMP_WWD_PAIR_ENTRIES,
        MAX_OCOMP_ACTIVE_SCURVE_ENTRIES,
        0,
    )
    .is_ok());
    assert_eq!(
        oracle_opening_slot_plan_v1(day, &pairs, MAX_OCOMP_WWD_PAIR_ENTRIES + 1, 0, 0),
        Err(OracleOpeningPlanError::WorldwideDayPairCountExceedsCap {
            actual: 257,
            cap: 256,
        })
    );
    assert_eq!(
        oracle_opening_slot_plan_v1(day, &pairs, 0, MAX_OCOMP_ACTIVE_SCURVE_ENTRIES + 1, 0,),
        Err(OracleOpeningPlanError::ActiveScurveCountExceedsCap {
            actual: 257,
            cap: 256,
        })
    );
    assert_eq!(
        oracle_opening_slot_plan_v1(day, &pairs, 0, 2, 3),
        Err(OracleOpeningPlanError::ScurveOldestExceedsCount {
            oldest: 3,
            count: 2,
        })
    );
}
