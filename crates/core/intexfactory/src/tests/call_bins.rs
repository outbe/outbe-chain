use super::*;

pub(super) fn qualify_series<'a>(
    s: &StorageHandle<'a>,
    id: u32,
    params: IssuanceParams,
) -> IntexFactoryContract<'a> {
    runtime::issue(s, params).unwrap();
    let mut f = IntexFactoryContract::new(s.clone());
    let mature = ISSUED_AT as u64 + 21 * DAY + 1;
    let floor = U256::from(EXPECTED_FLOOR);
    assert!(
        qualify_day(
            s,
            &mut f,
            id,
            QUALIFICATION_PERIOD,
            mature,
            floor + U256::from(1)
        ) == 1
    );
    f
}

/// Registry index the fixtures register the qualifier pair at; the rate
/// columns are keyed by it.
const PAIR_ID: u32 = 1;

/// List the qualifier currency in the oracle's reference registry, which the
/// scans walk to decide which currencies to price this block.
pub(super) fn list_reference(oracle: &OracleContract) {
    if oracle.reference_currencies.len().unwrap() == 0 {
        oracle.reference_currencies.push(REFERENCE_ISO).unwrap();
    }
}

pub(super) fn setup_pair(oracle: &OracleContract) -> AddressPair {
    let pair = outbe_oracle::api::AddressPair::new_coen_to(REFERENCE_ISO);
    let pair_id = PAIR_ID;
    oracle.pair_to_index.write(&pair, pair_id).unwrap();
    // Full registry entry so the production VWAP paths (calculate_vwaps
    // iterating registered vote-target pairs) see the pair too. `pair_at` reads
    // the reverse column.
    oracle.pair_count.write(pair_id).unwrap();
    oracle.pair_by_index.write_pair(&pair_id, pair).unwrap();
    oracle.vote_target.write(&pair, true).unwrap();
    list_reference(oracle);
    pair
}

fn set_vwap(oracle: &OracleContract, utc_day: u32, pair: AddressPair, value: U256) {
    // The value column is keyed by the pair's registry index.
    let pair_id = oracle.pair_index_of(pair).unwrap();
    oracle
        .utc_day_vwap_value
        .get_nested(&utc_day)
        .write(&pair_id, value)
        .unwrap();
    // Mirror the begin-block hook: the watermark covers every seeded day.
    if oracle.utc_day_vwap_last_finalized.read().unwrap() < utc_day {
        oracle.utc_day_vwap_last_finalized.write(utc_day).unwrap();
    }
}

/// Set `days` consecutive closed UTC days ending at `latest` to `value`.
pub(super) fn fill_days(
    oracle: &OracleContract,
    latest: u32,
    pair: AddressPair,
    days: u32,
    value: U256,
) {
    let mut d = latest;
    for _ in 0..days {
        set_vwap(oracle, d, pair, value);
        d = previous_date_key(d);
    }
}

#[test]
fn qualify_enrolls_in_call_trigger_bin() {
    with_factory(|s| {
        let f = qualify_series(&s, 7, sample(7));
        // Moved out of the floor index, into the call-trigger index.
        let floor_bin = IntexFactoryContract::price_to_bin(U256::from(EXPECTED_FLOOR)).unwrap();
        let trig_bin = IntexFactoryContract::price_to_bin(U256::from(EXPECTED_TRIGGER)).unwrap();
        assert_eq!(
            f.unqualified_bin_count
                .read(&IntexFactoryContract::scoped(REFERENCE_ISO, floor_bin))
                .unwrap(),
            0
        );
        assert_eq!(
            f.qualified_bin_count
                .read(&IntexFactoryContract::scoped(REFERENCE_ISO, trig_bin))
                .unwrap(),
            1
        );
    });
}

#[test]
fn insert_remove_qualified_roundtrip() {
    with_factory(|s| {
        let mut f = IntexFactoryContract::new(s.clone());
        let trigger = U256::from(EXPECTED_TRIGGER);
        let bin = IntexFactoryContract::price_to_bin(trigger).unwrap();
        f.insert_qualified_group(REFERENCE_ISO, WorldwideDay::new(11), trigger, &[sid(11)])
            .unwrap();
        f.insert_qualified_group(REFERENCE_ISO, WorldwideDay::new(22), trigger, &[sid(22)])
            .unwrap();
        assert_eq!(
            f.qualified_bin_count
                .read(&IntexFactoryContract::scoped(REFERENCE_ISO, bin))
                .unwrap(),
            2
        );
        f.remove_qualified_group(REFERENCE_ISO, WorldwideDay::new(11))
            .unwrap();
        assert_eq!(
            f.qualified_bin_count
                .read(&IntexFactoryContract::scoped(REFERENCE_ISO, bin))
                .unwrap(),
            1
        );
        f.remove_qualified_group(REFERENCE_ISO, WorldwideDay::new(22))
            .unwrap();
        assert_eq!(
            f.qualified_bin_count
                .read(&IntexFactoryContract::scoped(REFERENCE_ISO, bin))
                .unwrap(),
            0
        );
    });
}

#[test]
fn try_call_marks_called_when_threshold_met() {
    with_factory(|s| {
        let mut f = qualify_series(&s, 7, sample(7));
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        // All 30 window days above the trigger (threshold is 21).
        let scan_ts = ISSUED_AT as u64 + 60 * DAY;
        let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts));
        let breach = U256::from(EXPECTED_TRIGGER) + U256::from(1);
        fill_days(&oracle, last_closed_day, pair, 30, breach);

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
        let bin = IntexFactoryContract::price_to_bin(U256::from(EXPECTED_TRIGGER)).unwrap();
        assert_eq!(
            f.qualified_bin_count
                .read(&IntexFactoryContract::scoped(REFERENCE_ISO, bin))
                .unwrap(),
            0
        );
    });
}

#[test]
fn try_call_skips_when_below_threshold() {
    with_factory(|s| {
        let mut f = qualify_series(&s, 7, sample(7));
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        let scan_ts = ISSUED_AT as u64 + 60 * DAY;
        let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts));
        let breach = U256::from(EXPECTED_TRIGGER) + U256::from(1);
        let calm = U256::from(EXPECTED_TRIGGER); // equal: strict `>` is not a breach
                                                 // 20 breach days + 10 calm days; threshold is 21.
        let mut d = last_closed_day;
        for _ in 0..20 {
            set_vwap(&oracle, d, pair, breach);
            d = previous_date_key(d);
        }
        for _ in 0..10 {
            set_vwap(&oracle, d, pair, calm);
            d = previous_date_key(d);
        }

        let group = f
            .qualified_group(REFERENCE_ISO, WorldwideDay::new(7))
            .unwrap();
        assert_eq!(
            call_group(&s, &mut f, &oracle, pair, &group, last_closed_day, scan_ts),
            0
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
fn try_call_excludes_pre_issuance_days() {
    with_factory(|s| {
        // window 30, threshold 27: only days from issuance onward may count.
        // Seed the series directly with threshold 27 (above the 21d qualification
        // period), since the protocol default (21) does not exceed it and a
        // qualified series would always have >= 21 completed post-issuance days.
        outbe_intex::api::create_series(
            &s,
            outbe_intex::CreateSeriesParams {
                series_id: sid(8),
                worldwide_day: 8.into(),
                issued_intex_count: 100,
                promis_load_minor: PROMIS_LOAD_MINOR,
                entry_price_minor: U256::from(ENTRY_PRICE),
                floor_price_minor: U256::from(EXPECTED_FLOOR),
                call_price_minor: U256::from(EXPECTED_TRIGGER),
                call_trigger: outbe_intex::IntexCallTrigger {
                    call_window: 30 * DAY as u32,
                    call_threshold: 27 * DAY as u32,
                    call_notice_period: CALL_NOTICE_PERIOD,
                },
                issued_at: ISSUED_AT,
                issuance_currency: 840,
                reference_currency: 840,
            },
        )
        .unwrap();
        outbe_intex::api::mark_qualified(&s, sid(8)).unwrap();
        let mut f = IntexFactoryContract::new(s.clone());
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        // Scan only ~23 days after issuance, but set all 30 window days as
        // breaches: the ~7 pre-issuance days must not count, so 23 < 27.
        let scan_ts = ISSUED_AT as u64 + 23 * DAY;
        let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts));
        let breach = U256::from(EXPECTED_TRIGGER) + U256::from(1);
        fill_days(&oracle, last_closed_day, pair, 30, breach);

        let group = Group {
            iso_code: REFERENCE_ISO,
            worldwide_day: WorldwideDay::new(8),
            members: vec![sid(8)],
        };
        assert_eq!(
            call_group(&s, &mut f, &oracle, pair, &group, last_closed_day, scan_ts),
            0
        );
        assert_eq!(
            outbe_intex::api::read_series(&s, sid(8))
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            outbe_intex::IntexState::Qualified
        );
    });
}
