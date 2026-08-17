//! A call sweep too large for one run carries on across blocks, pinned to the
//! day it opened on.

use alloy_primitives::U256;
use outbe_common::WorldwideDay;
use outbe_intex::SeriesId;
use outbe_oracle::api::AddressPair;
use outbe_oracle::schema::OracleContract;
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::{previous_date_key, timestamp_to_date_key};

use crate::called;
use crate::constants::MAX_GROUP_DECISIONS_PER_SWEEP;
use crate::schema::IntexFactoryContract;

const CHAIN_ID: u64 = 1;
const REFERENCE_ISO: u16 = 840;
const PAIR_ID: u32 = 1;
const ISSUED_AT: u32 = 1_700_000_000;
const DAY: u64 = 24 * 60 * 60;
const TRIGGER: u64 = 1_000_000;
const WINDOW_DAYS: u32 = 28;

fn with_factory<R>(f: impl FnOnce(StorageHandle) -> R) -> R {
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

fn fill_window(oracle: &OracleContract, latest: u32, pair: AddressPair, value: U256) {
    let mut day = latest;
    for _ in 0..WINDOW_DAYS {
        set_vwap(oracle, day, pair, value);
        day = previous_date_key(day);
    }
}

/// A Qualified series of its own day, all sharing one call trigger.
fn seed_called_candidate(s: &StorageHandle<'_>, worldwide_day: u32) {
    let series_id =
        SeriesId::for_pair(WorldwideDay::new(worldwide_day), 840, REFERENCE_ISO).unwrap();
    let trigger = U256::from(TRIGGER);
    outbe_intex::api::create_series(
        s,
        outbe_intex::CreateSeriesParams {
            series_id,
            worldwide_day: WorldwideDay::new(worldwide_day),
            issued_intex_count: 100,
            promis_load_minor: 1_000_000_000_000_000_000,
            entry_price_minor: trigger,
            floor_price_minor: trigger,
            call_price_minor: trigger,
            call_trigger: outbe_intex::IntexCallTrigger {
                call_window: WINDOW_DAYS * DAY as u32,
                call_threshold: 21 * DAY as u32,
                call_notice_period: 7 * DAY as u32,
            },
            issued_at: ISSUED_AT,
            issuance_currency: 840,
            reference_currency: REFERENCE_ISO,
        },
    )
    .unwrap();
    outbe_intex::api::mark_qualified(s, series_id).unwrap();
    IntexFactoryContract::new(s.clone())
        .insert_qualified_group(
            REFERENCE_ISO,
            WorldwideDay::new(worldwide_day),
            trigger,
            &[series_id],
        )
        .unwrap();
}

fn called_count(s: &StorageHandle<'_>, days: std::ops::Range<u32>) -> u32 {
    days.filter(|d| {
        let series_id = SeriesId::for_pair(WorldwideDay::new(*d), 840, REFERENCE_ISO).unwrap();
        outbe_intex::api::read_series(s, series_id)
            .unwrap()
            .lifecycle_state()
            .unwrap()
            == outbe_intex::IntexState::Called
    })
    .count() as u32
}

#[test]
fn a_sweep_wider_than_one_run_finishes_over_the_next_blocks() {
    with_factory(|s| {
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        let scan_ts = ISSUED_AT as u64 + 60 * DAY;
        let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts));
        // Every window day above the trigger: all of them are due a call.
        fill_window(&oracle, last_closed_day, pair, U256::from(TRIGGER + 1));

        // One group per day, half again as many as a single run decides.
        let groups = MAX_GROUP_DECISIONS_PER_SWEEP + MAX_GROUP_DECISIONS_PER_SWEEP / 2;
        let days = 20260101..20260101 + groups;
        for day in days.clone() {
            seed_called_candidate(&s, day);
        }

        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, scan_ts, CHAIN_ID),
            s.clone(),
        );

        // The daily trigger opens the sweep and takes what it can.
        let first = called::scan_and_call(&ctx).unwrap();
        assert_eq!(first, MAX_GROUP_DECISIONS_PER_SWEEP);
        assert_ne!(
            IntexFactoryContract::new(s.clone())
                .call_sweep_day
                .read()
                .unwrap(),
            0,
            "the sweep stays open"
        );

        // The next block's slice finishes it and closes the sweep.
        let second = called::run_call_slice(&ctx).unwrap();
        assert_eq!(first + second, groups);
        assert_eq!(called_count(&s, days), groups);
        assert_eq!(
            IntexFactoryContract::new(s.clone())
                .call_sweep_day
                .read()
                .unwrap(),
            0,
            "the sweep closed"
        );
    });
}

/// A window whose breaches sit at its old end: it holds exactly the threshold
/// on `latest`, and one day later the oldest breach has aged out of it.
fn fill_expiring_window(oracle: &OracleContract, latest: u32, pair: AddressPair) {
    let breach = U256::from(TRIGGER + 1);
    let quiet = U256::from(TRIGGER);
    let mut day = latest;
    for _ in 0..WINDOW_DAYS - 21 {
        set_vwap(oracle, day, pair, quiet);
        day = previous_date_key(day);
    }
    for _ in 0..21 {
        set_vwap(oracle, day, pair, breach);
        day = previous_date_key(day);
    }
}

#[test]
fn a_sweep_that_runs_past_midnight_keeps_deciding_against_its_own_day() {
    with_factory(|s| {
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        let scan_ts = ISSUED_AT as u64 + 60 * DAY;
        let opened_on = previous_date_key(timestamp_to_date_key(scan_ts));
        fill_expiring_window(&oracle, opened_on, pair);
        // The day the sweep spills into is quiet, so the window it would see has
        // one breach fewer than the threshold.
        set_vwap(
            &oracle,
            timestamp_to_date_key(scan_ts),
            pair,
            U256::from(TRIGGER),
        );

        let groups = MAX_GROUP_DECISIONS_PER_SWEEP + 1;
        let days = 20260101..20260101 + groups;
        for day in days.clone() {
            seed_called_candidate(&s, day);
        }

        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, scan_ts, CHAIN_ID),
            s.clone(),
        );
        assert_eq!(
            called::scan_and_call(&ctx).unwrap(),
            MAX_GROUP_DECISIONS_PER_SWEEP
        );

        // The next slice lands after midnight. Pinned to the day it opened on,
        // the sweep finishes on the terms it started with.
        let next_day = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(2, scan_ts + DAY, CHAIN_ID),
            s.clone(),
        );
        assert_eq!(called::run_call_slice(&next_day).unwrap(), 1);
        assert_eq!(called_count(&s, days), groups);
    });
}

/// The control for the test above: opened a day later, the same window has
/// aged one breach out and calls nothing.
#[test]
fn a_sweep_opened_a_day_later_sees_the_window_expire() {
    with_factory(|s| {
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        let scan_ts = ISSUED_AT as u64 + 60 * DAY;
        let opened_on = previous_date_key(timestamp_to_date_key(scan_ts));
        fill_expiring_window(&oracle, opened_on, pair);
        set_vwap(
            &oracle,
            timestamp_to_date_key(scan_ts),
            pair,
            U256::from(TRIGGER),
        );
        seed_called_candidate(&s, 20260101);

        let next_day = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, scan_ts + DAY, CHAIN_ID),
            s.clone(),
        );
        assert_eq!(called::scan_and_call(&next_day).unwrap(), 0);
        assert_eq!(called_count(&s, 20260101..20260102), 0);
    });
}

#[test]
fn a_slice_without_an_open_sweep_does_nothing() {
    with_factory(|s| {
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        let scan_ts = ISSUED_AT as u64 + 60 * DAY;
        let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts));
        fill_window(&oracle, last_closed_day, pair, U256::from(TRIGGER + 1));
        seed_called_candidate(&s, 20260101);

        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, scan_ts, CHAIN_ID),
            s.clone(),
        );
        // No trigger has opened one, so the block hook leaves the group alone.
        assert_eq!(called::run_call_slice(&ctx).unwrap(), 0);
        assert_eq!(called_count(&s, 20260101..20260102), 0);
    });
}

#[test]
fn a_sweep_that_fits_in_one_run_closes_at_once() {
    with_factory(|s| {
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        let scan_ts = ISSUED_AT as u64 + 60 * DAY;
        let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts));
        fill_window(&oracle, last_closed_day, pair, U256::from(TRIGGER + 1));
        for day in 20260101..20260105 {
            seed_called_candidate(&s, day);
        }

        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, scan_ts, CHAIN_ID),
            s.clone(),
        );
        assert_eq!(called::scan_and_call(&ctx).unwrap(), 4);
        assert_eq!(
            IntexFactoryContract::new(s.clone())
                .call_sweep_day
                .read()
                .unwrap(),
            0
        );
    });
}

/// A young series: issued inside the window, so its truncated window cannot hold
/// the threshold however loud the days were.
fn seed_young_candidate(s: &StorageHandle<'_>, worldwide_day: u32, issued_at: u32) {
    let series_id =
        SeriesId::for_pair(WorldwideDay::new(worldwide_day), 840, REFERENCE_ISO).unwrap();
    let trigger = U256::from(TRIGGER);
    outbe_intex::api::create_series(
        s,
        outbe_intex::CreateSeriesParams {
            series_id,
            worldwide_day: WorldwideDay::new(worldwide_day),
            issued_intex_count: 100,
            promis_load_minor: 1_000_000_000_000_000_000,
            entry_price_minor: trigger,
            floor_price_minor: trigger,
            call_price_minor: trigger,
            call_trigger: outbe_intex::IntexCallTrigger {
                call_window: WINDOW_DAYS * DAY as u32,
                call_threshold: 21 * DAY as u32,
                call_notice_period: 7 * DAY as u32,
            },
            issued_at,
            issuance_currency: 840,
            reference_currency: REFERENCE_ISO,
        },
    )
    .unwrap();
    outbe_intex::api::mark_qualified(s, series_id).unwrap();
    IntexFactoryContract::new(s.clone())
        .insert_qualified_group(
            REFERENCE_ISO,
            WorldwideDay::new(worldwide_day),
            trigger,
            &[series_id],
        )
        .unwrap();
}

/// A bin can hold more groups than one run decides. When none of them transitions
/// there is nothing to shrink the bin, so the sweep has to walk past it on its own
/// budget rather than restarting on the same groups forever.
#[test]
fn a_bin_of_undecided_groups_does_not_stall_the_sweep() {
    with_factory(|s| {
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        let scan_ts = ISSUED_AT as u64 + 60 * DAY;
        let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts));
        fill_window(&oracle, last_closed_day, pair, U256::from(TRIGGER + 1));

        // All issued five days ago: every one is visited, decided, and left alone.
        let young_at = (scan_ts - 5 * DAY) as u32;
        let groups = MAX_GROUP_DECISIONS_PER_SWEEP + 1;
        for day in 20260101..20260101 + groups {
            seed_young_candidate(&s, day, young_at);
        }

        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, scan_ts, CHAIN_ID),
            s.clone(),
        );
        assert_eq!(
            called::scan_and_call(&ctx).unwrap(),
            0,
            "none is due a call"
        );

        // The next slices must reach the end of the range and close the sweep.
        for _ in 0..4 {
            called::run_call_slice(&ctx).unwrap();
        }
        assert_eq!(
            IntexFactoryContract::new(s.clone())
                .call_sweep_day
                .read()
                .unwrap(),
            0,
            "the sweep closed instead of restarting on the same groups"
        );
    });
}
