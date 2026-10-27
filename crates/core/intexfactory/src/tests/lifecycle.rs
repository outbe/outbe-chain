use super::*;
use outbe_primitives::time::SECONDS_PER_DAY_U32;

#[test]
fn issue_enrolls_in_call_bin() {
    with_factory(|s| {
        runtime::issue_intex(&s, sample(7)).unwrap();
        let f = IntexContract::new(s.clone());
        let bin = outbe_primitives::call_bins::price_to_bin(U256::from(EXPECTED_TRIGGER)).unwrap();
        assert_eq!(
            f.call_bin_count
                .read(&IntexContract::scoped(REFERENCE_ISO, bin))
                .unwrap(),
            1
        );
    });
}

/// Registry index the fixtures register the reference pair at. The rate
/// columns are keyed by it.
const PAIR_ID: u32 = 1;

/// List the reference currency in the oracle's reference registry, which the
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
    oracle.record_utc_day_vwap(utc_day, pair_id, value).unwrap();
    // Mirror the begin-block hook: the watermark covers every seeded day.
    if oracle.utc_day_vwap_last_finalized.read().unwrap() < utc_day {
        oracle.utc_day_vwap_last_finalized.write(utc_day).unwrap();
    }
}

/// Set `days` consecutive closed UTC days ending at `latest` to `value`.
/// The returned key is the next older day after that run.
pub(super) fn fill_days(
    oracle: &OracleContract,
    latest: u32,
    pair: AddressPair,
    days: u32,
    value: U256,
) -> u32 {
    let mut d = latest;
    for _ in 0..days {
        set_vwap(oracle, d, pair, value);
        d = previous_date_key(d).expect("date step");
    }
    d
}

#[test]
fn a_group_holds_its_bin_entry_until_its_last_member_leaves() {
    with_factory(|s| {
        runtime::issue_intex(&s, sample(11)).unwrap();
        runtime::issue_intex(&s, sample(22)).unwrap();
        let mut f = IntexContract::new(s.clone());
        let bin = outbe_primitives::call_bins::price_to_bin(U256::from(EXPECTED_TRIGGER)).unwrap();
        let count = |f: &IntexContract<'_>| {
            f.call_bin_count
                .read(&IntexContract::scoped(REFERENCE_ISO, bin))
                .unwrap()
        };
        let group = group_of(&s, sid(11));
        assert_eq!(group, group_of(&s, sid(22)));
        assert_eq!(count(&f), 1);
        f.leave_call_group(group, sid(11)).unwrap();
        assert_eq!(count(&f), 1);
        f.leave_call_group(group, sid(22)).unwrap();
        assert_eq!(count(&f), 0);
    });
}

#[test]
fn try_call_marks_called_when_threshold_met() {
    with_factory(|s| {
        runtime::issue_intex(&s, sample(7)).unwrap();
        let mut f = IntexContract::new(s.clone());
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        // All 30 window days above the trigger (threshold is 21).
        let scan_ts = ISSUED_AT as u64 + 60 * DAY;
        let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts)).expect("date step");
        let breach = U256::from(EXPECTED_TRIGGER) + U256::from(1);
        fill_days(&oracle, last_closed_day, pair, 30, breach);

        let group = group_of(&s, sid(7));
        assert_eq!(call_group(&s, &mut f, group, last_closed_day, scan_ts), 1);
        assert_eq!(series_state(&s, sid(7)), outbe_intex::IntexState::Called);
        let bin = outbe_primitives::call_bins::price_to_bin(U256::from(EXPECTED_TRIGGER)).unwrap();
        assert_eq!(
            f.call_bin_count
                .read(&IntexContract::scoped(REFERENCE_ISO, bin))
                .unwrap(),
            0
        );
    });
}

#[test]
fn try_call_skips_when_below_threshold() {
    with_factory(|s| {
        runtime::issue_intex(&s, sample(7)).unwrap();
        let mut f = IntexContract::new(s.clone());
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        let scan_ts = ISSUED_AT as u64 + 60 * DAY;
        let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts)).expect("date step");
        let breach = U256::from(EXPECTED_TRIGGER) + U256::from(1);
        // Equal is not a breach: the check is strict `>`. 20 breach days and 10 calm
        // days stay under the threshold of 21.
        let calm = U256::from(EXPECTED_TRIGGER);
        let after_breach = fill_days(&oracle, last_closed_day, pair, 20, breach);
        fill_days(&oracle, after_breach, pair, 10, calm);

        let group = group_of(&s, sid(7));
        assert_eq!(call_group(&s, &mut f, group, last_closed_day, scan_ts), 0);
        assert_eq!(series_state(&s, sid(7)), outbe_intex::IntexState::Issued);
    });
}

#[test]
fn try_call_excludes_pre_issuance_days() {
    with_factory(|s| {
        // window 30, threshold 27: only days from issuance onward may count.
        outbe_intex::api::create_series(
            &s,
            outbe_intex::CreateSeriesParams {
                series_id: sid(8),
                worldwide_day: 8.into(),
                issued_units: 100,
                promis_load_minor: PROMIS_LOAD_MINOR,
                entry_price_minor: U256::from(ENTRY_PRICE),
                call_price_minor: U256::from(EXPECTED_TRIGGER),
                call_trigger: outbe_intex::IntexCallTrigger {
                    call_window_seconds: 30 * SECONDS_PER_DAY_U32,
                    call_threshold_seconds: 27 * SECONDS_PER_DAY_U32,
                    call_notice_period_seconds: CALL_NOTICE_PERIOD,
                    transit_margin_seconds: 3_600,
                },
                issued_at: ISSUED_AT,
                issuance_currency: 840,
                reference_currency: 840,
            },
        )
        .unwrap();
        let mut f = IntexContract::new(s.clone());
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        // Scan only ~23 days after issuance, but set all 30 window days as
        // breaches: the ~7 pre-issuance days must not count, so 23 < 27.
        let scan_ts = ISSUED_AT as u64 + 23 * DAY;
        let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts)).expect("date step");
        let breach = U256::from(EXPECTED_TRIGGER) + U256::from(1);
        fill_days(&oracle, last_closed_day, pair, 30, breach);

        let group = group_of(&s, sid(8));
        assert_eq!(call_group(&s, &mut f, group, last_closed_day, scan_ts), 0);
        assert_eq!(series_state(&s, sid(8)), outbe_intex::IntexState::Issued);
    });
}

#[path = "lifecycle_call_sweep.rs"]
mod call_sweep;
#[path = "lifecycle_called_pstar.rs"]
mod called_pstar;

#[test]
fn a_group_is_called_once() {
    with_factory(|s| {
        runtime::issue_intex(&s, sample(9)).unwrap();
        outbe_intex::api::call_group_of(&s, sid(9), ISSUED_AT + 10).unwrap();
        let series = outbe_intex::api::read_series(&s, sid(9)).unwrap();
        assert_eq!(series_state(&s, sid(9)), outbe_intex::IntexState::Called);
        assert_eq!(
            outbe_intex::api::call_stamp(&s, &series).unwrap().called_at,
            ISSUED_AT + 10
        );
        assert!(outbe_intex::api::call_group_of(&s, sid(9), ISSUED_AT + 20).is_err());
    });
}
