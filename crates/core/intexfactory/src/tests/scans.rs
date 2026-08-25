use super::*;

use super::lifecycle::{fill_days, list_reference, qualify_series, setup_pair};

#[test]
fn scan_and_qualify_promotes_aged_series() {
    with_factory(|s| {
        runtime::issue(&s, sample(7)).unwrap();
        // Qualifier pair live rate above the floor.
        let oracle = OracleContract::new(s.clone());
        write_rate(
            &oracle,
            REFERENCE_ISO,
            PAIR_ID,
            U256::from(EXPECTED_FLOOR) + U256::from(1),
        );
        list_reference(&oracle);

        let mature_ts = ISSUED_AT as u64 + 21 * DAY + 1;
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, mature_ts, CHAIN_ID),
            s.clone(),
        );
        assert_eq!(qualified::scan_and_qualify(&ctx).unwrap(), 1);

        let r = outbe_intex::api::read_series(&s, sid(7)).unwrap();
        assert_eq!(
            r.lifecycle_state().unwrap(),
            outbe_intex::IntexState::Qualified
        );
        let f = IntexFactoryContract::new(s.clone());
        let trig_bin = IntexFactoryContract::price_to_bin(U256::from(EXPECTED_TRIGGER)).unwrap();
        assert_eq!(
            f.qualified_bin_count
                .read(&IntexFactoryContract::scoped(REFERENCE_ISO, trig_bin))
                .unwrap(),
            1
        );
    });
}

#[test]
fn scan_and_call_force_calls_breached_series() {
    with_factory(|s| {
        let _f = qualify_series(&s, 7, sample(7));
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        let scan_ts = ISSUED_AT as u64 + 60 * DAY;
        let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts));
        let breach = U256::from(EXPECTED_TRIGGER) + U256::from(1);
        fill_days(&oracle, last_closed_day, pair, 30, breach);

        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, scan_ts, CHAIN_ID),
            s.clone(),
        );
        assert_eq!(called::scan_and_call(&ctx).unwrap(), 1);
        assert_eq!(
            outbe_intex::api::read_series(&s, sid(7))
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            outbe_intex::IntexState::Called
        );
    });
}

#[test]
fn scan_and_call_reads_daily_vwap_at_midnight() {
    // Regression: the scan fires on the midnight Cycle tick, when yesterday's
    // WorldwideDay snapshot does not exist yet (metadosis writes it at noon of
    // the current day). The finalized per-UTC-day VWAP is already closed by
    // then and must be the scan's price source. Exactly `threshold` (21)
    // breach days are seeded through the production finalization path and the
    // scan day itself stays unfinalized, so reading any other day — or any
    // other store — drops below the threshold and fails the call.
    with_factory(|s| {
        let _f = qualify_series(&s, 7, sample(7));
        let mut oracle = OracleContract::new(s.clone());
        setup_pair(&oracle);
        // Exact midnight UTC, well past the qualification period.
        let scan_ts = (ISSUED_AT as u64 / DAY + 60) * DAY;
        let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts));
        let breach = U256::from(EXPECTED_TRIGGER) + U256::from(1);

        // Oldest-first: snapshot + finalize 21 closed days ending yesterday.
        let mut days = [0u32; 21];
        let mut d = last_closed_day;
        for slot in days.iter_mut().rev() {
            *slot = d;
            d = previous_date_key(d);
        }
        for day in days {
            let noon = date_key_to_utc_timestamp(day) + DAY / 2;
            oracle
                .write_snapshot(
                    noon,
                    &[(
                        outbe_oracle::api::AddressPair::new_coen_to(REFERENCE_ISO),
                        breach,
                        U256::from(1),
                    )],
                )
                .unwrap();
            oracle.finalize_utc_day_vwap(day).unwrap();
        }
        // The begin-block hook advances the watermark after finalizing.
        oracle
            .utc_day_vwap_last_finalized
            .write(last_closed_day)
            .unwrap();

        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, scan_ts, CHAIN_ID),
            s.clone(),
        );
        assert_eq!(called::scan_and_call(&ctx).unwrap(), 1);
        assert_eq!(
            outbe_intex::api::read_series(&s, sid(7))
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            outbe_intex::IntexState::Called
        );
    });
}

// ---------------------------------------------------------------------
// begin-block / daily scan error isolation (no chain halt)
// ---------------------------------------------------------------------

#[test]
fn scan_does_not_halt_on_overflow_rate() {
    with_factory(|s| {
        runtime::issue(&s, sample(7)).unwrap();
        let oracle = OracleContract::new(s.clone());
        // Out-of-range rate: price_to_bin overflows.
        write_rate(&oracle, REFERENCE_ISO, PAIR_ID, U256::MAX);
        list_reference(&oracle);

        let mature_ts = ISSUED_AT as u64 + 21 * DAY + 1;
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, mature_ts, CHAIN_ID),
            s.clone(),
        );
        // Must not halt: returns Ok(0) and leaves the series untouched.
        assert_eq!(qualified::scan_and_qualify(&ctx).unwrap(), 0);
        assert_eq!(
            outbe_intex::api::read_series(&s, sid(7))
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            outbe_intex::IntexState::Issued
        );
    });
}

#[test]
fn qualification_scan_skips_a_stale_rate_without_halting_the_block() {
    with_factory(|s| {
        runtime::issue(&s, sample(7)).unwrap();
        let oracle = OracleContract::new(s.clone());
        write_rate(
            &oracle,
            REFERENCE_ISO,
            PAIR_ID,
            U256::from(EXPECTED_FLOOR + 1),
        );
        list_reference(&oracle);
        let mature_ts = ISSUED_AT as u64 + QUALIFICATION_PERIOD as u64 + 1;
        s.set_block_timestamp(U256::from(mature_ts)).unwrap();
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, mature_ts, CHAIN_ID),
            s.clone(),
        );

        assert_eq!(qualified::scan_and_qualify(&ctx).unwrap(), 0);
        assert_eq!(
            outbe_intex::api::read_series(&s, sid(7))
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            outbe_intex::IntexState::Issued
        );
    });
}

#[test]
fn scan_isolates_bad_series() {
    with_factory(|s| {
        runtime::issue(&s, sample(7)).unwrap();
        // A bin entry whose series record does not exist: read_series errors -> the series must be
        // skipped (logged), not halt the block.
        IntexFactoryContract::new(s.clone())
            .insert_unqualified(sid(999), REFERENCE_ISO, U256::from(EXPECTED_FLOOR))
            .unwrap();

        let oracle = OracleContract::new(s.clone());
        write_rate(
            &oracle,
            REFERENCE_ISO,
            PAIR_ID,
            U256::from(EXPECTED_FLOOR) + U256::from(1),
        );
        list_reference(&oracle);

        let mature_ts = ISSUED_AT as u64 + 21 * DAY + 1;
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, mature_ts, CHAIN_ID),
            s.clone(),
        );
        // Bad series (999) skipped; healthy series (7) still qualifies.
        assert_eq!(qualified::scan_and_qualify(&ctx).unwrap(), 1);
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
fn call_scan_does_not_halt_on_overflow_vwap() {
    with_factory(|s| {
        let _f = qualify_series(&s, 7, sample(7));
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        let scan_ts = ISSUED_AT as u64 + 60 * DAY;
        let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts));
        // Out-of-range VWAP for the completed day: price_to_bin overflows.
        fill_days(&oracle, last_closed_day, pair, 1, U256::MAX);

        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, scan_ts, CHAIN_ID),
            s.clone(),
        );
        // Must not halt: returns Ok(0) and leaves the series Qualified.
        assert_eq!(called::scan_and_call(&ctx).unwrap(), 0);
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
fn scan_caps_work_per_block_and_resumes_via_cursor() {
    with_factory(|s| {
        let oracle = OracleContract::new(s.clone());
        // Rate well above both floors so both bins are eligible.
        write_rate(
            &oracle,
            REFERENCE_ISO,
            PAIR_ID,
            U256::from(EXPECTED_FLOOR) * U256::from(1000),
        );
        list_reference(&oracle);

        // Two distinct bins: the first holds exactly MAX_SERIES_PER_BLOCK entries, the second a few.
        // Bogus ids (no series record) are per-series skipped but still count toward the cap.
        let cap = crate::constants::MAX_GROUP_DECISIONS_PER_SWEEP;
        let f1 = U256::from(EXPECTED_FLOOR);
        let f2 = U256::from(EXPECTED_FLOOR) * U256::from(4);
        {
            let mut factory = IntexFactoryContract::new(s.clone());
            for id in 1..=cap {
                factory
                    .insert_unqualified(sid(id), REFERENCE_ISO, f1)
                    .unwrap();
            }
            for id in 1001..=1005u32 {
                factory
                    .insert_unqualified(sid(id), REFERENCE_ISO, f2)
                    .unwrap();
            }
        }
        let bin2 = IntexFactoryContract::price_to_bin(f2).unwrap();

        let ts = ISSUED_AT as u64 + 21 * DAY + 1;
        let ctx =
            BlockRuntimeContext::new(BlockContext::empty_for_tests(1, ts, CHAIN_ID), s.clone());

        // Block 1 caps after the first (cap-sized) bin; the second bin is deferred.
        qualified::scan_and_qualify(&ctx).unwrap();
        let cursor1 = IntexFactoryContract::new(s.clone())
            .qualify_scan_cursor
            .read(&REFERENCE_ISO)
            .unwrap();
        assert!(cursor1 > 0, "cursor advanced past the capped bin");
        assert_eq!(
            IntexFactoryContract::new(s.clone())
                .unqualified_bin_count
                .read(&IntexFactoryContract::scoped(REFERENCE_ISO, bin2))
                .unwrap(),
            5,
            "second bin untouched in block 1"
        );

        // Block 2 resumes at the second bin and wraps the cursor to 0.
        qualified::scan_and_qualify(&ctx).unwrap();
        let cursor2 = IntexFactoryContract::new(s.clone())
            .qualify_scan_cursor
            .read(&REFERENCE_ISO)
            .unwrap();
        assert_eq!(cursor2, 0, "cursor wrapped after a full sweep");
    });
}

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
