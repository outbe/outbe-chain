//! `trigger < p_star` and "breached on at least `threshold` days" are the same
//! statement. These tests check that they stay the same statement.

use alloy_primitives::U256;
use outbe_intex::SeriesId;
use outbe_oracle::api::AddressPair;
use outbe_oracle::schema::OracleContract;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::previous_date_key;
use outbe_primitives::time::WorldwideDay;

use crate::called;
use outbe_intex::IntexContract;
use outbe_oracle::call_window::CallWindow;
use outbe_primitives::call_breach::ScanTerms;

const REFERENCE_ISO: u16 = 840;
const LAST_DAY: u32 = 20260212;
const WINDOW: u32 = 28;
const THRESHOLD: u32 = 21;
use outbe_primitives::time::SECONDS_PER_DAY as DAY_SECS;

use super::with_factory as with_oracle;
use super::{set_vwap, setup_pair};

/// Write one window's days, latest first. `None` leaves the day unpriced.
fn seed_window(oracle: &OracleContract, pair: AddressPair, days: &[Option<u64>]) {
    let mut day = LAST_DAY;
    for value in days {
        if let Some(v) = value {
            set_vwap(oracle, day, pair, U256::from(*v));
        }
        day = previous_date_key(day).expect("date step");
    }
}

/// The decision as specified: count the days whose VWAP exceeded the trigger.
fn breaches_at_least(days: &[Option<u64>], trigger: u64, threshold: u32) -> bool {
    days.iter()
        .flatten()
        .filter(|value| **value > trigger)
        .count() as u32
        >= threshold
}

/// The decision as the scan takes it: one comparison against the window price.
fn called_by_p_star(s: &StorageHandle<'_>, trigger: u64) -> bool {
    // No ceiling: too few priced days for any trigger to reach the threshold.
    window(s)
        .ceiling()
        .is_some_and(|p_star| U256::from(trigger) < p_star)
}

fn window(s: &StorageHandle<'_>) -> CallWindow {
    let terms = ScanTerms {
        window_days: WINDOW,
        threshold_days: THRESHOLD,
    };
    CallWindow::load(s, REFERENCE_ISO, LAST_DAY, terms).unwrap()
}

/// Window shapes worth disagreeing on: unpriced days, zeros, ties, all-quiet,
/// all-breach, and a run of breaches that has since ended.
fn windows() -> Vec<Vec<Option<u64>>> {
    let quiet = |n: usize| vec![Some(100u64); n];
    let loud = |n: usize| vec![Some(300u64); n];
    let mut cases = vec![
        quiet(28),
        loud(28),
        [loud(21), quiet(7)].concat(),
        [loud(20), quiet(8)].concat(),
        [quiet(7), loud(21)].concat(),
        [quiet(3), loud(25)].concat(),
        [loud(14), vec![None; 7], loud(7)].concat(),
        [loud(21), vec![Some(0); 7]].concat(),
        [vec![Some(0); 21], loud(7)].concat(),
        vec![None; 28],
        [loud(20), vec![None; 8]].concat(),
        [loud(21), vec![None; 7]].concat(),
    ];
    // A window where every day differs: P* must pick the 21st largest exactly.
    cases.push((0..28).map(|i| Some(100 + i as u64 * 10)).collect());
    cases
}

#[test]
fn the_window_price_decides_exactly_what_the_walk_decides() {
    for (case, days) in windows().into_iter().enumerate() {
        with_oracle(|s| {
            let oracle = OracleContract::new(s.clone());
            let pair = setup_pair(&oracle);
            seed_window(&oracle, pair, &days);

            for trigger in [0u64, 99, 100, 101, 150, 199, 200, 299, 300, 301, 400] {
                assert_eq!(
                    called_by_p_star(&s, trigger),
                    breaches_at_least(&days, trigger, THRESHOLD),
                    "case {case}, trigger {trigger}, days {days:?}"
                );
            }
        });
    }
}

#[test]
fn a_window_with_too_few_priced_days_calls_nothing() {
    with_oracle(|s| {
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        // 20 priced days against a threshold of 21: no trigger can reach it.
        seed_window(
            &oracle,
            pair,
            &[vec![Some(300u64); 20], vec![None; 8]].concat(),
        );

        assert!(window(&s).ceiling().is_none());
    });
}

#[test]
fn a_trigger_equal_to_the_window_price_is_not_breached() {
    with_oracle(|s| {
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        seed_window(&oracle, pair, &vec![Some(300u64); 28]);

        // Strictly below calls. Equal does not.
        assert!(called_by_p_star(&s, 299));
        assert!(!called_by_p_star(&s, 300));
    });
}

/// Candidates used to come from the bins under yesterday's price, so a series that
/// had breached enough went unseen once the price dipped. P* is blind to the dip.
#[test]
fn a_group_still_calls_after_the_price_falls_back_under_its_trigger() {
    with_oracle(|s| {
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        // 24 breach days, then three quiet ones - including the last closed day.
        seed_window(
            &oracle,
            pair,
            &[vec![Some(100u64); 3], vec![Some(300u64); 25]].concat(),
        );

        assert!(called_by_p_star(&s, 200));
    });
}

#[test]
fn a_group_issued_inside_the_window_counts_only_its_own_days() {
    with_oracle(|s| {
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        seed_window(&oracle, pair, &vec![Some(300u64); 28]);

        // Issued 10 days before the window's last day: only those days count, so
        // it cannot reach a threshold of 21 however loud they were.
        let issued_at = day_start(LAST_DAY) - 10 * DAY_SECS;
        let series = seed_series(&s, issued_at as u32, U256::from(200u64));

        let mut f = IntexContract::new(s.clone());
        let window = window(&s);
        let group = crate::tests::group_of(&s, series);
        assert_eq!(
            called::call_if_breached(
                called::GroupCall {
                    storage: &s,
                    intex: &mut f,
                    factory: &crate::schema::IntexFactoryContract::new(s.clone()),
                },
                group,
                &window,
                day_start(LAST_DAY) + DAY_SECS
            )
            .unwrap(),
            0
        );
    });
}

fn day_start(date_key: u32) -> u64 {
    outbe_primitives::time::date_key_to_utc_timestamp(date_key)
}

/// A series with the protocol call parameters and the given trigger.
fn seed_series(s: &StorageHandle<'_>, issued_at: u32, trigger: U256) -> SeriesId {
    let series_id = SeriesId::for_pair(LAST_DAY, 840, REFERENCE_ISO).unwrap();
    outbe_intex::api::create_series(
        s,
        outbe_intex::CreateSeriesParams {
            series_id,
            worldwide_day: WorldwideDay::new(LAST_DAY),
            issued_units: 100,
            promis_load_minor: 1_000_000_000_000_000_000,
            entry_price_minor: trigger,
            call_price_minor: trigger,
            call_trigger: outbe_intex::IntexCallTrigger {
                call_window_seconds: WINDOW * DAY_SECS as u32,
                call_threshold_seconds: THRESHOLD * DAY_SECS as u32,
                call_notice_period_seconds: 7 * DAY_SECS as u32,
                transit_margin_seconds: 3_600,
            },
            issued_at,
            issuance_currency: 840,
            reference_currency: REFERENCE_ISO,
        },
    )
    .unwrap();
    series_id
}
