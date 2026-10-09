//! Daily price-path scan: the multi-week breach-count call, and the void of a
//! lapsed settlement window from the deadline queue.
//!
//! Tests drive [`crate::called::scan_and_call`] through the `scan` harness helper,
//! and later blocks' slices through `slice`, against a seeded finalized daily
//! series. This series is the only price source the production trigger reads.

use alloy_primitives::{Address, U256};

use outbe_credis::constants::{CALL_LOOKBACK_DAYS, CALL_THRESHOLD_DAYS, SECS_PER_DAY};
use outbe_credis::{CallBins, CredisContract, CredisState};
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::previous_date_key;

use crate::tests::common::*;

/// Enough headroom before `CREATED_AT` that a full lookback window never reaches
/// back past a position's origination day.
const AFTER_WINDOW: u64 = (CALL_LOOKBACK_DAYS as u64 + 2) * DAY;

/// An ISO code the fixture registers no `COEN/<iso>` pair for.
const UNPRICED_ISO: u16 = 392; // JPY

fn state_of(storage: &StorageHandle<'_>, position_id: U256) -> CredisState {
    CredisContract::new(storage.clone())
        .get_position(position_id)
        .unwrap()
        .lifecycle_state()
        .unwrap()
}

/// The `n`th closed day back from the one closed at `at` (0 = the newest).
fn day_back(at: u64, n: u32) -> u32 {
    let mut day = last_closed_day(at);
    for _ in 0..n {
        day = previous_date_key(day);
    }
    day
}

/// Opens a position and publishes `days` closed days at `price`, ending at the
/// day closed at `at`. Returns the position id.
fn open_with_series(storage: &StorageHandle<'_>, at: u64, days: u32, price: U256) -> U256 {
    let position_id = open(storage, 1);
    advance_to(storage, at);
    fill_days(storage, last_closed_day(at), days, price);
    position_id
}

/// Rewrites the call terms sealed on a position, the way a retuned constant
/// would have if the terms were still read live. Widens the currency's
/// high-water mark alongside, exactly as `open_position` does.
fn reterm(
    storage: &StorageHandle<'_>,
    position_id: U256,
    window_days: u32,
    threshold_days: u32,
    notice_days: u32,
) {
    let credis = CredisContract::new(storage.clone());
    let mut position = credis.get_position(position_id).unwrap();
    position.call_window_seconds = window_days * SECS_PER_DAY;
    position.call_threshold_seconds = threshold_days * SECS_PER_DAY;
    position.call_notice_period_seconds = notice_days * SECS_PER_DAY;
    credis.positions.update(&position).unwrap();
    outbe_primitives::call_breach::widen_scan_terms(
        &credis.max_call_window_seconds,
        &credis.min_call_threshold_seconds,
        REFERENCE_ISO,
        position.call_window_seconds,
        position.call_threshold_seconds,
    )
    .unwrap();
}

/// The terms a position is called and voided under are the ones sealed at
/// opening, not the live constants. A `const` cannot be retuned at runtime, so
/// this proves it from the other side: rewrite what the record holds and watch
/// the scan follow the record rather than the constant.
#[test]
fn the_scan_follows_the_terms_sealed_on_the_position_not_the_constants() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let at = CREATED_AT + AFTER_WINDOW;
        let position_id = open_with_series(&storage, at, CALL_LOOKBACK_DAYS, below_call());
        // Three breach days at the head of the window - far short of the 21 the
        // constant demands, and exactly the threshold the record will carry.
        for i in 0..3 {
            set_vwap(&storage, day_back(at, i), above_call());
        }

        assert_eq!(
            scan(&storage, at),
            0,
            "the constant's 21-of-28 threshold is unmet"
        );

        reterm(&storage, position_id, 3, 3, 1);
        assert_eq!(scan(&storage, at), 1, "3 of 3 meets the sealed threshold");
        assert_eq!(state_of(&storage, position_id), CredisState::Called);

        // And the sealed notice period governs the void: one day, not seven.
        let position = CredisContract::new(storage.clone())
            .get_position(position_id)
            .unwrap();
        assert_eq!(
            outbe_credis::settlement_deadline(&position),
            at + DAY,
            "the deadline follows the sealed notice period"
        );
        let lapsed = at + DAY + HOUR;
        advance_to(&storage, lapsed);
        assert_eq!(expire(&storage, lapsed), 1);
        assert_eq!(state_of(&storage, position_id), CredisState::Void);
    });
    teardown();
}

/// A position carrying zero terms is uncallable, not callable on every day.
/// Zero is what a record sealed before the terms existed reads back, and
/// `breaches >= 0` would otherwise call the whole book on the next scan.
#[test]
fn a_position_with_zero_call_terms_is_never_called() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let at = CREATED_AT + AFTER_WINDOW;
        let position_id = open_with_series(&storage, at, CALL_LOOKBACK_DAYS, above_call());

        reterm(&storage, position_id, 0, 0, 0);
        assert_eq!(
            scan(&storage, at),
            0,
            "a full breach window still does not call"
        );
        assert_eq!(state_of(&storage, position_id), CredisState::Open);
    });
    teardown();
}

/// A position whose sealed window outruns the current constant still gets its
/// whole span collected: the scan sizes the shared per-currency window off the
/// `max_call_window_seconds` high-water mark, not off the constant.
#[test]
fn a_window_wider_than_the_constant_is_collected_in_full() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        const WIDE_DAYS: u32 = 40;
        let at = CREATED_AT + (WIDE_DAYS as u64 + 5) * DAY;
        let position_id = open(&storage, 1);
        advance_to(&storage, at);
        fill_days(&storage, last_closed_day(at), WIDE_DAYS, above_call());

        // 35 of the 40 days must breach, which no 28-day window can supply.
        reterm(&storage, position_id, WIDE_DAYS, 35, 7);
        assert_eq!(scan(&storage, at), 1);
        assert_eq!(state_of(&storage, position_id), CredisState::Called);
    });
    teardown();
}

#[test]
fn a_node_local_failure_while_calling_a_position_fails_the_scan() {
    let mut storage = env();
    let at = CREATED_AT + AFTER_WINDOW;
    let position_id = StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        open_with_series(&storage, at, CALL_LOOKBACK_DAYS, above_call())
    });
    storage.fail_after_mutation_at(0);
    let result = StorageHandle::enter(&mut storage, |storage| {
        let ctx = outbe_primitives::block::BlockRuntimeContext::new(
            outbe_primitives::block::BlockContext::empty_for_tests(BLOCK_NUMBER, at, CHAIN_ID),
            storage.clone(),
        );
        crate::called::scan_and_call(&ctx)
    });
    storage.clear_mutation_failure();
    assert!(matches!(
        result,
        Err(outbe_primitives::error::PrecompileError::Storage(_))
    ));
    StorageHandle::enter(&mut storage, |storage| {
        assert_eq!(state_of(&storage, position_id), CredisState::Open);
    });
    teardown();
}

#[test]
fn a_full_window_above_the_call_price_calls_the_position() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let at = CREATED_AT + AFTER_WINDOW;
        let position_id = open_with_series(&storage, at, CALL_LOOKBACK_DAYS, above_call());

        assert_eq!(scan(&storage, at), 1);
        assert_eq!(state_of(&storage, position_id), CredisState::Called);

        let position = CredisContract::new(storage.clone())
            .get_position(position_id)
            .unwrap();
        assert_eq!(position.called_at, at, "stamped with the run's timestamp");
        assert_eq!(
            outbe_credis::settlement_deadline(&position),
            at + NOTICE,
            "the settlement window opens at the call"
        );

        // The owner's called-position counter tracks the unresolved call.
        assert!(CredisContract::new(storage.clone())
            .has_called_position(alice())
            .unwrap());

        // Idempotent: a second run does not move the deadline.
        assert_eq!(scan(&storage, at), 0);
        assert_eq!(
            CredisContract::new(storage.clone())
                .get_position(position_id)
                .unwrap()
                .called_at,
            at
        );
    });
    teardown();
}

/// The breach test is strictly above the call price, as it is for Nod, Gem and
/// Intex. A window that closes exactly on the call price every single day must
/// therefore leave the position open.
#[test]
fn a_full_window_exactly_at_the_call_price_does_not_call_the_position() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let at = CREATED_AT + AFTER_WINDOW;
        let position_id = open_with_series(&storage, at, CALL_LOOKBACK_DAYS, at_call());

        assert_eq!(scan(&storage, at), 0);
        assert_eq!(state_of(&storage, position_id), CredisState::Open);

        // One minor unit higher on every day is a breach window.
        fill_days(
            &storage,
            last_closed_day(at),
            CALL_LOOKBACK_DAYS,
            above_call(),
        );
        assert_eq!(scan(&storage, at), 1);
        assert_eq!(state_of(&storage, position_id), CredisState::Called);
    });
    teardown();
}

#[test]
fn the_window_absorbs_below_call_days_up_to_the_slack() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let at = CREATED_AT + AFTER_WINDOW;
        let position_id = open_with_series(&storage, at, CALL_LOOKBACK_DAYS, above_call());

        // Scatter the full slack of below-call days through the window. The rule
        // counts breach days rather than requiring a run, so their position must
        // not matter: exactly `CALL_THRESHOLD_DAYS` still calls.
        let slack = CALL_LOOKBACK_DAYS - CALL_THRESHOLD_DAYS;
        let stride = (CALL_LOOKBACK_DAYS - 1) / slack;
        for i in 0..slack {
            let offset = i * stride + 1;
            // Guards the boundary: a below-call day placed past the window would
            // silently leave more than `CALL_THRESHOLD_DAYS` breaches standing.
            // Then this test would stop probing the threshold.
            assert!(
                offset < CALL_LOOKBACK_DAYS,
                "below-call day {offset} falls outside the lookback window"
            );
            set_vwap(&storage, day_back(at, offset), below_call());
        }

        assert_eq!(scan(&storage, at), 1);
        assert_eq!(state_of(&storage, position_id), CredisState::Called);
    });
    teardown();
}

#[test]
fn one_breach_day_short_of_the_threshold_does_not_call() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let at = CREATED_AT + AFTER_WINDOW;
        let position_id = open_with_series(&storage, at, CALL_LOOKBACK_DAYS, above_call());

        // One more below-call day than the window can absorb.
        let below = CALL_LOOKBACK_DAYS - CALL_THRESHOLD_DAYS + 1;
        for i in 0..below {
            set_vwap(&storage, day_back(at, i + 1), below_call());
        }

        assert_eq!(scan(&storage, at), 0);
        assert_eq!(state_of(&storage, position_id), CredisState::Open);

        // Raising one of them back over the call price completes the threshold.
        set_vwap(&storage, day_back(at, 1), above_call());
        assert_eq!(scan(&storage, at), 1);
        assert_eq!(state_of(&storage, position_id), CredisState::Called);
    });
    teardown();
}

#[test]
fn missing_days_do_not_count_as_breaches() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let at = CREATED_AT + AFTER_WINDOW;
        let position_id = open(&storage, 1);
        advance_to(&storage, at);

        // Publish one day short of the threshold and leave the rest of the window
        // unpublished. Section 11.3's placeholder: a day with no reference price is not
        // a breach, so it can only delay a call.
        for i in 0..CALL_THRESHOLD_DAYS - 1 {
            set_vwap(&storage, day_back(at, i), above_call());
        }
        // The watermark must still cover the window, or the run would skip.
        finalize_through(&storage, at);

        assert_eq!(scan(&storage, at), 0);
        assert_eq!(state_of(&storage, position_id), CredisState::Open);

        // Filling one more published day reaches the threshold, even though the
        // rest of the window still has no price at all.
        set_vwap(
            &storage,
            day_back(at, CALL_THRESHOLD_DAYS - 1),
            above_call(),
        );
        assert_eq!(scan(&storage, at), 1);
        assert_eq!(state_of(&storage, position_id), CredisState::Called);
    });
    teardown();
}

#[test]
fn a_breach_run_that_predates_the_position_does_not_call_it() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        // The series is long and fully breached, but the position is 3 days old,
        // so the window reaches back before it existed.
        let at = CREATED_AT + 3 * DAY;
        let position_id = open(&storage, 1);
        advance_to(&storage, at);
        fill_days(
            &storage,
            last_closed_day(at),
            CALL_LOOKBACK_DAYS + 10,
            above_call(),
        );

        assert_eq!(scan(&storage, at), 0);
        assert_eq!(state_of(&storage, position_id), CredisState::Open);

        // Once the position is old enough for the window to sit entirely after
        // its origination day, the call fires.
        let later = CREATED_AT + AFTER_WINDOW;
        advance_to(&storage, later);
        fill_days(
            &storage,
            last_closed_day(later),
            CALL_LOOKBACK_DAYS,
            above_call(),
        );
        assert_eq!(scan(&storage, later), 1);
        assert_eq!(state_of(&storage, position_id), CredisState::Called);
    });
    teardown();
}

/// A position opened at 00:00 counts its issuance day. One opened at 00:00:01 starts
/// counting the next day, so the same threshold run falls one day short.
#[test]
fn the_issuance_day_counts_only_for_a_position_opened_at_midnight() {
    for (offset, called_at_threshold) in [(0, true), (1, false)] {
        let mut storage = env();
        StorageHandle::enter(&mut storage, |storage| {
            bootstrap(&storage, pledge_cost());
            let midnight = (CREATED_AT / DAY + 1) * DAY;
            advance_to(&storage, midnight + offset);
            let position_id = open(&storage, 1);

            let at = midnight + u64::from(CALL_THRESHOLD_DAYS) * DAY;
            advance_to(&storage, at);
            fill_days(
                &storage,
                last_closed_day(at),
                CALL_THRESHOLD_DAYS,
                above_call(),
            );
            assert_eq!(scan(&storage, at), u32::from(called_at_threshold));
            if called_at_threshold {
                assert_eq!(state_of(&storage, position_id), CredisState::Called);
                return;
            }
            assert_eq!(state_of(&storage, position_id), CredisState::Open);

            let next = at + DAY;
            advance_to(&storage, next);
            set_vwap(&storage, last_closed_day(next), above_call());
            assert_eq!(scan(&storage, next), 1);
            assert_eq!(state_of(&storage, position_id), CredisState::Called);
        });
        teardown();
    }
}

#[test]
fn an_unfinalized_day_skips_the_run_without_touching_state() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let at = CREATED_AT + AFTER_WINDOW;
        let position_id = open_with_series(&storage, at, CALL_LOOKBACK_DAYS, above_call());

        // Rewind the watermark behind the last closed day: the oracle has not
        // closed it yet, so the run must skip rather than read it as missing.
        let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
        oracle
            .utc_day_vwap_last_finalized
            .write(previous_date_key(last_closed_day(at)))
            .unwrap();

        assert_eq!(scan(&storage, at), 0);
        assert_eq!(state_of(&storage, position_id), CredisState::Open);
    });
    teardown();
}

#[test]
fn the_call_and_the_void_compose_across_runs() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let at = CREATED_AT + AFTER_WINDOW;
        let position_id = open_with_series(&storage, at, CALL_LOOKBACK_DAYS, above_call());

        assert_eq!(scan(&storage, at), 1);
        assert_eq!(state_of(&storage, position_id), CredisState::Called);

        // The same block can never void a position that it called: the
        // window opens at `called_at = now`.
        assert_eq!(expire(&storage, at), 0);

        // Inside the window, nothing happens.
        let inside = at + NOTICE - DAY;
        advance_to(&storage, inside);
        assert_eq!(expire(&storage, inside), 0);
        assert_eq!(state_of(&storage, position_id), CredisState::Called);

        // The window lapses with the whole principal outstanding: the void burns
        // the entire collateral and credits it to the Promis Reserve.
        let lapsed = at + NOTICE + HOUR;
        advance_to(&storage, lapsed);
        assert_eq!(expire(&storage, lapsed), 1);
        assert_eq!(state_of(&storage, position_id), CredisState::Void);
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
        assert_eq!(
            outbe_promislimit::PromisLimitContract::new(storage.clone())
                .get_total_unallocated()
                .unwrap(),
            pledge_cost()
        );

        // The void cleared the owner's called count and left the deadline queue.
        assert!(!CredisContract::new(storage.clone())
            .has_called_position(alice())
            .unwrap());
        assert_eq!(queued_at(&storage, position_id), 0);
    });
    teardown();
}

#[test]
fn each_reference_currency_prices_off_its_own_daily_series() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let at = CREATED_AT + AFTER_WINDOW;
        let position_id = open(&storage, 1);

        // Re-point the stored position's ANCHOR at an unregistered currency, so it
        // prices off a series that does not exist, and index it there.
        {
            use outbe_primitives::call_bins;
            let credis = CredisContract::new(storage.clone());
            let mut position = credis.get_position(position_id).unwrap();
            let bin = call_bins::price_to_bin(position.call_price_minor).unwrap();
            call_bins::remove(&CallBins(&credis, REFERENCE_ISO), position_id).unwrap();
            call_bins::insert(&CallBins(&credis, UNPRICED_ISO), position_id, bin).unwrap();
            position.reference_currency = UNPRICED_ISO;
            credis.positions.update(&position).unwrap();
        }

        // The seeded reference series is a full breach window, but this position is
        // no longer anchored to it.
        advance_to(&storage, at);
        fill_days(
            &storage,
            last_closed_day(at),
            CALL_LOOKBACK_DAYS,
            above_call(),
        );
        assert_eq!(
            scan(&storage, at),
            0,
            "an unpriced reference currency is never called"
        );
        assert_eq!(state_of(&storage, position_id), CredisState::Open);
    });
    teardown();
}

/// The call is anchored to the reference currency, never to the issuance currency
/// the position is denominated in. This test checks both directions, because a
/// wrong anchor fails silently in one of them. With the two series moving
/// together, an issuance-keyed scan still reaches the right verdict by coincidence.
#[test]
fn the_call_follows_the_reference_series_and_ignores_the_issuance_one() {
    // Breach published only on the reference series -> the position is called.
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let at = CREATED_AT + AFTER_WINDOW;
        let position_id = open(&storage, 1);
        assert_ne!(
            ISSUANCE_ISO, REFERENCE_ISO,
            "the fixture must keep the two codes distinct"
        );

        advance_to(&storage, at);
        fill_days_for(
            &storage,
            REFERENCE_ISO,
            last_closed_day(at),
            CALL_LOOKBACK_DAYS,
            above_call(),
        );
        // COEN/840 stays silent for the whole window.
        assert_eq!(scan(&storage, at), 1);
        assert_eq!(state_of(&storage, position_id), CredisState::Called);
    });
    teardown();

    // Breach published only on the issuance series -> nothing happens.
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let at = CREATED_AT + AFTER_WINDOW;
        let position_id = open(&storage, 1);

        advance_to(&storage, at);
        fill_days_for(
            &storage,
            ISSUANCE_ISO,
            last_closed_day(at),
            CALL_LOOKBACK_DAYS,
            above_call(),
        );
        assert_eq!(
            scan(&storage, at),
            0,
            "a breach in the issuance currency must not call the position"
        );
        assert_eq!(state_of(&storage, position_id), CredisState::Open);
    });
    teardown();
}

/// Opens one position for each of three distinct owners, so none of them trips
/// the called-position gate. Returns the ids in call-index order.
fn open_three(storage: &StorageHandle<'_>) -> Vec<U256> {
    let owners: [Address; 3] = [alice(), bob(), cca()];
    for owner in owners {
        bootstrap_for(storage, owner, pledge_cost());
    }
    owners
        .iter()
        .map(|owner| open_for(storage, *owner, 1))
        .collect()
}

/// Positions of the reference currency's walk in flight still to visit in its bin.
fn cursor_of(storage: &StorageHandle<'_>) -> u32 {
    let packed = CredisContract::new(storage.clone())
        .call_bin_cursor
        .read(&REFERENCE_ISO)
        .unwrap();
    outbe_primitives::call_bins::unpack_cursor(packed).1
}

fn is_indexed(storage: &StorageHandle<'_>, position_id: U256) -> bool {
    CredisContract::new(storage.clone())
        .call_position_slot
        .read(&position_id)
        .unwrap()
        != 0
}

#[test]
fn a_completed_pass_resets_the_cursor() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        let ids = open_three(&storage);
        for id in &ids {
            assert!(
                is_indexed(&storage, *id),
                "an open position waits for its call"
            );
        }

        let at = CREATED_AT + AFTER_WINDOW;
        advance_to(&storage, at);
        fill_days(
            &storage,
            last_closed_day(at),
            CALL_LOOKBACK_DAYS,
            above_call(),
        );

        assert_eq!(scan(&storage, at), 3);
        for id in &ids {
            assert_eq!(state_of(&storage, *id), CredisState::Called);
        }
        assert_eq!(cursor_of(&storage), 0, "a completed pass resets the cursor");
    });
    teardown();
}

fn sweep_state<'storage>(storage: &StorageHandle<'storage>) -> CredisContract<'storage> {
    CredisContract::new(storage.clone())
}

/// Pins `day` as the sweep in flight, stopped with `remaining` positions of the
/// bin `sample` sits in still to visit.
fn pin_sweep(storage: &StorageHandle<'_>, day: u32, sample: U256, remaining: u32) {
    sweep_state(storage).call_sweep_day.write(day).unwrap();
    let credis = CredisContract::new(storage.clone());
    let price = credis.get_position(sample).unwrap().call_price_minor;
    let bin = outbe_primitives::call_bins::price_to_bin(price).unwrap();
    credis
        .call_bin_cursor
        .write(
            &REFERENCE_ISO,
            outbe_primitives::call_bins::pack_cursor(bin, remaining),
        )
        .unwrap();
}

fn sweep_days(storage: &StorageHandle<'_>) -> (u32, u32) {
    let state = sweep_state(storage);
    (
        state.call_sweep_day.read().unwrap(),
        state.call_pending_day.read().unwrap(),
    )
}

#[test]
fn a_resumed_pass_starts_at_the_cursor_and_walks_down() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        let ids = open_three(&storage);
        let at = CREATED_AT + AFTER_WINDOW;
        advance_to(&storage, at);
        fill_days(
            &storage,
            last_closed_day(at),
            CALL_LOOKBACK_DAYS,
            above_call(),
        );
        // Two positions of the bin are left to visit: indices 1 and 0.
        pin_sweep(&storage, last_closed_day(at), ids[0], 2);

        assert_eq!(slice(&storage, at), 2);
        assert_eq!(state_of(&storage, ids[0]), CredisState::Called);
        assert_eq!(state_of(&storage, ids[1]), CredisState::Called);
        assert_eq!(
            state_of(&storage, ids[2]),
            CredisState::Open,
            "the entry above the resume point waits for the next pass"
        );
        assert_eq!(cursor_of(&storage), 0);
        assert_eq!(sweep_days(&storage), (0, 0), "the pass ended");

        // The next pass starts fresh from the top and picks it up.
        assert_eq!(scan(&storage, at), 1);
        assert_eq!(state_of(&storage, ids[2]), CredisState::Called);
    });
    teardown();
}

#[test]
fn a_newer_closed_day_waits_for_the_pass_in_flight() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        let ids = open_three(&storage);
        let at = CREATED_AT + AFTER_WINDOW;
        let next = at + DAY;
        advance_to(&storage, next);
        fill_days(
            &storage,
            last_closed_day(next),
            CALL_LOOKBACK_DAYS + 1,
            above_call(),
        );
        pin_sweep(&storage, last_closed_day(at), ids[0], 2);

        schedule(&storage, next);
        assert_eq!(
            sweep_days(&storage),
            (last_closed_day(at), last_closed_day(next))
        );
        assert_eq!(state_of(&storage, ids[2]), CredisState::Open);

        assert_eq!(slice(&storage, next), 2, "the pinned day finishes first");
        assert_eq!(state_of(&storage, ids[2]), CredisState::Open);
        assert_eq!(sweep_days(&storage), (last_closed_day(next), 0));
        assert_eq!(cursor_of(&storage), 0);

        assert_eq!(slice(&storage, next), 1);
        assert_eq!(state_of(&storage, ids[2]), CredisState::Called);
        assert_eq!(sweep_days(&storage), (0, 0));
    });
    teardown();
}

#[test]
fn a_third_closed_day_replaces_the_waiting_one_and_names_it() {
    use alloy_sol_types::SolEvent;

    let mut provider = env();
    let (in_flight, skipped) = StorageHandle::enter(&mut provider, |storage| {
        let ids = open_three(&storage);
        let at = CREATED_AT + AFTER_WINDOW;
        let later = at + 2 * DAY;
        advance_to(&storage, later);
        fill_days(
            &storage,
            last_closed_day(later),
            CALL_LOOKBACK_DAYS + 2,
            above_call(),
        );
        pin_sweep(&storage, last_closed_day(at), ids[0], 2);
        sweep_state(&storage)
            .call_pending_day
            .write(last_closed_day(at + DAY))
            .unwrap();

        schedule(&storage, later);
        assert_eq!(
            sweep_days(&storage),
            (last_closed_day(at), last_closed_day(later))
        );
        assert_eq!(cursor_of(&storage), 2, "the pass in flight keeps its place");
        (last_closed_day(at), last_closed_day(at + DAY))
    });
    teardown();

    let events: Vec<_> = provider
        .get_events(outbe_primitives::addresses::CREDIS_FACTORY_ADDRESS)
        .iter()
        .filter_map(|log| {
            crate::precompile::ICredisFactory::SweepDaySkipped::decode_log_data(log).ok()
        })
        .collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].sweep, 1);
    assert_eq!(events[0].skippedDay, skipped);
    assert_eq!(events[0].inFlightDay, in_flight);
}

#[test]
fn an_empty_book_opens_no_sweep() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        let at = CREATED_AT + AFTER_WINDOW;
        advance_to(&storage, at);
        finalize_through(&storage, at);

        assert_eq!(scan(&storage, at), 0);
        assert_eq!(sweep_days(&storage), (0, 0));
    });
    teardown();
}

#[test]
fn a_pinned_day_the_oracle_has_not_finalized_holds_the_sweep() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        let ids = open_three(&storage);
        let at = CREATED_AT + AFTER_WINDOW;
        advance_to(&storage, at);
        fill_days(&storage, day_back(at, 1), CALL_LOOKBACK_DAYS, above_call());
        pin_sweep(&storage, last_closed_day(at), ids[0], 2);

        assert_eq!(slice(&storage, at), 0);
        assert_eq!(state_of(&storage, ids[1]), CredisState::Open);
        assert_eq!(sweep_days(&storage), (last_closed_day(at), 0));
        assert_eq!(cursor_of(&storage), 2);
    });
    teardown();
}

/// Calls `ids` by hand at `called_at`, which queues them on their deadline.
fn call_by_hand(storage: &StorageHandle<'_>, ids: &[U256], called_at: u64) {
    let mut credis = CredisContract::new(storage.clone());
    for id in ids {
        assert!(credis.mark_called(*id, called_at).unwrap());
    }
}

fn queued_at(storage: &StorageHandle<'_>, position_id: U256) -> u64 {
    CredisContract::new(storage.clone())
        .called_position_slot
        .read(&position_id)
        .unwrap()
}

#[test]
fn voiding_several_positions_in_one_block_skips_none() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        let ids = open_three(&storage);
        call_by_hand(&storage, &ids, CREATED_AT);

        let lapsed = CREATED_AT + NOTICE + HOUR;
        advance_to(&storage, lapsed);
        assert_eq!(expire(&storage, lapsed), 3, "all three voided in one block");

        for id in &ids {
            assert_eq!(state_of(&storage, *id), CredisState::Void);
            assert_eq!(queued_at(&storage, *id), 0);
        }
    });
    teardown();
}

#[test]
fn a_void_waits_for_the_hour_its_deadline_falls_in_to_close() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        let ids = open_three(&storage);
        call_by_hand(&storage, &ids[..1], CREATED_AT);
        let deadline = CREATED_AT + NOTICE;
        let hour_end = (deadline / HOUR + 1) * HOUR;
        assert!(
            hour_end > deadline + 1,
            "the fixture deadline sits inside its hour"
        );

        advance_to(&storage, deadline + 1);
        assert_eq!(expire(&storage, deadline + 1), 0);
        assert_eq!(state_of(&storage, ids[0]), CredisState::Called);

        advance_to(&storage, hour_end);
        assert_eq!(expire(&storage, hour_end), 1);
        assert_eq!(state_of(&storage, ids[0]), CredisState::Void);
    });
    teardown();
}

#[test]
fn every_cycle_tick_voids_without_the_daily_trigger() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        let ids = open_three(&storage);
        call_by_hand(&storage, &ids[..1], CREATED_AT);

        let lapsed = CREATED_AT + NOTICE + HOUR;
        advance_to(&storage, lapsed);
        tick(&storage, lapsed);
        assert_eq!(state_of(&storage, ids[0]), CredisState::Void);
        assert_eq!(state_of(&storage, ids[1]), CredisState::Open);
    });
    teardown();
}

#[test]
fn settling_a_called_position_in_full_leaves_the_queue() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let position_id = open(&storage, 1);
        call_by_hand(&storage, &[position_id], CREATED_AT);
        assert_ne!(
            queued_at(&storage, position_id),
            0,
            "a call queues the position"
        );

        let outstanding = CredisContract::new(storage.clone())
            .get_position(position_id)
            .unwrap()
            .outstanding_principal_minor;
        settle_principal(&storage, alice(), position_id, outstanding);
        assert_eq!(state_of(&storage, position_id), CredisState::Settled);
        assert_eq!(queued_at(&storage, position_id), 0);

        let lapsed = CREATED_AT + NOTICE + HOUR;
        advance_to(&storage, lapsed);
        assert_eq!(expire(&storage, lapsed), 0);
        assert_eq!(
            CredisContract::new(storage.clone())
                .expiry_tree_root
                .read()
                .unwrap(),
            U256::ZERO,
            "no hour is left waiting"
        );
    });
    teardown();
}

#[test]
fn the_void_budget_bounds_one_block_and_the_next_block_drains_the_rest() {
    let mut provider = env();
    let budget = outbe_primitives::sweep_budget::SWEEP_WRITES_PER_BLOCK;
    let total = budget + 1;
    StorageHandle::enter(&mut provider, |storage| {
        bootstrap_for(&storage, alice(), pledge_cost() * U256::from(total));
    });
    // Open identical owner/CCA/asset tuples in successive blocks before calling.
    let ids: Vec<U256> = (1..=u64::from(total))
        .map(|nonce| {
            provider.set_block_number(BLOCK_NUMBER + nonce);
            StorageHandle::enter(&mut provider, |storage| open_for(&storage, alice(), nonce))
        })
        .collect();
    StorageHandle::enter(&mut provider, |storage| {
        call_by_hand(&storage, &ids, CREATED_AT);
        let lapsed = CREATED_AT + NOTICE + HOUR;
        advance_to(&storage, lapsed);

        assert_eq!(expire(&storage, lapsed), budget);
        assert_eq!(
            ids.iter()
                .filter(|id| queued_at(&storage, **id) != 0)
                .count(),
            1
        );
        assert_eq!(expire(&storage, lapsed), 1);
        for id in &ids {
            assert_eq!(state_of(&storage, *id), CredisState::Void);
        }
    });
    teardown();
}

#[test]
fn a_failed_void_fails_the_block_and_keeps_the_position_queued() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        let ids = open_three(&storage);
        call_by_hand(&storage, &ids[..1], CREATED_AT);
        let lapsed = CREATED_AT + NOTICE + HOUR;
        advance_to(&storage, lapsed);

        outbe_gratis::enclave_client::test_enclave::uninstall();
        let ctx = outbe_primitives::block::BlockRuntimeContext::new(
            outbe_primitives::block::BlockContext::empty_for_tests(BLOCK_NUMBER, lapsed, CHAIN_ID),
            storage.clone(),
        );
        assert!(crate::expired::sweep_expired(&ctx).is_err());
        assert_eq!(state_of(&storage, ids[0]), CredisState::Called);
        assert_ne!(queued_at(&storage, ids[0]), 0);
    });
    teardown();
}

/// A failure every node hits alike, here a broken pledged-supply total, defers the void.
#[test]
fn a_deterministic_void_failure_defers_the_position() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        let ids = open_three(&storage);
        call_by_hand(&storage, &ids[..1], CREATED_AT);
        let lapsed = CREATED_AT + NOTICE + HOUR;
        advance_to(&storage, lapsed);
        let queued = queued_at(&storage, ids[0]);

        outbe_gratis::schema::Gratis::new(storage.clone())
            .pledged_total_supply
            .write(U256::ZERO)
            .unwrap();
        assert_eq!(expire(&storage, lapsed), 0);
        assert_eq!(state_of(&storage, ids[0]), CredisState::Called);
        assert_ne!(queued_at(&storage, ids[0]), 0);
        assert_ne!(queued_at(&storage, ids[0]), queued);
    });
    teardown();
}
