use super::*;

use super::lifecycle::{fill_days, setup_pair};
use outbe_primitives::time::SECONDS_PER_DAY_U32;

/// Well past issuance: the closed day before it is one every seeded series held in full.
const MATURE_TS: u64 = ISSUED_AT + 21 * DAY + 1;

#[test]
fn scan_and_call_force_calls_breached_series() {
    with_factory(|s| {
        runtime::issue_intex(&s, sample(7)).unwrap();
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        let scan_ts = ISSUED_AT + 60 * DAY;
        let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts)).expect("date step");
        let breach = U256::from(EXPECTED_TRIGGER) + U256::from(1);
        fill_days(&oracle, last_closed_day, pair, 30, breach);

        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, scan_ts, CHAIN_ID),
            s.clone(),
        );
        assert_eq!(called::scan_and_call(&ctx).unwrap(), 1);
        assert_eq!(series_state(&s, sid(7)), outbe_intex::IntexState::Called);
    });
}

#[test]
fn a_called_series_reports_its_settlement_deadline() {
    use alloy_sol_types::SolEvent;

    let scan_ts = ISSUED_AT + 60 * DAY;
    let mut storage = factory_provider();
    let notice = StorageHandle::enter(&mut storage, |s| {
        select_prod_profile(&s);
        runtime::issue_intex(&s, sample(7)).unwrap();
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts)).expect("date step");
        let breach = U256::from(EXPECTED_TRIGGER) + U256::from(1);
        fill_days(&oracle, last_closed_day, pair, 30, breach);

        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, scan_ts, CHAIN_ID),
            s.clone(),
        );
        assert_eq!(called::scan_and_call(&ctx).unwrap(), 1);
        outbe_intex::api::get_series(&s, sid(7))
            .unwrap()
            .call_notice_period_seconds
    });

    let events: Vec<_> = storage
        .get_events(outbe_primitives::addresses::INTEX_ADDRESS)
        .iter()
        .filter_map(|log| {
            outbe_intex::precompile::IIntex::IntexGroupCalled::decode_log_data(log).ok()
        })
        .collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].calledAt, scan_ts);
    assert_eq!(events[0].settlementDeadline, scan_ts + u64::from(notice));
}

#[test]
fn scan_and_call_reads_daily_vwap_at_midnight() {
    // Regression: the scan fires on the midnight Cycle tick. At that time, yesterday's
    // EmissionDate snapshot does not exist yet (metadosis writes it at noon of
    // the current day). The finalized per-UTC-day VWAP is already closed by
    // then and must be the scan's price source. The test seeds exactly `threshold` (21)
    // breach days through the production finalization path. The scan day itself
    // stays unfinalized. So a read of any other day, or of any other store, drops
    // below the threshold and fails the call.
    with_factory(|s| {
        runtime::issue_intex(&s, sample(7)).unwrap();
        let oracle = OracleContract::new(s.clone());
        setup_pair(&oracle);
        // Exact midnight UTC, well past issuance.
        let scan_ts = (ISSUED_AT / DAY + 60) * DAY;
        let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts)).expect("date step");
        let breach = U256::from(EXPECTED_TRIGGER) + U256::from(1);

        // The finalized VWAP of the 21 closed days ending yesterday.
        let index = oracle
            .pair_index_of(outbe_oracle::api::AddressPair::new_coen_to(REFERENCE_ISO))
            .unwrap();
        let mut day = last_closed_day;
        for _ in 0..21 {
            oracle.record_utc_day_vwap(day, index, breach).unwrap();
            day = previous_date_key(day).expect("date step");
        }
        // The begin-block hook advances the watermark after finalizing.
        oracle
            .utc_day_vwap_last_finalized
            .write(last_closed_day)
            .unwrap();

        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, scan_ts, CHAIN_ID),
            s.clone(),
        );
        assert_eq!(called::scan_and_call(&ctx).unwrap(), 1);
        assert_eq!(series_state(&s, sid(7)), outbe_intex::IntexState::Called);
    });
}

// ---------------------------------------------------------------------
// begin-block / daily scan error isolation (no chain halt)
// ---------------------------------------------------------------------

#[test]
fn call_scan_does_not_halt_on_overflow_vwap() {
    with_factory(|s| {
        runtime::issue_intex(&s, sample(7)).unwrap();
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        let scan_ts = ISSUED_AT + 60 * DAY;
        let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts)).expect("date step");
        // Out-of-range VWAP for the completed day: price_to_bin overflows.
        fill_days(&oracle, last_closed_day, pair, 1, U256::MAX);

        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, scan_ts, CHAIN_ID),
            s.clone(),
        );
        // Must not halt: returns Ok(0) and leaves the series Issued.
        assert_eq!(called::scan_and_call(&ctx).unwrap(), 0);
        assert_eq!(series_state(&s, sid(7)), outbe_intex::IntexState::Issued);
    });
}

#[test]
fn a_node_local_failure_while_calling_fails_the_scan() {
    let mut provider = factory_provider();
    let scan_ts = ISSUED_AT + 60 * DAY;
    let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts)).expect("date step");
    StorageHandle::enter(&mut provider, |s| {
        select_prod_profile(&s);
        runtime::issue_intex(&s, sample(7)).unwrap();
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        fill_days(
            &oracle,
            last_closed_day,
            pair,
            30,
            U256::from(EXPECTED_TRIGGER) + U256::from(1),
        );
        let factory = IntexContract::new(s.clone());
        factory.daily_genesis_day.write(last_closed_day).unwrap();
        factory.call_latest_day.write(last_closed_day).unwrap();
        IntexContract::new(s)
            .call_sweep_day
            .write(last_closed_day)
            .unwrap();
    });
    provider.fail_after_mutation_at(0);
    let result = StorageHandle::enter(&mut provider, |s| {
        let ctx = BlockRuntimeContext::new(BlockContext::empty_for_tests(1, scan_ts, CHAIN_ID), s);
        called::process_call_pass(&ctx)
    });
    provider.clear_mutation_failure();
    assert!(matches!(
        result,
        Err(outbe_primitives::error::PrecompileError::Storage(_))
    ));
    StorageHandle::enter(&mut provider, |s| {
        assert_eq!(series_state(&s, sid(7)), outbe_intex::IntexState::Issued);
        let f = IntexContract::new(s.clone());
        assert_eq!(f.group_members(group_of(&s, sid(7))).unwrap(), vec![sid(7)]);
        assert_eq!(f.call_sweep_day.read().unwrap(), last_closed_day);
    });
}

/// Seed a series directly in the registry + call-price index, bypassing issue()
/// so tests can omit the OriginRouter stub.
fn seed_issued(s: &StorageHandle<'_>, id: u32) {
    seed_issued_at(s, id, ISSUED_AT);
}

fn seed_issued_at(s: &StorageHandle<'_>, id: u32, issued_at: u64) {
    outbe_intex::api::create_series(
        s,
        outbe_intex::CreateSeriesParams {
            series_id: sid(id),
            emission_day: id.into(),
            issued_units: 100,
            promis_load_minor: PROMIS_LOAD_MINOR,
            entry_price_minor: U256::from(ENTRY_PRICE),
            call_price_minor: U256::from(EXPECTED_TRIGGER),
            call_trigger: outbe_intex::IntexCallTrigger {
                call_window_seconds: 30 * SECONDS_PER_DAY_U32,
                call_threshold_seconds: 21 * SECONDS_PER_DAY_U32,
                call_notice_period_seconds: CALL_NOTICE_PERIOD,
                transit_margin_seconds: 3_600,
            },
            issued_at,
            issuance_currency: 840,
            reference_currency: 840,
        },
    )
    .unwrap();
}

#[test]
fn the_issue_day_counts_only_for_a_series_issued_at_midnight() {
    let scan_ts = ISSUED_AT + 60 * DAY;
    let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts)).expect("date step");
    // Oldest day of a breach run exactly as long as the series' threshold.
    let mut oldest_breach = last_closed_day;
    for _ in 1..21 {
        oldest_breach = previous_date_key(oldest_breach).expect("date step");
    }
    let midnight = date_key_to_utc_timestamp(oldest_breach).unwrap();

    // A second past midnight loses the day and falls one breach short.
    for (issued_at, expected) in [(midnight, 1), (midnight + 1, 0)] {
        let mut storage = HashMapStorageProvider::new(CHAIN_ID);
        storage.set_timestamp(U256::from(ISSUED_AT));
        storage.stub_sub_call_at(
            crate::constants::INTEX_NFT1155_ADDRESS,
            alloy_primitives::Bytes::from(vec![0u8; 32]),
        );
        storage.stub_sub_call_at(
            crate::constants::ORIGIN_ROUTER_ADDRESS,
            alloy_primitives::Bytes::from(vec![0u8; 32]),
        );
        StorageHandle::enter(&mut storage, |s| {
            select_prod_profile(&s);
            seed_issued_at(&s, 7, issued_at);
            let mut f = IntexContract::new(s.clone());

            let oracle = OracleContract::new(s.clone());
            let pair = setup_pair(&oracle);
            fill_days(
                &oracle,
                last_closed_day,
                pair,
                21,
                U256::from(EXPECTED_TRIGGER) + U256::from(1),
            );

            let group = group_of(&s, sid(7));
            assert_eq!(
                call_group(&s, &mut f, group, last_closed_day, scan_ts),
                expected,
                "issued at {issued_at}"
            );
        });
    }
}

#[test]
fn call_survives_router_failure() {
    // No OriginRouter stub: notify_called fails silently.
    // The Issued -> Called transition must still complete.
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(ISSUED_AT));
    storage.stub_sub_call_at(
        crate::constants::INTEX_NFT1155_ADDRESS,
        alloy_primitives::Bytes::from(vec![0u8; 32]),
    );
    StorageHandle::enter(&mut storage, |s| {
        seed_issued(&s, 7);
        let mut f = IntexContract::new(s.clone());

        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        let scan_ts = ISSUED_AT + 60 * DAY;
        let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts)).expect("date step");
        fill_days(
            &oracle,
            last_closed_day,
            pair,
            30,
            U256::from(EXPECTED_TRIGGER) + U256::from(1),
        );

        let group = group_of(&s, sid(7));
        assert_eq!(call_group(&s, &mut f, group, last_closed_day, scan_ts), 1);
        assert_eq!(series_state(&s, sid(7)), outbe_intex::IntexState::Called);
    });
}

#[test]
fn a_registry_edit_does_not_move_the_cursor_onto_another_currency() {
    let currencies = [840u16, 978u16];
    assert_eq!(
        crate::called::currency_position(&currencies, u32::from(EUR_ISO)),
        1,
        "the cursor names a currency, not a slot"
    );
    assert_eq!(
        crate::called::currency_position(&currencies[1..], u32::from(EUR_ISO)),
        0,
        "dropping the currency ahead of it does not shift the cursor onto a stranger"
    );
    assert_eq!(
        crate::called::currency_position(&currencies, 392),
        0,
        "a currency the registry no longer carries restarts at the head"
    );
}

fn terms(window_days: u32, threshold_days: u32) -> outbe_primitives::call_breach::ScanTerms {
    outbe_primitives::call_breach::ScanTerms {
        window_days,
        threshold_days,
    }
}

#[test]
fn the_scan_range_covers_terms_the_live_profile_no_longer_names() {
    with_factory(|s| {
        let mut f = IntexContract::new(s.clone());
        let live_window = 28 * SECONDS_PER_DAY_U32;
        let live_threshold = 21 * SECONDS_PER_DAY_U32;

        assert_eq!(
            f.scan_call_terms(REFERENCE_ISO, live_window, live_threshold)
                .unwrap(),
            terms(28, 21),
            "nothing issued yet, so the live profile is the whole range"
        );

        f.widen_call_terms(
            REFERENCE_ISO,
            40 * SECONDS_PER_DAY_U32,
            10 * SECONDS_PER_DAY_U32,
        )
        .unwrap();
        assert_eq!(
            f.scan_call_terms(REFERENCE_ISO, live_window, live_threshold)
                .unwrap(),
            terms(40, 10),
            "a series issued on wider terms widens the range in both directions"
        );

        f.widen_call_terms(REFERENCE_ISO, live_window, live_threshold)
            .unwrap();
        assert_eq!(
            f.scan_call_terms(REFERENCE_ISO, live_window, live_threshold)
                .unwrap(),
            terms(40, 10),
            "issuing on the narrow profile again does not shrink the range"
        );

        f.widen_call_terms(REFERENCE_ISO, live_window, 600).unwrap();
        assert_eq!(
            f.scan_call_terms(REFERENCE_ISO, live_window, live_threshold)
                .unwrap(),
            terms(40, 10),
            "a threshold under a day cannot be met, so it does not narrow the range"
        );
    });
}

/// A series in a call group of its own, with nothing realized, so expiry forfeits all of it.
fn grouped_series(s: &StorageHandle<'_>, emission_day: u32) -> SeriesId {
    series_in(s, emission_day, emission_day)
}

/// The series of `emission_day`, issued into the call group `group_day` names.
fn series_in(s: &StorageHandle<'_>, emission_day: u32, group_day: u32) -> SeriesId {
    let series_id = sid(emission_day);
    outbe_intex::api::create_series(
        s,
        outbe_intex::schema::CreateSeriesParams {
            series_id,
            emission_day: EmissionDate::new(emission_day),
            issued_units: 1,
            promis_load_minor: PROMIS_LOAD_MINOR,
            entry_price_minor: U256::from(ENTRY_PRICE),
            call_price_minor: U256::from(ENTRY_PRICE),
            call_trigger: outbe_intex::schema::IntexCallTrigger {
                call_window_seconds: (28 * DAY) as u32,
                call_threshold_seconds: (21 * DAY) as u32,
                call_notice_period_seconds: (7 * DAY) as u32,
                transit_margin_seconds: 3_600,
            },
            issued_at: ISSUED_AT - u64::from((group_day % 1000) * SECONDS_PER_DAY_U32),
            issuance_currency: 840,
            reference_currency: REFERENCE_ISO,
        },
    )
    .unwrap();
    series_id
}

/// A grouped series whose group was called and waits on `deadline`.
fn called_series(s: &StorageHandle<'_>, emission_day: u32, deadline: u64) -> SeriesId {
    let series_id = grouped_series(s, emission_day);
    IntexContract::new(s.clone())
        .seed_called_group(series_id, deadline)
        .unwrap();
    series_id
}

fn queued(s: &StorageHandle<'_>, member: SeriesId) -> bool {
    IntexContract::new(s.clone())
        .called_group_slot
        .read(&group_of(s, member))
        .unwrap()
        != 0
}

fn sweep_expiry(s: &StorageHandle<'_>, at: u64) {
    let ctx = BlockRuntimeContext::new(BlockContext::empty_for_tests(1, at, CHAIN_ID), s.clone());
    called::daily_pass(&ctx).unwrap();
}

#[test]
fn a_group_due_sooner_is_retired_even_when_a_later_one_was_called_first() {
    with_factory(|s| {
        let f = IntexContract::new(s.clone());
        let now = ISSUED_AT;
        called_series(&s, 20260101, now + 10 * DAY);
        called_series(&s, 20260102, now + DAY);

        sweep_expiry(&s, now + 2 * DAY);

        let near_day = IntexContract::deadline_day(now + DAY);
        let far_day = IntexContract::deadline_day(now + 10 * DAY);
        assert_eq!(
            f.forfeit_day_live.read(&near_day).unwrap(),
            0,
            "the group whose window closed leaves its day"
        );
        assert_eq!(
            f.forfeit_day_live.read(&far_day).unwrap(),
            1,
            "the one still inside its window stays"
        );
        assert_eq!(
            f.first_expiry_day().unwrap(),
            Some(far_day),
            "only the day still waiting is left in the tree"
        );
    });
}

#[test]
fn a_group_waits_for_its_transit_close_before_it_forfeits() {
    with_factory(|s| {
        let deadline_day = IntexContract::deadline_day(ISSUED_AT + DAY);
        let deadline = IntexContract::day_end(deadline_day) - 1_800;
        let member = called_series(&s, 20260101, deadline);

        sweep_expiry(&s, IntexContract::day_end(deadline_day));
        assert!(
            queued(&s, member),
            "the deadline day closed inside the margin"
        );
        let f = IntexContract::new(s.clone());
        assert!(!f.group_forfeited(group_of(&s, member)).unwrap());

        sweep_expiry(&s, IntexContract::day_end(deadline_day + 1));
        assert!(
            !queued(&s, member),
            "forfeited once the transit close passed"
        );
        assert!(f.group_forfeited(group_of(&s, member)).unwrap());
    });
}

#[test]
fn an_uncalled_group_never_enters_the_deadline_queue() {
    with_factory(|s| {
        runtime::issue_intex(&s, sample(7)).unwrap();
        sweep_expiry(&s, ISSUED_AT + 400 * DAY);
        let f = IntexContract::new(s.clone());
        assert_eq!(f.first_expiry_day().unwrap(), None);
        assert_eq!(series_state(&s, sid(7)), outbe_intex::IntexState::Issued);
        assert_eq!(f.group_members(group_of(&s, sid(7))).unwrap(), vec![sid(7)]);
    });
}

/// Fails each storage write of the daily pass at `at` in turn: the pass errors, and its
/// checkpoint leaves the armed state as it was.
fn every_failed_pass_write_rolls_back(
    arm: impl Fn(&StorageHandle<'_>) -> SeriesId,
    at: u64,
    done: impl Fn(&StorageHandle<'_>, SeriesId),
    check: impl Fn(&StorageHandle<'_>, SeriesId),
) {
    let armed = || {
        let mut provider = factory_provider();
        let member = StorageHandle::enter(&mut provider, |s| {
            select_prod_profile(&s);
            let member = arm(&s);
            called::prepare_daily_pass(&BlockRuntimeContext::new(
                BlockContext::empty_for_tests(1, at, CHAIN_ID),
                s,
            ))
            .unwrap();
            member
        });
        (provider, member)
    };
    let pass = |provider: &mut HashMapStorageProvider| {
        StorageHandle::enter(provider, |s| {
            called::daily_pass(&BlockRuntimeContext::new(
                BlockContext::empty_for_tests(1, at, CHAIN_ID),
                s,
            ))
            .map(|_| ())
        })
    };
    let (mut probe, member) = armed();
    probe.fail_after_mutation_at(usize::MAX);
    pass(&mut probe).unwrap();
    let writes = probe.clear_mutation_failure();
    assert!(writes > 0);
    StorageHandle::enter(&mut probe, |s| done(&s, member));
    for write in 0..writes {
        let (mut provider, member) = armed();
        let before = provider.storage.clone();
        provider.fail_after_mutation_at(write);
        let result = pass(&mut provider);
        provider.clear_mutation_failure();
        assert!(
            matches!(
                result,
                Err(outbe_primitives::error::PrecompileError::Storage(_))
            ),
            "write {write}: {result:?}"
        );
        assert_eq!(provider.storage, before, "write {write}");
        StorageHandle::enter(&mut provider, |s| check(&s, member));
    }
}

#[test]
fn a_node_local_failure_while_expiring_fails_the_pass() {
    let now = ISSUED_AT;
    let due = IntexContract::day_end(IntexContract::deadline_day(now + DAY));
    every_failed_pass_write_rolls_back(
        |s| called_series(s, 20260101, now + DAY),
        due,
        |s, member| assert!(!queued(s, member)),
        |s, member| {
            assert!(queued(s, member));
            assert_eq!(
                IntexContract::new(s.clone()).first_expiry_day().unwrap(),
                Some(IntexContract::deadline_day(now + DAY))
            );
        },
    );
}

#[test]
fn a_node_local_failure_while_deferring_fails_the_pass() {
    let now = ISSUED_AT;
    let day = IntexContract::deadline_day(now + DAY);
    every_failed_pass_write_rolls_back(
        |s| {
            let member = called_series(s, 20260101, now + DAY);
            // More settled than issued: the group cannot expire, so the pass defers it.
            IntexContract::new(s.clone())
                .settled_units
                .write(&outbe_intex::intex_id(member), 1_000)
                .unwrap();
            member
        },
        IntexContract::day_end(day),
        |s, member| {
            assert!(queued(s, member));
            assert_eq!(
                IntexContract::new(s.clone())
                    .forfeit_day_live
                    .read(&day)
                    .unwrap(),
                0
            );
        },
        |s, member| {
            assert!(queued(s, member));
            let f = IntexContract::new(s.clone());
            assert_eq!(f.forfeit_day_live.read(&day).unwrap(), 1);
            assert_eq!(f.first_expiry_day().unwrap(), Some(day));
        },
    );
}

#[test]
fn a_day_the_sweep_cannot_finish_is_retired_rather_than_left_in_front() {
    with_factory(|s| {
        let f = IntexContract::new(s.clone());
        let now = ISSUED_AT;
        let member = called_series(&s, 20260101, now + DAY);
        let day = IntexContract::deadline_day(now + DAY);
        f.called_deadline
            .write(&group_of(&s, member), now + 400 * DAY)
            .unwrap();

        sweep_expiry(&s, IntexContract::day_end(day));

        let later = IntexContract::deadline_day(now + 400 * DAY);
        assert_eq!(f.forfeit_day_live.read(&day).unwrap(), 0);
        assert_eq!(
            f.first_expiry_day().unwrap(),
            Some(later),
            "the day leaves the tree instead of blocking every later one"
        );
        assert!(
            queued(&s, member),
            "and its group waits in the day its deadline falls in"
        );
        assert_eq!(f.forfeit_day_live.read(&later).unwrap(), 1);
    });
}

#[test]
fn a_group_requeued_at_retirement_is_expired_and_credited_once() {
    use alloy_sol_types::SolEvent;

    let mut provider = factory_provider();
    let now = ISSUED_AT;
    let day = IntexContract::deadline_day(now + DAY);
    let later = IntexContract::deadline_day(now + 3 * DAY);
    StorageHandle::enter(&mut provider, |s| {
        select_prod_profile(&s);
        let f = IntexContract::new(s.clone());
        let member = called_series(&s, 20260101, now + DAY);
        let group = group_of(&s, member);
        f.called_deadline.write(&group, now + 3 * DAY).unwrap();
        let unallocated = || {
            outbe_promislimit::PromisLimitContract::new(s.clone())
                .promis_limit()
                .unwrap()
        };

        sweep_expiry(&s, IntexContract::day_end(day));
        assert_eq!(f.first_expiry_day().unwrap(), Some(later));
        assert!(queued(&s, member));
        assert_eq!(unallocated(), U256::ZERO);

        sweep_expiry(&s, IntexContract::day_end(later));
        assert_eq!(f.first_expiry_day().unwrap(), None);
        assert!(!queued(&s, member));
        assert_eq!(unallocated(), U256::from(PROMIS_LOAD_MINOR));

        sweep_expiry(&s, IntexContract::day_end(later) + DAY);
        assert_eq!(unallocated(), U256::from(PROMIS_LOAD_MINOR));
    });

    let forfeited = provider
        .get_events(outbe_primitives::addresses::INTEX_ADDRESS)
        .iter()
        .filter_map(|log| {
            outbe_intex::precompile::IIntex::IntexGroupForfeited::decode_log_data(log).ok()
        })
        .count();
    assert_eq!(forfeited, 1);
}

#[test]
fn a_day_wider_than_one_block_resumes_where_it_gave_out() {
    with_factory(|s| {
        let f = IntexContract::new(s.clone());
        let now = ISSUED_AT;
        let deadline = now + DAY;
        let day = IntexContract::deadline_day(deadline);

        let queued = outbe_primitives::pass_budget::SWEEP_WRITES_PER_BLOCK + 44;
        for index in 0..queued {
            called_series(&s, 20260101 + index, deadline);
        }
        assert_eq!(f.forfeit_day_live.read(&day).unwrap(), queued);

        let due = IntexContract::day_end(day);
        sweep_expiry(&s, due);
        let left = f.forfeit_day_live.read(&day).unwrap();
        assert!(
            left > 0 && left < queued,
            "one block takes what it is budgeted for and no more, got {left} of {queued}"
        );
        assert_eq!(
            f.forfeit_pass_day.read().unwrap(),
            day,
            "the unfinished day is remembered"
        );
        assert!(
            f.forfeit_cursor.read().unwrap() > 0,
            "along with the slot it stopped at"
        );

        sweep_expiry(&s, due + 1);
        assert_eq!(
            f.forfeit_day_live.read(&day).unwrap(),
            0,
            "the next block finishes the tail rather than re-walking the head"
        );
        assert_eq!(f.first_expiry_day().unwrap(), None);
        assert_eq!(
            f.forfeit_cursor.read().unwrap(),
            0,
            "and the cursor is released with the day"
        );
    });
}

#[test]
fn a_short_notice_waits_for_its_deadline_day_to_close() {
    with_factory(|s| {
        let f = IntexContract::new(s.clone());
        let now = (ISSUED_AT / DAY) * DAY;
        let deadline = now + 600;
        called_series(&s, 20260101, deadline);

        sweep_expiry(&s, deadline + 1);
        assert_eq!(
            f.first_expiry_day().unwrap(),
            Some(IntexContract::deadline_day(deadline)),
            "still waiting while its own day is open"
        );

        sweep_expiry(&s, now + 3600);
        assert_eq!(
            f.first_expiry_day().unwrap(),
            Some(IntexContract::deadline_day(deadline))
        );
        sweep_expiry(&s, now + DAY);
        assert_eq!(
            f.first_expiry_day().unwrap(),
            None,
            "retired at the next midnight, not the next hour"
        );
    });
}

#[test]
fn the_cleanup_deletes_a_spent_series_and_keeps_one_with_settled_units() {
    with_factory(|s| {
        let now = ISSUED_AT;
        let deadline = now + DAY;
        let spent = grouped_series(&s, 20260101);
        let kept = series_in(&s, 20260102, 20260101);
        outbe_intex::api::record_settled_units(&s, kept, owner(), 1).unwrap();
        IntexContract::new(s.clone())
            .seed_called_group(spent, deadline)
            .unwrap();
        let group = group_of(&s, spent);
        assert_eq!(group, group_of(&s, kept));

        let due = IntexContract::day_end(IntexContract::deadline_day(deadline));
        let ctx =
            BlockRuntimeContext::new(BlockContext::empty_for_tests(1, due, CHAIN_ID), s.clone());
        let block = called::daily_pass(&ctx).unwrap();
        crate::hooks::cleanup(&block).unwrap();

        let f = IntexContract::new(s.clone());
        assert!(!outbe_intex::api::series_exists(&s, spent).unwrap());
        assert!(outbe_intex::api::series_exists(&s, kept).unwrap());
        assert_eq!(f.group_members(group).unwrap(), vec![kept]);
        assert_eq!(f.first_cleanup_day().unwrap(), None);
        let record = outbe_intex::api::get_series(&s, kept).unwrap();
        assert_eq!(
            outbe_intex::api::call_stamp(&s, &record)
                .unwrap()
                .state(due),
            outbe_intex::IntexState::Forfeited
        );
    });
}

fn block_at<'s>(s: &StorageHandle<'s>, number: u64, ts: u64) -> BlockRuntimeContext<'s> {
    BlockRuntimeContext::new(
        BlockContext::empty_for_tests(number, ts, CHAIN_ID),
        s.clone(),
    )
}

/// The call pass marks a currency it cannot price: the day's window price is out
/// of the bin ladder, so the currency waits a day.
#[test]
fn an_unindexable_window_price_marks_the_currency_for_the_day() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(ISSUED_AT));
    StorageHandle::enter(&mut storage, |s| {
        select_prod_profile(&s);
        seed_issued(&s, 7);
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        let day = previous_date_key(timestamp_to_date_key(MATURE_TS)).expect("date step");
        fill_days(&oracle, day, pair, 30, U256::MAX);

        assert_eq!(
            called::scan_and_call(&block_at(&s, 1, MATURE_TS)).unwrap(),
            0
        );
        assert_eq!(series_state(&s, sid(7)), outbe_intex::IntexState::Issued);
        assert_eq!(
            IntexContract::new(s.clone())
                .call_scan_failed_day
                .read(&REFERENCE_ISO)
                .unwrap(),
            day,
            "the currency is marked for the day it failed on"
        );
    });
}
