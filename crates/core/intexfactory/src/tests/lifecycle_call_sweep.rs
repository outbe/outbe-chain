//! A call sweep too large for one run continues across blocks, pinned to the
//! day it opened on.

use crate::tests::{at_height, group_of, series_state};

use alloy_primitives::U256;
use outbe_intex::SeriesId;
use outbe_oracle::api::AddressPair;
use outbe_oracle::schema::OracleContract;
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
use outbe_primitives::stablecoin::ISO_4217_NUMERIC_CODES;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::time::{previous_date_key, timestamp_to_date_key};

use crate::tests::called;
use crate::tests::owner;
use outbe_intex::IntexContract;
use outbe_primitives::sweep_budget::{SWEEP_VISITS_PER_BLOCK, SWEEP_WRITES_PER_BLOCK};

pub(super) const CHAIN_ID: u64 = 1;
pub(super) const REFERENCE_ISO: u16 = 840;
pub(super) const ISSUED_AT: u32 = 1_700_000_000;
pub(super) use outbe_primitives::time::{SECONDS_PER_DAY as DAY, SECONDS_PER_DAY_U32};
pub(super) const TRIGGER: u64 = 1_000_000;
pub(super) const WINDOW_DAYS: u32 = 28;

use super::set_vwap;
pub(super) use super::setup_pair;
use super::with_factory;

pub(super) fn fill_window(oracle: &OracleContract, latest: u32, pair: AddressPair, value: U256) {
    super::fill_days(oracle, latest, pair, WINDOW_DAYS, value);
}

/// Each day's candidate is issued on its own day, so it opens its own call group.
pub(super) fn seed_called_candidate(s: &StorageHandle<'_>, worldwide_day: u32) {
    let issued_at = ISSUED_AT - (worldwide_day % 1000) * SECONDS_PER_DAY_U32;
    seed_candidate_at(s, worldwide_day, issued_at, TRIGGER)
}

fn series_of(worldwide_day: u32) -> SeriesId {
    SeriesId::for_pair(WorldwideDay::new(worldwide_day), 840, REFERENCE_ISO).unwrap()
}

fn seed_young_candidate_at(
    s: &StorageHandle<'_>,
    worldwide_day: u32,
    issued_at: u32,
    trigger: u64,
) {
    seed_candidate_at(s, worldwide_day, issued_at, trigger)
}

/// A series of its own day, carrying `trigger` as its call price.
fn seed_candidate_at(
    s: &StorageHandle<'_>,
    worldwide_day: u32,
    issued_at: u32,
    trigger_minor: u64,
) {
    currencies::seed_series(s, worldwide_day, issued_at, trigger_minor, REFERENCE_ISO)
}

pub(super) fn called_count(s: &StorageHandle<'_>, days: std::ops::Range<u32>) -> u32 {
    days.filter(|d| {
        let series_id = SeriesId::for_pair(WorldwideDay::new(*d), 840, REFERENCE_ISO).unwrap();
        series_state(s, series_id) == outbe_intex::IntexState::Called
    })
    .count() as u32
}

#[test]
fn a_called_group_is_parked_with_its_members_and_call_time() {
    with_factory(|s| {
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        let scan_ts = ISSUED_AT as u64 + 60 * DAY;
        let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts)).expect("date step");
        fill_window(&oracle, last_closed_day, pair, U256::from(TRIGGER + 1));
        seed_called_candidate(&s, 20260101);

        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, scan_ts, CHAIN_ID),
            s.clone(),
        );
        assert_eq!(called::scan_and_call(&ctx).unwrap(), 1);

        let factory = IntexContract::new(s.clone());
        let key = group_of(&s, series_of(20260101));

        // The group left its bin and waits for the expiry sweep on its transit close.
        assert_eq!(factory.call_group_slot.read(&key).unwrap(), 0);
        let close_at = scan_ts + 7 * DAY + 3_600;
        let day = IntexContract::deadline_day(close_at);
        assert_eq!(
            factory
                .expiry_day_at
                .read(&IntexContract::day_slot_key(day, 0))
                .unwrap(),
            key
        );
        assert_eq!(factory.expiry_day_live.read(&day).unwrap(), 1);
        assert_eq!(factory.called_deadline.read(&key).unwrap(), close_at);
        assert_eq!(
            factory.group_members(key).unwrap(),
            vec![series_of(20260101)]
        );
        assert_eq!(
            factory.call_group_called_at.read(&key).unwrap(),
            scan_ts as u32
        );
    });
}

/// Drive `worldwide_day` to Called at `scan_ts` and return the deadline its
/// group now carries.
fn call_and_deadline(s: &StorageHandle<'_>, member: SeriesId, scan_ts: u64) -> u64 {
    let ctx = BlockRuntimeContext::new(
        BlockContext::empty_for_tests(1, scan_ts, CHAIN_ID),
        s.clone(),
    );
    called::scan_and_call(&ctx).unwrap();
    IntexContract::new(s.clone())
        .called_deadline
        .read(&group_of(s, member))
        .unwrap()
}

/// First instant a group with this deadline can be retired: the sweep works a
/// deadline day only once that day has closed.
fn due(deadline: u64) -> u64 {
    IntexContract::day_end(IntexContract::deadline_day(deadline))
}

fn sweep_at(s: &StorageHandle<'_>, now: u64) {
    let ctx = BlockRuntimeContext::new(BlockContext::empty_for_tests(2, now, CHAIN_ID), s.clone());
    crate::expired::sweep_expired(&ctx).unwrap();
}

fn unallocated(s: &StorageHandle<'_>) -> U256 {
    outbe_promislimit::PromisLimitContract::new(s.clone())
        .promis_limit()
        .unwrap()
}

fn priced_window(s: &StorageHandle<'_>, scan_ts: u64) {
    let oracle = OracleContract::new(s.clone());
    let pair = setup_pair(&oracle);
    let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts)).expect("date step");
    fill_window(&oracle, last_closed_day, pair, U256::from(TRIGGER + 1));
}

/// The one place an off-by-one costs capacity twice: settlement is legal at
/// the deadline itself (`settle_rejects_expired_deadline` pins that side). A
/// block hook runs before the block's transactions, so the sweep must wait
/// for the tick after it.
#[test]
fn the_sweep_waits_until_settlement_has_actually_closed() {
    with_factory(|s| {
        let scan_ts = ISSUED_AT as u64 + 60 * DAY;
        priced_window(&s, scan_ts);
        seed_called_candidate(&s, 20260101);
        let deadline = call_and_deadline(&s, series_of(20260101), scan_ts);

        sweep_at(&s, deadline);
        assert_eq!(unallocated(&s), U256::ZERO, "still settleable");
        assert_eq!(
            series_state(&s, series_of(20260101)),
            outbe_intex::IntexState::Called
        );

        sweep_at(&s, due(deadline));
        assert_eq!(
            unallocated(&s),
            U256::from(100u64) * U256::from(1_000_000_000_000_000_000u128)
        );
    });
}

#[test]
fn expiry_returns_only_the_load_nobody_realized() {
    with_factory(|s| {
        let scan_ts = ISSUED_AT as u64 + 60 * DAY;
        priced_window(&s, scan_ts);
        seed_called_candidate(&s, 20260101);
        let series_id =
            SeriesId::for_pair(WorldwideDay::new(20260101), 840, REFERENCE_ISO).unwrap();
        outbe_intex::api::record_settled_units(&s, series_id, 30).unwrap();
        outbe_intex::api::record_gem_factory_units(&s, series_id, owner(), 25).unwrap();

        let deadline = call_and_deadline(&s, series_of(20260101), scan_ts);
        sweep_at(&s, due(deadline));

        assert_eq!(
            unallocated(&s),
            U256::from(45u64) * U256::from(1_000_000_000_000_000_000u128)
        );
    });
}

/// A day's group holds one series per issuance currency, and the credit is
/// written once for the whole group. So the total has to be the sum over its
/// members, with neither double-counted nor dropped.
#[test]
fn a_group_credits_every_member_exactly_once() {
    with_factory(|s| {
        let scan_ts = ISSUED_AT as u64 + 60 * DAY;
        priced_window(&s, scan_ts);
        let (members, deadline) = called_two_member_group(&s, scan_ts);
        sweep_at(&s, due(deadline));

        assert_eq!(unallocated(&s), U256::from(140u64) * U256::from(LOAD));
        assert!(!expiring(&s, members[0]));
    });
}

const LOAD: u128 = 1_000_000_000_000_000_000;

/// A called day group of two series against USD: 100 units issued in USD, 40 in EUR.
fn called_two_member_group(s: &StorageHandle<'_>, scan_ts: u64) -> (Vec<SeriesId>, u64) {
    let day = WorldwideDay::new(20260101);
    let trigger = U256::from(TRIGGER);
    let mut members = Vec::new();
    for (issuance, count) in [(840u16, 100u32), (978u16, 40u32)] {
        let series_id = SeriesId::for_pair(day, issuance, REFERENCE_ISO).unwrap();
        let params = outbe_intex::CreateSeriesParams {
            series_id,
            worldwide_day: day,
            issued_units: count,
            promis_load_minor: LOAD,
            entry_price_minor: trigger,
            call_price_minor: trigger,
            call_trigger: outbe_intex::IntexCallTrigger {
                call_window_seconds: WINDOW_DAYS * SECONDS_PER_DAY_U32,
                call_threshold_seconds: 21 * SECONDS_PER_DAY_U32,
                call_notice_period_seconds: 7 * SECONDS_PER_DAY_U32,
                transit_margin_seconds: 3_600,
            },
            issued_at: ISSUED_AT,
            issuance_currency: issuance,
            reference_currency: REFERENCE_ISO,
        };
        outbe_intex::api::create_series(s, params).unwrap();
        members.push(series_id);
    }
    let deadline = call_and_deadline(s, members[0], scan_ts);
    (members, deadline)
}

/// Whether the group of `member` still waits in the expiry queue.
fn expiring(s: &StorageHandle<'_>, member: SeriesId) -> bool {
    IntexContract::new(s.clone())
        .called_group_slot
        .read(&group_of(s, member))
        .unwrap()
        != 0
}

/// One series per issuance code, a unit each, all in one group, then called.
fn called_wide_group(s: &StorageHandle<'_>, scan_ts: u64, width: u16) -> (Vec<SeriesId>, u64) {
    let day = WorldwideDay::new(20260101);
    let trigger = U256::from(TRIGGER);
    let mut members = Vec::new();
    for &issuance in ISO_4217_NUMERIC_CODES.iter().take(width.into()) {
        let series_id = SeriesId::for_pair(day, issuance, REFERENCE_ISO).unwrap();
        let params = outbe_intex::CreateSeriesParams {
            series_id,
            worldwide_day: day,
            issued_units: 1,
            promis_load_minor: LOAD,
            entry_price_minor: trigger,
            call_price_minor: trigger,
            call_trigger: outbe_intex::IntexCallTrigger {
                call_window_seconds: WINDOW_DAYS * SECONDS_PER_DAY_U32,
                call_threshold_seconds: 21 * SECONDS_PER_DAY_U32,
                call_notice_period_seconds: 7 * SECONDS_PER_DAY_U32,
                transit_margin_seconds: 3_600,
            },
            issued_at: ISSUED_AT,
            issuance_currency: issuance,
            reference_currency: REFERENCE_ISO,
        };
        outbe_intex::api::create_series(s, params).unwrap();
        members.push(series_id);
    }
    let deadline = call_and_deadline(s, members[0], scan_ts);
    (members, deadline)
}

/// A group is called whole even when it is wider than a block's writes.
#[test]
fn a_group_wider_than_the_write_budget_is_called_whole() {
    with_factory(|s| {
        let scan_ts = ISSUED_AT as u64 + 60 * DAY;
        priced_window(&s, scan_ts);
        let width = SWEEP_WRITES_PER_BLOCK as u16 + 1;
        let (members, deadline) = called_wide_group(&s, scan_ts, width);
        assert_ne!(deadline, 0);
        for series_id in members {
            assert_eq!(series_state(&s, series_id), outbe_intex::IntexState::Called);
        }
    });
}

/// Its expiry is one step however wide it is: the credit is the group's sum.
#[test]
fn a_group_wider_than_the_write_budget_expires_in_one_step() {
    with_factory(|s| {
        let scan_ts = ISSUED_AT as u64 + 60 * DAY;
        priced_window(&s, scan_ts);
        let width = SWEEP_WRITES_PER_BLOCK as u16 + 1;
        let (members, deadline) = called_wide_group(&s, scan_ts, width);

        sweep_at(&s, due(deadline));
        assert!(!expiring(&s, members[0]));
        assert_eq!(unallocated(&s), U256::from(width) * U256::from(LOAD));
        assert!(IntexContract::new(s.clone())
            .first_cleanup_day()
            .unwrap()
            .is_some());
    });
}

/// A member whose accounting fails defers the whole group, which credits nothing
/// until every member adds up, and then all of them once.
#[test]
fn a_group_with_a_failing_member_is_deferred_whole() {
    with_factory(|s| {
        let scan_ts = ISSUED_AT as u64 + 60 * DAY;
        priced_window(&s, scan_ts);
        let (members, deadline) = called_two_member_group(&s, scan_ts);
        let registry = outbe_intex::IntexContract::new(s.clone());
        // More settled than issued: the EUR series fails its accounting.
        registry.settled_units.write(&members[1], 1_000).unwrap();

        sweep_at(&s, due(deadline));
        assert_eq!(unallocated(&s), U256::ZERO);
        assert!(expiring(&s, members[0]));

        registry.settled_units.write(&members[1], 0).unwrap();
        let retry_day = IntexContract::deadline_day(due(deadline));
        assert_eq!(registry.first_expiry_day().unwrap(), Some(retry_day));
        assert_eq!(
            registry
                .called_deadline
                .read(&group_of(&s, members[0]))
                .unwrap(),
            deadline
        );
        let retry = IntexContract::day_end(retry_day);
        sweep_at(&s, retry - 1);
        assert_eq!(unallocated(&s), U256::ZERO);
        sweep_at(&s, retry);
        assert_eq!(unallocated(&s), U256::from(140u64) * U256::from(LOAD));
        assert!(!expiring(&s, members[0]));
    });
}

#[test]
fn a_second_pass_over_an_expired_group_credits_nothing() {
    with_factory(|s| {
        let scan_ts = ISSUED_AT as u64 + 60 * DAY;
        priced_window(&s, scan_ts);
        seed_called_candidate(&s, 20260101);
        let deadline = call_and_deadline(&s, series_of(20260101), scan_ts);

        sweep_at(&s, due(deadline));
        let after_first = unallocated(&s);
        sweep_at(&s, due(deadline) + 1);
        assert_eq!(unallocated(&s), after_first);

        // The day emptied, so the tree no longer returns that day every block.
        let factory = IntexContract::new(s.clone());
        assert_eq!(factory.first_expiry_day().unwrap(), None);
    });
}

#[test]
fn a_backlog_larger_than_one_block_drains_over_the_next_ones() {
    with_factory(|s| {
        let scan_ts = ISSUED_AT as u64 + 60 * DAY;
        priced_window(&s, scan_ts);
        // One series per group, so the action budget bounds the groups taken.
        let groups = SWEEP_WRITES_PER_BLOCK + SWEEP_WRITES_PER_BLOCK / 2;
        for day in 20260101..20260101 + groups {
            seed_called_candidate(&s, day);
        }
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, scan_ts, CHAIN_ID),
            s.clone(),
        );
        called::scan_and_call(&ctx).unwrap();
        called::run_call_slice(&at_height(&ctx, 2)).unwrap();
        assert_eq!(
            IntexContract::new(s.clone()).call_sweep_day.read().unwrap(),
            0
        );

        let deadline = scan_ts + 7 * DAY;
        let per_group = U256::from(100u64) * U256::from(1_000_000_000_000_000_000u128);

        sweep_at(&s, due(deadline));
        assert_eq!(
            unallocated(&s),
            per_group * U256::from(SWEEP_WRITES_PER_BLOCK)
        );

        sweep_at(&s, due(deadline) + 1);
        assert_eq!(unallocated(&s), per_group * U256::from(groups));
    });
}

#[test]
fn a_sweep_wider_than_one_run_finishes_over_the_next_blocks() {
    with_factory(|s| {
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        let scan_ts = ISSUED_AT as u64 + 60 * DAY;
        let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts)).expect("date step");
        // Every window day above the trigger: all of them are due a call.
        fill_window(&oracle, last_closed_day, pair, U256::from(TRIGGER + 1));

        // One group per day, half again as many as one slice may move.
        let groups = SWEEP_WRITES_PER_BLOCK + SWEEP_WRITES_PER_BLOCK / 2;
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
        assert_eq!(first, SWEEP_WRITES_PER_BLOCK);
        assert_ne!(
            IntexContract::new(s.clone()).call_sweep_day.read().unwrap(),
            0,
            "the sweep stays open"
        );

        // The next block's slice finishes it and closes the sweep.
        assert_eq!(
            called::run_call_slice(&ctx).unwrap(),
            0,
            "same-height entry retains the spent allowance"
        );
        let second = called::run_call_slice(&at_height(&ctx, 2)).unwrap();
        assert_eq!(first + second, groups);
        assert_eq!(called_count(&s, days), groups);
        assert_eq!(
            IntexContract::new(s.clone()).call_sweep_day.read().unwrap(),
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
    let older = super::fill_days(oracle, latest, pair, WINDOW_DAYS - 21, quiet);
    super::fill_days(oracle, older, pair, 21, breach);
}

#[test]
fn a_sweep_that_runs_past_midnight_keeps_deciding_against_its_own_day() {
    with_factory(|s| {
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        let scan_ts = ISSUED_AT as u64 + 60 * DAY;
        let opened_on = previous_date_key(timestamp_to_date_key(scan_ts)).expect("date step");
        fill_expiring_window(&oracle, opened_on, pair);
        // The day the sweep spills into is quiet, so the window it would see has
        // one breach fewer than the threshold.
        set_vwap(
            &oracle,
            timestamp_to_date_key(scan_ts),
            pair,
            U256::from(TRIGGER),
        );

        let groups = SWEEP_WRITES_PER_BLOCK + 1;
        let days = 20260101..20260101 + groups;
        for day in days.clone() {
            seed_called_candidate(&s, day);
        }

        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, scan_ts, CHAIN_ID),
            s.clone(),
        );
        assert_eq!(called::scan_and_call(&ctx).unwrap(), SWEEP_WRITES_PER_BLOCK);

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
        let opened_on = previous_date_key(timestamp_to_date_key(scan_ts)).expect("date step");
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
fn a_slice_schedules_a_ready_day_without_waiting_for_a_trigger() {
    with_factory(|s| {
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        let scan_ts = ISSUED_AT as u64 + 60 * DAY;
        let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts)).expect("date step");
        fill_window(&oracle, last_closed_day, pair, U256::from(TRIGGER + 1));
        seed_called_candidate(&s, 20260101);

        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, scan_ts, CHAIN_ID),
            s.clone(),
        );
        // Every block retries scheduling, independently of daily trigger success.
        assert_eq!(called::run_call_slice(&ctx).unwrap(), 1);
        assert_eq!(called_count(&s, 20260101..20260102), 1);
        assert_eq!(called::run_call_slice(&ctx).unwrap(), 0);
    });
}

#[test]
fn a_sweep_that_fits_in_one_run_closes_at_once() {
    with_factory(|s| {
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        let scan_ts = ISSUED_AT as u64 + 60 * DAY;
        let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts)).expect("date step");
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
            IntexContract::new(s.clone()).call_sweep_day.read().unwrap(),
            0
        );
    });
}

/// A young series: issued inside the window, so its truncated window cannot hold
/// the threshold however loud the days were.
fn seed_young_candidate(s: &StorageHandle<'_>, worldwide_day: u32, issued_at: u32) {
    currencies::seed_series(s, worldwide_day, issued_at, TRIGGER, REFERENCE_ISO);
}

/// Nothing shrinks a bin whose groups all decide against a move, so the sweep has
/// to walk past it rather than restart on them forever.
#[test]
fn a_bin_of_undecided_groups_does_not_stall_the_sweep() {
    with_factory(|s| {
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        let scan_ts = ISSUED_AT as u64 + 60 * DAY;
        let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts)).expect("date step");
        fill_window(&oracle, last_closed_day, pair, U256::from(TRIGGER + 1));

        // All issued five days ago: every one is visited, decided, and left alone.
        let young_at = (scan_ts - 5 * DAY) as u32;
        let groups = SWEEP_VISITS_PER_BLOCK + 1;
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
        for height in 2..=5 {
            called::run_call_slice(&at_height(&ctx, height)).unwrap();
        }
        assert_eq!(
            IntexContract::new(s.clone()).call_sweep_day.read().unwrap(),
            0,
            "the sweep closed instead of restarting on the same groups"
        );
    });
}

/// A price the bin ladder cannot hold skips its currency instead of halting the
/// block. The scan runs in `begin_block`, where an error is not survivable.
#[test]
fn a_window_price_out_of_range_skips_the_currency() {
    with_factory(|s| {
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        let scan_ts = ISSUED_AT as u64 + 60 * DAY;
        let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts)).expect("date step");
        // Priced days enough to reach the threshold, at a price no bin covers.
        fill_window(&oracle, last_closed_day, pair, U256::MAX);
        seed_called_candidate(&s, 20260101);

        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, scan_ts, CHAIN_ID),
            s.clone(),
        );
        assert_eq!(called::scan_and_call(&ctx).unwrap(), 0, "nothing is called");
        assert_eq!(called_count(&s, 20260101..20260102), 0);
        assert_eq!(
            IntexContract::new(s.clone()).call_sweep_day.read().unwrap(),
            0,
            "the sweep closed rather than retrying a currency it cannot price"
        );
    });
}

/// A group the previous sweep left behind its cursor has to be walked again when
/// the next day opens, or a day's worth of calls is skipped in silence.
#[test]
fn a_new_sweep_walks_the_bins_the_last_one_stopped_above() {
    with_factory(|s| {
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        let scan_ts = ISSUED_AT as u64 + 60 * DAY;
        let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts)).expect("date step");
        fill_window(&oracle, last_closed_day, pair, U256::from(TRIGGER * 4));

        // Below the budget-stop: issued inside the window, so it decides against a
        // call today however loud the days were.
        let young = 20260001;
        seed_young_candidate_at(&s, young, (scan_ts - 5 * DAY) as u32, TRIGGER);
        // Above it: enough mature groups at a higher trigger to spend every action.
        for day in 20260101..20260101 + SWEEP_WRITES_PER_BLOCK + 1 {
            let issued_at = ISSUED_AT - (day % 1000) * SECONDS_PER_DAY_U32;
            seed_candidate_at(&s, day, issued_at, TRIGGER * 2);
        }

        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, scan_ts, CHAIN_ID),
            s.clone(),
        );
        assert_eq!(
            called::scan_and_call(&ctx).unwrap(),
            SWEEP_WRITES_PER_BLOCK,
            "the slice stops once its calls spent the writes"
        );
        assert_ne!(
            IntexContract::new(s.clone())
                .call_bin_cursor
                .read(&REFERENCE_ISO)
                .unwrap(),
            0,
            "the cursor stands above the young group's bin"
        );

        // Twenty days on, the young group's own window reaches the threshold. The
        // day's trigger has to find it, which it only does from a reset cursor.
        let later_ts = scan_ts + 20 * DAY;
        let later_day = previous_date_key(timestamp_to_date_key(later_ts)).expect("date step");
        fill_window(&oracle, later_day, pair, U256::from(TRIGGER * 4));
        let later = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(2, later_ts, CHAIN_ID),
            s.clone(),
        );
        called::scan_and_call(&later).unwrap();
        // The in-flight day finishes, then every day up to the new one is walked.
        let factory = IntexContract::new(s.clone());
        for height in 3..=66 {
            if factory.call_sweep_day.read().unwrap() == 0 {
                break;
            }
            called::run_call_slice(&at_height(&later, height)).unwrap();
        }
        assert_eq!(factory.call_sweep_day.read().unwrap(), 0);

        assert_eq!(
            called_count(&s, young..young + 1),
            1,
            "the group below the old cursor was called"
        );
    });
}

#[path = "lifecycle_call_sweep_currencies.rs"]
mod currencies;
