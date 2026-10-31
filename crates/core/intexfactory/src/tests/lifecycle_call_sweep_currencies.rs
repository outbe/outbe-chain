use alloy_primitives::U256;
use outbe_intex::{IntexContract, SeriesId};
use outbe_oracle::api::AddressPair;
use outbe_oracle::schema::OracleContract;
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
use outbe_primitives::pass_budget::{SWEEP_VISITS_PER_BLOCK, SWEEP_WRITES_PER_BLOCK};
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::EmissionDate;
use outbe_primitives::time::{previous_date_key, timestamp_to_date_key};

use super::super::with_factory;
use super::{
    called_count, fill_window, seed_called_candidate, setup_pair, CHAIN_ID, DAY, ISSUED_AT,
    REFERENCE_ISO, SECONDS_PER_DAY_U32, TRIGGER, WINDOW_DAYS,
};
use crate::tests::{at_height, called};

const SECOND_ISO: u16 = 978;
const SECOND_PAIR_ID: u32 = 2;

fn setup_second_pair(oracle: &OracleContract) -> AddressPair {
    let pair = AddressPair::new_coen_to(SECOND_ISO);
    oracle.pair_to_index.write(&pair, SECOND_PAIR_ID).unwrap();
    oracle.pair_count.write(SECOND_PAIR_ID).unwrap();
    oracle
        .pair_by_index
        .write_pair(&SECOND_PAIR_ID, pair)
        .unwrap();
    oracle.vote_target.write(&pair, true).unwrap();
    pair
}

/// Seeds one callable group of `iso`, priced so its trigger sits under the window.
/// Each day's series is issued on its own day, so it opens its own group.
fn seed_candidate_for(s: &StorageHandle<'_>, iso: u16, emission_day: u32) {
    let issued_at = ISSUED_AT - u64::from((emission_day % 1000) * SECONDS_PER_DAY_U32);
    seed_series(s, emission_day, issued_at, TRIGGER, iso);
}

/// A budget that is exhausted inside the last currency of the rotation must resume
/// there, not back at the first. Otherwise the ones already closed would eat the
/// next slice's allowance before it ever reached the one where the budget stopped.
#[test]
fn a_slice_resumes_at_the_currency_it_gave_out_on() {
    with_factory(|s| {
        let oracle = OracleContract::new(s.clone());
        let first = setup_pair(&oracle);
        let second = setup_second_pair(&oracle);
        let scan_ts = ISSUED_AT + 60 * DAY;
        let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts)).expect("date step");
        fill_window(&oracle, last_closed_day, first, U256::from(TRIGGER + 1));
        fill_window(&oracle, last_closed_day, second, U256::from(TRIGGER + 1));

        // The first currency is small. The second holds more than one slice can move.
        seed_candidate_for(&s, REFERENCE_ISO, 20260101);
        for day in 20260201..20260201 + SWEEP_WRITES_PER_BLOCK + 1 {
            seed_candidate_for(&s, SECOND_ISO, day);
        }

        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, scan_ts, CHAIN_ID),
            s.clone(),
        );
        called::scan_and_call(&ctx).unwrap();

        // The rotation is [840, 978]. The budget was exhausted on the last one, so
        // that is where the cursor stands.
        assert_eq!(
            IntexContract::new(s.clone())
                .call_currency_cursor
                .read()
                .unwrap(),
            u32::from(SECOND_ISO),
            "the cursor points at the currency that did not finish"
        );
    });
}

/// A trigger that finds the sweep unfinished queues its day rather than restarting
/// the walk, so the day in flight still reaches every bin against its own prices.
#[test]
fn a_trigger_during_a_running_sweep_queues_its_day() {
    with_factory(|s| {
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        let scan_ts = ISSUED_AT + 60 * DAY;
        let day = previous_date_key(timestamp_to_date_key(scan_ts)).expect("date step");
        fill_window(&oracle, day, pair, U256::from(TRIGGER + 1));
        let groups = SWEEP_WRITES_PER_BLOCK + SWEEP_WRITES_PER_BLOCK / 2;
        let days = 20260101..20260101 + groups;
        for d in days.clone() {
            seed_called_candidate(&s, d);
        }
        let factory = IntexContract::new(s.clone());

        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, scan_ts, CHAIN_ID),
            s.clone(),
        );
        called::scan_and_call(&ctx).unwrap();
        let cursor = factory.call_bin_cursor.read(&REFERENCE_ISO).unwrap();
        assert_ne!(cursor, 0, "the first slice gave out inside the range");

        let next_ts = scan_ts + DAY;
        let next_day = previous_date_key(timestamp_to_date_key(next_ts)).expect("date step");
        fill_window(&oracle, next_day, pair, U256::from(TRIGGER + 1));
        let next = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(2, next_ts, CHAIN_ID),
            s.clone(),
        );
        called::schedule_call_pass(&next).unwrap();
        assert_eq!(factory.call_sweep_day.read().unwrap(), day);
        assert_eq!(factory.call_latest_day.read().unwrap(), next_day);
        assert_eq!(
            factory.call_bin_cursor.read(&REFERENCE_ISO).unwrap(),
            cursor,
            "the walk in flight was not restarted"
        );

        // The next slice finishes the old day and hands the sweep to the queued one.
        called::process_call_pass(&next).unwrap();
        assert_eq!(called_count(&s, days), groups);
        assert_eq!(factory.call_sweep_day.read().unwrap(), next_day);
        assert_eq!(factory.call_latest_day.read().unwrap(), next_day);
    });
}

/// Days that closed while the sweep was still walking an older one are walked after
/// it, in order, none skipped.
#[test]
fn days_closed_behind_a_running_call_sweep_are_walked_in_order() {
    with_factory(|s| {
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        for d in 20260101..20260101 + SWEEP_WRITES_PER_BLOCK + 1 {
            seed_called_candidate(&s, d);
        }
        let mut closed = Vec::new();
        for offset in 0..3 {
            let ts = ISSUED_AT + (60 + offset) * DAY;
            let day = previous_date_key(timestamp_to_date_key(ts)).expect("date step");
            fill_window(&oracle, day, pair, U256::from(TRIGGER + 1));
            let ctx = BlockRuntimeContext::new(
                BlockContext::empty_for_tests(1 + offset, ts, CHAIN_ID),
                s.clone(),
            );
            // Only the first trigger's block walks: the later days arrive meanwhile.
            match offset {
                0 => called::scan_and_call(&ctx).map(drop),
                _ => called::schedule_call_pass(&ctx),
            }
            .unwrap();
            closed.push(day);
        }
        let factory = IntexContract::new(s.clone());
        assert_eq!(factory.call_sweep_day.read().unwrap(), closed[0]);
        assert_eq!(factory.call_latest_day.read().unwrap(), closed[2]);

        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(4, ISSUED_AT + 62 * DAY, CHAIN_ID),
            s.clone(),
        );
        let mut walked = vec![closed[0]];
        for height in 4..20 {
            if walked.last() == Some(&0) {
                break;
            }
            called::process_call_pass(&at_height(&ctx, height)).unwrap();
            let day = factory.call_sweep_day.read().unwrap();
            if walked.last() != Some(&day) {
                walked.push(day);
            }
        }
        assert_eq!(walked, vec![closed[0], closed[1], closed[2], 0]);
    });
}

/// Each currency is walked once a sweep. Assume the sweep walked the ones closed
/// behind the cursor again. Then two currencies, each holding more undecided groups
/// than a slice may decide, would keep the sweep open for good.
#[test]
fn a_sweep_over_several_currencies_always_ends() {
    with_factory(|s| {
        let oracle = OracleContract::new(s.clone());
        let first = setup_pair(&oracle);
        let second = setup_second_pair(&oracle);
        let scan_ts = ISSUED_AT + 60 * DAY;
        let day = previous_date_key(timestamp_to_date_key(scan_ts)).expect("date step");
        fill_window(&oracle, day, first, U256::from(TRIGGER + 1));
        fill_window(&oracle, day, second, U256::from(TRIGGER + 1));

        // Issued five days ago: every group is decided and left where it is.
        let young_at = scan_ts - 5 * DAY;
        for d in 20260101..20260101 + SWEEP_VISITS_PER_BLOCK + 1 {
            seed_young_candidate_for(&s, REFERENCE_ISO, d, young_at);
            seed_young_candidate_for(&s, SECOND_ISO, d, young_at);
        }

        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, scan_ts, CHAIN_ID),
            s.clone(),
        );
        called::scan_and_call(&ctx).unwrap();
        for height in 2..=4 {
            called::process_call_pass(&at_height(&ctx, height)).unwrap();
        }
        assert_eq!(
            IntexContract::new(s.clone()).call_sweep_day.read().unwrap(),
            0,
            "the sweep closed"
        );
    });
}

/// A group of `iso` issued at `issued_at`, its trigger under the window.
fn seed_young_candidate_for(s: &StorageHandle<'_>, iso: u16, emission_day: u32, issued_at: u64) {
    seed_series(s, emission_day, issued_at, TRIGGER, iso)
}

pub(super) fn seed_series(
    s: &StorageHandle<'_>,
    emission_day: u32,
    issued_at: u64,
    trigger_minor: u64,
    reference_currency: u16,
) {
    let series_id = SeriesId::for_pair(emission_day, 840, reference_currency).unwrap();
    let trigger = U256::from(trigger_minor);
    outbe_intex::api::create_series(
        s,
        outbe_intex::CreateSeriesParams {
            series_id,
            emission_day: EmissionDate::new(emission_day),
            issued_units: 100,
            promis_load_minor: 1_000_000_000_000_000_000,
            entry_price_minor: trigger,
            call_price_minor: trigger,
            call_trigger: outbe_intex::IntexCallTrigger {
                call_window_seconds: WINDOW_DAYS * SECONDS_PER_DAY_U32,
                call_threshold_seconds: 21 * SECONDS_PER_DAY_U32,
                call_notice_period_seconds: 7 * SECONDS_PER_DAY_U32,
                transit_margin_seconds: 3_600,
            },
            issued_at,
            issuance_currency: 840,
            reference_currency,
        },
    )
    .unwrap();
}
