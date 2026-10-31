//! Daily call-scan tests: the 21-of-28 breach rule and the seven-day forfeit.
//!
//! Every test drives the real [`crate::called::scan_and_call`] and the forfeit
//! queue through [`scan`] against seeded oracle history, so the breach count is
//! recomputed from the finalized daily VWAP series exactly as it is in a block.

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolEvent;
use outbe_oracle::{api::AddressPair, schema::OracleContract};
use outbe_primitives::pass_budget::{PassBudget, SWEEP_VISITS_PER_BLOCK, SWEEP_WRITES_PER_BLOCK};
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::time::SECONDS_PER_DAY_U32;
use outbe_primitives::{
    addresses::NOD_ADDRESS,
    block::{BlockContext, BlockRuntimeContext},
    math::constants::MAX_BIN_ID,
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
    time::{date_key_to_utc_timestamp, first_full_day, previous_date_key, timestamp_to_date_key},
};

use outbe_nod::{
    api,
    constants::{
        CALL_LOOKBACK_DAYS, CALL_NOTICE_PERIOD, CALL_RATE_PCT, CALL_THRESHOLD, CALL_THRESHOLD_DAYS,
        CALL_WINDOW,
    },
    precompile::INod,
    NodContract, NodItemState,
};

const CHAIN_ID: u64 = 1;
const BLOCK_NUMBER: u64 = 42;
use outbe_primitives::time::SECONDS_PER_DAY as DAY;
/// The notice period a group seals at issuance, in the width these tests
/// do timestamp arithmetic in.
const NOTICE: u64 = CALL_NOTICE_PERIOD as u64;
/// Adding this delay to a deadline advances time past the end of its UTC day.
const CLOSED_DAY_DELAY: u64 = DAY;
const ISO: u16 = 840;
const OTHER_ISO: u16 = 978;
/// Issuance instant, 2027-01-15 08:00 UTC.
const START: u64 = 1_800_000_000;

/// The worldwide day [`START`] falls in. The breach walk now stops at
/// `first_full_day(issued_at)`, not this WWD key. The two still have to
/// agree so fixtures that reason in WWD days do not silently miss the
/// issuance cutoff.
const WWD: u32 = 20_270_115;

/// Entry price every group here is issued at: 2.0 at scale 1e6. The call price
/// is therefore `2.0 x 3.56 = 7.12`.
fn entry_price() -> U256 {
    U256::from(2_000_000u64)
}

/// Exactly the call price (7.12). The breach test is strict `>`, so a day at
/// this value must NOT count.
fn at_call() -> U256 {
    U256::from(7_120_000u64)
}

/// A day above the call price.
fn above_call() -> U256 {
    U256::from(7_200_000u64)
}

/// A day below the call price.
fn below_call() -> U256 {
    U256::from(4_000_000u64)
}

fn seed_production_nod_genesis(storage: &StorageHandle<'_>) {
    // These tests exercise the production call terms.
    NodContract::new(storage.clone())
        .config_profile
        .write(outbe_nod::config::PROFILE_PROD)
        .unwrap();
}

fn nod_item(owner: Address, iso: u16) -> NodItemState {
    nod_item_at(owner, iso, entry_price())
}

/// A Nod whose group is keyed by its entry price, so distinct entries give
/// distinct groups on the same worldwide day.
fn nod_item_at(owner: Address, iso: u16, entry_price_minor: U256) -> NodItemState {
    nod_item_issued(owner, iso, entry_price_minor, WWD, START)
}

fn nod_item_issued(
    owner: Address,
    iso: u16,
    entry_price_minor: U256,
    worldwide_day: u32,
    issued_at: u64,
) -> NodItemState {
    let worldwide_day = WorldwideDay::new(worldwide_day);
    outbe_nod::test_support::item(
        outbe_nod::test_support::NodItemFixture {
            is_settled: false,
            nod_id: outbe_nod::test_support::nod_id_of(owner, worldwide_day.value(), iso, iso),
            owner,
            gratis_load_minor: U256::from(11),
            emission_day: worldwide_day.value(),
            league_id: 4,
            group_key: outbe_nod::identity::group_key(
                issued_at,
                outbe_primitives::markup::marked_up(entry_price_minor, CALL_RATE_PCT).unwrap(),
                iso,
            )
            .expect("date step"),
            issuance_currency: iso,
            reference_currency: iso,
            issued_at,
        },
        entry_price_minor,
    )
}

/// Registers `COEN/<iso>` and lists `iso` as a reference currency.
fn register(storage: &StorageHandle<'_>, iso: u16) {
    if outbe_oracle::api::coen_pair_index_opt(storage.clone(), iso)
        .unwrap()
        .is_none()
    {
        outbe_oracle::api::register_pair(storage.clone(), AddressPair::new_coen_to(iso)).unwrap();
    }
}

fn last_closed_day(timestamp: u64) -> u32 {
    previous_date_key(timestamp_to_date_key(timestamp)).expect("date step")
}

fn bump_watermark(storage: &StorageHandle<'_>, utc_day: u32) {
    let oracle = OracleContract::new(storage.clone());
    if oracle.utc_day_vwap_last_finalized.read().unwrap() < utc_day {
        oracle.utc_day_vwap_last_finalized.write(utc_day).unwrap();
    }
}

/// Publishes a finalized daily reference price for one UTC day on `COEN/<iso>`.
fn set_vwap_for(storage: &StorageHandle<'_>, iso: u16, utc_day: u32, value: U256) {
    let index = outbe_oracle::api::coen_pair_index_opt(storage.clone(), iso)
        .unwrap()
        .expect("the pair must be registered before its series is seeded");
    let oracle = OracleContract::new(storage.clone());
    oracle.record_utc_day_vwap(utc_day, index, value).unwrap();
    bump_watermark(storage, utc_day);
}

/// Sets `days` consecutive closed UTC days ending at `latest` to `value`.
fn fill_days_for(storage: &StorageHandle<'_>, iso: u16, latest: u32, days: u32, value: U256) {
    let mut day = latest;
    for _ in 0..days {
        set_vwap_for(storage, iso, day, value);
        day = previous_date_key(day).expect("date step");
    }
}

fn fill_days(storage: &StorageHandle<'_>, latest: u32, days: u32, value: U256) {
    fill_days_for(storage, ISO, latest, days, value);
}

/// Advances the watermark without publishing any price.
fn finalize_through(storage: &StorageHandle<'_>, timestamp: u64) {
    bump_watermark(storage, last_closed_day(timestamp));
}

/// Issues one Nod owned by `owner`.
fn issue_owned(storage: &StorageHandle<'_>, owner: Address, iso: u16) -> NodItemState {
    issue_item(storage, nod_item(owner, iso))
}

fn issue_item(storage: &StorageHandle<'_>, item: NodItemState) -> NodItemState {
    add(storage, &item);
    item
}

thread_local! {
    /// Groups this test issued into, for `slice` to see which leave the forfeit queue.
    static ISSUED_GROUPS: std::cell::RefCell<std::collections::HashSet<B256>> =
        std::cell::RefCell::default();
}

/// Records `item` as NodFactory issues it: the terms its entry price seals and its load.
fn add(storage: &StorageHandle<'_>, item: &NodItemState) {
    ISSUED_GROUPS.with(|groups| groups.borrow_mut().insert(item.group_key));
    api::add_nod(
        storage,
        item,
        crate::runtime::call_terms(storage, item.entry_price_minor(), item.reference_currency)
            .unwrap(),
        api::calculation_amount(item).unwrap(),
    )
    .unwrap();
}

/// One block's call scan and forfeits, then its cleanup. Returns the groups called and
/// forfeited.
fn scan(storage: &StorageHandle<'_>, timestamp: u64) -> u32 {
    let nod = NodContract::new(storage.clone());
    let mut decided = 0;
    // This loop simulates catch-up blocks. It never refills a pending day's allowance within one block.
    loop {
        decided += slice(storage, timestamp);
        let selected =
            outbe_primitives::pass_budget::BlockAllowance::unpack(nod.daily_budget.read().unwrap())
                .unwrap()
                .unwrap()
                .day;
        let next = nod.call_sweep_day.read().unwrap();
        if next == 0 || next == selected {
            break;
        }
    }
    decided
}

/// The daily pass and its cleanup under one checkpoint, as the native transaction runs them.
/// Returns the groups forfeited.
fn forfeit_block(
    storage: &StorageHandle<'_>,
    timestamp: u64,
) -> outbe_primitives::error::Result<u32> {
    storage.with_checkpoint(|| {
        let forfeited = forfeit_pass(storage, timestamp)?;
        crate::cleanup::cleanup(&block_at(storage, timestamp))?;
        Ok(forfeited)
    })
}

/// The daily pass through the closed days before `timestamp`, one day per block, without its
/// cleanup. Returns the groups it forfeited.
fn forfeit_pass(
    storage: &StorageHandle<'_>,
    timestamp: u64,
) -> outbe_primitives::error::Result<u32> {
    let queued: Vec<_> = ISSUED_GROUPS.with(|groups| {
        groups
            .borrow()
            .iter()
            .copied()
            .filter(|group| is_queued(storage, *group))
            .collect()
    });
    let nod = NodContract::new(storage.clone());
    loop {
        crate::called::process_call_pass(&block_at(storage, timestamp))?;
        let selected =
            outbe_primitives::pass_budget::BlockAllowance::unpack(nod.daily_budget.read()?)?
                .map_or(0, |allowance| allowance.day);
        let next = nod.call_sweep_day.read()?;
        if next == 0 || next == selected {
            assert!(
                next == 0 || selected == nod.call_latest_day.read()?,
                "the pass stopped on day {selected} before the last closed day"
            );
            break;
        }
    }
    Ok(queued
        .into_iter()
        .filter(|group| !is_queued(storage, *group) && nod.group_is_forfeited.read(group).unwrap())
        .count() as u32)
}

fn block_at<'s>(storage: &StorageHandle<'s>, timestamp: u64) -> BlockRuntimeContext<'s> {
    let nod = NodContract::new(storage.clone());
    if nod.daily_genesis_day.read().unwrap() == 0 {
        // This fixture starts its pass on the first observation day under test.
        nod.daily_genesis_day
            .write(last_closed_day(timestamp))
            .unwrap();
    }
    let height = [
        nod.daily_budget.read().unwrap(),
        nod.cleanup_budget.read().unwrap(),
    ]
    .into_iter()
    .filter_map(|packed| outbe_primitives::pass_budget::BlockAllowance::unpack(packed).unwrap())
    .map(|previous| previous.height + 1)
    .max()
    .unwrap_or(BLOCK_NUMBER);
    BlockRuntimeContext::new(
        BlockContext::empty_for_tests(height, timestamp, CHAIN_ID),
        storage.clone(),
    )
}

fn is_queued(storage: &StorageHandle<'_>, group_key: B256) -> bool {
    NodContract::new(storage.clone())
        .called_group_slot
        .read(&group_key)
        .unwrap()
        != 0
}

/// Promis Reserve balance - what a forfeited `gratis_load_minor` returns to.
fn reserve(storage: &StorageHandle<'_>) -> U256 {
    outbe_promislimit::PromisLimitContract::new(storage.clone())
        .promis_limit()
        .unwrap()
}

fn called_at(storage: &StorageHandle<'_>, group_key: B256) -> u64 {
    NodContract::new(storage.clone())
        .group_called_at
        .read(&group_key)
        .unwrap()
}

/// Runs `body` with EVM storage and the
/// default `COEN/840` pair registered.
fn harness(body: impl FnOnce(&StorageHandle<'_>)) {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);

    StorageHandle::enter(&mut provider, |storage| {
        seed_production_nod_genesis(&storage);

        register(&storage, ISO);
        body(&storage);
    });
}

/// Issues `specs` as (owner-byte, load, paid) into one group,
/// force-calls it, and leaves the clock past the sealed notice.
fn arm_lapsed(storage: &StorageHandle<'_>, specs: &[(u8, u64, bool)]) -> (B256, Vec<NodItemState>) {
    let items: Vec<NodItemState> = specs
        .iter()
        .map(|&(owner, load, _)| {
            let mut item = nod_item(Address::repeat_byte(owner), ISO);
            outbe_nod::test_support::set_amount(&mut item, U256::from(load));
            add(storage, &item);
            item
        })
        .collect();
    let group_key = items[0].group_key;
    let id = group_key;
    for (item, &(_, _, paid)) in items.iter().zip(specs) {
        if paid {
            api::record_settled(
                storage,
                api::load_item(storage, item.nod_id).unwrap().unwrap(),
                api::load_group(storage, id).unwrap().unwrap(),
            )
            .unwrap();
        }
    }
    let at = START + 30 * DAY;
    fill_days(
        storage,
        last_closed_day(at),
        CALL_LOOKBACK_DAYS,
        above_call(),
    );
    assert_eq!(scan(storage, at), 1);
    finalize_through(storage, at + NOTICE + CLOSED_DAY_DELAY);
    (group_key, items)
}

/// Each `NodGroupForfeited` as (Nods forfeited, Gratis load returned).
fn group_forfeits(provider: &HashMapStorageProvider) -> Vec<(u32, U256)> {
    provider
        .get_events(NOD_ADDRESS)
        .iter()
        .filter_map(|log| INod::NodGroupForfeited::decode_log_data(log).ok())
        .map(|event| (event.nods, event.gratisLoadMinor))
        .collect()
}

// --- Sealed call terms -----------------------------------------------------

struct SealedCallTerms {
    window_days: u32,
    threshold_days: u32,
    notice_days: u32,
}

/// Rewrites sealed terms and widens the currency high-water mark as at issuance.
fn reterm(storage: &StorageHandle<'_>, group_key: B256, iso: u16, terms: SealedCallTerms) {
    let nod = NodContract::new(storage.clone());
    let window = terms.window_days * SECONDS_PER_DAY_U32;
    nod.group_call_window_seconds
        .write(&group_key, window)
        .unwrap();
    nod.group_call_threshold_seconds
        .write(&group_key, terms.threshold_days * SECONDS_PER_DAY_U32)
        .unwrap();
    nod.group_call_notice_period_seconds
        .write(&group_key, terms.notice_days * SECONDS_PER_DAY_U32)
        .unwrap();
    outbe_primitives::call_breach::widen_scan_terms(
        &nod.max_call_window_seconds,
        &nod.min_call_threshold_seconds,
        iso,
        window,
        terms.threshold_days * SECONDS_PER_DAY_U32,
    )
    .unwrap();
}

/// Issuance seals the terms, reads the constants exactly once, and enrolls the
/// group in the call index.
#[test]
fn issuance_seals_the_call_terms_on_the_group() {
    harness(|storage| {
        let item = nod_item(Address::repeat_byte(0x11), ISO);
        add(storage, &item);
        let nod = NodContract::new(storage.clone());
        assert_eq!(
            nod.group_call_rate.read(&item.group_key).unwrap(),
            CALL_RATE_PCT
        );
        assert_eq!(
            nod.group_call_window_seconds.read(&item.group_key).unwrap(),
            CALL_WINDOW
        );
        assert_eq!(
            nod.group_call_threshold_seconds
                .read(&item.group_key)
                .unwrap(),
            CALL_THRESHOLD
        );
        assert_eq!(
            nod.group_call_notice_period_seconds
                .read(&item.group_key)
                .unwrap(),
            CALL_NOTICE_PERIOD
        );
        assert_eq!(nod.max_call_window_seconds.read(&ISO).unwrap(), CALL_WINDOW);
        assert_eq!(
            api::get_group(storage, item.group_key)
                .unwrap()
                .unwrap()
                .full_day_after_issue
                .value(),
            first_full_day(START).expect("date step")
        );
        assert_ne!(
            nod.call_group_slot.read(&item.group_key).unwrap(),
            0,
            "issuance puts the group in its call bin"
        );
    });
}

/// Off mainnet an unset profile seals the DEV terms, as Gem and Intex run them.
#[test]
fn an_unset_profile_seals_the_dev_terms_off_mainnet() {
    harness(|storage| {
        let nod = NodContract::new(storage.clone());
        nod.config_profile
            .write(outbe_nod::config::PROFILE_AUTO)
            .unwrap();
        let dev = outbe_nod::config::NodParams::DEV;
        let mut item = nod_item(Address::repeat_byte(0x11), ISO);
        item.group_key = outbe_nod::identity::group_key(
            item.issued_at,
            outbe_primitives::markup::marked_up(entry_price(), dev.call_rate).unwrap(),
            ISO,
        )
        .expect("date step");
        add(storage, &item);
        assert_eq!(
            (
                nod.group_call_rate.read(&item.group_key).unwrap(),
                nod.group_call_window_seconds.read(&item.group_key).unwrap(),
                nod.group_call_threshold_seconds
                    .read(&item.group_key)
                    .unwrap(),
                nod.group_call_notice_period_seconds
                    .read(&item.group_key)
                    .unwrap(),
            ),
            (
                dev.call_rate,
                dev.call_window_seconds,
                dev.call_threshold_seconds,
                dev.call_notice_period_seconds
            )
        );
    });
}

/// A Nod of a called group's Worldwide Day issued after the call opens its own group:
/// a group is closed to new members before it can be called.
#[test]
fn a_late_nod_opens_a_new_group_instead_of_joining_a_called_one() {
    harness(|storage| {
        let at = START + 30 * DAY;
        let first = call_group(storage, Address::repeat_byte(0x11), at);
        let late = issue_item(
            storage,
            nod_item_issued(
                Address::repeat_byte(0x22),
                ISO,
                entry_price(),
                WWD,
                at + CLOSED_DAY_DELAY,
            ),
        );
        assert_ne!(first.group_key, late.group_key);
        assert_eq!(called_at(storage, first.group_key), at);
        assert_eq!(called_at(storage, late.group_key), 0);

        let past = at + NOTICE + CLOSED_DAY_DELAY;
        finalize_through(storage, past);
        assert_eq!(scan(storage, past), 1);
        assert!(api::get_item(storage, first.nod_id).unwrap().is_none());
        assert!(api::get_item(storage, late.nod_id).unwrap().is_some());
    });
}

/// The terms a group is called and forfeited under are the ones sealed at
/// issuance, not the live constants. A `const` cannot be retuned at
/// runtime, so this test proves it from the other side. It rewrites what the
/// group holds and checks that the scan follows the group rather than the
/// constant.
#[test]
fn the_scan_follows_the_terms_sealed_on_the_group_not_the_constants() {
    harness(|storage| {
        let item = issue_owned(storage, Address::repeat_byte(0x11), ISO);
        let at = START + 30 * DAY;
        let latest = last_closed_day(at);
        fill_days(storage, latest, CALL_LOOKBACK_DAYS, below_call());
        // Three breach days at the head - far short of the 21 the constant
        // demands, and exactly the threshold the group will carry.
        fill_days(storage, latest, 3, above_call());

        assert_eq!(
            scan(storage, at),
            0,
            "the constant's 21-of-28 threshold is unmet"
        );

        reterm(
            storage,
            item.group_key,
            ISO,
            SealedCallTerms {
                window_days: 3,
                threshold_days: 3,
                notice_days: 1,
            },
        );
        assert_eq!(
            scan(storage, at),
            0,
            "completed days cannot reopen after retuning"
        );
        let at = at + DAY;
        set_vwap_for(storage, ISO, last_closed_day(at), above_call());
        assert_eq!(scan(storage, at), 1);
        assert_eq!(called_at(storage, item.group_key), at);

        // And the sealed notice governs the forfeit: one day, not seven.
        assert_eq!(
            api::settlement_deadline(storage, item.group_key).unwrap(),
            at + DAY,
            "the deadline follows the sealed notice period"
        );
        finalize_through(storage, at + DAY + CLOSED_DAY_DELAY);
        assert_eq!(scan(storage, at + DAY), 0, "not past it yet");
        assert_eq!(scan(storage, at + DAY + CLOSED_DAY_DELAY), 1);
        assert!(api::get_item(storage, item.nod_id).unwrap().is_none());
    });
}

/// A group whose sealed window outruns the current constant still gets its
/// whole span collected. The scan sizes the shared per-currency window off the
/// `max_call_window_seconds` high-water mark, not off the constant.
#[test]
fn a_window_wider_than_the_constant_is_collected_in_full() {
    harness(|storage| {
        const WIDE_DAYS: u32 = 40;
        let item = issue_owned(storage, Address::repeat_byte(0x11), ISO);
        let at = START + 60 * DAY;
        fill_days(storage, last_closed_day(at), WIDE_DAYS, above_call());

        // 35 of the 40 days must breach, which no 28-day window can supply.
        reterm(
            storage,
            item.group_key,
            ISO,
            SealedCallTerms {
                window_days: WIDE_DAYS,
                threshold_days: 35,
                notice_days: 7,
            },
        );
        assert_eq!(scan(storage, at), 1);
        assert_eq!(called_at(storage, item.group_key), at);
    });
}

// --- Call arm --------------------------------------------------------------

#[test]
fn the_call_price_is_the_entry_price_times_the_call_rate() {
    harness(|storage| {
        let item = issue_owned(storage, Address::repeat_byte(0x11), ISO);
        let stored = NodContract::new(storage.clone())
            .group_call_price_minor
            .read(&item.group_key)
            .unwrap();
        assert_eq!(
            stored,
            entry_price() * U256::from(100 + CALL_RATE_PCT) / U256::from(100)
        );
        assert_eq!(stored, at_call(), "2.0 x 3.56 == 7.12");
    });
}

#[test]
fn a_full_window_above_the_call_price_calls_the_group() {
    harness(|storage| {
        let item = issue_owned(storage, Address::repeat_byte(0x11), ISO);
        let at = START + 30 * DAY;
        fill_days(
            storage,
            last_closed_day(at),
            CALL_LOOKBACK_DAYS,
            above_call(),
        );

        assert_eq!(scan(storage, at), 1);
        assert_eq!(called_at(storage, item.group_key), at);

        // Idempotent: a called group is not called twice.
        assert_eq!(scan(storage, at + DAY), 0);
        assert_eq!(called_at(storage, item.group_key), at);
    });
}

#[test]
fn one_breach_day_short_of_the_threshold_does_not_call() {
    harness(|storage| {
        let item = issue_owned(storage, Address::repeat_byte(0x11), ISO);
        let at = START + 30 * DAY;
        let latest = last_closed_day(at);
        fill_days(storage, latest, CALL_LOOKBACK_DAYS, below_call());
        fill_days(storage, latest, CALL_THRESHOLD_DAYS - 1, above_call());

        assert_eq!(scan(storage, at), 0);
        assert_eq!(called_at(storage, item.group_key), 0);
    });
}

fn call_progress_revert(storage: &StorageHandle<'_>, nod_id: U256) -> alloy_primitives::Bytes {
    match crate::call_progress::call_progress(storage, nod_id) {
        Err(outbe_primitives::error::PrecompileError::RevertBytes(reason)) => reason,
        other => panic!("expected a revert, got {other:?}"),
    }
}

#[test]
fn the_call_progress_is_one_day_short_until_the_group_is_called() {
    use crate::precompile::INodFactory;
    use alloy_sol_types::SolError;

    harness(|storage| {
        let item = issue_owned(storage, Address::repeat_byte(0x11), ISO);
        let at = START + 30 * DAY;
        let latest = last_closed_day(at);
        fill_days(storage, latest, CALL_LOOKBACK_DAYS, below_call());
        fill_days(storage, latest, CALL_THRESHOLD_DAYS - 1, above_call());

        let progress = crate::call_progress::call_progress(storage, item.nod_id).unwrap();
        assert_eq!(
            (progress.callWindowDays, progress.callThresholdDays),
            (CALL_LOOKBACK_DAYS, CALL_THRESHOLD_DAYS)
        );
        assert_eq!(
            (
                progress.daysChecked,
                progress.daysAboveCallPrice,
                progress.minDaysToCall
            ),
            (CALL_LOOKBACK_DAYS, CALL_THRESHOLD_DAYS - 1, 1)
        );
        assert_eq!(progress.referenceCurrency, ISO);
        assert_eq!(progress.callPriceMinor, at_call());
        assert_eq!(progress.lastDayPriceMinor, above_call());

        fill_days(storage, latest, CALL_LOOKBACK_DAYS, above_call());
        assert_eq!(scan(storage, at), 1);
        assert_eq!(
            call_progress_revert(storage, item.nod_id),
            INodFactory::NodNotIssued {}.abi_encode()
        );
        assert_eq!(
            call_progress_revert(storage, U256::from(1u64)),
            INodFactory::NodNotFound {}.abi_encode()
        );
    });
}

#[test]
fn the_window_absorbs_below_call_days_up_to_the_slack() {
    harness(|storage| {
        let item = issue_owned(storage, Address::repeat_byte(0x11), ISO);
        let at = START + 30 * DAY;
        let latest = last_closed_day(at);
        fill_days(storage, latest, CALL_LOOKBACK_DAYS, above_call());

        // Scatter exactly the slack (28 - 21 = 7) below-call days through the
        // window; the count, not a streak, is what decides.
        let slack = CALL_LOOKBACK_DAYS - CALL_THRESHOLD_DAYS;
        let mut day = latest;
        let mut dropped = 0;
        let mut offset = 0;
        while dropped < slack {
            if offset % 3 == 0 {
                set_vwap_for(storage, ISO, day, below_call());
                dropped += 1;
            }
            day = previous_date_key(day).expect("date step");
            offset += 1;
        }

        assert_eq!(scan(storage, at), 1);
        assert_eq!(called_at(storage, item.group_key), at);
    });
}

#[test]
fn a_day_exactly_at_the_call_price_does_not_count_as_a_breach() {
    harness(|storage| {
        let item = issue_owned(storage, Address::repeat_byte(0x11), ISO);
        let at = START + 30 * DAY;
        fill_days(storage, last_closed_day(at), CALL_LOOKBACK_DAYS, at_call());

        assert_eq!(scan(storage, at), 0);
        assert_eq!(called_at(storage, item.group_key), 0);
    });
}

#[test]
fn missing_days_do_not_count_as_breaches() {
    harness(|storage| {
        let item = issue_owned(storage, Address::repeat_byte(0x11), ISO);
        let at = START + 30 * DAY;
        let latest = last_closed_day(at);
        // Only one day short of the threshold is published; the rest are absent.
        fill_days(storage, latest, CALL_THRESHOLD_DAYS - 1, above_call());
        finalize_through(storage, at);

        assert_eq!(scan(storage, at), 0);
        assert_eq!(called_at(storage, item.group_key), 0);
    });
}

#[test]
fn a_breach_run_that_predates_the_group_does_not_call_it() {
    harness(|storage| {
        let item = issue_owned(storage, Address::repeat_byte(0x11), ISO);
        // Scan two days after issuance. Only a couple of closed UTC days sit at
        // or after `first_full_day(issued_at)`. The scan ignores the
        // pre-existence run beyond them, even though the whole series breaches.
        let at = START + 2 * DAY;
        fill_days(
            storage,
            last_closed_day(at),
            CALL_LOOKBACK_DAYS,
            above_call(),
        );

        assert_eq!(scan(storage, at), 0);
        assert_eq!(called_at(storage, item.group_key), 0);
    });
}

/// Q027: the issuance UTC day counts only when the Nod was issued at midnight.
/// A second later drops that day and a threshold-length breach falls short.
#[test]
fn the_issue_day_counts_only_for_a_nod_issued_at_midnight() {
    let scan_at = START + 30 * DAY;
    let latest = last_closed_day(scan_at);
    let mut oldest_breach = latest;
    for _ in 1..CALL_THRESHOLD_DAYS {
        oldest_breach = previous_date_key(oldest_breach).expect("date step");
    }
    let midnight = date_key_to_utc_timestamp(oldest_breach).expect("date step");

    for (issued_at, expected) in [(midnight, 1u32), (midnight + 1, 0u32)] {
        harness(|storage| {
            let wwd = WorldwideDay::from_timestamp(issued_at).value();
            let item = issue_item(
                storage,
                nod_item_issued(
                    Address::repeat_byte(0x11),
                    ISO,
                    entry_price(),
                    wwd,
                    issued_at,
                ),
            );
            fill_days(storage, latest, CALL_LOOKBACK_DAYS, below_call());
            fill_days(storage, latest, CALL_THRESHOLD_DAYS, above_call());
            assert_eq!(scan(storage, scan_at), expected, "issued at {issued_at}");
            assert_eq!(
                called_at(storage, item.group_key) != 0,
                expected == 1,
                "issued at {issued_at}"
            );
        });
    }
}

/// Q027: a delayed materialization must not inherit VWAP days from its Tribute
/// WWD. A 21-day breach run that sits on or after WWD but before
/// `first_full_day(issued_at)` would have called under the old cutoff.
#[test]
fn a_delayed_issuance_does_not_count_pre_issuance_wwd_days() {
    harness(|storage| {
        // WWD a month before issuance. 21 closed UTC days ending on the
        // partial issuance day all post-date that WWD and all predate
        // `first_full_day(START)` = 2027-01-16.
        let delayed_wwd = 20_261_216;
        let item = issue_item(
            storage,
            nod_item_issued(
                Address::repeat_byte(0x11),
                ISO,
                entry_price(),
                delayed_wwd,
                START,
            ),
        );
        let issuance_utc_day = timestamp_to_date_key(START);
        fill_days(storage, issuance_utc_day, CALL_THRESHOLD_DAYS, above_call());
        let scan_at = date_key_to_utc_timestamp(first_full_day(START).expect("date step"))
            .expect("date step");
        finalize_through(storage, scan_at);
        assert_eq!(scan(storage, scan_at), 0);
        assert_eq!(called_at(storage, item.group_key), 0);
    });
}

#[test]
fn an_unfinalized_day_skips_the_run_without_touching_state() {
    harness(|storage| {
        let item = issue_owned(storage, Address::repeat_byte(0x11), ISO);
        let at = START + 30 * DAY;
        fill_days(
            storage,
            last_closed_day(at),
            CALL_LOOKBACK_DAYS,
            above_call(),
        );

        // Ask a day later than anything the oracle has finalized.
        let ahead = at + 2 * DAY;
        assert_eq!(scan(storage, ahead), 0);
        assert_eq!(called_at(storage, item.group_key), 0);
    });
}

#[test]
fn each_group_reads_its_own_currency_series() {
    harness(|storage| {
        register(storage, OTHER_ISO);
        let usd = issue_owned(storage, Address::repeat_byte(0x11), ISO);
        let eur = issue_owned(storage, Address::repeat_byte(0x22), OTHER_ISO);

        let at = START + 30 * DAY;
        let latest = last_closed_day(at);
        // Only the EUR series breaches.
        fill_days_for(storage, ISO, latest, CALL_LOOKBACK_DAYS, below_call());
        fill_days_for(storage, OTHER_ISO, latest, CALL_LOOKBACK_DAYS, above_call());

        assert_eq!(scan(storage, at), 1);
        assert_eq!(called_at(storage, usd.group_key), 0);
        assert_eq!(called_at(storage, eur.group_key), at);
    });
}

// --- Forfeit sweep ---------------------------------------------------------

/// Calls a group at `at`, returning the issued Nod.
fn call_group(storage: &StorageHandle<'_>, owner: Address, at: u64) -> NodItemState {
    let item = issue_owned(storage, owner, ISO);
    fill_days(
        storage,
        last_closed_day(at),
        CALL_LOOKBACK_DAYS,
        above_call(),
    );
    assert_eq!(scan(storage, at), 1);
    item
}

#[test]
fn the_notice_period_burns_once_the_day_of_its_deadline_closes() {
    harness(|storage| {
        let at = START + 30 * DAY;
        let item = call_group(storage, Address::repeat_byte(0x11), at);
        let deadline = at + NOTICE;

        let day_end = outbe_primitives::deadline_queue::day_end(
            outbe_primitives::deadline_queue::deadline_day(deadline).unwrap(),
        );
        // Exact settlement state is independent of delayed physical deletion.
        for now in [deadline, deadline + 1, day_end - 1] {
            finalize_through(storage, now);
            assert_eq!(scan(storage, now), 0);
            assert!(api::get_item(storage, item.nod_id).unwrap().is_some());
        }

        // The first midnight after the deadline makes the queue entry eligible.
        finalize_through(storage, day_end);
        assert_eq!(scan(storage, day_end), 1);
        assert!(api::get_item(storage, item.nod_id).unwrap().is_none());
    });
}

#[test]
fn forfeiting_the_last_member_drops_the_group_from_the_call_index() {
    harness(|storage| {
        let at = START + 30 * DAY;
        let item = call_group(storage, Address::repeat_byte(0x11), at);
        let nod = NodContract::new(storage.clone());
        assert!(is_queued(storage, item.group_key));
        assert_eq!(
            nod.call_group_slot.read(&item.group_key).unwrap(),
            0,
            "the call moved the group out of its bin"
        );

        let past = at + NOTICE + CLOSED_DAY_DELAY;
        finalize_through(storage, past);
        assert_eq!(scan(storage, past), 1);

        assert!(!is_queued(storage, item.group_key));
        assert_eq!(nod.group_called_at.read(&item.group_key).unwrap(), 0);
        assert_eq!(nod.group_nod_count.read(&item.group_key).unwrap(), 0);
        // The group body is gone with its last member.
        let group_id = item.group_key;
        assert!(api::get_group(storage, group_id).unwrap().is_none());
    });
}

// --- Member index ----------------------------------------------------------

#[test]
fn the_member_index_tracks_issuance_and_removal_in_a_group() {
    harness(|storage| {
        // Two owners on the same worldwide day share one group: identical entry
        // price and currency.
        let a = issue_owned(storage, Address::repeat_byte(0x11), ISO);
        let b_item = nod_item(Address::repeat_byte(0x22), ISO);
        add(storage, &b_item);
        assert_eq!(a.group_key, b_item.group_key, "same group");

        let nod = NodContract::new(storage.clone());
        assert_eq!(nod.group_nod_count.read(&a.group_key).unwrap(), 2);
        let group_id = a.group_key;
        let original = nod.groups.get(group_id).unwrap().unwrap();

        // Removing one keeps the group unchanged and clears only its own entry.
        let item = api::load_item(storage, a.nod_id).unwrap().unwrap();
        let group = api::load_group(storage, group_id).unwrap().unwrap();
        api::remove_nod(storage, item, group).unwrap();

        let nod = NodContract::new(storage.clone());
        assert_eq!(nod.group_nod_count.read(&a.group_key).unwrap(), 1);
        assert_eq!(nod.groups.get(group_id).unwrap().unwrap(), original);
        assert_eq!(
            nod.group_nod_index.read(&a.nod_id).unwrap(),
            0,
            "the removed Nod's reverse index entry is cleared"
        );
        // The survivor is still reachable at slot 0.
        assert_eq!(
            nod.group_nods
                .read(&outbe_nod::index_keys::group_nod_key(a.group_key, 0))
                .unwrap(),
            b_item.nod_id
        );
    });
}

/// A group is callable from issuance, and its members settle once it is called.
#[test]
fn a_group_is_callable_from_issuance_and_settles_once_called() {
    harness(|storage| {
        let item = nod_item(Address::repeat_byte(0x11), ISO);
        add(storage, &item);

        let at = START + 30 * DAY;
        fill_days(
            storage,
            last_closed_day(at),
            CALL_LOOKBACK_DAYS,
            above_call(),
        );
        assert_eq!(scan(storage, at), 1);
        assert_eq!(called_at(storage, item.group_key), at);

        let id = item.group_key;
        let group = api::load_group(storage, id).unwrap().unwrap();
        api::record_settled(
            storage,
            api::load_item(storage, item.nod_id).unwrap().unwrap(),
            group,
        )
        .unwrap();
    });
}

#[test]
fn every_member_of_a_lapsed_group_burns_in_one_pass() {
    harness(|storage| {
        // Three owners on the same day, entry price and currency share one group.
        let owners = [0x11u8, 0x22, 0x33].map(Address::repeat_byte);
        let items: Vec<NodItemState> = owners
            .iter()
            .map(|owner| {
                let item = nod_item(*owner, ISO);
                add(storage, &item);
                item
            })
            .collect();

        let at = START + 30 * DAY;
        fill_days(
            storage,
            last_closed_day(at),
            CALL_LOOKBACK_DAYS,
            above_call(),
        );
        assert_eq!(scan(storage, at), 1);

        let past = at + NOTICE + CLOSED_DAY_DELAY;
        finalize_through(storage, past);
        assert_eq!(scan(storage, past), 1, "the group forfeits as one");

        for item in &items {
            assert!(
                api::get_item(storage, item.nod_id).unwrap().is_none(),
                "nod {} survived the forfeit",
                item.nod_id
            );
        }
    });
}

#[test]
fn a_lapsed_group_returns_every_forfeited_load_to_the_promis_reserve() {
    harness(|storage| {
        let owners = [0x11u8, 0x22, 0x33].map(Address::repeat_byte);
        let items: Vec<NodItemState> = owners
            .iter()
            .map(|owner| {
                let item = nod_item(*owner, ISO);
                add(storage, &item);
                item
            })
            .collect();

        let at = START + 30 * DAY;
        fill_days(
            storage,
            last_closed_day(at),
            CALL_LOOKBACK_DAYS,
            above_call(),
        );
        assert_eq!(scan(storage, at), 1);

        // The call alone forfeits nothing, so nothing is returned yet.
        assert_eq!(reserve(storage), U256::ZERO);

        let past = at + NOTICE + CLOSED_DAY_DELAY;
        finalize_through(storage, past);
        assert_eq!(scan(storage, past), 1);

        let expected: U256 = items
            .iter()
            .map(|item| outbe_nod::api::calculation_amount(item).unwrap())
            .sum();
        assert_eq!(reserve(storage), expected);
    });
}

#[test]
fn a_group_inside_its_notice_waits_while_its_neighbours_burn() {
    harness(|storage| {
        // Three distinct groups: distinct entry prices on one worldwide day.
        let items: Vec<NodItemState> = [11u64, 22, 33]
            .into_iter()
            .zip([0x11u8, 0x22, 0x33])
            .map(|(offset, owner)| {
                let entry = entry_price() + U256::from(offset);
                let item = nod_item_at(Address::repeat_byte(owner), ISO, entry);
                add(storage, &item);
                item
            })
            .collect();
        let at = START + 30 * DAY;
        fill_days(
            storage,
            last_closed_day(at),
            CALL_LOOKBACK_DAYS,
            above_call(),
        );
        assert_eq!(scan(storage, at), 3, "all three call");

        // Keep the middle group inside its notice. The sweep burns the two around it.
        let mut nod = NodContract::new(storage.clone());
        let kept = items[1].group_key;
        let past = at + NOTICE + CLOSED_DAY_DELAY;
        nod.remove_called(kept).unwrap();
        nod.push_called(kept, past + NOTICE).unwrap();
        finalize_through(storage, past);
        assert_eq!(scan(storage, past), 2);

        for item in &items {
            assert_eq!(
                api::get_item(storage, item.nod_id).unwrap().is_some(),
                item.group_key == kept,
                "only the group still inside its notice survives"
            );
            assert_eq!(is_queued(storage, item.group_key), item.group_key == kept);
        }
    });
}

/// A walk that runs out of visits inside a bin resumes there. The walk neither
/// visits twice nor skips the group that a call swap-pops into each hole.
#[test]
fn a_bin_walk_that_runs_out_resumes_inside_the_bin() {
    harness(|storage| {
        let at = START + 30 * DAY;
        // One call bin; the last group is too young to hold the threshold.
        let items: Vec<NodItemState> = [
            (11u64, 0x11u8, START),
            (22, 0x22, START),
            (33, 0x33, at - 5 * DAY),
        ]
        .into_iter()
        .map(|(offset, owner, issued_at)| {
            let entry = entry_price() + U256::from(offset);
            let item = nod_item_issued(Address::repeat_byte(owner), ISO, entry, WWD, issued_at);
            add(storage, &item);
            item
        })
        .collect();
        let latest = last_closed_day(at);
        fill_days(storage, latest, CALL_LOOKBACK_DAYS, above_call());
        let mut day = latest;
        let window = outbe_oracle::call_window::CallWindow::from_vwaps(
            (0..CALL_LOOKBACK_DAYS)
                .map(|_| {
                    let entry = (day, Some(above_call()));
                    day = previous_date_key(day).expect("date step");
                    entry
                })
                .collect(),
            CALL_THRESHOLD_DAYS,
        );
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(BLOCK_NUMBER, at, CHAIN_ID),
            storage.clone(),
        );
        let mut nod = NodContract::new(storage.clone());

        // Two visits: the young group on top, then the middle one, which is called.
        let mut visits = PassBudget::new(2, u32::MAX);
        assert_eq!(
            crate::called::call_currency(
                &ctx,
                &mut nod,
                crate::called::CurrencyScan {
                    iso_code: ISO,
                    window: &window,
                    ceiling: MAX_BIN_ID,
                },
                &mut visits,
            )
            .unwrap(),
            (1, false)
        );
        let mut visits = PassBudget::new(SWEEP_VISITS_PER_BLOCK, u32::MAX);
        assert_eq!(
            crate::called::call_currency(
                &ctx,
                &mut nod,
                crate::called::CurrencyScan {
                    iso_code: ISO,
                    window: &window,
                    ceiling: MAX_BIN_ID,
                },
                &mut visits,
            )
            .unwrap(),
            (1, true)
        );
        assert_eq!(
            visits.visits_left(),
            SWEEP_VISITS_PER_BLOCK - 1,
            "the resumed walk visits only what was left"
        );
        assert_eq!(called_at(storage, items[0].group_key), at);
        assert_eq!(called_at(storage, items[1].group_key), at);
        assert_eq!(called_at(storage, items[2].group_key), 0);
        let bin = outbe_primitives::call_bins::price_to_bin(at_call()).unwrap();
        assert_eq!(
            nod.call_bin_count
                .read(&outbe_primitives::call_bins::scoped(ISO, bin))
                .unwrap(),
            1
        );
        assert_eq!(
            nod.call_bin_groups
                .read(&outbe_primitives::call_bins::bin_index_key(ISO, bin, 0))
                .unwrap(),
            items[2].group_key
        );
        nod.remove_call_bin(items[2].group_key).unwrap();
        assert_eq!(
            nod.call_bin_count
                .read(&outbe_primitives::call_bins::scoped(ISO, bin))
                .unwrap(),
            0
        );
    });
}

fn slice(storage: &StorageHandle<'_>, timestamp: u64) -> u32 {
    let groups: Vec<_> = ISSUED_GROUPS.with(|groups| {
        groups
            .borrow()
            .iter()
            .copied()
            .filter(|group| is_queued(storage, *group))
            .collect()
    });
    let ctx = block_at(storage, timestamp);
    let called = crate::called::process_call_pass(&ctx).unwrap();
    crate::cleanup::cleanup(&ctx).unwrap();
    called
        + groups
            .into_iter()
            .filter(|group| !is_queued(storage, *group))
            .count() as u32
}

/// A forfeited group's Nods read Forfeited until the cleanup deletes them, and the
/// deletion keeps supply and the owner's list in step.
#[test]
fn a_forfeited_group_reads_forfeited_until_the_cleanup_deletes_it() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let past = START + 30 * DAY + NOTICE + CLOSED_DAY_DELAY;
    let items = StorageHandle::enter(&mut provider, |storage| {
        seed_production_nod_genesis(&storage);
        register(&storage, ISO);
        let (group_key, items) = arm_lapsed(&storage, &[(0x61, 5, false), (0x62, 7, false)]);
        assert_eq!(forfeit_pass(&storage, past).unwrap(), 1);
        assert_eq!(reserve(&storage), U256::from(12u64));
        for item in &items {
            let stored = api::get_item(&storage, item.nod_id).unwrap().unwrap();
            assert_eq!(
                api::effective_state(
                    &stored,
                    called_at(&storage, group_key),
                    api::settlement_deadline(&storage, group_key).unwrap(),
                    U256::from(past),
                ),
                outbe_nod::EffectiveState::Forfeited
            );
        }

        assert_eq!(
            crate::cleanup::cleanup(&block_at(&storage, past)).unwrap(),
            2
        );
        assert_eq!(
            reserve(&storage),
            U256::from(12u64),
            "the cleanup moves no value"
        );
        assert_eq!(api::balance_of(&storage, items[0].owner).unwrap(), 0);
        assert!(api::get_group(&storage, group_key).unwrap().is_none());
        items
    });
    let burns = provider
        .get_events(NOD_ADDRESS)
        .iter()
        .filter_map(|log| INod::Transfer::decode_log_data(log).ok())
        .filter(|event| event.to == Address::ZERO)
        .count();
    assert_eq!(burns, items.len());
    assert_eq!(group_forfeits(&provider), vec![(2, U256::from(12u64))]);
}

#[test]
fn a_group_over_the_cleanup_budget_is_deleted_across_blocks_and_credited_once() {
    harness(|storage| {
        let specs: Vec<(u8, u64, bool)> = (0..SWEEP_WRITES_PER_BLOCK as u8 + 1)
            .map(|n| (0x40 + n, 1, false))
            .collect();
        let (group_key, _) = arm_lapsed(storage, &specs);
        let past = START + 30 * DAY + NOTICE + CLOSED_DAY_DELAY;
        assert_eq!(scan(storage, past), 1);
        let nod = NodContract::new(storage.clone());
        assert_eq!(nod.group_nod_count.read(&group_key).unwrap(), 1);
        assert_eq!(reserve(storage), U256::from(specs.len()));
        let allowance = outbe_primitives::pass_budget::BlockAllowance::unpack(
            nod.cleanup_budget.read().unwrap(),
        )
        .unwrap()
        .unwrap();
        let same_height = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(allowance.height, past, CHAIN_ID),
            storage.clone(),
        );
        assert_eq!(crate::cleanup::cleanup(&same_height).unwrap(), 0);
        assert_eq!(nod.group_nod_count.read(&group_key).unwrap(), 1);
        let after = outbe_primitives::pass_budget::BlockAllowance::unpack(
            nod.cleanup_budget.read().unwrap(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(after.budget.writes_left(), 0);
        assert!(after.budget.visits_left() <= allowance.budget.visits_left());

        assert_eq!(slice(storage, past + 1), 0);
        assert_eq!(nod.group_nod_count.read(&group_key).unwrap(), 0);
        assert_eq!(reserve(storage), U256::from(specs.len()));
    });
}

/// Failing members of a forfeited group do not prevent cleanup of the other members.
/// The cleanup deletes the other members. The group waits for the current UTC day to close.
#[test]
fn failing_members_do_not_hold_the_cleanup() {
    harness(|storage| {
        let specs: Vec<(u8, u64, bool)> = (0..5u8).map(|n| (0x60 + n, 5, false)).collect();
        let (group_key, _items) = arm_lapsed(storage, &specs);
        let nod = NodContract::new(storage.clone());
        // The top member slots name Nods with no body, each reverting on its own.
        for index in 3..specs.len() as u32 {
            let ghost = U256::from_be_bytes([index as u8; 32]);
            nod.group_nods
                .write(
                    &outbe_nod::index_keys::group_nod_key(group_key, index),
                    ghost,
                )
                .unwrap();
        }

        let past = START + 30 * DAY + NOTICE + CLOSED_DAY_DELAY;
        assert_eq!(forfeit_pass(storage, past).unwrap(), 1);
        assert_eq!(
            crate::cleanup::cleanup(&block_at(storage, past)).unwrap(),
            3
        );
        assert_eq!(reserve(storage), U256::from(25u64));
        let (day, _) = outbe_primitives::deadline_queue::unpack_slot(
            nod.cleanup_group_slot.read(&group_key).unwrap(),
        );
        assert_eq!(
            day,
            outbe_primitives::deadline_queue::deadline_day(past).unwrap()
        );
    });
}

/// A forfeit and its cleanup read no encrypted load, so this node's enclave is not needed.
#[test]
fn a_forfeit_does_not_need_the_enclave() {
    harness(|storage| {
        let (group_key, _) = arm_lapsed(storage, &[(0x61, 5, false)]);
        let past = START + 30 * DAY + NOTICE + CLOSED_DAY_DELAY;

        outbe_nod::test_support::uninstall();
        let forfeited = forfeit_pass(storage, past);
        let deleted = crate::cleanup::cleanup(&block_at(storage, past));
        outbe_nod::test_support::install();
        assert_eq!(forfeited.unwrap(), 1);
        assert_eq!(deleted.unwrap(), 1);
        assert!(!is_queued(storage, group_key));
        assert_eq!(reserve(storage), U256::from(5u64));
    });
}

#[test]
fn pending_calls_hold_forfeits_and_same_height_reentry_keeps_the_allowance() {
    harness(|storage| {
        register(storage, OTHER_ISO);
        let at = START + 30 * DAY;
        fill_days(
            storage,
            last_closed_day(at),
            CALL_LOOKBACK_DAYS,
            above_call(),
        );
        let lapsed = nod_item(Address::repeat_byte(0x11), ISO);
        add(storage, &lapsed);
        assert_eq!(scan(storage, at), 1);

        // Settled groups still take a visit each and use up the budget in the first currency.
        let mut nod = NodContract::new(storage.clone());
        for index in 1..=SWEEP_VISITS_PER_BLOCK {
            let key = B256::left_padding_from(&index.to_be_bytes());
            nod.group_currency.write(&key, ISO).unwrap();
            nod.group_call_price_minor.write(&key, at_call()).unwrap();
            nod.insert_call_bin(key).unwrap();
        }
        let fresh = nod_item(Address::repeat_byte(0x22), OTHER_ISO);
        add(storage, &fresh);
        let past = at + NOTICE + CLOSED_DAY_DELAY;
        let latest = last_closed_day(past);
        fill_days(storage, latest, CALL_LOOKBACK_DAYS, above_call());
        fill_days_for(storage, OTHER_ISO, latest, CALL_LOOKBACK_DAYS, above_call());
        // The daily triggers in between walked their own days.
        nod.call_latest_day
            .write(previous_date_key(latest).expect("date step"))
            .unwrap();

        let ctx = block_at(storage, past);
        assert_eq!(crate::called::process_call_pass(&ctx).unwrap(), 0);
        assert_eq!(
            nod.call_currency_cursor.read().unwrap(),
            u32::from(OTHER_ISO)
        );
        assert_eq!(called_at(storage, fresh.group_key), 0);
        assert_eq!(
            nod.group_nod_count.read(&lapsed.group_key).unwrap(),
            1,
            "forfeits wait for all calls in the pinned day"
        );
        let allowance = nod.daily_budget.read().unwrap();
        assert_eq!(crate::called::process_call_pass(&ctx).unwrap(), 0);
        assert_eq!(
            nod.daily_budget.read().unwrap(),
            allowance,
            "same-height entry cannot refill visits"
        );
        assert_eq!(nod.daily_phase.read().unwrap(), 0);

        assert_eq!(
            slice(storage, past),
            2,
            "the next block finishes calls and then forfeits"
        );
        assert_eq!(called_at(storage, fresh.group_key), past);
        assert_eq!(nod.group_nod_count.read(&lapsed.group_key).unwrap(), 0);
        assert_eq!(nod.call_sweep_day.read().unwrap(), 0);
    });
}

#[test]
fn mixed_group_forfeits_only_unpaid_loads_and_preserves_paid_terms_until_mined() {
    harness(|storage| {
        let items: Vec<_> = [0x91, 0x92, 0x93, 0x95]
            .into_iter()
            .map(|seed| {
                let item = nod_item(Address::repeat_byte(seed), ISO);
                add(storage, &item);
                item
            })
            .collect();
        let key = items[0].group_key;
        let id = key;
        let nod = NodContract::new(storage.clone());
        // Settle the middle member, exercising swap-remove of the unpaid tail.
        api::record_settled(
            storage,
            api::load_item(storage, items[1].nod_id).unwrap().unwrap(),
            api::load_group(storage, id).unwrap().unwrap(),
        )
        .unwrap();
        api::record_settled(
            storage,
            api::load_item(storage, items[3].nod_id).unwrap().unwrap(),
            api::load_group(storage, id).unwrap().unwrap(),
        )
        .unwrap();
        api::remove_nod(
            storage,
            api::load_item(storage, items[3].nod_id).unwrap().unwrap(),
            api::load_group(storage, id).unwrap().unwrap(),
        )
        .unwrap();
        let group = api::get_group(storage, id).unwrap().unwrap();
        assert_eq!(group.settled_nods, 1);
        assert_eq!(nod.group_nod_count.read(&key).unwrap(), 2);
        let at = START + 30 * DAY;
        fill_days(
            storage,
            last_closed_day(at),
            CALL_LOOKBACK_DAYS,
            above_call(),
        );
        assert_eq!(scan(storage, at), 1);
        let past = at + NOTICE + CLOSED_DAY_DELAY;
        finalize_through(storage, past);
        // A failed Promis credit leaves the group whole for the retry.
        let mut limit = outbe_promislimit::PromisLimitContract::new(storage.clone());
        limit.checked_take_up_to(U256::MAX).unwrap();
        limit.checked_credit(U256::MAX).unwrap();
        assert_eq!(scan(storage, past), 0);
        assert_eq!(nod.group_nod_count.read(&key).unwrap(), 2);
        assert!(api::get_item(storage, items[0].nod_id).unwrap().is_some());
        assert!(api::get_item(storage, items[2].nod_id).unwrap().is_some());
        assert_eq!(reserve(storage), U256::MAX);
        limit.checked_take_up_to(U256::MAX).unwrap();
        // The failed group retries at the next UTC midnight.
        let retry = outbe_primitives::deadline_queue::day_end(
            outbe_primitives::deadline_queue::deadline_day(past).unwrap(),
        );
        finalize_through(storage, retry);
        assert_eq!(scan(storage, retry), 1);
        let expected = outbe_nod::api::calculation_amount(&items[0]).unwrap()
            + outbe_nod::api::calculation_amount(&items[2]).unwrap();
        assert_eq!(reserve(storage), expected);
        assert!(!is_queued(storage, key), "no unpaid member is left to burn");
        assert_eq!(scan(storage, retry), 0);
        assert_eq!(reserve(storage), expected);
        let group = api::get_group(storage, id).unwrap().unwrap();
        assert_eq!(group.settled_nods, 1);
        assert_eq!(nod.group_nod_count.read(&key).unwrap(), 0);
        assert_eq!(called_at(storage, key), at);
        assert_eq!(
            nod.read_call_terms(key).unwrap().call_notice_period_seconds,
            CALL_NOTICE_PERIOD
        );
        api::remove_nod(
            storage,
            api::load_item(storage, items[1].nod_id).unwrap().unwrap(),
            api::load_group(storage, id).unwrap().unwrap(),
        )
        .unwrap();
        assert!(api::get_group(storage, id).unwrap().is_none());
        assert!(!is_queued(storage, key));
        assert_eq!(reserve(storage), expected);
    });
}

#[test]
fn a_fully_paid_group_is_not_called_and_corrupt_paid_membership_is_not_forfeited() {
    harness(|storage| {
        let item = issue_owned(storage, Address::repeat_byte(0x94), ISO);
        let id = item.group_key;
        api::record_settled(
            storage,
            api::load_item(storage, item.nod_id).unwrap().unwrap(),
            api::load_group(storage, id).unwrap().unwrap(),
        )
        .unwrap();
        let at = START + 30 * DAY;
        fill_days(
            storage,
            last_closed_day(at),
            CALL_LOOKBACK_DAYS,
            above_call(),
        );
        assert_eq!(scan(storage, at), 0);
        assert_eq!(called_at(storage, item.group_key), 0);
        // Issuance into a paid-only group must retain its committed paid state.
        let newcomer = nod_item(Address::repeat_byte(0x96), ISO);
        let paid_group = api::get_group(storage, id).unwrap().unwrap();
        add(storage, &newcomer);
        assert_eq!(
            outbe_nod::canonical_group(&api::get_group(storage, id).unwrap().unwrap()),
            outbe_nod::canonical_group(&paid_group),
        );
        api::remove_nod(
            storage,
            api::load_item(storage, newcomer.nod_id).unwrap().unwrap(),
            api::load_group(storage, id).unwrap().unwrap(),
        )
        .unwrap();
        assert_eq!(
            api::get_group(storage, id).unwrap().unwrap().settled_nods,
            1
        );
        let nod = NodContract::new(storage.clone());
        nod.group_called_at.write(&item.group_key, at).unwrap();
        nod.group_nod_count.write(&item.group_key, 1).unwrap();
        nod.group_nods
            .write(
                &outbe_nod::index_keys::group_nod_key(item.group_key, 0),
                item.nod_id,
            )
            .unwrap();
        let past = at + NOTICE + CLOSED_DAY_DELAY;
        finalize_through(storage, past);
        assert_eq!(scan(storage, past), 0);
        assert!(
            api::get_item(storage, item.nod_id)
                .unwrap()
                .unwrap()
                .is_settled
        );
        assert_eq!(reserve(storage), U256::ZERO);
    });
}

/// A group paid in full leaves its bin once no Nod can join it, so later walks do not
/// visit it again. Its paid Nod stays with the group until mined.
#[test]
fn a_group_paid_in_full_leaves_its_call_bin() {
    harness(|storage| {
        let item = issue_owned(storage, Address::repeat_byte(0x31), ISO);
        api::record_settled(
            storage,
            api::load_item(storage, item.nod_id).unwrap().unwrap(),
            api::load_group(storage, item.group_key).unwrap().unwrap(),
        )
        .unwrap();
        let nod = NodContract::new(storage.clone());
        assert_ne!(nod.call_group_slot.read(&item.group_key).unwrap(), 0);

        let at = START + 30 * DAY;
        fill_days(
            storage,
            last_closed_day(at),
            CALL_LOOKBACK_DAYS,
            above_call(),
        );
        assert_eq!(scan(storage, at), 0);
        assert_eq!(nod.call_group_slot.read(&item.group_key).unwrap(), 0);
        assert_eq!(called_at(storage, item.group_key), 0);
        assert!(api::get_group(storage, item.group_key).unwrap().is_some());
    });
}

/// An unfinished call sweep keeps the day it opened on: a later clock whose
/// own window would not call still force-calls against the pinned day's
/// trailing VWAP.
#[test]
fn a_running_call_sweep_keeps_its_day() {
    harness(|storage| {
        let item = issue_owned(storage, Address::repeat_byte(0x21), ISO);
        let at = START + 30 * DAY;
        let day = last_closed_day(at);
        let later = at + 21 * DAY;
        let later_day = last_closed_day(later);

        fill_days(storage, later_day, CALL_LOOKBACK_DAYS, below_call());
        fill_days(storage, day, CALL_THRESHOLD_DAYS, above_call());

        let nod = NodContract::new(storage.clone());
        nod.call_sweep_day.write(day).unwrap();
        nod.call_latest_day.write(day).unwrap();
        nod.daily_genesis_day.write(day).unwrap();

        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(BLOCK_NUMBER, later, CHAIN_ID),
            storage.clone(),
        );
        crate::called::process_call_pass(&ctx).unwrap();
        assert_eq!(called_at(storage, item.group_key), later);
        assert_eq!(
            nod.call_sweep_day.read().unwrap(),
            outbe_primitives::time::next_date_key(day).expect("date step"),
            "only the pinned day ran at this height"
        );
    });
}

#[test]
fn the_daily_pass_forfeits_without_the_daily_trigger() {
    harness(|storage| {
        let (group_key, _) = arm_lapsed(storage, &[(0x97, 3, false)]);
        let nod = NodContract::new(storage.clone());
        assert_eq!(
            nod.call_sweep_day.read().unwrap(),
            0,
            "no call sweep in flight"
        );

        let past = START + 30 * DAY + NOTICE + CLOSED_DAY_DELAY;
        forfeit_pass(storage, past).unwrap();
        crate::hooks::cleanup(&block_at(storage, past)).unwrap();
        assert_eq!(nod.group_nod_count.read(&group_key).unwrap(), 0);
        assert!(!is_queued(storage, group_key));
        assert_eq!(reserve(storage), U256::from(3u64));
    });
}

#[test]
fn a_node_local_failure_while_calling_a_group_fails_the_slice() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);

    let at = START + 30 * DAY;
    let group_key = StorageHandle::enter(&mut provider, |storage| {
        seed_production_nod_genesis(&storage);

        register(&storage, ISO);
        let item = issue_owned(&storage, Address::repeat_byte(0x61), ISO);
        fill_days(
            &storage,
            last_closed_day(at),
            CALL_LOOKBACK_DAYS,
            above_call(),
        );
        NodContract::new(storage.clone())
            .call_sweep_day
            .write(last_closed_day(at))
            .unwrap();
        let nod = NodContract::new(storage.clone());
        nod.daily_genesis_day.write(last_closed_day(at)).unwrap();
        nod.call_latest_day.write(last_closed_day(at)).unwrap();
        item.group_key
    });
    provider.fail_after_mutation_at(0);
    let result = StorageHandle::enter(&mut provider, |storage| {
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(BLOCK_NUMBER, at, CHAIN_ID),
            storage.clone(),
        );
        crate::called::process_call_pass(&ctx)
    });
    provider.clear_mutation_failure();
    assert!(matches!(
        result,
        Err(outbe_primitives::error::PrecompileError::Storage(_))
    ));
    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(called_at(&storage, group_key), 0);
    });
}

/// Days that closed while a call sweep was stalled are walked in order after it,
/// none skipped.
#[test]
fn days_closed_behind_a_stalled_call_sweep_are_walked_in_order() {
    harness(|storage| {
        issue_owned(storage, Address::repeat_byte(0x22), ISO);

        let at = START + 30 * DAY;
        let closed = [
            last_closed_day(at),
            last_closed_day(at + DAY),
            last_closed_day(at + 2 * DAY),
        ];
        fill_days(storage, closed[2], CALL_LOOKBACK_DAYS, below_call());

        let nod = NodContract::new(storage.clone());
        nod.call_sweep_day.write(closed[0]).unwrap();
        nod.daily_genesis_day.write(closed[0]).unwrap();
        nod.call_latest_day.write(closed[0]).unwrap();
        nod.call_bin_cursor.write(&ISO, 1).unwrap();

        // Only the newest day's trigger fires.
        crate::called::schedule_call_pass(&block_at(storage, at + 2 * DAY)).unwrap();
        assert_eq!(nod.call_sweep_day.read().unwrap(), closed[0]);
        assert_eq!(nod.call_latest_day.read().unwrap(), closed[2]);
        assert_eq!(
            nod.call_bin_cursor.read(&ISO).unwrap(),
            1,
            "the walk in flight was not restarted"
        );

        for next in [closed[1], closed[2], 0] {
            slice(storage, at + 2 * DAY);
            assert_eq!(nod.call_sweep_day.read().unwrap(), next);
        }
        assert_eq!(nod.call_latest_day.read().unwrap(), closed[2]);
    });
}

/// Promis Limit moves by the unpaid loads, not by reaching
/// Forfeited or emitting `NodGroupForfeited`. A paid-but-unmined member is
/// not credited. If `checked_credit` were removed, this delta
/// would fail while the events and burns could still look complete.
#[test]
fn forfeit_credits_distinct_unpaid_loads_and_ignores_paid_members() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);

    StorageHandle::enter(&mut provider, |storage| {
        seed_production_nod_genesis(&storage);

        register(&storage, ISO);
        let (group_key, items) = arm_lapsed(
            &storage,
            &[
                (0x31, 3, false),
                (0x32, 7, true),
                (0x33, 11, false),
                (0x34, 20, false),
            ],
        );
        let unpaid: U256 = items
            .iter()
            .zip([(false, 3u64), (true, 7), (false, 11), (false, 20)])
            .filter_map(|(item, (paid, _))| {
                (!paid).then_some(outbe_nod::api::calculation_amount(item).unwrap())
            })
            .fold(U256::ZERO, |sum, load| sum + load);
        assert_eq!(unpaid, U256::from(34u64));
        let past = START + 30 * DAY + NOTICE + CLOSED_DAY_DELAY;
        assert_eq!(forfeit_block(&storage, past).unwrap(), 1);
        assert_eq!(reserve(&storage), unpaid);
        assert_eq!(forfeit_block(&storage, past + CLOSED_DAY_DELAY).unwrap(), 0);
        assert_eq!(reserve(&storage), unpaid);
        assert_eq!(
            NodContract::new(storage.clone())
                .group_nod_count
                .read(&group_key)
                .unwrap(),
            0
        );
        assert!(
            api::get_item(&storage, items[1].nod_id)
                .unwrap()
                .unwrap()
                .is_settled
        );
    });
    assert_eq!(group_forfeits(&provider), vec![(3, U256::from(34u64))]);
}

#[test]
fn native_storage_out_of_gas_reverts_the_forfeit_transaction_then_retries_once() {
    use outbe_primitives::{error::PrecompileError, system_tx::SYSTEM_TX_ARTIFACT_GAS_LIMIT};

    let specs = [(0x71u8, 3u64, false), (0x72, 5, false), (0x73, 8, false)];
    let past = START + 30 * DAY + NOTICE + CLOSED_DAY_DELAY;
    let arm = |provider: &mut HashMapStorageProvider| {
        StorageHandle::enter(provider, |storage| {
            seed_production_nod_genesis(&storage);

            register(&storage, ISO);
            arm_lapsed(&storage, &specs)
        })
    };
    let mut probe = HashMapStorageProvider::new(CHAIN_ID);

    arm(&mut probe);
    probe.enable_production_storage_gas_metering();
    probe.set_gas_limit(SYSTEM_TX_ARTIFACT_GAS_LIMIT);
    let full_pass_gas = StorageHandle::enter(&mut probe, |storage| {
        assert_eq!(forfeit_block(&storage, past).unwrap(), 1);
        storage.gas_used().unwrap()
    });
    assert!(full_pass_gas > 0);

    let mut provider = HashMapStorageProvider::new(CHAIN_ID);

    let (group_key, _) = arm(&mut provider);
    let before = provider.storage.clone();
    let events = provider.get_ordered_events().to_vec();
    provider.enable_production_storage_gas_metering();
    provider.set_gas_limit(full_pass_gas / 2);
    let result = StorageHandle::enter(&mut provider, |storage| {
        // The native transaction owns the outer checkpoint. A hard storage
        // OOG must restore the credit and the deletions together with queue metadata.
        forfeit_block(&storage, past)
    });
    assert!(
        matches!(result, Err(PrecompileError::OutOfGas)),
        "{result:?}"
    );
    assert_eq!(provider.storage, before);
    assert_eq!(provider.get_ordered_events(), events);

    provider.set_gas_limit(SYSTEM_TX_ARTIFACT_GAS_LIMIT);
    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(forfeit_block(&storage, past).unwrap(), 1);
        let nod = NodContract::new(storage.clone());
        assert_eq!(nod.group_nod_count.read(&group_key).unwrap(), 0);
        assert_eq!(reserve(&storage), U256::from(16u64));
        assert!(!is_queued(&storage, group_key));
    });
    assert_eq!(group_forfeits(&provider), vec![(3, U256::from(16u64))]);
}

#[test]
fn configured_forfeit_slice_fits_the_native_storage_gas_limit() {
    use outbe_primitives::system_tx::SYSTEM_TX_ARTIFACT_GAS_LIMIT;

    let mut provider = HashMapStorageProvider::new(CHAIN_ID);

    let at = START + 30 * DAY;
    let past = at + NOTICE + CLOSED_DAY_DELAY;
    let issued = StorageHandle::enter(&mut provider, |storage| {
        seed_production_nod_genesis(&storage);

        register(&storage, ISO);
        let items: Vec<NodItemState> = (1..=SWEEP_WRITES_PER_BLOCK)
            .map(|owner| {
                let mut item = nod_item(Address::left_padding_from(&owner.to_be_bytes()), ISO);
                outbe_nod::test_support::set_amount(&mut item, U256::from(owner));
                add(&storage, &item);
                item
            })
            .collect();
        fill_days(
            &storage,
            last_closed_day(at),
            CALL_LOOKBACK_DAYS,
            above_call(),
        );
        assert_eq!(scan(&storage, at), 1);
        finalize_through(&storage, past);
        items
    });
    let expected = issued
        .iter()
        .map(|item| outbe_nod::api::calculation_amount(item).unwrap())
        .fold(U256::ZERO, |sum, load| sum + load);
    provider.enable_production_storage_gas_metering();
    provider.set_gas_limit(SYSTEM_TX_ARTIFACT_GAS_LIMIT);
    let native_gas = StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(scan(&storage, past), 1);
        let nod = NodContract::new(storage.clone());
        assert_eq!(nod.group_nod_count.read(&issued[0].group_key).unwrap(), 0);
        assert_eq!(nod.call_sweep_day.read().unwrap(), 0);
        assert_eq!(reserve(&storage), expected);
        storage.gas_used().unwrap()
    });
    let (reads, writes) = provider.metered_storage_operations();
    assert!(reads > 0 && writes > 0);
    assert!(native_gas >= reads * 100 + writes * 5_000);
    assert!(native_gas <= SYSTEM_TX_ARTIFACT_GAS_LIMIT);
    assert_eq!(
        group_forfeits(&provider),
        vec![(SWEEP_WRITES_PER_BLOCK, expected)]
    );
}

#[test]
fn a_node_local_failure_while_forfeiting_fails_the_slice() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);

    let past = START + 30 * DAY + NOTICE + CLOSED_DAY_DELAY;
    let group_key = StorageHandle::enter(&mut provider, |storage| {
        seed_production_nod_genesis(&storage);

        register(&storage, ISO);
        arm_lapsed(&storage, &[(0x81, 3, false)]).0
    });
    provider.fail_mutation_at_address(outbe_primitives::addresses::PROMIS_LIMIT_ADDRESS);
    let result = StorageHandle::enter(&mut provider, |storage| forfeit_pass(&storage, past));
    provider.clear_mutation_failure();
    assert!(matches!(
        result,
        Err(outbe_primitives::error::PrecompileError::Storage(_))
    ));
    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(
            NodContract::new(storage.clone())
                .group_nod_count
                .read(&group_key)
                .unwrap(),
            1
        );
        assert_eq!(reserve(&storage), U256::ZERO);
    });
}

/// T15/A18.6: every persistent write of a forfeit and its cleanup (credit, event,
/// deletion) rolls back with its checkpoint. Retry credits the original unpaid
/// loads once, neither zero nor twice.
#[test]
fn every_nod_forfeit_mutation_rolls_back_then_retries_the_same_credit() {
    let specs = [(0x51u8, 3u64, false), (0x52, 11, false), (0x53, 20, false)];
    let expected = U256::from(34u64);

    let mut probe = HashMapStorageProvider::new(CHAIN_ID);

    StorageHandle::enter(&mut probe, |storage| {
        seed_production_nod_genesis(&storage);

        register(&storage, ISO);
        arm_lapsed(&storage, &specs);
    });
    let past = START + 30 * DAY + NOTICE + CLOSED_DAY_DELAY;
    probe.fail_after_mutation_at(usize::MAX);
    StorageHandle::enter(&mut probe, |storage| {
        forfeit_block(&storage, past).unwrap();
    });
    let mutation_count = probe.clear_mutation_failure();
    assert!(
        mutation_count >= 4,
        "a three-member forfeit must write the credit, the event and the deletions"
    );
    assert_eq!(group_forfeits(&probe), vec![(3, expected)]);

    for operation in 0..mutation_count {
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);

        let group_key = StorageHandle::enter(&mut provider, |storage| {
            seed_production_nod_genesis(&storage);

            register(&storage, ISO);
            arm_lapsed(&storage, &specs).0
        });
        let storage_before = provider.storage.clone();
        let events_before = provider.events.clone();
        let ordered_before = provider.get_ordered_events().to_vec();

        provider.fail_after_mutation_at(operation);
        let failed = StorageHandle::enter(&mut provider, |storage| forfeit_block(&storage, past));
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
            assert_eq!(forfeit_block(&storage, past).unwrap(), 1);
            assert_eq!(reserve(&storage), expected);
            assert_eq!(forfeit_block(&storage, past + CLOSED_DAY_DELAY).unwrap(), 0);
            assert_eq!(reserve(&storage), expected);
            assert_eq!(
                NodContract::new(storage.clone())
                    .group_nod_count
                    .read(&group_key)
                    .unwrap(),
                0
            );
        });
    }
}

#[test]
fn a_call_pass_announces_one_batch_metadata_update() {
    use alloy_sol_types::SolEvent;

    let mut provider = HashMapStorageProvider::new(CHAIN_ID);

    StorageHandle::enter(&mut provider, |storage| {
        seed_production_nod_genesis(&storage);

        register(&storage, ISO);
        register(&storage, OTHER_ISO);
        issue_owned(&storage, Address::repeat_byte(0x11), ISO);
        issue_owned(&storage, Address::repeat_byte(0x22), OTHER_ISO);
        let at = START + 30 * DAY;
        let latest = last_closed_day(at);
        fill_days_for(&storage, ISO, latest, CALL_LOOKBACK_DAYS, above_call());
        fill_days_for(
            &storage,
            OTHER_ISO,
            latest,
            CALL_LOOKBACK_DAYS,
            above_call(),
        );

        assert_eq!(scan(&storage, at), 2);
        assert_eq!(scan(&storage, at + DAY), 0);
    });

    let batches: Vec<(U256, U256)> = provider
        .get_events(outbe_primitives::addresses::NOD_ADDRESS)
        .iter()
        .filter_map(|log| {
            outbe_nod::precompile::INod::BatchMetadataUpdate::decode_log_data(log).ok()
        })
        .map(|event| (event._fromTokenId, event._toTokenId))
        .collect();
    assert_eq!(batches, vec![(U256::ZERO, U256::MAX)]);
}

#[test]
fn token_uri_turns_forfeited_past_the_settlement_deadline() {
    use alloy_sol_types::SolCall;
    use base64::Engine;
    use outbe_nod::precompile::{dispatch, INod};

    let mut provider = HashMapStorageProvider::new(CHAIN_ID);

    let at = START + 30 * DAY;
    let item = StorageHandle::enter(&mut provider, |storage| {
        seed_production_nod_genesis(&storage);

        register(&storage, ISO);
        call_group(&storage, Address::repeat_byte(0x71), at)
    });
    let json_at = |provider: &mut HashMapStorageProvider, timestamp: u64| {
        provider.set_timestamp(U256::from(timestamp));
        StorageHandle::enter(provider, |storage| {
            let data = INod::tokenURICall { nodId: item.nod_id }.abi_encode();
            let out = dispatch(storage, &data, Address::ZERO, U256::ZERO).unwrap();
            let uri = INod::tokenURICall::abi_decode_returns(&out).unwrap();
            let json = base64::engine::general_purpose::STANDARD
                .decode(uri.strip_prefix("data:application/json;base64,").unwrap())
                .unwrap();
            String::from_utf8(json).unwrap()
        })
    };

    let deadline = at + NOTICE;
    let json = json_at(&mut provider, deadline);
    assert!(json.contains(r#"{"trait_type":"State","value":"Called"}"#));
    assert!(json.contains(&format!(
        r#"{{"trait_type":"Settlement Deadline","value":{deadline},"display_type":"date"}}"#
    )));
    let json = json_at(&mut provider, deadline + 1);
    assert!(json.contains(r#"{"trait_type":"State","value":"Forfeited"}"#));
    assert!(json.contains(&format!(
        r#"{{"trait_type":"Settlement Deadline","value":{deadline},"display_type":"date"}}"#
    )));
}

#[cfg(feature = "e2e-test")]
#[test]
fn static_test_command_is_rejected_before_group_validation() {
    use crate::precompile::dispatch;
    use crate::test_arming_e2e::INodFactoryTestArming;
    use alloy_sol_types::SolCall;
    use outbe_primitives::error::PrecompileError;
    let mut provider = outbe_primitives::storage::hashmap::HashMapStorageProvider::new(1);
    provider.set_static(true);

    let result = dispatch(
        outbe_primitives::storage::StorageHandle::new(&mut provider),
        &INodFactoryTestArming::closeCallNoticeForTestCall {
            groupKey: alloy_primitives::B256::repeat_byte(1),
            deadline: 1,
        }
        .abi_encode(),
        Address::ZERO,
        U256::ZERO,
    );
    assert!(
        matches!(result, Err(PrecompileError::WriteProtection)),
        "{result:?}"
    );
}
