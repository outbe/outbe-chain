use super::call_bins::{fill_days, list_reference, qualify_series, setup_pair};
use super::*;

#[test]
fn scan_and_qualify_promotes_aged_series() {
    with_factory(|s| {
        runtime::issue(&s, sample(7)).unwrap();
        // Qualifier pair live rate above the floor.
        let oracle = OracleContract::new(s.clone());
        let pair = outbe_oracle::api::AddressPair::new_coen_to(REFERENCE_ISO);
        // The ISO resolves through the pair registry, so the pair must exist
        // and the rate columns are keyed by its index.
        oracle.pair_to_index.write(&pair, PAIR_ID).unwrap();
        oracle
            .exchange_rate
            .write(&PAIR_ID, U256::from(EXPECTED_FLOOR) + U256::from(1))
            .unwrap();
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
        let pair = outbe_oracle::api::AddressPair::new_coen_to(REFERENCE_ISO);
        oracle.pair_to_index.write(&pair, PAIR_ID).unwrap();
        // Out-of-range rate: price_to_bin overflows.
        oracle.exchange_rate.write(&PAIR_ID, U256::MAX).unwrap();
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
fn scan_isolates_bad_series() {
    with_factory(|s| {
        runtime::issue(&s, sample(7)).unwrap();
        // A bin entry whose series record does not exist: read_series errors -> the series must be
        // skipped (logged), not halt the block.
        IntexFactoryContract::new(s.clone())
            .insert_unqualified(sid(999), REFERENCE_ISO, U256::from(EXPECTED_FLOOR))
            .unwrap();

        let oracle = OracleContract::new(s.clone());
        let pair = outbe_oracle::api::AddressPair::new_coen_to(REFERENCE_ISO);
        // The ISO resolves through the pair registry, so the pair must exist
        // and the rate columns are keyed by its index.
        oracle.pair_to_index.write(&pair, PAIR_ID).unwrap();
        oracle
            .exchange_rate
            .write(&PAIR_ID, U256::from(EXPECTED_FLOOR) + U256::from(1))
            .unwrap();
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
        let pair = outbe_oracle::api::AddressPair::new_coen_to(REFERENCE_ISO);
        // The ISO resolves through the pair registry, so the pair must exist
        // and the rate columns are keyed by its index.
        oracle.pair_to_index.write(&pair, PAIR_ID).unwrap();
        // Rate well above both floors so both bins are eligible.
        oracle
            .exchange_rate
            .write(&PAIR_ID, U256::from(EXPECTED_FLOOR) * U256::from(1000))
            .unwrap();
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
