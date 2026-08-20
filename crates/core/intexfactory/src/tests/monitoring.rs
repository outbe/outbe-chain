use super::*;

/// A series priced in the second reference currency.
fn eur_series(worldwide_day: u32) -> IssuanceParams {
    IssuanceParams {
        series_id: SeriesId::pack(WorldwideDay::new(worldwide_day), *b"EUR", b'E').unwrap(),
        reference_currency: EUR_ISO,
        issuance_currency: EUR_ISO,
        ..sample(worldwide_day)
    }
}

#[test]
fn a_currency_rate_never_qualifies_another_currency_series() {
    with_factory(|s| {
        // Both series carry the same entry price, so their floors land in the
        // same bin: only the currency namespace keeps them apart.
        runtime::issue(&s, sample(7)).unwrap();
        runtime::issue(&s, eur_series(8)).unwrap();

        let oracle = OracleContract::new(s.clone());
        oracle.reference_currencies.push(REFERENCE_ISO).unwrap();
        oracle.reference_currencies.push(EUR_ISO).unwrap();
        let above = U256::from(EXPECTED_FLOOR) + U256::from(1);
        let below = U256::from(EXPECTED_FLOOR) - U256::from(1);
        write_rate(&oracle, REFERENCE_ISO, PAIR_ID, above);
        write_rate(&oracle, EUR_ISO, EUR_PAIR_ID, below);

        let mature_ts = ISSUED_AT as u64 + 21 * DAY + 1;
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, mature_ts, CHAIN_ID),
            s.clone(),
        );
        assert_eq!(qualified::scan_and_qualify(&ctx).unwrap(), 1);

        // The euro series shares the dollar series' bin and sits below its own
        // rate, so a scan that crossed the currency border would have taken it.
        assert_eq!(
            outbe_intex::api::read_series(&s, sid(7))
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            outbe_intex::IntexState::Qualified
        );
        let eur_id = eur_series(8).series_id;
        assert_eq!(
            outbe_intex::api::read_series(&s, eur_id)
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            outbe_intex::IntexState::Issued
        );

        // Its own rate crossing the floor is what qualifies it.
        write_rate(&oracle, EUR_ISO, EUR_PAIR_ID, above);
        assert_eq!(qualified::scan_and_qualify(&ctx).unwrap(), 1);
        assert_eq!(
            outbe_intex::api::read_series(&s, eur_id)
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            outbe_intex::IntexState::Qualified
        );
    });
}

#[test]
fn an_unpriced_reference_currency_is_skipped_not_fatal() {
    with_factory(|s| {
        runtime::issue(&s, sample(7)).unwrap();
        let oracle = OracleContract::new(s.clone());
        // Listed before its pair exists — the registry and the pair registry are
        // populated independently.
        oracle.reference_currencies.push(EUR_ISO).unwrap();
        oracle.reference_currencies.push(REFERENCE_ISO).unwrap();
        write_rate(
            &oracle,
            REFERENCE_ISO,
            PAIR_ID,
            U256::from(EXPECTED_FLOOR) + U256::from(1),
        );

        let mature_ts = ISSUED_AT as u64 + 21 * DAY + 1;
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, mature_ts, CHAIN_ID),
            s.clone(),
        );
        assert_eq!(qualified::scan_and_qualify(&ctx).unwrap(), 1);
    });
}

#[test]
fn a_currency_cut_off_by_the_budget_is_scanned_first_next_block() {
    with_factory(|s| {
        runtime::issue(&s, eur_series(8)).unwrap();

        let oracle = OracleContract::new(s.clone());
        oracle.reference_currencies.push(REFERENCE_ISO).unwrap();
        oracle.reference_currencies.push(EUR_ISO).unwrap();
        let above = U256::from(EXPECTED_FLOOR) + U256::from(1);
        write_rate(&oracle, REFERENCE_ISO, PAIR_ID, above);
        write_rate(&oracle, EUR_ISO, EUR_PAIR_ID, above);

        // The dollar bin alone fills a whole block's budget. Ids without a series
        // record are skipped per series but still count against it.
        {
            let mut factory = IntexFactoryContract::new(s.clone());
            for id in 1..=crate::constants::MAX_GROUP_DECISIONS_PER_SWEEP {
                factory
                    .insert_unqualified(sid(id), REFERENCE_ISO, U256::from(EXPECTED_FLOOR))
                    .unwrap();
            }
        }

        let mature_ts = ISSUED_AT as u64 + 21 * DAY + 1;
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, mature_ts, CHAIN_ID),
            s.clone(),
        );

        let eur_id = eur_series(8).series_id;
        qualified::scan_and_qualify(&ctx).unwrap();
        assert_eq!(
            outbe_intex::api::read_series(&s, eur_id)
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            outbe_intex::IntexState::Issued,
            "the dollar bin consumed the block's budget"
        );
        assert_eq!(
            IntexFactoryContract::new(s.clone())
                .qualify_currency_cursor
                .read()
                .unwrap(),
            1,
            "the next block resumes at the currency that was cut off"
        );

        qualified::scan_and_qualify(&ctx).unwrap();
        assert_eq!(
            outbe_intex::api::read_series(&s, eur_id)
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            outbe_intex::IntexState::Qualified
        );
    });
}
