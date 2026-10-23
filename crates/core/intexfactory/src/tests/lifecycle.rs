use super::*;
use outbe_primitives::time::SECONDS_PER_DAY_U32;

#[test]
fn issue_enrolls_in_call_bin() {
    with_factory(|s| {
        runtime::issue_intex(&s, sample(7)).unwrap();
        let f = IntexContract::new(s.clone());
        let bin = outbe_primitives::call_bins::price_to_bin(U256::from(EXPECTED_TRIGGER)).unwrap();
        assert_eq!(
            f.call_bin_count
                .read(&IntexContract::scoped(REFERENCE_ISO, bin))
                .unwrap(),
            1
        );
    });
}

/// Registry index the fixtures register the reference pair at. The rate
/// columns are keyed by it.
const PAIR_ID: u32 = 1;

/// List the reference currency in the oracle's reference registry, which the
/// scans walk to decide which currencies to price this block.
pub(super) fn list_reference(oracle: &OracleContract) {
    if oracle.reference_currencies.len().unwrap() == 0 {
        oracle.reference_currencies.push(REFERENCE_ISO).unwrap();
    }
}

pub(super) fn setup_pair(oracle: &OracleContract) -> AddressPair {
    let pair = outbe_oracle::api::AddressPair::new_coen_to(REFERENCE_ISO);
    let pair_id = PAIR_ID;
    oracle.pair_to_index.write(&pair, pair_id).unwrap();
    // Full registry entry so the production VWAP paths (calculate_vwaps
    // iterating registered vote-target pairs) see the pair too. `pair_at` reads
    // the reverse column.
    oracle.pair_count.write(pair_id).unwrap();
    oracle.pair_by_index.write_pair(&pair_id, pair).unwrap();
    oracle.vote_target.write(&pair, true).unwrap();
    list_reference(oracle);
    pair
}

fn set_vwap(oracle: &OracleContract, utc_day: u32, pair: AddressPair, value: U256) {
    // The value column is keyed by the pair's registry index.
    let pair_id = oracle.pair_index_of(pair).unwrap();
    oracle.record_utc_day_vwap(utc_day, pair_id, value).unwrap();
    // Mirror the begin-block hook: the watermark covers every seeded day.
    if oracle.utc_day_vwap_last_finalized.read().unwrap() < utc_day {
        oracle.utc_day_vwap_last_finalized.write(utc_day).unwrap();
    }
}

/// Set `days` consecutive closed UTC days ending at `latest` to `value`.
/// The returned key is the next older day after that run.
pub(super) fn fill_days(
    oracle: &OracleContract,
    latest: u32,
    pair: AddressPair,
    days: u32,
    value: U256,
) -> u32 {
    let mut d = latest;
    for _ in 0..days {
        set_vwap(oracle, d, pair, value);
        d = previous_date_key(d).expect("date step");
    }
    d
}

#[test]
fn a_group_holds_its_bin_entry_until_its_last_member_leaves() {
    with_factory(|s| {
        runtime::issue_intex(&s, sample(11)).unwrap();
        runtime::issue_intex(&s, sample(22)).unwrap();
        let mut f = IntexContract::new(s.clone());
        let bin = outbe_primitives::call_bins::price_to_bin(U256::from(EXPECTED_TRIGGER)).unwrap();
        let count = |f: &IntexContract<'_>| {
            f.call_bin_count
                .read(&IntexContract::scoped(REFERENCE_ISO, bin))
                .unwrap()
        };
        let group = group_of(&s, sid(11));
        assert_eq!(group, group_of(&s, sid(22)));
        assert_eq!(count(&f), 1);
        f.leave_call_group(group, sid(11)).unwrap();
        assert_eq!(count(&f), 1);
        f.leave_call_group(group, sid(22)).unwrap();
        assert_eq!(count(&f), 0);
    });
}

#[test]
fn try_call_marks_called_when_threshold_met() {
    with_factory(|s| {
        runtime::issue_intex(&s, sample(7)).unwrap();
        let mut f = IntexContract::new(s.clone());
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        // All 30 window days above the trigger (threshold is 21).
        let scan_ts = ISSUED_AT as u64 + 60 * DAY;
        let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts)).expect("date step");
        let breach = U256::from(EXPECTED_TRIGGER) + U256::from(1);
        fill_days(&oracle, last_closed_day, pair, 30, breach);

        let group = group_of(&s, sid(7));
        assert_eq!(call_group(&s, &mut f, group, last_closed_day, scan_ts), 1);
        assert_eq!(series_state(&s, sid(7)), outbe_intex::IntexState::Called);
        let bin = outbe_primitives::call_bins::price_to_bin(U256::from(EXPECTED_TRIGGER)).unwrap();
        assert_eq!(
            f.call_bin_count
                .read(&IntexContract::scoped(REFERENCE_ISO, bin))
                .unwrap(),
            0
        );
    });
}

#[test]
fn try_call_skips_when_below_threshold() {
    with_factory(|s| {
        runtime::issue_intex(&s, sample(7)).unwrap();
        let mut f = IntexContract::new(s.clone());
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        let scan_ts = ISSUED_AT as u64 + 60 * DAY;
        let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts)).expect("date step");
        let breach = U256::from(EXPECTED_TRIGGER) + U256::from(1);
        // Equal is not a breach: the check is strict `>`. 20 breach days and 10 calm
        // days stay under the threshold of 21.
        let calm = U256::from(EXPECTED_TRIGGER);
        let after_breach = fill_days(&oracle, last_closed_day, pair, 20, breach);
        fill_days(&oracle, after_breach, pair, 10, calm);

        let group = group_of(&s, sid(7));
        assert_eq!(call_group(&s, &mut f, group, last_closed_day, scan_ts), 0);
        assert_eq!(series_state(&s, sid(7)), outbe_intex::IntexState::Issued);
    });
}

#[test]
fn try_call_excludes_pre_issuance_days() {
    with_factory(|s| {
        // window 30, threshold 27: only days from issuance onward may count.
        outbe_intex::api::create_series(
            &s,
            outbe_intex::CreateSeriesParams {
                series_id: sid(8),
                worldwide_day: 8.into(),
                issued_units: 100,
                promis_load_minor: PROMIS_LOAD_MINOR,
                entry_price_minor: U256::from(ENTRY_PRICE),
                call_price_minor: U256::from(EXPECTED_TRIGGER),
                call_trigger: outbe_intex::IntexCallTrigger {
                    call_window_seconds: 30 * SECONDS_PER_DAY_U32,
                    call_threshold_seconds: 27 * SECONDS_PER_DAY_U32,
                    call_notice_period_seconds: CALL_NOTICE_PERIOD,
                },
                issued_at: ISSUED_AT,
                issuance_currency: 840,
                reference_currency: 840,
            },
        )
        .unwrap();
        let mut f = IntexContract::new(s.clone());
        let oracle = OracleContract::new(s.clone());
        let pair = setup_pair(&oracle);
        // Scan only ~23 days after issuance, but set all 30 window days as
        // breaches: the ~7 pre-issuance days must not count, so 23 < 27.
        let scan_ts = ISSUED_AT as u64 + 23 * DAY;
        let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts)).expect("date step");
        let breach = U256::from(EXPECTED_TRIGGER) + U256::from(1);
        fill_days(&oracle, last_closed_day, pair, 30, breach);

        let group = group_of(&s, sid(8));
        assert_eq!(call_group(&s, &mut f, group, last_closed_day, scan_ts), 0);
        assert_eq!(series_state(&s, sid(8)), outbe_intex::IntexState::Issued);
    });
}

mod call_sweep {
    //! A call sweep too large for one run continues across blocks, pinned to the
    //! day it opened on.

    use crate::tests::{group_of, series_state};

    use alloy_primitives::U256;
    use outbe_intex::SeriesId;
    use outbe_oracle::api::AddressPair;
    use outbe_oracle::schema::OracleContract;
    use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
    use outbe_primitives::storage::StorageHandle;
    use outbe_primitives::time::WorldwideDay;
    use outbe_primitives::time::{previous_date_key, timestamp_to_date_key};

    use crate::called;
    use crate::tests::owner;
    use outbe_intex::IntexContract;
    use outbe_primitives::sweep_budget::{SWEEP_VISITS_PER_BLOCK, SWEEP_WRITES_PER_BLOCK};

    const CHAIN_ID: u64 = 1;
    const REFERENCE_ISO: u16 = 840;
    const ISSUED_AT: u32 = 1_700_000_000;
    use outbe_primitives::time::{SECONDS_PER_DAY as DAY, SECONDS_PER_DAY_U32};
    const TRIGGER: u64 = 1_000_000;
    const WINDOW_DAYS: u32 = 28;

    /// Issue one series. `iso` is its reference currency, so each value opens its own group.
    fn seed_series_for(
        s: &StorageHandle<'_>,
        iso: u16,
        worldwide_day: u32,
        issued_at: u32,
        trigger_minor: u64,
    ) {
        let series_id = SeriesId::for_pair(WorldwideDay::new(worldwide_day), 840, iso).unwrap();
        let trigger = U256::from(trigger_minor);
        outbe_intex::api::create_series(
            s,
            outbe_intex::CreateSeriesParams {
                series_id,
                worldwide_day: WorldwideDay::new(worldwide_day),
                issued_units: 100,
                promis_load_minor: 1_000_000_000_000_000_000,
                entry_price_minor: trigger,
                call_price_minor: trigger,
                call_trigger: outbe_intex::IntexCallTrigger {
                    call_window_seconds: WINDOW_DAYS * SECONDS_PER_DAY_U32,
                    call_threshold_seconds: 21 * SECONDS_PER_DAY_U32,
                    call_notice_period_seconds: 7 * SECONDS_PER_DAY_U32,
                },
                issued_at,
                issuance_currency: 840,
                reference_currency: iso,
            },
        )
        .unwrap();
    }

    fn issued_on(worldwide_day: u32) -> u32 {
        ISSUED_AT - (worldwide_day % 1000) * SECONDS_PER_DAY_U32
    }

    fn seed_days(
        s: &StorageHandle<'_>,
        days: std::ops::Range<u32>,
        iso: u16,
        trigger_minor: u64,
        issued_at_of: impl Fn(u32) -> u32,
    ) {
        for day in days {
            seed_series_for(s, iso, day, issued_at_of(day), trigger_minor);
        }
    }

    /// Each day's candidate is issued on its own day, so it opens its own call group.
    fn seed_called_range(s: &StorageHandle<'_>, days: std::ops::Range<u32>) {
        seed_days(s, days, REFERENCE_ISO, TRIGGER, issued_on);
    }

    fn seed_called_candidate(s: &StorageHandle<'_>, worldwide_day: u32) {
        seed_series_for(
            s,
            REFERENCE_ISO,
            worldwide_day,
            issued_on(worldwide_day),
            TRIGGER,
        );
    }

    fn run_call_slices(ctx: &BlockRuntimeContext<'_>, times: usize) {
        for _ in 0..times {
            called::run_call_slice(ctx).unwrap();
        }
    }

    fn drain_call_slices(ctx: &BlockRuntimeContext<'_>) {
        while called::run_call_slice(ctx).unwrap() > 0 {}
    }

    fn series_of(worldwide_day: u32) -> SeriesId {
        SeriesId::for_pair(WorldwideDay::new(worldwide_day), 840, REFERENCE_ISO).unwrap()
    }

    fn called_count(s: &StorageHandle<'_>, days: std::ops::Range<u32>) -> u32 {
        days.filter(|d| {
            let series_id = SeriesId::for_pair(WorldwideDay::new(*d), 840, REFERENCE_ISO).unwrap();
            series_state(s, series_id) == outbe_intex::IntexState::Called
        })
        .count() as u32
    }

    #[test]
    fn a_called_group_is_parked_with_its_members_and_call_time() {
        crate::tests::with_factory(|s| {
            let oracle = OracleContract::new(s.clone());
            let pair = super::setup_pair(&oracle);
            let scan_ts = ISSUED_AT as u64 + 60 * DAY;
            let last_closed_day =
                previous_date_key(timestamp_to_date_key(scan_ts)).expect("date step");
            super::fill_days(
                &oracle,
                last_closed_day,
                pair,
                WINDOW_DAYS,
                U256::from(TRIGGER + 1),
            );
            seed_called_candidate(&s, 20260101);

            let ctx = BlockRuntimeContext::new(
                BlockContext::empty_for_tests(1, scan_ts, CHAIN_ID),
                s.clone(),
            );
            assert_eq!(called::scan_and_call(&ctx).unwrap(), 1);

            let factory = IntexContract::new(s.clone());
            let key = group_of(&s, series_of(20260101));

            // The group left its bin and keeps its members for the expiry sweep.
            assert_eq!(factory.call_group_slot.read(&key).unwrap(), 0);
            let bucket = IntexContract::deadline_bucket(scan_ts + 7 * DAY);
            assert_eq!(
                factory
                    .expiry_bucket_at
                    .read(&IntexContract::bucket_slot_key(bucket, 0))
                    .unwrap(),
                key
            );
            assert_eq!(factory.expiry_bucket_live.read(&bucket).unwrap(), 1);
            assert_eq!(
                factory.called_deadline.read(&key).unwrap(),
                scan_ts + 7 * DAY
            );
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
        IntexContract::bucket_end(IntexContract::deadline_bucket(deadline))
    }

    fn sweep_at(s: &StorageHandle<'_>, now: u64) {
        let ctx =
            BlockRuntimeContext::new(BlockContext::empty_for_tests(2, now, CHAIN_ID), s.clone());
        crate::expired::sweep_expired(&ctx).unwrap();
    }

    fn unallocated(s: &StorageHandle<'_>) -> U256 {
        outbe_promislimit::PromisLimitContract::new(s.clone())
            .promis_limit()
            .unwrap()
    }

    fn priced_window(s: &StorageHandle<'_>, scan_ts: u64) {
        let oracle = OracleContract::new(s.clone());
        let pair = super::setup_pair(&oracle);
        let last_closed_day = previous_date_key(timestamp_to_date_key(scan_ts)).expect("date step");
        super::fill_days(
            &oracle,
            last_closed_day,
            pair,
            WINDOW_DAYS,
            U256::from(TRIGGER + 1),
        );
    }

    /// The one place an off-by-one costs capacity twice: settlement is legal at
    /// the deadline itself (`settle_rejects_expired_deadline` pins that side). A
    /// block hook runs before the block's transactions, so the sweep must wait
    /// for the tick after it.
    #[test]
    fn the_sweep_waits_until_settlement_has_actually_closed() {
        crate::tests::with_factory(|s| {
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
        crate::tests::with_factory(|s| {
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
        crate::tests::with_factory(|s| {
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
        for issuance in 1..=width {
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
        crate::tests::with_factory(|s| {
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
        crate::tests::with_factory(|s| {
            let scan_ts = ISSUED_AT as u64 + 60 * DAY;
            priced_window(&s, scan_ts);
            let width = SWEEP_WRITES_PER_BLOCK as u16 + 1;
            let (members, deadline) = called_wide_group(&s, scan_ts, width);

            sweep_at(&s, due(deadline));
            assert!(!expiring(&s, members[0]));
            assert_eq!(unallocated(&s), U256::from(width) * U256::from(LOAD));
            assert!(IntexContract::new(s.clone())
                .first_cleanup_hour()
                .unwrap()
                .is_some());
        });
    }

    /// A member whose accounting fails defers the whole group, which credits nothing
    /// until every member adds up, and then all of them once.
    #[test]
    fn a_group_with_a_failing_member_is_deferred_whole() {
        crate::tests::with_factory(|s| {
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
            let retry =
                IntexContract::bucket_end(IntexContract::deadline_bucket(due(deadline)) + 1);
            sweep_at(&s, retry);
            assert_eq!(unallocated(&s), U256::from(140u64) * U256::from(LOAD));
            assert!(!expiring(&s, members[0]));
        });
    }

    #[test]
    fn a_second_pass_over_an_expired_group_credits_nothing() {
        crate::tests::with_factory(|s| {
            let scan_ts = ISSUED_AT as u64 + 60 * DAY;
            priced_window(&s, scan_ts);
            seed_called_candidate(&s, 20260101);
            let deadline = call_and_deadline(&s, series_of(20260101), scan_ts);

            sweep_at(&s, due(deadline));
            let after_first = unallocated(&s);
            sweep_at(&s, due(deadline) + 1);
            assert_eq!(unallocated(&s), after_first);

            // The bucket emptied, so its day left the tree rather than being handed
            // back every block.
            let factory = IntexContract::new(s.clone());
            assert_eq!(factory.first_expiry_day().unwrap(), None);
        });
    }

    #[test]
    fn a_backlog_larger_than_one_block_drains_over_the_next_ones() {
        crate::tests::with_factory(|s| {
            let scan_ts = ISSUED_AT as u64 + 60 * DAY;
            priced_window(&s, scan_ts);
            // One series per group, so the action budget bounds the groups taken.
            let groups = SWEEP_WRITES_PER_BLOCK + SWEEP_WRITES_PER_BLOCK / 2;
            seed_called_range(&s, 20260101..20260101 + groups);
            let ctx = BlockRuntimeContext::new(
                BlockContext::empty_for_tests(1, scan_ts, CHAIN_ID),
                s.clone(),
            );
            drain_call_slices(&ctx);
            called::scan_and_call(&ctx).unwrap();
            drain_call_slices(&ctx);

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
        crate::tests::with_factory(|s| {
            let oracle = OracleContract::new(s.clone());
            let pair = super::setup_pair(&oracle);
            let scan_ts = ISSUED_AT as u64 + 60 * DAY;
            let last_closed_day =
                previous_date_key(timestamp_to_date_key(scan_ts)).expect("date step");
            // Every window day above the trigger: all of them are due a call.
            super::fill_days(
                &oracle,
                last_closed_day,
                pair,
                WINDOW_DAYS,
                U256::from(TRIGGER + 1),
            );

            // One group per day, half again as many as one slice may move.
            let groups = SWEEP_WRITES_PER_BLOCK + SWEEP_WRITES_PER_BLOCK / 2;
            let days = 20260101..20260101 + groups;
            seed_called_range(&s, days.clone());

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
            let second = called::run_call_slice(&ctx).unwrap();
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
        // Quiet days sit at the new end. The 21 breach days are the older ones.
        let older = super::fill_days(oracle, latest, pair, WINDOW_DAYS - 21, quiet);
        super::fill_days(oracle, older, pair, 21, breach);
    }

    #[test]
    fn a_sweep_that_runs_past_midnight_keeps_deciding_against_its_own_day() {
        crate::tests::with_factory(|s| {
            let oracle = OracleContract::new(s.clone());
            let pair = super::setup_pair(&oracle);
            let scan_ts = ISSUED_AT as u64 + 60 * DAY;
            let opened_on = previous_date_key(timestamp_to_date_key(scan_ts)).expect("date step");
            fill_expiring_window(&oracle, opened_on, pair);
            // The day the sweep spills into is quiet, so the window it would see has
            // one breach fewer than the threshold.
            super::set_vwap(
                &oracle,
                timestamp_to_date_key(scan_ts),
                pair,
                U256::from(TRIGGER),
            );

            let groups = SWEEP_WRITES_PER_BLOCK + 1;
            let days = 20260101..20260101 + groups;
            seed_called_range(&s, days.clone());

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
        crate::tests::with_factory(|s| {
            let oracle = OracleContract::new(s.clone());
            let pair = super::setup_pair(&oracle);
            let scan_ts = ISSUED_AT as u64 + 60 * DAY;
            let opened_on = previous_date_key(timestamp_to_date_key(scan_ts)).expect("date step");
            fill_expiring_window(&oracle, opened_on, pair);
            super::set_vwap(
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
        crate::tests::with_factory(|s| {
            let oracle = OracleContract::new(s.clone());
            let pair = super::setup_pair(&oracle);
            let scan_ts = ISSUED_AT as u64 + 60 * DAY;
            let last_closed_day =
                previous_date_key(timestamp_to_date_key(scan_ts)).expect("date step");
            super::fill_days(
                &oracle,
                last_closed_day,
                pair,
                WINDOW_DAYS,
                U256::from(TRIGGER + 1),
            );
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
        crate::tests::with_factory(|s| {
            let oracle = OracleContract::new(s.clone());
            let pair = super::setup_pair(&oracle);
            let scan_ts = ISSUED_AT as u64 + 60 * DAY;
            let last_closed_day =
                previous_date_key(timestamp_to_date_key(scan_ts)).expect("date step");
            super::fill_days(
                &oracle,
                last_closed_day,
                pair,
                WINDOW_DAYS,
                U256::from(TRIGGER + 1),
            );
            seed_called_range(&s, 20260101..20260105);

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

    /// Nothing shrinks a bin whose groups all decide against a move, so the sweep has
    /// to walk past it rather than restart on them forever.
    #[test]
    fn a_bin_of_undecided_groups_does_not_stall_the_sweep() {
        crate::tests::with_factory(|s| {
            let oracle = OracleContract::new(s.clone());
            let pair = super::setup_pair(&oracle);
            let scan_ts = ISSUED_AT as u64 + 60 * DAY;
            let last_closed_day =
                previous_date_key(timestamp_to_date_key(scan_ts)).expect("date step");
            super::fill_days(
                &oracle,
                last_closed_day,
                pair,
                WINDOW_DAYS,
                U256::from(TRIGGER + 1),
            );

            // Issued inside the window, five days ago, so the truncated window cannot
            // hold the threshold. Every one is visited, decided, and left alone.
            let young_at = (scan_ts - 5 * DAY) as u32;
            let groups = SWEEP_VISITS_PER_BLOCK + 1;
            seed_days(
                &s,
                20260101..20260101 + groups,
                REFERENCE_ISO,
                TRIGGER,
                |_| young_at,
            );

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
            run_call_slices(&ctx, 4);
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
        crate::tests::with_factory(|s| {
            let oracle = OracleContract::new(s.clone());
            let pair = super::setup_pair(&oracle);
            let scan_ts = ISSUED_AT as u64 + 60 * DAY;
            let last_closed_day =
                previous_date_key(timestamp_to_date_key(scan_ts)).expect("date step");
            // Priced days enough to reach the threshold, at a price no bin covers.
            super::fill_days(&oracle, last_closed_day, pair, WINDOW_DAYS, U256::MAX);
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
        crate::tests::with_factory(|s| {
            let oracle = OracleContract::new(s.clone());
            let pair = super::setup_pair(&oracle);
            let scan_ts = ISSUED_AT as u64 + 60 * DAY;
            let last_closed_day =
                previous_date_key(timestamp_to_date_key(scan_ts)).expect("date step");
            super::fill_days(
                &oracle,
                last_closed_day,
                pair,
                WINDOW_DAYS,
                U256::from(TRIGGER * 4),
            );

            // Below the budget-stop: issued inside the window, so it decides against a
            // call today however loud the days were.
            let young = 20260001;
            seed_series_for(
                &s,
                REFERENCE_ISO,
                young,
                (scan_ts - 5 * DAY) as u32,
                TRIGGER,
            );
            // Above it: enough mature groups at a higher trigger to spend every action.
            seed_days(
                &s,
                20260101..20260101 + SWEEP_WRITES_PER_BLOCK + 1,
                REFERENCE_ISO,
                TRIGGER * 2,
                issued_on,
            );

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
            super::fill_days(
                &oracle,
                later_day,
                pair,
                WINDOW_DAYS,
                U256::from(TRIGGER * 4),
            );
            let later = BlockRuntimeContext::new(
                BlockContext::empty_for_tests(2, later_ts, CHAIN_ID),
                s.clone(),
            );
            called::scan_and_call(&later).unwrap();
            // The in-flight day finishes, then every day up to the new one is walked.
            let factory = IntexContract::new(s.clone());
            for _ in 0..64 {
                if factory.call_sweep_day.read().unwrap() == 0 {
                    break;
                }
                called::run_call_slice(&later).unwrap();
            }
            assert_eq!(factory.call_sweep_day.read().unwrap(), 0);

            assert_eq!(
                called_count(&s, young..young + 1),
                1,
                "the group below the old cursor was called"
            );
        });
    }

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
        oracle.reference_currencies.push(SECOND_ISO).unwrap();
        pair
    }

    /// A budget that is exhausted inside the last currency of the rotation must resume
    /// there, not back at the first. Otherwise the ones already closed would eat the
    /// next slice's allowance before it ever reached the one where the budget stopped.
    #[test]
    fn a_slice_resumes_at_the_currency_it_gave_out_on() {
        crate::tests::with_factory(|s| {
            let oracle = OracleContract::new(s.clone());
            let first = super::setup_pair(&oracle);
            let second = setup_second_pair(&oracle);
            let scan_ts = ISSUED_AT as u64 + 60 * DAY;
            let last_closed_day =
                previous_date_key(timestamp_to_date_key(scan_ts)).expect("date step");
            super::fill_days(
                &oracle,
                last_closed_day,
                first,
                WINDOW_DAYS,
                U256::from(TRIGGER + 1),
            );
            super::fill_days(
                &oracle,
                last_closed_day,
                second,
                WINDOW_DAYS,
                U256::from(TRIGGER + 1),
            );

            // The first currency is small. The second holds more than one slice can move.
            seed_called_candidate(&s, 20260101);
            seed_days(
                &s,
                20260201..20260201 + SWEEP_WRITES_PER_BLOCK + 1,
                SECOND_ISO,
                TRIGGER,
                issued_on,
            );

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
        crate::tests::with_factory(|s| {
            let oracle = OracleContract::new(s.clone());
            let pair = super::setup_pair(&oracle);
            let scan_ts = ISSUED_AT as u64 + 60 * DAY;
            let day = previous_date_key(timestamp_to_date_key(scan_ts)).expect("date step");
            super::fill_days(&oracle, day, pair, WINDOW_DAYS, U256::from(TRIGGER + 1));
            let groups = SWEEP_WRITES_PER_BLOCK + SWEEP_WRITES_PER_BLOCK / 2;
            let days = 20260101..20260101 + groups;
            seed_called_range(&s, days.clone());
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
            super::fill_days(
                &oracle,
                next_day,
                pair,
                WINDOW_DAYS,
                U256::from(TRIGGER + 1),
            );
            let next = BlockRuntimeContext::new(
                BlockContext::empty_for_tests(2, next_ts, CHAIN_ID),
                s.clone(),
            );
            called::run_daily(&next).unwrap();
            assert_eq!(factory.call_sweep_day.read().unwrap(), day);
            assert_eq!(factory.call_latest_day.read().unwrap(), next_day);
            assert_eq!(
                factory.call_bin_cursor.read(&REFERENCE_ISO).unwrap(),
                cursor,
                "the walk in flight was not restarted"
            );

            // The next slice finishes the old day and hands the sweep to the queued one.
            called::run_call_slice(&next).unwrap();
            assert_eq!(called_count(&s, days), groups);
            assert_eq!(factory.call_sweep_day.read().unwrap(), next_day);
            assert_eq!(factory.call_latest_day.read().unwrap(), next_day);
        });
    }

    /// Days that closed while the sweep was still walking an older one are walked after
    /// it, in order, none skipped.
    #[test]
    fn days_closed_behind_a_running_call_sweep_are_walked_in_order() {
        crate::tests::with_factory(|s| {
            let oracle = OracleContract::new(s.clone());
            let pair = super::setup_pair(&oracle);
            seed_called_range(&s, 20260101..20260101 + SWEEP_WRITES_PER_BLOCK + 1);
            let mut closed = Vec::new();
            for offset in 0..3 {
                let ts = ISSUED_AT as u64 + (60 + offset) * DAY;
                let day = previous_date_key(timestamp_to_date_key(ts)).expect("date step");
                super::fill_days(&oracle, day, pair, WINDOW_DAYS, U256::from(TRIGGER + 1));
                let ctx = BlockRuntimeContext::new(
                    BlockContext::empty_for_tests(1 + offset, ts, CHAIN_ID),
                    s.clone(),
                );
                // Only the first trigger's block walks: the later days arrive meanwhile.
                match offset {
                    0 => called::scan_and_call(&ctx).map(drop),
                    _ => called::run_daily(&ctx),
                }
                .unwrap();
                closed.push(day);
            }
            let factory = IntexContract::new(s.clone());
            assert_eq!(factory.call_sweep_day.read().unwrap(), closed[0]);
            assert_eq!(factory.call_latest_day.read().unwrap(), closed[2]);

            let ctx = BlockRuntimeContext::new(
                BlockContext::empty_for_tests(4, ISSUED_AT as u64 + 62 * DAY, CHAIN_ID),
                s.clone(),
            );
            let mut walked = vec![closed[0]];
            while walked.last() != Some(&0) && walked.len() < 8 {
                called::run_call_slice(&ctx).unwrap();
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
        crate::tests::with_factory(|s| {
            let oracle = OracleContract::new(s.clone());
            let first = super::setup_pair(&oracle);
            let second = setup_second_pair(&oracle);
            let scan_ts = ISSUED_AT as u64 + 60 * DAY;
            let day = previous_date_key(timestamp_to_date_key(scan_ts)).expect("date step");
            super::fill_days(&oracle, day, first, WINDOW_DAYS, U256::from(TRIGGER + 1));
            super::fill_days(&oracle, day, second, WINDOW_DAYS, U256::from(TRIGGER + 1));

            // Issued five days ago: every group is decided and left where it is.
            let young_at = (scan_ts - 5 * DAY) as u32;
            for d in 20260101..20260101 + SWEEP_VISITS_PER_BLOCK + 1 {
                seed_series_for(&s, REFERENCE_ISO, d, young_at, TRIGGER);
                seed_series_for(&s, SECOND_ISO, d, young_at, TRIGGER);
            }

            let ctx = BlockRuntimeContext::new(
                BlockContext::empty_for_tests(1, scan_ts, CHAIN_ID),
                s.clone(),
            );
            called::scan_and_call(&ctx).unwrap();
            run_call_slices(&ctx, 3);
            assert_eq!(
                IntexContract::new(s.clone()).call_sweep_day.read().unwrap(),
                0,
                "the sweep closed"
            );
        });
    }
}

mod called_pstar {
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

    /// Write one window's days, latest first. `None` leaves the day unpriced.
    fn seed_window(oracle: &OracleContract, pair: AddressPair, days: &[Option<u64>]) {
        let mut day = LAST_DAY;
        for value in days {
            if let Some(v) = value {
                super::set_vwap(oracle, day, pair, U256::from(*v));
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
            crate::tests::with_factory(|s| {
                let oracle = OracleContract::new(s.clone());
                let pair = super::setup_pair(&oracle);
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
        crate::tests::with_factory(|s| {
            let oracle = OracleContract::new(s.clone());
            let pair = super::setup_pair(&oracle);
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
        crate::tests::with_factory(|s| {
            let oracle = OracleContract::new(s.clone());
            let pair = super::setup_pair(&oracle);
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
        crate::tests::with_factory(|s| {
            let oracle = OracleContract::new(s.clone());
            let pair = super::setup_pair(&oracle);
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
        crate::tests::with_factory(|s| {
            let oracle = OracleContract::new(s.clone());
            let pair = super::setup_pair(&oracle);
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
        let series_id =
            SeriesId::for_pair(WorldwideDay::new(LAST_DAY), 840, REFERENCE_ISO).unwrap();
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
                },
                issued_at,
                issuance_currency: 840,
                reference_currency: REFERENCE_ISO,
            },
        )
        .unwrap();
        series_id
    }
}

#[test]
fn a_group_is_called_once() {
    with_factory(|s| {
        runtime::issue_intex(&s, sample(9)).unwrap();
        outbe_intex::api::call_group_of(&s, sid(9), ISSUED_AT + 10).unwrap();
        let series = outbe_intex::api::read_series(&s, sid(9)).unwrap();
        assert_eq!(series_state(&s, sid(9)), outbe_intex::IntexState::Called);
        assert_eq!(
            outbe_intex::api::call_stamp(&s, &series).unwrap().called_at,
            ISSUED_AT + 10
        );
        assert!(outbe_intex::api::call_group_of(&s, sid(9), ISSUED_AT + 20).is_err());
    });
}
