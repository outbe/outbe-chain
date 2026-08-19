//! `trigger < p_star` and "breached on at least `threshold` days" are the same
//! statement; these check that they stay the same statement.

use alloy_primitives::U256;
use outbe_common::WorldwideDay;
use outbe_intex::SeriesId;
use outbe_oracle::api::AddressPair;
use outbe_oracle::schema::OracleContract;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::previous_date_key;

use crate::called::{self, DayVwaps};
use crate::schema::IntexFactoryContract;
use crate::state::Group;

const CHAIN_ID: u64 = 1;
const REFERENCE_ISO: u16 = 840;
const PAIR_ID: u32 = 1;
const LAST_DAY: u32 = 20260212;
const WINDOW: u32 = 28;
const THRESHOLD: u32 = 21;
const ISSUED_AT: u32 = 1_700_000_000;
const DAY_SECS: u64 = 24 * 60 * 60;

fn with_oracle<R>(f: impl FnOnce(StorageHandle) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(ISSUED_AT as u64));
    storage.stub_sub_call_at(
        crate::constants::INTEX_NFT1155_ADDRESS,
        alloy_primitives::Bytes::from(vec![0u8; 32]),
    );
    storage.stub_sub_call_at(
        crate::constants::ORIGIN_ROUTER_ADDRESS,
        alloy_primitives::Bytes::from(vec![0u8; 32]),
    );
    StorageHandle::enter(&mut storage, f)
}

fn setup_pair(oracle: &OracleContract) -> AddressPair {
    let pair = AddressPair::new_coen_to(REFERENCE_ISO);
    oracle.pair_to_index.write(&pair, PAIR_ID).unwrap();
    oracle.pair_count.write(PAIR_ID).unwrap();
    oracle.pair_by_index.write_pair(&PAIR_ID, pair).unwrap();
    oracle.vote_target.write(&pair, true).unwrap();
    oracle.reference_currencies.push(REFERENCE_ISO).unwrap();
    pair
}

fn set_vwap(oracle: &OracleContract, utc_day: u32, pair: AddressPair, value: U256) {
    let pair_id = oracle.pair_index_of(pair).unwrap();
    oracle
        .utc_day_vwap_value
        .get_nested(&utc_day)
        .write(&pair_id, value)
        .unwrap();
    if oracle.utc_day_vwap_last_finalized.read().unwrap() < utc_day {
        oracle.utc_day_vwap_last_finalized.write(utc_day).unwrap();
    }
}

/// Write one window's days, latest first. `None` leaves the day unpriced.
fn seed_window(oracle: &OracleContract, pair: AddressPair, days: &[Option<u64>]) {
    let mut day = LAST_DAY;
    for value in days {
        if let Some(v) = value {
            set_vwap(oracle, day, pair, U256::from(*v));
        }
        day = previous_date_key(day);
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
fn called_by_p_star(oracle: &OracleContract, pair: AddressPair, trigger: u64) -> bool {
    let mut vwaps = DayVwaps::new(oracle.pair_index_of(pair).unwrap());
    match called::call_window(oracle, &mut vwaps, LAST_DAY, WINDOW, THRESHOLD).unwrap() {
        Some(window) => U256::from(trigger) < window.p_star,
        // Too few priced days for any trigger to reach the threshold.
        None => false,
    }
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
                    called_by_p_star(&oracle, pair, trigger),
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

        let mut vwaps = DayVwaps::new(oracle.pair_index_of(pair).unwrap());
        assert!(
            called::call_window(&oracle, &mut vwaps, LAST_DAY, WINDOW, THRESHOLD)
                .unwrap()
                .is_none()
        );
    });
}

#[test]
fn a_trigger_equal_to_the_window_price_is_not_breached() {
    with_oracle(|s| {
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        seed_window(&oracle, pair, &vec![Some(300u64); 28]);

        // Strictly below calls; equal does not.
        assert!(called_by_p_star(&oracle, pair, 299));
        assert!(!called_by_p_star(&oracle, pair, 300));
    });
}

/// Candidates used to come from the bins under yesterday's price, so a series that
/// had breached enough went unseen once the price dipped. P* is blind to the dip.
#[test]
fn a_group_still_calls_after_the_price_falls_back_under_its_trigger() {
    with_oracle(|s| {
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        // 24 breach days, then three quiet ones — including the last closed day.
        seed_window(
            &oracle,
            pair,
            &[vec![Some(100u64); 3], vec![Some(300u64); 25]].concat(),
        );

        assert!(called_by_p_star(&oracle, pair, 200));
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
        let series = seed_qualified(&s, issued_at as u32, U256::from(200u64));

        let mut f = IntexFactoryContract::new(s.clone());
        let mut vwaps = DayVwaps::new(oracle.pair_index_of(pair).unwrap());
        let window = called::call_window(&oracle, &mut vwaps, LAST_DAY, WINDOW, THRESHOLD)
            .unwrap()
            .unwrap();
        let group = Group {
            iso_code: REFERENCE_ISO,
            worldwide_day: series.worldwide_day(),
            members: vec![series],
        };
        assert_eq!(
            called::try_call_group(
                &s,
                &mut f,
                &oracle,
                &mut vwaps,
                &group,
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

/// A Qualified series with the protocol call parameters and the given trigger.
fn seed_qualified(s: &StorageHandle<'_>, issued_at: u32, trigger: U256) -> SeriesId {
    let series_id = SeriesId::for_pair(WorldwideDay::new(LAST_DAY), 840, REFERENCE_ISO).unwrap();
    outbe_intex::api::create_series(
        s,
        outbe_intex::CreateSeriesParams {
            series_id,
            worldwide_day: WorldwideDay::new(LAST_DAY),
            issued_intex_count: 100,
            promis_load_minor: 1_000_000_000_000_000_000,
            entry_price_minor: trigger,
            floor_price_minor: trigger,
            call_price_minor: trigger,
            call_trigger: outbe_intex::IntexCallTrigger {
                call_window: WINDOW * DAY_SECS as u32,
                call_threshold: THRESHOLD * DAY_SECS as u32,
                call_notice_period: 7 * DAY_SECS as u32,
            },
            issued_at,
            issuance_currency: 840,
            reference_currency: REFERENCE_ISO,
        },
    )
    .unwrap();
    outbe_intex::api::mark_qualified(s, series_id).unwrap();
    series_id
}
