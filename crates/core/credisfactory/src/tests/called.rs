//! Daily price-path scan: the floor latch, the 21-consecutive-day call, and the
//! void of a lapsed settlement window.
//!
//! Every test drives [`crate::called::scan_and_call`] through the `scan` harness
//! helper against a seeded finalized daily series, which is the only price source
//! the production trigger reads.

use alloy_primitives::{Address, U256};

use outbe_credis::constants::CALL_STREAK_DAYS;
use outbe_credis::{CredisContract, CredisState};
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::previous_date_key;

use crate::tests::common::*;

/// Enough headroom before `CREATED_AT` that a full streak window never reaches
/// back past a position's origination day.
const AFTER_STREAK: u64 = (CALL_STREAK_DAYS as u64 + 2) * DAY;

fn state_of(storage: &StorageHandle<'_>, position_id: U256) -> CredisState {
    CredisContract::new(storage.clone())
        .get_position(position_id)
        .unwrap()
        .lifecycle_state()
        .unwrap()
}

/// Opens a position and publishes `days` closed days at `price`, ending at the
/// day closed at `at`. Returns the position id.
fn open_with_series(storage: &StorageHandle<'_>, at: u64, days: u32, price: U256) -> U256 {
    let position_id = open(storage, 1);
    advance_to(storage, at);
    fill_days(storage, last_closed_day(at), days, price);
    position_id
}

#[test]
fn a_day_above_the_floor_latches_an_open_position() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let position_id = open(&storage, 1);
        assert_eq!(state_of(&storage, position_id), CredisState::Open);

        // A closed day below the floor (2.16) leaves it Open.
        let at = CREATED_AT + 2 * DAY;
        advance_to(&storage, at);
        set_vwap(&storage, last_closed_day(at), oracle_rate());
        assert_eq!(scan(&storage, at), 0);
        assert_eq!(state_of(&storage, position_id), CredisState::Open);

        // A closed day above it latches, with no settle call involved.
        let at = at + DAY;
        advance_to(&storage, at);
        set_vwap(&storage, last_closed_day(at), above_floor());
        assert_eq!(scan(&storage, at), 1);
        assert_eq!(state_of(&storage, position_id), CredisState::Settleable);
    });
    teardown();
}

#[test]
fn the_latch_is_one_way_across_runs() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let position_id = open(&storage, 1);

        let at = CREATED_AT + 2 * DAY;
        advance_to(&storage, at);
        set_vwap(&storage, last_closed_day(at), above_floor());
        assert_eq!(scan(&storage, at), 1);

        // The price falls back below the floor; the position stays settleable and
        // the next run has nothing to do.
        let at = at + DAY;
        advance_to(&storage, at);
        set_vwap(&storage, last_closed_day(at), oracle_rate());
        assert_eq!(scan(&storage, at), 0);
        assert_eq!(state_of(&storage, position_id), CredisState::Settleable);
    });
    teardown();
}

#[test]
fn a_full_streak_at_the_call_price_calls_the_position() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let at = CREATED_AT + AFTER_STREAK;
        let position_id = open_with_series(&storage, at, CALL_STREAK_DAYS, at_call());

        // Latch and call land in the same run: a day at the call price is by
        // construction above the floor.
        assert_eq!(scan(&storage, at), 1);
        assert_eq!(state_of(&storage, position_id), CredisState::Called);

        let position = CredisContract::new(storage.clone())
            .get_position(position_id)
            .unwrap();
        assert_eq!(position.called_at, at, "stamped with the run's timestamp");
        assert_eq!(
            outbe_credis::settlement_deadline(&position),
            at + 14 * DAY,
            "the 14-day settlement window opens at the call"
        );

        // The owner is blocked from opening new positions while it is unresolved.
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

#[test]
fn one_day_short_of_the_streak_does_not_call() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let at = CREATED_AT + AFTER_STREAK;
        // 20 days at the call price, and the 21st (oldest) below it.
        let position_id = open_with_series(&storage, at, CALL_STREAK_DAYS - 1, at_call());
        let mut oldest = last_closed_day(at);
        for _ in 0..CALL_STREAK_DAYS - 1 {
            oldest = previous_date_key(oldest);
        }
        set_vwap(&storage, oldest, above_floor());

        // Latches on the recent days but the streak is incomplete.
        assert_eq!(scan(&storage, at), 1);
        assert_eq!(state_of(&storage, position_id), CredisState::Settleable);
    });
    teardown();
}

#[test]
fn a_single_below_call_day_resets_the_streak() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let at = CREATED_AT + AFTER_STREAK;
        let position_id = open_with_series(&storage, at, CALL_STREAK_DAYS + 5, at_call());

        // Drop day 11 of the window one wei below the call price.
        let mut day = last_closed_day(at);
        for _ in 0..10 {
            day = previous_date_key(day);
        }
        set_vwap(&storage, day, at_call() - U256::from(1u64));

        assert_eq!(scan(&storage, at), 1, "latches only");
        assert_eq!(state_of(&storage, position_id), CredisState::Settleable);

        // Once that day rolls out of the window the streak completes.
        let later = at + 11 * DAY;
        advance_to(&storage, later);
        fill_days(&storage, last_closed_day(later), 11, at_call());
        assert_eq!(scan(&storage, later), 1);
        assert_eq!(state_of(&storage, position_id), CredisState::Called);
    });
    teardown();
}

#[test]
fn a_missing_day_resets_the_streak() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let at = CREATED_AT + AFTER_STREAK;
        let position_id = open(&storage, 1);
        advance_to(&storage, at);

        // Publish the whole window except one day in the middle. §11.3's
        // placeholder: a missing reference price resets the run.
        let mut day = last_closed_day(at);
        for i in 0..CALL_STREAK_DAYS {
            if i != 10 {
                set_vwap(&storage, day, at_call());
            }
            day = previous_date_key(day);
        }
        // The watermark must still cover the window, or the run would skip.
        finalize_through(&storage, at);

        assert_eq!(scan(&storage, at), 1, "latches only");
        assert_eq!(state_of(&storage, position_id), CredisState::Settleable);
    });
    teardown();
}

#[test]
fn a_streak_that_predates_the_position_does_not_call_it() {
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
            CALL_STREAK_DAYS + 10,
            at_call(),
        );

        assert_eq!(scan(&storage, at), 1, "latches only");
        assert_eq!(state_of(&storage, position_id), CredisState::Settleable);

        // Once the position is old enough for the window to sit entirely after
        // its origination day, the call fires.
        let later = CREATED_AT + AFTER_STREAK;
        advance_to(&storage, later);
        fill_days(
            &storage,
            last_closed_day(later),
            CALL_STREAK_DAYS,
            at_call(),
        );
        assert_eq!(scan(&storage, later), 1);
        assert_eq!(state_of(&storage, position_id), CredisState::Called);
    });
    teardown();
}

#[test]
fn an_unfinalized_day_skips_the_run_without_touching_state() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let at = CREATED_AT + AFTER_STREAK;
        let position_id = open_with_series(&storage, at, CALL_STREAK_DAYS, at_call());

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
        let at = CREATED_AT + AFTER_STREAK;
        let position_id = open_with_series(&storage, at, CALL_STREAK_DAYS, at_call());

        assert_eq!(scan(&storage, at), 1);
        assert_eq!(state_of(&storage, position_id), CredisState::Called);

        // A position called in this same run can never be voided by it: the
        // window opens at `called_at = now`.
        assert_eq!(scan(&storage, at), 0);

        // Inside the window, nothing happens.
        let inside = at + 13 * DAY;
        advance_to(&storage, inside);
        finalize_through(&storage, inside);
        assert_eq!(scan(&storage, inside), 0);
        assert_eq!(state_of(&storage, position_id), CredisState::Called);

        // The window lapses with the whole principal outstanding: the entire
        // collateral is burned and credited to the Promis Reserve.
        let lapsed = at + 14 * DAY;
        advance_to(&storage, lapsed);
        finalize_through(&storage, lapsed);
        assert_eq!(scan(&storage, lapsed), 1);
        assert_eq!(state_of(&storage, position_id), CredisState::Void);
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
        assert_eq!(
            outbe_promislimit::PromisLimitContract::new(storage.clone())
                .get_total_unallocated()
                .unwrap(),
            pledge_cost()
        );

        // The void released the owner's call block and left the active index.
        assert!(!CredisContract::new(storage.clone())
            .has_called_position(alice())
            .unwrap());
        assert_eq!(
            CredisContract::new(storage.clone()).active_len().unwrap(),
            0
        );
    });
    teardown();
}

#[test]
fn each_currency_latches_off_its_own_daily_series() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let position_id = open(&storage, 1);

        // Re-point the asset's `isoCode()` stub at an unregistered currency and
        // hand-rewrite the position's currency, so this position prices off a
        // series that does not exist.
        {
            let credis = CredisContract::new(storage.clone());
            let mut position = credis.get_position(position_id).unwrap();
            position.issuance_currency = 978; // EUR — no COEN pair registered
            credis.positions.update(&position).unwrap();
        }

        // USD's series is far above the floor, but this position is not in USD.
        let at = CREATED_AT + 2 * DAY;
        advance_to(&storage, at);
        set_vwap(&storage, last_closed_day(at), at_call());
        assert_eq!(scan(&storage, at), 0, "an unpriced currency never latches");
        assert_eq!(state_of(&storage, position_id), CredisState::Open);
    });
    teardown();
}

/// Opens one position for each of three distinct owners, so none of them trips
/// the called-position gate. Returns the ids in active-index order.
fn open_three(storage: &StorageHandle<'_>) -> Vec<U256> {
    let owners: [Address; 3] = [alice(), bob(), cca()];
    bootstrap(storage, pledge_cost());
    for owner in owners {
        if owner != alice() {
            bootstrap_for(storage, owner, pledge_cost());
        }
    }
    owners
        .iter()
        .map(|owner| open_for(storage, *owner, 1))
        .collect()
}

fn cursor_of(storage: &StorageHandle<'_>) -> u32 {
    crate::schema::CredisFactoryContract::new(storage.clone())
        .call_scan_cursor
        .read()
        .unwrap()
}

#[test]
fn a_completed_pass_resets_the_cursor() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        let ids = open_three(&storage);
        assert_eq!(
            CredisContract::new(storage.clone()).active_len().unwrap(),
            3
        );

        let at = CREATED_AT + 2 * DAY;
        advance_to(&storage, at);
        set_vwap(&storage, last_closed_day(at), above_floor());

        assert_eq!(scan(&storage, at), 3);
        for id in &ids {
            assert_eq!(state_of(&storage, *id), CredisState::Settleable);
        }
        assert_eq!(cursor_of(&storage), 0, "a completed pass resets the cursor");
    });
    teardown();
}

#[test]
fn a_resumed_pass_starts_at_the_cursor_and_walks_down() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        let ids = open_three(&storage);

        // Pretend the previous run stopped after index 2: the stored cursor is
        // `index + 1`, so 2 means "resume at index 1".
        crate::schema::CredisFactoryContract::new(storage.clone())
            .call_scan_cursor
            .write(2)
            .unwrap();

        let at = CREATED_AT + 2 * DAY;
        advance_to(&storage, at);
        set_vwap(&storage, last_closed_day(at), above_floor());

        // Only indices 1 and 0 are visited; the position at index 2 is untouched.
        assert_eq!(scan(&storage, at), 2);
        assert_eq!(state_of(&storage, ids[0]), CredisState::Settleable);
        assert_eq!(state_of(&storage, ids[1]), CredisState::Settleable);
        assert_eq!(
            state_of(&storage, ids[2]),
            CredisState::Open,
            "the entry above the resume point waits for the next pass"
        );
        assert_eq!(cursor_of(&storage), 0);

        // The next pass starts fresh from the top and picks it up.
        assert_eq!(scan(&storage, at), 1);
        assert_eq!(state_of(&storage, ids[2]), CredisState::Settleable);
    });
    teardown();
}

#[test]
fn voiding_several_positions_in_one_pass_skips_none() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        let ids = open_three(&storage);

        // Latch and call all three by hand, then let the window lapse. The point
        // of the test is the traversal: each void swap-pops the active list, and
        // the descending walk must still visit every entry exactly once.
        let called_at = CREATED_AT;
        {
            let mut credis = CredisContract::new(storage.clone());
            for id in &ids {
                assert!(credis.mark_settleable(*id).unwrap());
                assert!(credis.mark_called(*id, called_at).unwrap());
            }
        }

        let lapsed = called_at + 14 * DAY;
        advance_to(&storage, lapsed);
        finalize_through(&storage, lapsed);
        assert_eq!(scan(&storage, lapsed), 3, "all three voided in one pass");

        for id in &ids {
            assert_eq!(state_of(&storage, *id), CredisState::Void);
        }
        assert_eq!(
            CredisContract::new(storage.clone()).active_len().unwrap(),
            0
        );
    });
    teardown();
}
