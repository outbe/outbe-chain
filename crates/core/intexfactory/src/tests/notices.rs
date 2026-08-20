use super::call_bins::{fill_days, setup_pair};
use super::*;

/// Seed a series directly in the registry + bin index, bypassing issue()
/// so tests can omit the OriginRouter stub.
fn seed_issued(s: &StorageHandle<'_>, id: u32) {
    outbe_intex::api::create_series(
        s,
        outbe_intex::CreateSeriesParams {
            series_id: sid(id),
            worldwide_day: id.into(),
            issued_intex_count: 100,
            promis_load_minor: PROMIS_LOAD_MINOR,
            entry_price_minor: U256::from(ENTRY_PRICE),
            floor_price_minor: U256::from(EXPECTED_FLOOR),
            call_price_minor: U256::from(EXPECTED_TRIGGER),
            call_trigger: outbe_intex::IntexCallTrigger {
                call_window: 30 * DAY as u32,
                call_threshold: 21 * DAY as u32,
                call_notice_period: CALL_NOTICE_PERIOD,
            },
            issued_at: ISSUED_AT,
            issuance_currency: 840,
            reference_currency: 840,
        },
    )
    .unwrap();
    IntexFactoryContract::new(s.clone())
        .insert_unqualified(sid(id), REFERENCE_ISO, U256::from(EXPECTED_FLOOR))
        .unwrap();
}

#[test]
fn qualify_survives_router_failure() {
    // No OriginRouter stub: notify_qualified fails silently.
    // The Issued -> Qualified transition must still complete.
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(ISSUED_AT as u64));
    storage.stub_sub_call_at(
        crate::constants::INTEX_NFT1155_ADDRESS,
        alloy_primitives::Bytes::from(vec![0u8; 32]),
    );
    StorageHandle::enter(&mut storage, |s| {
        seed_issued(&s, 7);
        let mut f = IntexFactoryContract::new(s.clone());
        let mature = ISSUED_AT as u64 + 21 * DAY + 1;
        assert_eq!(
            qualify_day(
                &s,
                &mut f,
                7,
                QUALIFICATION_PERIOD,
                mature,
                U256::from(EXPECTED_FLOOR) + U256::from(1)
            ),
            1
        );
        assert_eq!(
            outbe_intex::api::read_series(&s, sid(7))
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            outbe_intex::IntexState::Qualified
        );
    });
}

#[test]
fn call_survives_router_failure() {
    // No OriginRouter stub: notify_called fails silently.
    // The Qualified -> Called transition must still complete.
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(ISSUED_AT as u64));
    storage.stub_sub_call_at(
        crate::constants::INTEX_NFT1155_ADDRESS,
        alloy_primitives::Bytes::from(vec![0u8; 32]),
    );
    StorageHandle::enter(&mut storage, |s| {
        seed_issued(&s, 7);
        outbe_intex::api::mark_qualified(&s, sid(7)).unwrap();
        let mut f = IntexFactoryContract::new(s.clone());
        f.insert_qualified_group(
            REFERENCE_ISO,
            WorldwideDay::new(7),
            U256::from(EXPECTED_TRIGGER),
            &[sid(7)],
        )
        .unwrap();

        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        let scan_ts = ISSUED_AT as u64 + 60 * DAY;
        let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts));
        fill_days(
            &oracle,
            last_closed_day,
            pair,
            30,
            U256::from(EXPECTED_TRIGGER) + U256::from(1),
        );

        let group = f
            .qualified_group(REFERENCE_ISO, WorldwideDay::new(7))
            .unwrap();
        assert_eq!(
            call_group(&s, &mut f, &oracle, pair, &group, last_closed_day, scan_ts),
            1
        );
        assert_eq!(
            outbe_intex::api::read_series(&s, sid(7))
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            outbe_intex::IntexState::Called
        );
    });
}
