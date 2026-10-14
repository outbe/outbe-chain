//! The daily call scan: bucket bins, slices, cursors and the day queue. The scan
//! reads only compact EVM state; the bodies are untouched until a forfeit.

use alloy_primitives::U256;
use alloy_sol_types::SolEvent;
use outbe_oracle::schema::OracleContract;
use outbe_primitives::addresses::GEM_ADDRESS;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::sweep_budget::{SweepBudget, SWEEP_VISITS_PER_BLOCK, SWEEP_WRITES_PER_BLOCK};
use outbe_primitives::time::{date_key_to_utc_timestamp, previous_date_key, timestamp_to_date_key};

use super::*;
use crate::called::{run_call_slice, run_daily, scan_and_call};
use outbe_gem::config::GemParams;
use outbe_gem::schema::{GemContract, GemState};

/// Every day of the window closed at `vwap` on `pair`, finalized through yesterday.
fn window_at(storage: &StorageHandle, pair: u32, vwap: U256) -> u32 {
    let last_closed_day = previous_date_key(timestamp_to_date_key(T_NOW));
    priced_window(storage, pair, last_closed_day, vwap);
    last_closed_day
}

fn full_window(last_closed_day: u32, vwap: U256) -> outbe_oracle::call_window::CallWindow {
    outbe_oracle::call_window::CallWindow::from_vwaps(
        vec![(last_closed_day, Some(vwap)); (outbe_gem::constants::CALL_WINDOW / 86_400) as usize],
        outbe_gem::constants::CALL_THRESHOLD / 86_400,
    )
}

fn walk(
    storage: &StorageHandle,
    window: &outbe_oracle::call_window::CallWindow,
    budget: u32,
) -> (u32, bool) {
    let ctx = block_ctx(storage);
    let mut budget = SweepBudget::new(budget, u32::MAX, 0);
    crate::called::call_currency(
        &ctx,
        USD,
        window,
        outbe_primitives::math::constants::MAX_BIN_ID,
        &mut budget,
    )
    .unwrap()
}

/// The call-price bins mix currencies, so the call scan must read each gem's
/// breaches off its own `COEN/<iso>` VWAP window.
#[test]
fn call_scan_reads_each_gem_own_pair_window() {
    with_storage(|storage, world| {
        let usd_id = mature_gem(storage, world);
        let mut p = sample_params(BOB);
        p.reference_currency = EUR;
        p.issued_at = T_NOW - 100 * 86_400;
        let eur_id = add(storage, world, p);

        let rate = U256::from(600_000u64);
        seed_currency(storage, USD, Some(rate));
        let eur_pair = seed_currency(storage, EUR, Some(rate));

        // Only the EUR pair breaches: the USD pair has no published VWAPs.
        let breach = view(storage, world, eur_id).terms.call_price_minor + U256::from(1u64);
        let oracle = OracleContract::new(storage.clone());
        let last_closed_day = previous_date_key(timestamp_to_date_key(T_NOW));
        let mut day = last_closed_day;
        for _ in 0..(outbe_gem::constants::CALL_THRESHOLD / 86_400) {
            oracle.record_utc_day_vwap(day, eur_pair, breach).unwrap();
            day = previous_date_key(day);
        }
        oracle
            .utc_day_vwap_last_finalized
            .write(last_closed_day)
            .unwrap();

        assert_eq!(scan_and_call(&block_ctx(storage)).unwrap(), 1);
        assert_eq!(gem_state(storage, world, eur_id), GemState::Called as u8);
        assert_eq!(gem_state(storage, world, usd_id), GemState::Issued as u8);
    });
}

#[test]
fn the_call_pass_resumes_from_its_bin_cursor() {
    with_storage(|storage, world| {
        let issued_at = T_NOW - 100 * 86_400;
        let low_id = callable_gem_of(storage, world, USD, 0, issued_at, U256::from(100_000u64));
        let high_id = callable_gem_of(storage, world, USD, 1, issued_at, U256::from(120_000u64));

        // Every day of the window sits above both call prices.
        let pair = seed_currency(storage, USD, Some(U256::from(600_000u64)));
        let last_closed_day = window_at(storage, pair, U256::from(300_000u64));
        let window = full_window(last_closed_day, U256::from(300_000u64));

        // A budget of one takes the lower bin and persists the cursor above it.
        assert_eq!(walk(storage, &window, 1), (1, false));
        assert_eq!(gem_state(storage, world, low_id), GemState::Called as u8);
        assert_eq!(gem_state(storage, world, high_id), GemState::Issued as u8);

        assert_eq!(walk(storage, &window, 8), (1, true));
        assert_eq!(gem_state(storage, world, high_id), GemState::Called as u8);
    });
}

/// The daily trigger opens a sweep and closes it once nothing is left to walk.
/// With no sweep open, a block costs nothing and calls nobody.
#[test]
fn a_finished_sweep_closes_itself_and_idle_blocks_do_nothing() {
    with_storage(|storage, world| {
        let issued_at = T_NOW - 100 * 86_400;
        let first_id = callable_gem_of(storage, world, USD, 0, issued_at, U256::from(100_000u64));
        let pair = seed_currency(storage, USD, Some(U256::from(600_000u64)));
        window_at(storage, pair, U256::from(300_000u64));

        let ctx = block_ctx(storage);
        scan_and_call(&ctx).unwrap();
        let gem = GemContract::new(storage.clone());
        assert_eq!(gem_state(storage, world, first_id), GemState::Called as u8);
        assert_eq!(gem.call_sweep_day.read().unwrap(), 0);

        // A gem that breaches just as hard is left alone: no sweep is open.
        let second_id = callable_gem_of(storage, world, USD, 1, issued_at, U256::from(100_000u64));
        assert_eq!(run_call_slice(&ctx).unwrap(), 0);
        assert_eq!(gem_state(storage, world, second_id), GemState::Issued as u8);
    });
}

/// A bin holding more buckets than the budget resumes inside itself on the next slice.
#[test]
fn a_bin_wider_than_the_budget_resumes_inside_it() {
    with_storage(|storage, world| {
        let issued_at = T_NOW - 100 * 86_400;
        let ids = [
            callable_gem_of(storage, world, USD, 0, issued_at, U256::from(100_000u64)),
            callable_gem_of(storage, world, USD, 1, issued_at, U256::from(100_000u64)),
        ];
        let bin = |id: U256| {
            outbe_primitives::call_bins::price_to_bin(
                view(storage, world, id).terms.call_price_minor,
            )
            .unwrap()
        };
        assert_eq!(bin(ids[0]), bin(ids[1]), "one bin, two buckets");
        let last_closed_day = previous_date_key(timestamp_to_date_key(T_NOW));
        let window = full_window(last_closed_day, U256::from(300_000u64));

        assert_eq!(walk(storage, &window, 1), (1, false));
        let called = ids
            .iter()
            .filter(|id| gem_state(storage, world, **id) == GemState::Called as u8)
            .count();
        assert_eq!(called, 1);
        assert_eq!(
            GemContract::new(storage.clone())
                .call_bin_cursor
                .read(&USD)
                .unwrap(),
            (u64::from(bin(ids[0])) << 32) | 1
        );

        assert_eq!(walk(storage, &window, 8), (1, true));
        for id in ids {
            assert_eq!(gem_state(storage, world, id), GemState::Called as u8);
        }
    });
}

#[test]
fn call_then_forfeit_lifecycle() {
    with_storage(|storage, world| {
        let gem_id = mature_gem(storage, world);
        let threshold = view(storage, world, gem_id).terms.call_price_minor;
        let breach_days = (outbe_gem::constants::CALL_THRESHOLD / 86_400) as usize;
        let window = breach_window(T_NOW, threshold + U256::from(1u64), breach_days);

        let mut gem = GemContract::new(storage.clone());
        assert!(crate::called::trigger_bucket_call(
            &mut gem,
            &window,
            bucket_of(storage, world, gem_id),
            T_NOW
        )
        .unwrap());
        let called = view(storage, world, gem_id);
        assert_eq!(called.effective_state(T_NOW), GemState::Called as u8);
        assert_eq!(called.called_at(), T_NOW);

        // Within the 7-day notice period: no forfeit.
        begin_block_at(storage, world, T_NOW + 6 * 86_400);
        assert!(item(storage, world, gem_id).is_some());
        // Past the deadline: forfeit-burned.
        begin_block_at(
            storage,
            world,
            GemContract::hour_end(GemContract::deadline_hour(T_NOW + 7 * 86_400)),
        );
        assert!(item(storage, world, gem_id).is_none());
    });
}

#[test]
fn the_issue_day_counts_only_for_a_gem_issued_at_midnight() {
    let threshold_days = (outbe_gem::constants::CALL_THRESHOLD / 86_400) as usize;
    // Oldest day of a breach run that is exactly threshold-long.
    let mut oldest_breach = timestamp_to_date_key(T_NOW);
    for _ in 1..threshold_days {
        oldest_breach = previous_date_key(oldest_breach);
    }
    let midnight = date_key_to_utc_timestamp(oldest_breach);

    // A second past midnight loses the day and falls one breach short.
    for (issued_at, expected) in [(midnight, true), (midnight + 1, false)] {
        with_storage(|storage, world| {
            set_profile(storage, outbe_gem::config::PROFILE_PROD);
            let mut p = sample_params(ALICE);
            p.issued_at = issued_at;
            let gem_id = add(storage, world, p);
            let threshold = view(storage, world, gem_id).terms.call_price_minor;
            let window = breach_window(T_NOW, threshold + U256::from(1u64), threshold_days);

            let mut gem = GemContract::new(storage.clone());
            assert_eq!(
                crate::called::trigger_bucket_call(
                    &mut gem,
                    &window,
                    bucket_of(storage, world, gem_id),
                    T_NOW
                )
                .unwrap(),
                expected,
                "issued at {issued_at}"
            );
        });
    }
}

/// The breach count of a bucket starts on the first member's first full day, so a
/// later member issued the same day does not move it.
#[test]
fn second_member_with_a_different_issued_at_joins_the_first_members_bucket() {
    let threshold_days = (outbe_gem::constants::CALL_THRESHOLD / 86_400) as usize;
    let mut oldest_breach = timestamp_to_date_key(T_NOW);
    for _ in 1..threshold_days {
        oldest_breach = previous_date_key(oldest_breach);
    }
    let midnight = date_key_to_utc_timestamp(oldest_breach);
    with_storage(|storage, world| {
        set_profile(storage, outbe_gem::config::PROFILE_PROD);
        let mut first = sample_params(ALICE);
        first.issued_at = midnight;
        let first = add(storage, world, first);
        let mut second = sample_params(BOB);
        second.issued_at = midnight + 3_600;
        let second = add(storage, world, second);
        assert_eq!(
            bucket_of(storage, world, first),
            bucket_of(storage, world, second)
        );

        let threshold = view(storage, world, first).terms.call_price_minor;
        let window = breach_window(T_NOW, threshold + U256::from(1u64), threshold_days);
        let mut gem = GemContract::new(storage.clone());
        assert!(
            crate::called::trigger_bucket_call(
                &mut gem,
                &window,
                bucket_of(storage, world, first),
                T_NOW
            )
            .unwrap(),
            "the bucket counts from its first member's midnight"
        );
        assert_eq!(gem_state(storage, world, second), GemState::Called as u8);
    });
}

#[test]
fn call_skips_below_threshold() {
    with_storage(|storage, world| {
        let gem_id = mature_gem(storage, world);
        let threshold = view(storage, world, gem_id).terms.call_price_minor;
        // One below the threshold: not enough breach-days to force a call.
        let breach_days = (outbe_gem::constants::CALL_THRESHOLD / 86_400) as usize - 1;
        let window = breach_window(T_NOW, threshold + U256::from(1u64), breach_days);

        let mut gem = GemContract::new(storage.clone());
        assert!(!crate::called::trigger_bucket_call(
            &mut gem,
            &window,
            bucket_of(storage, world, gem_id),
            T_NOW
        )
        .unwrap());
        assert_eq!(gem_state(storage, world, gem_id), GemState::Issued as u8);
    });
}

#[test]
fn a_registry_edit_does_not_move_the_cursor_onto_another_currency() {
    use outbe_primitives::daily_sweep::currency_position;
    let currencies = [USD, EUR];
    assert_eq!(
        currency_position(&currencies, u32::from(EUR)),
        1,
        "the cursor names a currency, not a slot"
    );
    assert_eq!(
        currency_position(&currencies[1..], u32::from(EUR)),
        0,
        "dropping the currency ahead of it does not shift the cursor onto a stranger"
    );
    assert_eq!(
        currency_position(&currencies, 392),
        0,
        "a currency the registry no longer carries restarts at the head"
    );
}

#[test]
fn a_wider_window_than_the_live_profile_widens_the_span_the_scan_collects() {
    with_storage(|storage, world| {
        let gem = GemContract::new(storage.clone());
        let iso = sample_params(ALICE).reference_currency;

        set_profile(storage, outbe_gem::config::PROFILE_DEV);
        add(storage, world, sample_params(ALICE));
        assert_eq!(
            gem.max_call_window_seconds.read(&iso).unwrap(),
            GemParams::DEV.call_window_seconds
        );

        // A new bucket is sealed on the wider profile.
        set_profile(storage, outbe_gem::config::PROFILE_PROD);
        let mut other = sample_params(BOB);
        other.entry_price_minor = U256::from(600_000u64);
        add(storage, world, other);
        assert_eq!(
            gem.max_call_window_seconds.read(&iso).unwrap(),
            GemParams::PROD.call_window_seconds,
            "a wider profile widens the span"
        );

        set_profile(storage, outbe_gem::config::PROFILE_DEV);
        let mut third = sample_params(ALICE);
        third.entry_price_minor = U256::from(700_000u64);
        third.promis_load_minor = U256::from(2_000_000u64);
        add(storage, world, third);
        assert_eq!(
            gem.max_call_window_seconds.read(&iso).unwrap(),
            GemParams::PROD.call_window_seconds,
            "going back to the narrow one does not shrink it"
        );
    });
}

#[test]
fn an_unindexable_price_skips_its_currency_for_the_day_and_says_so() {
    let (mut provider, world) = provider_at(T_NOW);
    let pinned_day = StorageHandle::enter(&mut provider, |storage| {
        let gem_id = mature_gem(&storage, &world);
        // A price no bin can hold, on enough days to be the window's ceiling. A
        // begin-block error would fail the whole block.
        let pair = seed_currency(&storage, USD, Some(U256::from(600_000u64)));
        let oracle = OracleContract::new(storage.clone());
        let last_closed_day = previous_date_key(timestamp_to_date_key(T_NOW));
        let mut day = last_closed_day;
        for _ in 0..outbe_gem::constants::CALL_THRESHOLD / 86_400 {
            oracle.record_utc_day_vwap(day, pair, U256::MAX).unwrap();
            day = previous_date_key(day);
        }
        oracle
            .utc_day_vwap_last_finalized
            .write(last_closed_day)
            .unwrap();

        let ctx = block_ctx(&storage);
        assert_eq!(scan_and_call(&ctx).unwrap(), 0);
        assert_eq!(
            gem_state(&storage, &world, gem_id),
            GemState::Issued as u8,
            "the gem is untouched, not lost"
        );
        let gem = GemContract::new(storage.clone());
        assert_eq!(
            gem.call_scan_failed_day.read(&USD).unwrap(),
            last_closed_day,
            "the currency is marked for the day it failed on"
        );

        // Another slice of the same pass skips it instead of re-reading the window.
        gem.call_sweep_day.write(last_closed_day).unwrap();
        assert_eq!(run_call_slice(&ctx).unwrap(), 0);
        last_closed_day
    });

    let skipped: Vec<_> = provider
        .get_events(GEM_ADDRESS)
        .iter()
        .filter_map(|log| IGem::CallScanSkipped::decode_log_data(log).ok())
        .collect();
    assert_eq!(skipped.len(), 1, "one event for the day, not one per slice");
    assert_eq!(skipped[0].referenceCurrency, USD);
    assert_eq!(skipped[0].utcDay, pinned_day);
}

/// Gem ids carry no order, so a slice refreshes the whole range once, however many
/// buckets it called.
#[test]
fn a_call_slice_refreshes_all_metadata_once() {
    let (mut provider, world) = provider_at(T_NOW);
    StorageHandle::enter(&mut provider, |storage| {
        set_profile(&storage, outbe_gem::config::PROFILE_PROD);
        let pair = seed_currency(&storage, USD, Some(U256::from(600_000u64)));
        for nonce in 0..3 {
            callable_gem_of(
                &storage,
                &world,
                USD,
                nonce,
                T_NOW - 100 * 86_400,
                U256::from(100_000u64),
            );
        }
        window_at(&storage, pair, U256::from(300_000u64));
        assert_eq!(scan_and_call(&block_ctx_at(&storage, T_NOW)).unwrap(), 3);
    });
    let events = provider.get_events(GEM_ADDRESS);
    let batches: Vec<(U256, U256)> = events
        .iter()
        .filter_map(|log| IGem::BatchMetadataUpdate::decode_log_data(log).ok())
        .map(|event| (event._fromTokenId, event._toTokenId))
        .collect();
    assert_eq!(batches, vec![(U256::ZERO, U256::MAX)]);
    let called = events
        .iter()
        .filter(|log| IGem::GemBucketCalled::decode_log_data(log).is_ok())
        .count();
    assert_eq!(called, 3);
}

/// A trigger that finds the sweep unfinished queues its day rather than restarting
/// the walk, so the day in flight still reaches every bin against its own prices.
#[test]
fn a_trigger_during_a_running_call_sweep_queues_its_day() {
    with_storage(|storage, world| {
        let gem = GemContract::new(storage.clone());
        set_profile(storage, outbe_gem::config::PROFILE_PROD);
        let pair = seed_currency(storage, USD, Some(U256::from(600_000u64)));
        let issued_at = T_NOW - 100 * 86_400;
        // A bin whose calls spend the whole write budget, and one gem priced above it.
        for nonce in 0..u64::from(SWEEP_WRITES_PER_BLOCK) {
            callable_gem_of(
                storage,
                world,
                USD,
                nonce,
                issued_at,
                U256::from(100_000u64),
            );
        }
        // Priced above the bin that spends the budget, still under the window's ceiling.
        let above = callable_gem_of(storage, world, USD, 999, issued_at, U256::from(120_000u64));

        let day = window_at(storage, pair, U256::from(300_000u64));
        scan_and_call(&block_ctx_at(storage, T_NOW)).unwrap();
        let cursor = gem.call_bin_cursor.read(&USD).unwrap();
        assert_ne!(cursor, 0, "the first slice gave out inside the range");

        let next_ts = T_NOW + 86_400;
        let next_day = previous_date_key(timestamp_to_date_key(next_ts));
        priced_window(storage, pair, next_day, U256::from(300_000u64));
        let next = block_ctx_at(storage, next_ts);
        run_daily(&next).unwrap();
        assert_eq!(gem.call_sweep_day.read().unwrap(), day);
        assert_eq!(gem.call_pending_day.read().unwrap(), next_day);
        assert_eq!(
            gem.call_bin_cursor.read(&USD).unwrap(),
            cursor,
            "the walk in flight was not restarted"
        );

        // The next slice finishes the old day and hands the sweep to the queued one.
        run_call_slice(&next).unwrap();
        assert_eq!(gem_state(storage, world, above), GemState::Called as u8);
        assert_eq!(gem.call_sweep_day.read().unwrap(), next_day);
        assert_eq!(gem.call_pending_day.read().unwrap(), 0);
    });
}

/// With a day already waiting, a newer one takes its place, and the day that will
/// never be walked is named.
#[test]
fn a_newer_day_pushes_out_the_waiting_call_day_and_names_it() {
    let (mut provider, world) = provider_at(T_NOW);
    let (in_flight, skipped) = StorageHandle::enter(&mut provider, |storage| {
        set_profile(&storage, outbe_gem::config::PROFILE_PROD);
        let pair = seed_currency(&storage, USD, Some(U256::from(600_000u64)));
        for nonce in 0..=u64::from(SWEEP_VISITS_PER_BLOCK) {
            callable_gem_of(
                &storage,
                &world,
                USD,
                nonce,
                T_NOW - 100 * 86_400,
                U256::from(100_000u64),
            );
        }
        let mut closed = Vec::new();
        for offset in 0..3u64 {
            let ts = T_NOW + offset * 86_400;
            let day = previous_date_key(timestamp_to_date_key(ts));
            priced_window(&storage, pair, day, U256::from(300_000u64));
            // Only the first trigger's block walks: the later days arrive meanwhile.
            let ctx = block_ctx_at(&storage, ts);
            match offset {
                0 => scan_and_call(&ctx).map(drop),
                _ => run_daily(&ctx),
            }
            .unwrap();
            closed.push(day);
        }
        let gem = GemContract::new(storage.clone());
        assert_eq!(gem.call_sweep_day.read().unwrap(), closed[0]);
        assert_eq!(gem.call_pending_day.read().unwrap(), closed[2]);
        (closed[0], closed[1])
    });

    let events: Vec<_> = provider
        .get_events(GEM_ADDRESS)
        .iter()
        .filter_map(|log| IGem::SweepDaySkipped::decode_log_data(log).ok())
        .collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].sweep, outbe_oracle::call_sweep::CALL_SWEEP);
    assert_eq!(events[0].skippedDay, skipped);
    assert_eq!(events[0].inFlightDay, in_flight);
}

/// A sweep walks each currency once. Take two currencies that each hold more
/// undecided gems than a slice may visit. If the sweep walked the ones closed
/// behind the cursor again, these two would keep the sweep open for good.
#[test]
fn a_call_sweep_over_several_currencies_always_ends() {
    with_storage(|storage, world| {
        let gem = GemContract::new(storage.clone());
        set_profile(storage, outbe_gem::config::PROFILE_PROD);
        let day = previous_date_key(timestamp_to_date_key(T_NOW));
        for iso in [USD, EUR] {
            let pair = seed_currency(storage, iso, Some(U256::from(600_000u64)));
            priced_window(storage, pair, day, U256::from(300_000u64));
            // Issued five days ago: every gem is visited, decided and left where it is.
            for nonce in 0..=u64::from(SWEEP_VISITS_PER_BLOCK) {
                callable_gem_of(
                    storage,
                    world,
                    iso,
                    nonce,
                    T_NOW - 5 * 86_400,
                    U256::from(100_000u64),
                );
            }
        }

        let ctx = block_ctx_at(storage, T_NOW);
        scan_and_call(&ctx).unwrap();
        for _ in 0..3 {
            run_call_slice(&ctx).unwrap();
        }
        assert_eq!(gem.call_sweep_day.read().unwrap(), 0, "the sweep closed");
    });
}

#[test]
fn an_unpriced_currency_does_not_prevent_calling_the_next_currency() {
    with_storage(|storage, world| {
        let unpriced = callable_gem_of(
            storage,
            world,
            EUR,
            0,
            T_NOW - 100 * 86_400,
            U256::from(100_000),
        );
        seed_currency(storage, EUR, None);
        let priced = callable_gem_of(
            storage,
            world,
            USD,
            1,
            T_NOW - 100 * 86_400,
            U256::from(100_000),
        );
        let pair = seed_currency(storage, USD, Some(U256::from(600_000)));
        window_at(storage, pair, U256::from(300_000));

        assert_eq!(scan_and_call(&block_ctx_at(storage, T_NOW)).unwrap(), 1);
        assert_eq!(gem_state(storage, world, unpriced), GemState::Issued as u8);
        assert_eq!(gem_state(storage, world, priced), GemState::Called as u8);
        assert_eq!(
            GemContract::new(storage.clone())
                .call_sweep_day
                .read()
                .unwrap(),
            0
        );
    });
}

#[test]
fn only_a_slice_that_calls_buckets_emits_a_batch_metadata_update() {
    let (mut provider, world) = provider_at(T_NOW);
    StorageHandle::enter(&mut provider, |storage| {
        callable_gem_of(
            &storage,
            &world,
            USD,
            0,
            T_NOW - 100 * 86_400,
            U256::from(100_000),
        );
        let pair = seed_currency(&storage, USD, Some(U256::from(600_000)));
        window_at(&storage, pair, U256::from(300_000));
        let ctx = block_ctx_at(&storage, T_NOW);

        assert_eq!(scan_and_call(&ctx).unwrap(), 1);
        assert_eq!(run_call_slice(&ctx).unwrap(), 0);
    });

    let updates: Vec<_> = provider
        .get_events(GEM_ADDRESS)
        .iter()
        .filter_map(|log| IGem::BatchMetadataUpdate::decode_log_data(log).ok())
        .collect();
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0]._fromTokenId, U256::ZERO);
    assert_eq!(updates[0]._toTokenId, U256::MAX);
}

/// Idle daily scans read the oracle and write nothing to the body store.
#[test]
fn idle_daily_scans_do_not_write_bodies() {
    let (mut provider, world) = provider_at(T_NOW);
    StorageHandle::enter(&mut provider, |storage| {
        mature_gem(&storage, &world);
        seed_currency(&storage, USD, Some(U256::from(600_000u64)));
    });
    let before = provider.get_events(GEM_ADDRESS).len();
    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(scan_and_call(&block_ctx(&storage)).unwrap(), 0);
    });
    assert_eq!(provider.get_events(GEM_ADDRESS).len(), before);
}

/// A corrupt gem body is this node's store: the call sweep fails the block.
#[test]
fn a_corrupt_body_fails_the_call_sweep() {
    use outbe_oracle::call_sweep::CallSweep;
    use outbe_primitives::error::{PrecompileError, SweepFailure};
    assert_eq!(
        <crate::called::GemCallSweep as CallSweep>::classify(&PrecompileError::BodyReadCorruption(
            "x".into()
        )),
        SweepFailure::Propagate
    );
}
