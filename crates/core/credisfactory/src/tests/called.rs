//! Daily price-path scan: the multi-week breach-count call, and the forfeit of a
//! lapsed settlement window from the deadline queue.
//!
//! Tests drive [`crate::called::scan_and_call`] through the `scan` harness helper,
//! and later blocks' slices through `slice`, against a seeded finalized daily
//! series. This series is the only price source the production trigger reads.

use alloy_primitives::{Address, B256, U256};

use outbe_credis::constants::{CALL_LOOKBACK_DAYS, CALL_THRESHOLD_DAYS};
use outbe_credis::{CallBins, CredisContract, CredisState};
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::previous_date_key;
use outbe_primitives::time::SECONDS_PER_DAY_U32;

use crate::tests::*;

/// Enough headroom before `CREATED_AT` that a full lookback window never reaches
/// back past a Credis's origination day.
const AFTER_WINDOW: u64 = (CALL_LOOKBACK_DAYS as u64 + 2) * DAY;

/// An ISO code the fixture registers no `COEN/<iso>` pair for.
const UNPRICED_ISO: u16 = 392; // JPY

fn state_of(storage: &StorageHandle<'_>, credis_id: U256) -> CredisState {
    CredisContract::new(storage.clone())
        .get_credis(credis_id)
        .unwrap()
        .lifecycle_state()
        .unwrap()
}

fn is_deleted(storage: &StorageHandle<'_>, credis_id: U256) -> bool {
    !CredisContract::new(storage.clone())
        .credis_exists(credis_id)
        .unwrap()
}

fn group_of(storage: &StorageHandle<'_>, credis_id: U256) -> B256 {
    CredisContract::new(storage.clone())
        .get_credis(credis_id)
        .unwrap()
        .group_key()
        .expect("date step")
}

/// The `n`th closed day back from the one closed at `at` (0 = the newest).
fn day_back(at: u64, n: u32) -> u32 {
    let mut day = last_closed_day(at);
    for _ in 0..n {
        day = previous_date_key(day).expect("date step");
    }
    day
}

/// Opens a Credis and publishes `days` closed days at `price`, ending at the
/// day closed at `at`. Returns the Credis id.
fn open_with_series(storage: &StorageHandle<'_>, at: u64, days: u32, price: U256) -> U256 {
    let credis_id = open(storage, 1);
    advance_to(storage, at);
    fill_days(storage, last_closed_day(at), days, price);
    block_at(storage, at);
    credis_id
}

/// Rewrites the call terms sealed on a Credis's group, the way a switched profile
/// would have if the terms were still read live. Widens the currency's
/// high-water mark alongside, exactly as `issue` does.
fn reterm(
    storage: &StorageHandle<'_>,
    credis_id: U256,
    window_days: u32,
    threshold_days: u32,
    notice_days: u32,
) {
    let credis = CredisContract::new(storage.clone());
    let group = group_of(storage, credis_id);
    let window = window_days * SECONDS_PER_DAY_U32;
    let threshold = threshold_days * SECONDS_PER_DAY_U32;
    credis
        .group_call_window_seconds
        .write(&group, window)
        .unwrap();
    credis
        .group_call_threshold_seconds
        .write(&group, threshold)
        .unwrap();
    credis
        .group_call_notice_period_seconds
        .write(&group, notice_days * SECONDS_PER_DAY_U32)
        .unwrap();
    outbe_primitives::call_breach::widen_scan_terms(
        &credis.max_call_window_seconds,
        &credis.min_call_threshold_seconds,
        REFERENCE_ISO,
        window,
        threshold,
    )
    .unwrap();
}

/// The terms a group is called and forfeited under are the ones sealed at
/// issuance, not the live profile: rewrite what the group holds and watch the
/// scan follow the group rather than the profile.
#[test]
fn the_scan_follows_the_terms_sealed_on_the_group_not_the_profile() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let at = CREATED_AT + AFTER_WINDOW;
        let credis_id = open_with_series(&storage, at, CALL_LOOKBACK_DAYS, below_call());
        // Three breach days at the head of the window - far short of the 21 the
        // profile demands, and exactly the threshold the record will carry.
        for i in 0..3 {
            set_vwap(&storage, day_back(at, i), above_call());
        }

        assert_eq!(
            scan(&storage, at),
            0,
            "the profile's 21-of-28 threshold is unmet"
        );

        reterm(&storage, credis_id, 3, 3, 1);
        assert_eq!(
            scan(&storage, at),
            0,
            "a completed day cannot reopen after retuning"
        );
        let at = at + DAY;
        advance_to(&storage, at);
        set_vwap(&storage, last_closed_day(at), above_call());
        assert_eq!(
            scan(&storage, at),
            1,
            "3 of 3 meets the sealed threshold on the next day"
        );
        assert_eq!(state_of(&storage, credis_id), CredisState::Called);

        // And the sealed notice period governs the forfeit: one day, not seven.
        let record = CredisContract::new(storage.clone())
            .get_credis(credis_id)
            .unwrap();
        assert_eq!(
            outbe_credis::settlement_deadline(&record),
            at + DAY,
            "the deadline follows the sealed notice period"
        );
        let lapsed = at + DAY + CLOSED_DAY_DELAY;
        advance_to(&storage, lapsed);
        assert_eq!(expire(&storage, lapsed), 1);
        assert!(is_deleted(&storage, credis_id));
    });
    teardown();
}

/// A Credis carrying zero terms is uncallable, not callable on every day.
/// Zero is what a record sealed before the terms existed reads back, and
/// `breaches >= 0` would otherwise call the whole book on the next scan.
#[test]
fn a_credis_with_zero_call_terms_is_never_called() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let at = CREATED_AT + AFTER_WINDOW;
        let credis_id = open_with_series(&storage, at, CALL_LOOKBACK_DAYS, above_call());

        reterm(&storage, credis_id, 0, 0, 0);
        assert_eq!(
            scan(&storage, at),
            0,
            "a full breach window still does not call"
        );
        assert_eq!(state_of(&storage, credis_id), CredisState::Issued);
    });
    teardown();
}

/// A Credis whose sealed window outruns the live profile still gets its whole
/// span collected: the scan sizes the shared per-currency window off the
/// `max_call_window_seconds` high-water mark, not off the profile.
#[test]
fn a_window_wider_than_the_profile_is_collected_in_full() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        const WIDE_DAYS: u32 = 40;
        let at = CREATED_AT + (WIDE_DAYS as u64 + 5) * DAY;
        let credis_id = open(&storage, 1);
        advance_to(&storage, at);
        fill_days(&storage, last_closed_day(at), WIDE_DAYS, above_call());

        // 35 of the 40 days must breach, which no 28-day window can supply.
        reterm(&storage, credis_id, WIDE_DAYS, 35, 7);
        assert_eq!(scan(&storage, at), 1);
        assert_eq!(state_of(&storage, credis_id), CredisState::Called);
    });
    teardown();
}

#[test]
fn a_node_local_failure_while_calling_a_credis_fails_the_scan() {
    let mut storage = env();
    let at = CREATED_AT + AFTER_WINDOW;
    let credis_id = StorageHandle::enter(&mut storage, |storage| {
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
        assert_eq!(state_of(&storage, credis_id), CredisState::Issued);
    });
    teardown();
}

#[test]
fn a_full_window_above_the_call_price_calls_the_credis() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let at = CREATED_AT + AFTER_WINDOW;
        let credis_id = open_with_series(&storage, at, CALL_LOOKBACK_DAYS, above_call());

        assert_eq!(scan(&storage, at), 1);
        assert_eq!(state_of(&storage, credis_id), CredisState::Called);

        let record = CredisContract::new(storage.clone())
            .get_credis(credis_id)
            .unwrap();
        assert_eq!(record.called_at, at, "stamped with the run's timestamp");
        assert_eq!(
            outbe_credis::settlement_deadline(&record),
            at + NOTICE,
            "the settlement window opens at the call"
        );

        // Idempotent: a second run does not move the deadline.
        assert_eq!(scan(&storage, at), 0);
        assert_eq!(
            CredisContract::new(storage.clone())
                .get_credis(credis_id)
                .unwrap()
                .called_at,
            at
        );
    });
    teardown();
}

/// The breach test is strictly above the call price, as it is for Nod, Gem and
/// Intex. A window that closes exactly on the call price every single day must
/// therefore leave the Credis open.
#[test]
fn a_full_window_exactly_at_the_call_price_does_not_call_the_credis() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let at = CREATED_AT + AFTER_WINDOW;
        let credis_id = open_with_series(&storage, at, CALL_LOOKBACK_DAYS, at_call());

        assert_eq!(scan(&storage, at), 0);
        assert_eq!(state_of(&storage, credis_id), CredisState::Issued);

        // One minor unit higher on every day is a breach window.
        fill_days(
            &storage,
            last_closed_day(at + DAY),
            CALL_LOOKBACK_DAYS,
            above_call(),
        );
        assert_eq!(scan(&storage, at), 0, "finalized days cannot reopen");
        advance_to(&storage, at + DAY);
        assert_eq!(scan(&storage, at + DAY), 1);
        assert_eq!(state_of(&storage, credis_id), CredisState::Called);
    });
    teardown();
}

#[test]
fn the_window_absorbs_below_call_days_up_to_the_slack() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let at = CREATED_AT + AFTER_WINDOW;
        let credis_id = open_with_series(&storage, at, CALL_LOOKBACK_DAYS, above_call());

        // Scatter the full slack of below-call days through the window. The rule
        // counts breach days rather than requiring a run, so their Credis must
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
        assert_eq!(state_of(&storage, credis_id), CredisState::Called);
    });
    teardown();
}

#[test]
fn one_breach_day_short_of_the_threshold_does_not_call() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let at = CREATED_AT + AFTER_WINDOW;
        let credis_id = open_with_series(&storage, at, CALL_LOOKBACK_DAYS, above_call());

        // One more below-call day than the window can absorb.
        let below = CALL_LOOKBACK_DAYS - CALL_THRESHOLD_DAYS + 1;
        for i in 0..below {
            set_vwap(&storage, day_back(at, i + 1), below_call());
        }

        assert_eq!(scan(&storage, at), 0);
        assert_eq!(state_of(&storage, credis_id), CredisState::Issued);

        // Raising one of them back over the call price completes the threshold.
        set_vwap(&storage, day_back(at, 1), above_call());
        assert_eq!(
            scan(&storage, at),
            0,
            "completed observation day stays complete"
        );
        set_vwap(&storage, last_closed_day(at + DAY), above_call());
        advance_to(&storage, at + DAY);
        assert_eq!(scan(&storage, at + DAY), 1);
        assert_eq!(state_of(&storage, credis_id), CredisState::Called);
    });
    teardown();
}

#[test]
fn missing_days_do_not_count_as_breaches() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let at = CREATED_AT + AFTER_WINDOW;
        let credis_id = open(&storage, 1);
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
        assert_eq!(state_of(&storage, credis_id), CredisState::Issued);

        // The scenario reaches the threshold when it publishes the next day, although the rest of the window still has no price.
        set_vwap(&storage, last_closed_day(at + DAY), above_call());
        advance_to(&storage, at + DAY);
        assert_eq!(scan(&storage, at + DAY), 1);
        assert_eq!(state_of(&storage, credis_id), CredisState::Called);
    });
    teardown();
}

#[test]
fn a_breach_run_that_predates_the_credis_does_not_call_it() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        // The series is long and fully breached, but the Credis is 3 days old,
        // so the window reaches back before it existed.
        let at = CREATED_AT + 3 * DAY;
        let credis_id = open(&storage, 1);
        advance_to(&storage, at);
        fill_days(
            &storage,
            last_closed_day(at),
            CALL_LOOKBACK_DAYS + 10,
            above_call(),
        );

        assert_eq!(scan(&storage, at), 0);
        assert_eq!(state_of(&storage, credis_id), CredisState::Issued);

        // Once the Credis is old enough for the window to sit entirely after
        // its origination day, the call fires.
        let later = CREATED_AT + AFTER_WINDOW;
        advance_to(&storage, later);
        fill_days(
            &storage,
            last_closed_day(later),
            CALL_LOOKBACK_DAYS,
            above_call(),
        );
        // The daily triggers in between walked their own days.
        sweep_state(&storage)
            .call_latest_day
            .write(previous_date_key(last_closed_day(later)).expect("date step"))
            .unwrap();
        assert_eq!(scan(&storage, later), 1);
        assert_eq!(state_of(&storage, credis_id), CredisState::Called);
    });
    teardown();
}

/// A Credis opened at 00:00 counts its issuance day. One opened at 00:00:01 starts
/// counting the next day, so the same threshold run falls one day short.
#[test]
fn the_issuance_day_counts_only_for_a_credis_issued_at_midnight() {
    for (offset, called_at_threshold) in [(0, true), (1, false)] {
        let mut storage = env();
        StorageHandle::enter(&mut storage, |storage| {
            bootstrap(&storage, pledge_cost());
            let midnight = (CREATED_AT / DAY + 1) * DAY;
            advance_to(&storage, midnight + offset);
            let credis_id = open(&storage, 1);

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
                assert_eq!(state_of(&storage, credis_id), CredisState::Called);
                return;
            }
            assert_eq!(state_of(&storage, credis_id), CredisState::Issued);

            let next = at + DAY;
            advance_to(&storage, next);
            set_vwap(&storage, last_closed_day(next), above_call());
            assert_eq!(scan(&storage, next), 1);
            assert_eq!(state_of(&storage, credis_id), CredisState::Called);
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
        let credis_id = open_with_series(&storage, at, CALL_LOOKBACK_DAYS, above_call());

        // Rewind the watermark behind the last closed day: the oracle has not
        // closed it yet, so the run must skip rather than read it as missing.
        let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
        oracle
            .utc_day_vwap_last_finalized
            .write(previous_date_key(last_closed_day(at)).expect("date step"))
            .unwrap();

        assert_eq!(scan(&storage, at), 0);
        assert_eq!(state_of(&storage, credis_id), CredisState::Issued);
    });
    teardown();
}

#[test]
fn the_call_and_the_forfeit_compose_across_runs() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let at = CREATED_AT + AFTER_WINDOW;
        let credis_id = open_with_series(&storage, at, CALL_LOOKBACK_DAYS, above_call());
        let group = group_of(&storage, credis_id);

        assert_eq!(scan(&storage, at), 1);
        assert_eq!(state_of(&storage, credis_id), CredisState::Called);

        // The same block can never forfeit a Credis that it called: the
        // window opens at `called_at = now`.
        assert_eq!(expire(&storage, at), 0);

        // Inside the window, nothing happens.
        let inside = at + NOTICE - DAY;
        advance_to(&storage, inside);
        assert_eq!(expire(&storage, inside), 0);
        assert_eq!(state_of(&storage, credis_id), CredisState::Called);

        // The window lapses with the whole principal outstanding: the forfeit burns
        // the entire collateral and credits it to the Promis Reserve.
        let lapsed = at + NOTICE + CLOSED_DAY_DELAY;
        advance_to(&storage, lapsed);
        assert_eq!(expire(&storage, lapsed), 1);
        assert!(is_deleted(&storage, credis_id));
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
        assert_eq!(
            outbe_promislimit::PromisLimitContract::new(storage.clone())
                .promis_limit()
                .unwrap(),
            pledge_cost()
        );

        // The forfeit left the deadline queue and the cleanup closed the group.
        assert_eq!(queued_at(&storage, group), 0);
        assert_eq!(cleanup_queued_at(&storage, group), 0);
    });
    teardown();
}

#[test]
fn each_reference_currency_prices_off_its_own_daily_series() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let at = CREATED_AT + AFTER_WINDOW;
        let credis_id = open(&storage, 1);

        // Re-point the Credis's group at an unregistered currency, so it prices off a
        // series that does not exist, and index it there.
        {
            use outbe_primitives::call_bins;
            let credis = CredisContract::new(storage.clone());
            let record = credis.get_credis(credis_id).unwrap();
            let group = record.group_key().expect("date step");
            let bin = call_bins::price_to_bin(record.call_price_minor).unwrap();
            call_bins::remove(&CallBins(&credis, REFERENCE_ISO), group).unwrap();
            call_bins::insert(&CallBins(&credis, UNPRICED_ISO), group, bin).unwrap();
            credis.group_currency.write(&group, UNPRICED_ISO).unwrap();
        }

        // The seeded reference series is a full breach window, but this Credis is
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
        assert_eq!(state_of(&storage, credis_id), CredisState::Issued);
    });
    teardown();
}

/// The call is anchored to the reference currency, never to the issuance currency
/// the Credis is denominated in. This test checks both directions, because a
/// wrong anchor fails silently in one of them. With the two series moving
/// together, an issuance-keyed scan still reaches the right verdict by coincidence.
#[test]
fn the_call_follows_the_reference_series_and_ignores_the_issuance_one() {
    // Breach published only on the reference series -> the Credis is called.
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let at = CREATED_AT + AFTER_WINDOW;
        let credis_id = open(&storage, 1);
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
        assert_eq!(state_of(&storage, credis_id), CredisState::Called);
    });
    teardown();

    // Breach published only on the issuance series -> nothing happens.
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let at = CREATED_AT + AFTER_WINDOW;
        let credis_id = open(&storage, 1);

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
            "a breach in the issuance currency must not call the Credis"
        );
        assert_eq!(state_of(&storage, credis_id), CredisState::Issued);
    });
    teardown();
}

/// Opens one Credis for each of three distinct owners, each in a group of its own in
/// one call bin. Returns the ids in call-index order.
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

/// Opens a Credis on the fixture's own call anchor, which every such Credis of the
/// day shares a group on.
fn open_on_fixture_anchor(storage: &StorageHandle<'_>, owner: Address, nonce: u64) -> U256 {
    let reservation_id = seed_reservation(storage, owner, owner, pledge_stables());
    let router = outbe_vaultrouter::VaultRouterContract::new(storage.clone());
    let mut reservation = router.reservations.get(reservation_id).unwrap().unwrap();
    reservation.call_anchor_price_minor = oracle_rate();
    router.reservations.update(&reservation).unwrap();
    pledge(storage, owner, reservation_id, nonce);
    fund_stake(storage, pledge_stake());
    crate::runtime::issue_credis(storage.clone(), cca(), reservation_id, pledge_stake())
        .unwrap()
        .0
}

/// Groups of the reference currency's walk in flight still to visit in its bin.
fn cursor_of(storage: &StorageHandle<'_>) -> u32 {
    let packed = CredisContract::new(storage.clone())
        .call_bin_cursor
        .read(&REFERENCE_ISO)
        .unwrap();
    outbe_primitives::call_bins::unpack_cursor(packed).1
}

fn is_indexed(storage: &StorageHandle<'_>, group: B256) -> bool {
    CredisContract::new(storage.clone())
        .call_group_slot
        .read(&group)
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
                is_indexed(&storage, group_of(&storage, *id)),
                "an open group waits for its call"
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

/// Pins `day` as the sweep in flight, stopped with `remaining` groups of the
/// bin `sample` sits in still to visit.
fn pin_sweep(storage: &StorageHandle<'_>, day: u32, sample: U256, remaining: u32) {
    sweep_state(storage).call_sweep_day.write(day).unwrap();
    let credis = CredisContract::new(storage.clone());
    credis.call_latest_day.write(day).unwrap();
    credis.daily_genesis_day.write(day).unwrap();
    let price = credis.get_credis(sample).unwrap().call_price_minor;
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
        state.call_latest_day.read().unwrap(),
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
        // Two groups of the bin are left to visit: indices 1 and 0.
        pin_sweep(&storage, last_closed_day(at), ids[0], 2);

        assert_eq!(slice(&storage, at), 2);
        assert_eq!(state_of(&storage, ids[0]), CredisState::Called);
        assert_eq!(state_of(&storage, ids[1]), CredisState::Called);
        assert_eq!(
            state_of(&storage, ids[2]),
            CredisState::Issued,
            "the entry above the resume point waits for the next pass"
        );
        assert_eq!(cursor_of(&storage), 0);
        assert_eq!(
            sweep_days(&storage),
            (0, last_closed_day(at)),
            "the pass ended"
        );

        // The next pass starts fresh from the top and picks it up.
        assert_eq!(scan(&storage, at), 0, "a finished day does not reopen");
        set_vwap(&storage, last_closed_day(at + DAY), above_call());
        advance_to(&storage, at + DAY);
        assert_eq!(scan(&storage, at + DAY), 1);
        assert_eq!(state_of(&storage, ids[2]), CredisState::Called);
    });
    teardown();
}

#[test]
fn an_owner_forfeit_mid_pass_leaves_the_rest_of_the_bin_to_the_scan() {
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
        pin_sweep(&storage, last_closed_day(at), ids[0], 2);

        let group = group_of(&storage, ids[0]);
        crate::runtime::forfeit_by_owner(storage.clone(), alice(), ids[0]).unwrap();
        assert!(!is_indexed(&storage, group));

        assert_eq!(slice(&storage, at), 2);
        assert!(is_deleted(&storage, ids[0]));
        assert_eq!(state_of(&storage, ids[1]), CredisState::Called);
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
        assert_eq!(state_of(&storage, ids[2]), CredisState::Issued);

        assert_eq!(slice(&storage, next), 2, "the pinned day finishes first");
        assert_eq!(state_of(&storage, ids[2]), CredisState::Issued);
        assert_eq!(
            sweep_days(&storage),
            (last_closed_day(next), last_closed_day(next))
        );
        assert_eq!(cursor_of(&storage), 0);

        assert_eq!(slice(&storage, next), 1);
        assert_eq!(state_of(&storage, ids[2]), CredisState::Called);
        assert_eq!(sweep_days(&storage), (0, last_closed_day(next)));
    });
    teardown();
}

/// Days that closed while a pass was in flight are walked after it, in order, none
/// skipped.
#[test]
fn days_closed_behind_a_pass_in_flight_are_walked_in_order() {
    let mut provider = env();
    StorageHandle::enter(&mut provider, |storage| {
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
            .call_latest_day
            .write(last_closed_day(at))
            .unwrap();

        // Only the newest day's trigger fires.
        schedule(&storage, later);
        assert_eq!(
            sweep_days(&storage),
            (last_closed_day(at), last_closed_day(later))
        );
        assert_eq!(cursor_of(&storage), 2, "the pass in flight keeps its place");

        let mut walked = vec![last_closed_day(at)];
        while walked.last() != Some(&0) && walked.len() < 8 {
            slice(&storage, later);
            let day = sweep_days(&storage).0;
            if walked.last() != Some(&day) {
                walked.push(day);
            }
        }
        assert_eq!(
            walked,
            vec![
                last_closed_day(at),
                last_closed_day(at + DAY),
                last_closed_day(later),
                0
            ]
        );
    });
    teardown();
}

#[test]
fn an_empty_book_opens_no_sweep() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        let at = CREATED_AT + AFTER_WINDOW;
        advance_to(&storage, at);
        finalize_through(&storage, at);

        assert_eq!(scan(&storage, at), 0);
        assert_eq!(sweep_days(&storage), (0, last_closed_day(at)));
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
        assert_eq!(state_of(&storage, ids[1]), CredisState::Issued);
        assert_eq!(
            sweep_days(&storage),
            (last_closed_day(at), last_closed_day(at))
        );
        assert_eq!(cursor_of(&storage), 2);
    });
    teardown();
}

/// Calls the groups of `ids` by hand at `called_at`, which queues them on their deadline.
fn call_by_hand(storage: &StorageHandle<'_>, ids: &[U256], called_at: u64) {
    for id in ids {
        call_credis(storage, *id, called_at);
    }
}

fn queued_at(storage: &StorageHandle<'_>, group: B256) -> u64 {
    CredisContract::new(storage.clone())
        .called_group_slot
        .read(&group)
        .unwrap()
}

fn cleanup_queued_at(storage: &StorageHandle<'_>, group: B256) -> u64 {
    CredisContract::new(storage.clone())
        .cleanup_group_slot
        .read(&group)
        .unwrap()
}

#[test]
fn forfeiting_several_credis_in_one_block_skips_none() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        let ids = open_three(&storage);
        let groups: Vec<B256> = ids.iter().map(|id| group_of(&storage, *id)).collect();
        call_by_hand(&storage, &ids, CREATED_AT);

        let lapsed = CREATED_AT + NOTICE + CLOSED_DAY_DELAY;
        advance_to(&storage, lapsed);
        assert_eq!(
            expire(&storage, lapsed),
            3,
            "all three groups forfeited in one block"
        );

        for (id, group) in ids.iter().zip(&groups) {
            assert!(is_deleted(&storage, *id));
            assert_eq!(queued_at(&storage, *group), 0);
        }
    });
    teardown();
}

#[test]
fn a_forfeit_waits_for_the_day_its_deadline_falls_in_to_close() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        let ids = open_three(&storage);
        call_by_hand(&storage, &ids[..1], CREATED_AT);
        let deadline = CREATED_AT + NOTICE;
        let day_end = outbe_primitives::deadline_queue::day_end(
            outbe_primitives::deadline_queue::deadline_day(deadline).unwrap(),
        );
        assert!(
            day_end > deadline + 1,
            "the fixture deadline sits inside its day"
        );

        advance_to(&storage, deadline + 1);
        assert_eq!(expire(&storage, deadline + 1), 0);
        assert_eq!(state_of(&storage, ids[0]), CredisState::Called);

        advance_to(&storage, day_end);
        assert_eq!(expire(&storage, day_end), 1);
        assert!(is_deleted(&storage, ids[0]));
    });
    teardown();
}

#[test]
fn the_daily_pass_forfeits_without_the_daily_trigger() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        let ids = open_three(&storage);
        call_by_hand(&storage, &ids[..1], CREATED_AT);

        let lapsed = CREATED_AT + NOTICE + CLOSED_DAY_DELAY;
        advance_to(&storage, lapsed);
        daily_pass(&storage, lapsed).unwrap();
        assert!(is_deleted(&storage, ids[0]));
        assert_eq!(state_of(&storage, ids[1]), CredisState::Issued);
    });
    teardown();
}

#[test]
fn settling_a_called_credis_in_full_leaves_the_queue() {
    let mut storage = env();
    let (credis_id, outstanding) = StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let credis_id = open(&storage, 1);
        call_by_hand(&storage, &[credis_id], CREATED_AT);
        assert_ne!(
            queued_at(&storage, group_of(&storage, credis_id)),
            0,
            "a call queues the group"
        );

        let outstanding = CredisContract::new(storage.clone())
            .get_credis(credis_id)
            .unwrap()
            .outstanding_principal_minor;
        (credis_id, outstanding)
    });
    settle_principal(&mut storage, alice(), credis_id, outstanding);
    StorageHandle::enter(&mut storage, |storage| {
        assert_eq!(state_of(&storage, credis_id), CredisState::Settled);
        assert_eq!(queued_at(&storage, group_of(&storage, credis_id)), 0);

        let lapsed = CREATED_AT + NOTICE + CLOSED_DAY_DELAY;
        advance_to(&storage, lapsed);
        assert_eq!(expire(&storage, lapsed), 0);
        assert_eq!(
            CredisContract::new(storage.clone())
                .forfeit_tree_root
                .read()
                .unwrap(),
            U256::ZERO,
            "no day is left waiting"
        );
    });
    teardown();
}

/// A group is forfeited at once, and its cleanup deletes what the block's write budget
/// leaves, burning each Credis's Gratis into the reserve, and leaves the rest to the next block.
#[test]
fn the_cleanup_budget_bounds_one_block_and_the_next_block_drains_the_rest() {
    let mut provider = env();
    let budget = outbe_primitives::pass_budget::SWEEP_WRITES_PER_BLOCK;
    let total = budget + 1;
    StorageHandle::enter(&mut provider, |storage| {
        bootstrap_for(&storage, alice(), pledge_cost() * U256::from(total));
    });
    // Identical owner/CCA/asset tuples in successive blocks of one day share one group.
    let ids: Vec<U256> = (1..=u64::from(total))
        .map(|nonce| {
            provider.set_block_number(BLOCK_NUMBER + nonce);
            StorageHandle::enter(&mut provider, |storage| {
                open_on_fixture_anchor(&storage, alice(), nonce)
            })
        })
        .collect();
    StorageHandle::enter(&mut provider, |storage| {
        let group = group_of(&storage, ids[0]);
        assert!(ids.iter().all(|id| group_of(&storage, *id) == group));
        call_by_hand(&storage, &ids[..1], CREATED_AT);
        let lapsed = CREATED_AT + NOTICE + CLOSED_DAY_DELAY;
        advance_to(&storage, lapsed);

        // The pass is caught up to the day before, so one block works the deadline day.
        daily_pass(&storage, lapsed - DAY).unwrap();
        // Each deleted Credis burns its Gratis into the reserve as it goes.
        let reserve = || {
            outbe_promislimit::PromisLimitContract::new(storage.clone())
                .promis_limit()
                .unwrap()
        };
        assert_eq!(expire(&storage, lapsed), 1);
        let deleted = ids.iter().filter(|id| is_deleted(&storage, **id)).count();
        assert!(
            deleted > 0 && deleted < ids.len(),
            "one block deletes what its write budget leaves after the forfeit, got {deleted}"
        );
        assert_eq!(reserve(), pledge_cost() * U256::from(deleted as u64));
        assert_eq!(expire(&storage, lapsed), 0);
        assert!(ids.iter().all(|id| is_deleted(&storage, *id)));
        assert_eq!(cleanup_queued_at(&storage, group), 0);
        assert_eq!(reserve(), pledge_cost() * U256::from(total));
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
    });
    teardown();
}

/// A cleanup that cannot burn on this node fails the block: the forfeit rolls back with it,
/// and the Credis stays queued until a pass with the enclave.
#[test]
fn a_failed_cleanup_fails_the_block_and_keeps_the_credis_queued() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        let ids = open_three(&storage);
        let group = group_of(&storage, ids[0]);
        call_by_hand(&storage, &ids[..1], CREATED_AT);
        let lapsed = CREATED_AT + NOTICE + CLOSED_DAY_DELAY;
        advance_to(&storage, lapsed);

        outbe_gratis::enclave_client::test_enclave::uninstall();
        assert!(matches!(
            daily_pass(&storage, lapsed),
            Err(outbe_primitives::error::PrecompileError::EnclaveUnavailable(_))
        ));
        outbe_gratis::enclave_client::test_enclave::install();
        assert!(!is_deleted(&storage, ids[0]));
        assert_eq!(queued_for_forfeit(&storage, lapsed).unwrap(), 1);
        assert_eq!(cleanup_queued_at(&storage, group), 0);

        assert_eq!(expire(&storage, lapsed), 1);
        assert!(is_deleted(&storage, ids[0]));
    });
    teardown();
}

/// A failure every node hits alike, here a broken pledged-supply total, defers the cleanup.
#[test]
fn a_deterministic_cleanup_failure_defers_the_group() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        let ids = open_three(&storage);
        let group = group_of(&storage, ids[0]);
        call_by_hand(&storage, &ids[..1], CREATED_AT);
        let lapsed = CREATED_AT + NOTICE + CLOSED_DAY_DELAY;
        advance_to(&storage, lapsed);

        outbe_gratis::schema::Gratis::new(storage.clone())
            .pledged_total_supply
            .write(U256::ZERO)
            .unwrap();
        assert_eq!(expire(&storage, lapsed), 1);
        assert!(!is_deleted(&storage, ids[0]));
        let (day, _) =
            outbe_primitives::deadline_queue::unpack_slot(cleanup_queued_at(&storage, group));
        assert_eq!(
            day,
            outbe_primitives::deadline_queue::deadline_day(lapsed).unwrap()
        );
    });
    teardown();
}

/// This fixture creates three called Credis in one group and advances past the day that contains their deadline.
fn lapsed_group_of_three() -> (
    outbe_primitives::storage::hashmap::HashMapStorageProvider,
    Vec<U256>,
) {
    let mut provider = env();
    StorageHandle::enter(&mut provider, |storage| {
        bootstrap_for(&storage, alice(), pledge_cost() * U256::from(3u64));
    });
    let ids: Vec<U256> = (1..=3u64)
        .map(|nonce| {
            provider.set_block_number(BLOCK_NUMBER + nonce);
            StorageHandle::enter(&mut provider, |storage| {
                open_on_fixture_anchor(&storage, alice(), nonce)
            })
        })
        .collect();
    StorageHandle::enter(&mut provider, |storage| {
        call_by_hand(&storage, &ids[..1], CREATED_AT);
        advance_to(&storage, CREATED_AT + NOTICE + CLOSED_DAY_DELAY);
    });
    provider.clear_mutation_failure();
    (provider, ids)
}

#[test]
fn the_daily_pass_finishes_only_after_atomic_pledge_burns_and_credits() {
    use outbe_primitives::pass_budget::BlockAllowance;

    let (mut provider, ids) = lapsed_group_of_three();
    StorageHandle::enter(&mut provider, |storage| {
        let at = CREATED_AT + NOTICE + CLOSED_DAY_DELAY;
        finalize_through(&storage, at);
        let ctx = block_at(&storage, at);
        let credis = CredisContract::new(storage.clone());
        let day = last_closed_day(at);
        let mut allowance = BlockAllowance::new(ctx.block.block_number, day);
        assert!(allowance.budget.admit_writes(62));
        credis.daily_budget.write(allowance.pack()).unwrap();
        let reserve = || {
            outbe_promislimit::PromisLimitContract::new(storage.clone())
                .promis_limit()
                .unwrap()
        };
        let before = reserve();

        assert_eq!(crate::called::process_call_pass(&ctx).unwrap(), 0);
        assert_eq!(
            credis.daily_phase.read().unwrap(),
            2,
            "the group marker is not economic completion"
        );
        assert_eq!(credis.call_sweep_day.read().unwrap(), day);
        assert_eq!(
            credis
                .group_member_count(group_of(&storage, ids[0]))
                .unwrap(),
            2
        );
        assert_eq!(reserve() - before, pledge_cost());
        assert_eq!(
            view_pledged(&storage, alice()),
            pledge_cost() * U256::from(2)
        );
        let saved = credis.daily_budget.read().unwrap();
        assert_eq!(crate::called::process_call_pass(&ctx).unwrap(), 0);
        let before_allowance = BlockAllowance::unpack(saved).unwrap().unwrap();
        let after_allowance = BlockAllowance::unpack(credis.daily_budget.read().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(after_allowance.height, before_allowance.height);
        assert_eq!(after_allowance.day, before_allowance.day);
        assert_eq!(after_allowance.budget.writes_left(), 0);
        assert!(after_allowance.budget.visits_left() <= before_allowance.budget.visits_left());
        assert_eq!(
            reserve() - before,
            pledge_cost(),
            "same-height retry cannot burn or credit again"
        );

        let next = block_at(&storage, at);
        assert_eq!(crate::called::process_call_pass(&next).unwrap(), 0);
        assert_eq!(credis.call_sweep_day.read().unwrap(), 0);
        assert_eq!(reserve() - before, pledge_cost() * U256::from(3));
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
        for id in &ids {
            assert!(is_deleted(&storage, *id));
        }
        assert_eq!(crate::called::process_call_pass(&next).unwrap(), 0);
        assert_eq!(reserve() - before, pledge_cost() * U256::from(3));
    });
    teardown();
}

/// The daily pass, with its forfeits and cleanup, under one checkpoint as the native
/// transaction runs it. Returns the groups forfeited.
fn forfeit_block(
    storage: &StorageHandle<'_>,
    timestamp: u64,
) -> outbe_primitives::error::Result<u32> {
    storage.with_checkpoint(|| {
        let before = queued_for_forfeit(storage, timestamp)?;
        daily_pass(storage, timestamp)?;
        Ok(before - queued_for_forfeit(storage, timestamp)?)
    })
}

/// Every write of a group's forfeit and of its cleanup (CCA default, pledge burn, credit,
/// deletion, events) rolls back with the block. The retry credits the pledged Gratis once,
/// and leaves the state a clean run leaves.
#[test]
fn every_credis_forfeit_and_cleanup_mutation_rolls_back_then_retries_once() {
    use alloy_sol_types::SolEvent;
    use outbe_primitives::addresses::{CREDIS_ADDRESS, GRATIS_ADDRESS};

    let lapsed = CREATED_AT + NOTICE + CLOSED_DAY_DELAY;
    let expected = pledge_cost() * U256::from(3u64);
    let reserve = |storage: &StorageHandle<'_>| {
        outbe_promislimit::PromisLimitContract::new(storage.clone())
            .promis_limit()
            .unwrap()
    };

    let (mut clean, ids) = lapsed_group_of_three();
    let group = StorageHandle::enter(&mut clean, |storage| group_of(&storage, ids[0]));
    clean.fail_after_mutation_at(usize::MAX);
    StorageHandle::enter(&mut clean, |storage| {
        assert_eq!(forfeit_block(&storage, lapsed).unwrap(), 1);
    });
    let mutations = clean.clear_mutation_failure();
    assert!(mutations > 0);
    StorageHandle::enter(&mut clean, |storage| {
        assert_eq!(
            forfeit_block(&storage, lapsed + CLOSED_DAY_DELAY).unwrap(),
            0
        );
    });
    let count = |provider: &outbe_primitives::storage::hashmap::HashMapStorageProvider| {
        let credis = provider.get_events(CREDIS_ADDRESS);
        let groups = credis
            .iter()
            .filter_map(|log| {
                outbe_credis::precompile::ICredis::CredisGroupForfeited::decode_log_data(log).ok()
            })
            .map(|event| (event.groupKey, event.credis))
            .collect::<Vec<_>>();
        let forfeited = credis
            .iter()
            .filter_map(|log| {
                outbe_credis::precompile::ICredis::CredisForfeited::decode_log_data(log).ok()
            })
            .count();
        let burned_tokens = credis
            .iter()
            .filter_map(|log| {
                outbe_credis::precompile::ICredis::Transfer::decode_log_data(log).ok()
            })
            .filter(|event| event.to == Address::ZERO)
            .count();
        let burned_gratis = provider
            .get_events(GRATIS_ADDRESS)
            .iter()
            .filter_map(|log| {
                outbe_gratis::precompile::IGratis::GratisBurned::decode_log_data(log).ok()
            })
            .count();
        (groups, forfeited, burned_tokens, burned_gratis)
    };
    assert_eq!(count(&clean), (vec![(group, 3)], 3, 3, 3));
    StorageHandle::enter(&mut clean, |storage| {
        assert_eq!(reserve(&storage), expected);
        let credis = CredisContract::new(storage.clone());
        assert!(ids.iter().all(|id| is_deleted(&storage, *id)));
        assert_eq!(credis.credis_count_of(alice()).unwrap(), 0);
        assert_eq!(credis.group_member_count(group).unwrap(), 0);
        assert_eq!(queued_at(&storage, group), 0);
        assert_eq!(cleanup_queued_at(&storage, group), 0);
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
        assert_eq!(
            outbe_gratis::api::pledged_total_supply(storage.clone()).unwrap(),
            U256::ZERO
        );
        for id in &ids {
            assert_eq!(
                outbe_gratisfactory::api::collateral_of(&storage, *id)
                    .unwrap()
                    .remaining_minor,
                U256::ZERO
            );
        }
    });

    for operation in 0..mutations {
        let (mut provider, _) = lapsed_group_of_three();
        let storage_before = provider.storage.clone();
        let events_before = provider.events.clone();
        let ordered_before = provider.get_ordered_events().to_vec();
        provider.fail_after_mutation_at(operation);
        let failed = StorageHandle::enter(&mut provider, |storage| forfeit_block(&storage, lapsed));
        assert!(
            failed.is_err(),
            "mutation {operation} unexpectedly succeeded"
        );
        assert_eq!(provider.clear_mutation_failure(), operation + 1);
        assert_eq!(provider.storage, storage_before, "storage at {operation}");
        assert_eq!(provider.events, events_before, "events at {operation}");
        assert_eq!(
            provider.get_ordered_events(),
            ordered_before.as_slice(),
            "ordered events at {operation}"
        );

        StorageHandle::enter(&mut provider, |storage| {
            assert_eq!(reserve(&storage), U256::ZERO);
            assert_eq!(forfeit_block(&storage, lapsed).unwrap(), 1);
            assert_eq!(reserve(&storage), expected);
            assert_eq!(
                forfeit_block(&storage, lapsed + CLOSED_DAY_DELAY).unwrap(),
                0
            );
            assert_eq!(reserve(&storage), expected);
        });
        assert_eq!(
            count(&provider),
            count(&clean),
            "events after retry at {operation}"
        );
        assert_eq!(
            provider.storage, clean.storage,
            "state after retry at {operation}"
        );
    }
    teardown();
}
