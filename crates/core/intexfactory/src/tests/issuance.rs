use super::*;

#[test]
fn issue_creates_series_in_registry() {
    with_factory(|s| {
        runtime::issue(&s, sample(7)).unwrap();

        // The series is captured in Intex with the issuance identity.
        let r = outbe_intex::api::read_series(&s, sid(7)).unwrap();
        assert_eq!(r.series_id, sid(7));
        assert_eq!(r.promis_load_minor, U256::from(PROMIS_LOAD_MINOR));
        assert_eq!(r.entry_price_minor, U256::from(ENTRY_PRICE));
        // Floor and trigger are derived from the clearing price at issuance.
        assert_eq!(r.floor_price_minor, U256::from(EXPECTED_FLOOR));
        assert_eq!(r.issued_intex_count, 100);
        assert_eq!(r.call_notice_period, CALL_NOTICE_PERIOD);
        // Window/threshold/call-period are IntexFactory protocol constants now.
        assert_eq!(r.call_price_minor, U256::from(EXPECTED_TRIGGER));
        assert_eq!(
            r.call_trigger(),
            outbe_intex::IntexCallTrigger {
                call_window: 28 * DAY as u32,
                call_threshold: 21 * DAY as u32,
                call_notice_period: CALL_NOTICE_PERIOD,
            }
        );
        // Born Issued; issued_at is the block timestamp.
        assert_eq!(
            r.lifecycle_state().unwrap(),
            outbe_intex::IntexState::Issued
        );
        assert_eq!(r.issued_at, ISSUED_AT);
        assert_eq!(r.called_at, 0);
    });
}

#[test]
fn issue_rejects_duplicate_series() {
    with_factory(|s| {
        runtime::issue(&s, sample(7)).unwrap();
        // The registry record-create rejects a duplicate series id.
        assert!(runtime::issue(&s, sample(7)).is_err());
    });
}

#[test]
fn issue_zero_winners_leaves_the_day_untouched() {
    with_factory(|s| {
        // Lysis recorded contributors for the day, but this group had no winners.
        outbe_intex::api::record_contributors(
            &s,
            WorldwideDay::new(7),
            &[(holder(), U256::from(100u64))],
        )
        .unwrap();
        let mut p = sample(7);
        p.issued_intex_count = 0;
        runtime::issue(&s, p).unwrap();

        // No series is created, and the day's map is left for its caller to
        // decide on: sibling groups of the same day may still distribute.
        assert!(!outbe_intex::api::series_exists(&s, sid(7)).unwrap());
        assert_eq!(
            outbe_intex::api::contributor_count(&s, WorldwideDay::new(7)).unwrap(),
            1
        );
    });
}

#[test]
fn issuance_legs_route_winners_to_their_own_chain() {
    // One winner on chain 10, one on chain 20; chain 30 in the snapshot has none.
    let other = address!("0xCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC");
    let mut p = sample(7);
    p.recipients = vec![holder(), other];
    p.quantities = vec![U256::from(1), U256::from(2)];
    p.recipient_chains = vec![10, 20];
    p.snapshot_chains = vec![10, 20, 30];

    let legs = runtime::issuance_legs(&p);
    assert_eq!(legs.len(), 3);
    assert_eq!(legs[0], (10, vec![holder()], vec![U256::from(1)]));
    assert_eq!(legs[1], (20, vec![other], vec![U256::from(2)]));
    assert_eq!(legs[2], (30, vec![], vec![])); // create-only leg
}

#[test]
fn issue_enrolls_series_in_dense_enumeration() {
    with_factory(|s| {
        runtime::issue(&s, sample(11)).unwrap();
        runtime::issue(&s, sample(22)).unwrap();
        assert_eq!(outbe_intex::api::total_series(&s).unwrap(), 2);
        assert_eq!(outbe_intex::api::series_id_at(&s, 0).unwrap(), sid(11));
        assert_eq!(outbe_intex::api::series_id_at(&s, 1).unwrap(), sid(22));
    });
}

#[test]
fn floor_and_call_derivation() {
    let floor = runtime::marked_up(U256::from(ENTRY_PRICE), FLOOR_RATE).unwrap();
    let call = runtime::marked_up(U256::from(ENTRY_PRICE), CALL_RATE).unwrap();
    assert_eq!(floor, U256::from(EXPECTED_FLOOR));
    assert_eq!(call, U256::from(EXPECTED_TRIGGER));

    let one = U256::from(1_000_000u64);
    assert_eq!(
        runtime::marked_up(one, FLOOR_RATE).unwrap(),
        U256::from(1_080_000u64)
    );
    assert_eq!(
        runtime::marked_up(one, CALL_RATE).unwrap(),
        U256::from(2_280_000u64)
    );
}

#[test]
fn coen_iso_one_maps_to_the_center_price_bin_at_six_decimals() {
    assert_eq!(
        IntexFactoryContract::price_to_bin(U256::from(1_000_000u64)).unwrap(),
        REAL_ID_SHIFT as u32
    );
}

#[test]
fn coen_iso_wire_price_preserves_the_six_decimal_integer() {
    assert_eq!(
        runtime::to_wire_price(U256::from(1_234_567u64)).unwrap(),
        1_234_567
    );
    assert_eq!(
        runtime::to_wire_price(U256::from(u64::MAX)).unwrap(),
        u64::MAX
    );
    assert!(runtime::to_wire_price(U256::from(u64::MAX) + U256::ONE).is_err());
}
