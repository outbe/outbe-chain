//! Daily call-scan tests: the 21-of-28 breach rule and the seven-day forfeit.
//!
//! Every test drives the real [`crate::called::scan_and_call`] through [`scan`]
//! against seeded oracle history, so the breach count is recomputed from the
//! finalized daily VWAP series exactly as it is in a block.

use std::sync::Arc;

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolEvent;
use outbe_compressed_entities::{begin_block, ExecutionScope, WwdEntityId};
use outbe_offchain_storage::MemoryStorage;
use outbe_oracle::{api::AddressPair, schema::OracleContract};
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{
    addresses::{COMPRESSED_ENTITIES_ADDRESS, NOD_ADDRESS},
    block::{BlockContext, BlockRuntimeContext},
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
    time::{date_key_to_utc_timestamp, first_full_day, previous_date_key, timestamp_to_date_key},
};

use crate::{
    api,
    constants::{
        CALL_LOOKBACK_DAYS, CALL_NOTICE_PERIOD, CALL_RATE_PCT, CALL_SWEEP, CALL_THRESHOLD,
        CALL_THRESHOLD_DAYS, CALL_WINDOW, SECS_PER_DAY,
    },
    precompile::INod,
    NodContract, NodItemState, NodRepositoryReader,
};

const CHAIN_ID: u64 = 1;
const BLOCK_NUMBER: u64 = 42;
const DAY: u64 = 86_400;
/// The notice period a bucket seals at issuance, in the width these tests
/// do timestamp arithmetic in.
const NOTICE: u64 = CALL_NOTICE_PERIOD as u64;
const ISO: u16 = 840;
const OTHER_ISO: u16 = 978;
/// Issuance instant, 2027-01-15 08:00 UTC.
const START: u64 = 1_800_000_000;

/// The worldwide day [`START`] falls in. The breach walk now stops at
/// `first_full_day(issued_at)`, not this WWD key; the two still have to
/// agree so fixtures that reason in WWD days do not silently miss the
/// issuance cutoff.
const WWD: u32 = 20_270_115;

/// Entry price every bucket here is issued at: 2.0 at scale 1e6. The call price
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

fn seed_compressed_entities_genesis(storage: &StorageHandle<'_>) {
    storage
        .sstore(COMPRESSED_ENTITIES_ADDRESS, U256::ZERO, U256::from(4))
        .unwrap();
    storage
        .sstore(
            COMPRESSED_ENTITIES_ADDRESS,
            U256::from(1),
            U256::from_be_slice(
                outbe_compressed_entities::sealed_root(B256::ZERO)
                    .unwrap()
                    .as_slice(),
            ),
        )
        .unwrap();
}

fn nod_item(owner: Address, iso: u16) -> NodItemState {
    nod_item_at(owner, iso, U256::from(13))
}

/// A Nod whose bucket is keyed by `floor_price_minor`, so distinct floors give
/// distinct buckets on the same worldwide day.
fn nod_item_at(owner: Address, iso: u16, floor_price_minor: U256) -> NodItemState {
    nod_item_issued(owner, iso, floor_price_minor, WWD, START)
}

fn nod_item_issued(
    owner: Address,
    iso: u16,
    floor_price_minor: U256,
    worldwide_day: u32,
    issued_at: u64,
) -> NodItemState {
    let worldwide_day = WorldwideDay::new(worldwide_day);
    NodItemState {
        is_settled: false,
        nod_id: NodContract::generate_nod_id(owner, worldwide_day).unwrap(),
        owner,
        gratis_load_minor: U256::from(11),
        worldwide_day,
        league_id: 4,
        floor_price_minor,
        bucket_key: NodContract::bucket_key(worldwide_day, floor_price_minor, iso),
        issuance_currency: iso,
        reference_currency: iso,
        issued_at,
    }
}

fn register(storage: &StorageHandle<'_>, iso: u16) {
    if outbe_oracle::api::coen_pair_index_opt(storage.clone(), iso)
        .unwrap()
        .is_none()
    {
        outbe_oracle::api::register_pair(storage.clone(), AddressPair::new_coen_to(iso)).unwrap();
    }
}

fn last_closed_day(timestamp: u64) -> u32 {
    previous_date_key(timestamp_to_date_key(timestamp))
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
    oracle
        .utc_day_vwap_value
        .get_nested(&utc_day)
        .write(&index, value)
        .unwrap();
    bump_watermark(storage, utc_day);
}

/// Sets `days` consecutive closed UTC days ending at `latest` to `value`.
fn fill_days_for(storage: &StorageHandle<'_>, iso: u16, latest: u32, days: u32, value: U256) {
    let mut day = latest;
    for _ in 0..days {
        set_vwap_for(storage, iso, day, value);
        day = previous_date_key(day);
    }
}

fn fill_days(storage: &StorageHandle<'_>, latest: u32, days: u32, value: U256) {
    fill_days_for(storage, ISO, latest, days, value);
}

/// Advances the watermark without publishing any price.
fn finalize_through(storage: &StorageHandle<'_>, timestamp: u64) {
    bump_watermark(storage, last_closed_day(timestamp));
}

/// Issues one Nod owned by `owner` and qualifies its bucket, which is what arms
/// the bucket for the call scan.
fn issue_qualified(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &NodRepositoryReader,
    owner: Address,
    iso: u16,
) -> NodItemState {
    issue_qualified_item(storage, scope, parent, nod_item(owner, iso))
}

fn issue_qualified_item(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &NodRepositoryReader,
    item: NodItemState,
) -> NodItemState {
    api::add_nod(storage, scope, parent, &item, entry_price()).unwrap();
    NodContract::new(storage.clone())
        .qualify_bucket(scope, parent, item.bucket_key)
        .unwrap();
    item
}

fn scan(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &NodRepositoryReader,
    timestamp: u64,
) -> u32 {
    let ctx = BlockRuntimeContext::new(
        BlockContext::empty_for_tests(BLOCK_NUMBER, timestamp, CHAIN_ID),
        storage.clone(),
    );
    crate::called::scan_and_call(&ctx, scope, parent).unwrap()
}

/// Promis Reserve balance - what a forfeited `gratis_load_minor` returns to.
fn reserve(storage: &StorageHandle<'_>) -> U256 {
    outbe_promislimit::PromisLimitContract::new(storage.clone())
        .get_total_unallocated()
        .unwrap()
}

fn called_at(storage: &StorageHandle<'_>, bucket_key: B256) -> u64 {
    NodContract::new(storage.clone())
        .bucket_called_at
        .read(&bucket_key)
        .unwrap()
}

/// Runs `body` inside a storage scope with compressed entities open and the
/// default `COEN/840` pair registered.
fn harness(body: impl FnOnce(&StorageHandle<'_>, &ExecutionScope, &NodRepositoryReader)) {
    let parent = NodRepositoryReader::new(Arc::new(MemoryStorage::new()));
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let scope = ExecutionScope::new();
    StorageHandle::enter(&mut provider, |storage| {
        seed_compressed_entities_genesis(&storage);
        begin_block(storage.clone(), &scope).unwrap();
        register(&storage, ISO);
        body(&storage, &scope, &parent);
    });
}

// --- Sealed call terms -----------------------------------------------------

/// Rewrites the terms a bucket sealed at issuance, the way a retuned constant
/// would have if later checks still read the constants. Widens the currency's
/// high-water mark alongside, exactly as `seal_bucket_call_terms` does.
fn reterm(
    storage: &StorageHandle<'_>,
    bucket_key: B256,
    iso: u16,
    window_days: u32,
    threshold_days: u32,
    notice_days: u32,
) {
    let nod = NodContract::new(storage.clone());
    let window = window_days * SECS_PER_DAY;
    nod.callable_bucket_call_window
        .write(&bucket_key, window)
        .unwrap();
    nod.callable_bucket_call_threshold
        .write(&bucket_key, threshold_days * SECS_PER_DAY)
        .unwrap();
    nod.callable_bucket_call_notice_period
        .write(&bucket_key, notice_days * SECS_PER_DAY)
        .unwrap();
    if window > nod.max_call_window.read(&iso).unwrap() {
        nod.max_call_window.write(&iso, window).unwrap();
    }
}

/// Issuance seals the terms; the constants are read exactly once, there.
/// Qualification must not be required for the snapshot to exist, and must not
/// put the bucket on the callable list by itself.
#[test]
fn issuance_seals_the_call_terms_on_the_bucket() {
    harness(|storage, scope, parent| {
        let item = nod_item(Address::repeat_byte(0x11), ISO);
        api::add_nod(storage, scope, parent, &item, entry_price()).unwrap();
        let nod = NodContract::new(storage.clone());
        assert_eq!(
            nod.callable_bucket_call_rate
                .read(&item.bucket_key)
                .unwrap(),
            CALL_RATE_PCT
        );
        assert_eq!(
            nod.callable_bucket_call_window
                .read(&item.bucket_key)
                .unwrap(),
            CALL_WINDOW
        );
        assert_eq!(
            nod.callable_bucket_call_threshold
                .read(&item.bucket_key)
                .unwrap(),
            CALL_THRESHOLD
        );
        assert_eq!(
            nod.callable_bucket_call_notice_period
                .read(&item.bucket_key)
                .unwrap(),
            CALL_NOTICE_PERIOD
        );
        assert_eq!(nod.max_call_window.read(&ISO).unwrap(), CALL_WINDOW);
        assert_eq!(
            nod.callable_bucket_issued_at
                .read(&item.bucket_key)
                .unwrap(),
            START
        );
        assert_eq!(
            nod.callable_buckets.len().unwrap(),
            0,
            "issuance seals terms without arming the call scan"
        );
    });
}

/// Later Nods join the first member's issuance stamp. A delayed second mint
/// must not move the call-history cutoff.
#[test]
fn a_later_member_does_not_reissue_the_bucket_stamp() {
    harness(|storage, scope, parent| {
        let first = nod_item(Address::repeat_byte(0x11), ISO);
        api::add_nod(storage, scope, parent, &first, entry_price()).unwrap();
        let mut second = nod_item(Address::repeat_byte(0x22), ISO);
        second.issued_at = START + 3 * DAY;
        api::add_nod(storage, scope, parent, &second, entry_price()).unwrap();
        assert_eq!(first.bucket_key, second.bucket_key);
        assert_eq!(
            NodContract::new(storage.clone())
                .callable_bucket_issued_at
                .read(&first.bucket_key)
                .unwrap(),
            START
        );
    });
}

/// Q022: a parameter change between issuance and qualification must not re-term
/// an already-issued bucket. Rewrite the stored copy, then qualify; the scan
/// and deadline follow the issuance-time terms, not the live constants.
#[test]
fn qualification_does_not_reterm_an_already_issued_bucket() {
    harness(|storage, scope, parent| {
        let item = nod_item(Address::repeat_byte(0x11), ISO);
        api::add_nod(storage, scope, parent, &item, entry_price()).unwrap();
        reterm(storage, item.bucket_key, ISO, 3, 3, 1);

        NodContract::new(storage.clone())
            .qualify_bucket(scope, parent, item.bucket_key)
            .unwrap();

        let nod = NodContract::new(storage.clone());
        assert_eq!(
            nod.callable_bucket_call_window
                .read(&item.bucket_key)
                .unwrap(),
            3 * SECS_PER_DAY
        );
        assert_eq!(
            nod.callable_bucket_call_threshold
                .read(&item.bucket_key)
                .unwrap(),
            3 * SECS_PER_DAY
        );
        assert_eq!(
            nod.callable_bucket_call_notice_period
                .read(&item.bucket_key)
                .unwrap(),
            SECS_PER_DAY
        );

        let at = START + 30 * DAY;
        let latest = last_closed_day(at);
        fill_days(storage, latest, CALL_LOOKBACK_DAYS, below_call());
        fill_days(storage, latest, 3, above_call());
        assert_eq!(scan(storage, scope, parent, at), 1);
        assert_eq!(called_at(storage, item.bucket_key), at);
        assert_eq!(
            api::settlement_deadline(storage, item.bucket_key).unwrap(),
            at + DAY
        );
    });
}

/// The terms a bucket is called and forfeited under are the ones sealed at
/// issuance, not the live constants. A `const` cannot be retuned at
/// runtime, so this proves it from the other side: rewrite what the bucket
/// holds and watch the scan follow the bucket rather than the constant.
#[test]
fn the_scan_follows_the_terms_sealed_on_the_bucket_not_the_constants() {
    harness(|storage, scope, parent| {
        let item = issue_qualified(storage, scope, parent, Address::repeat_byte(0x11), ISO);
        let at = START + 30 * DAY;
        let latest = last_closed_day(at);
        fill_days(storage, latest, CALL_LOOKBACK_DAYS, below_call());
        // Three breach days at the head - far short of the 21 the constant
        // demands, and exactly the threshold the bucket will carry.
        fill_days(storage, latest, 3, above_call());

        assert_eq!(
            scan(storage, scope, parent, at),
            0,
            "the constant's 21-of-28 threshold is unmet"
        );

        reterm(storage, item.bucket_key, ISO, 3, 3, 1);
        assert_eq!(scan(storage, scope, parent, at), 1);
        assert_eq!(called_at(storage, item.bucket_key), at);

        // And the sealed notice governs the forfeit: one day, not seven.
        assert_eq!(
            api::settlement_deadline(storage, item.bucket_key).unwrap(),
            at + DAY,
            "the deadline follows the sealed notice period"
        );
        finalize_through(storage, at + DAY + 1);
        assert_eq!(scan(storage, scope, parent, at + DAY), 0, "not past it yet");
        assert_eq!(scan(storage, scope, parent, at + DAY + 1), 1);
        assert!(api::get_item(storage, scope, parent, item.nod_id)
            .unwrap()
            .is_none());
    });
}

/// A bucket carrying zero terms is uncallable, not callable on every day. Zero
/// is what a bucket armed before the terms existed reads back, and
/// `breaches >= 0` would otherwise call the whole index on the next scan.
#[test]
fn a_bucket_with_zero_call_terms_is_never_called() {
    harness(|storage, scope, parent| {
        let item = issue_qualified(storage, scope, parent, Address::repeat_byte(0x11), ISO);
        let at = START + 30 * DAY;
        fill_days(
            storage,
            last_closed_day(at),
            CALL_LOOKBACK_DAYS,
            above_call(),
        );

        reterm(storage, item.bucket_key, ISO, 0, 0, 0);
        assert_eq!(
            scan(storage, scope, parent, at),
            0,
            "a full breach window still does not call"
        );
        assert_eq!(called_at(storage, item.bucket_key), 0);
    });
}

/// A zero notice on an already-called bucket means "no deadline", not "lapsed
/// at the moment of the call" - otherwise the next run would forfeit it.
#[test]
fn a_called_bucket_with_a_zero_notice_period_is_never_forfeited() {
    harness(|storage, scope, parent| {
        let item = issue_qualified(storage, scope, parent, Address::repeat_byte(0x11), ISO);
        let at = START + 30 * DAY;
        fill_days(
            storage,
            last_closed_day(at),
            CALL_LOOKBACK_DAYS,
            above_call(),
        );
        assert_eq!(scan(storage, scope, parent, at), 1);

        reterm(
            storage,
            item.bucket_key,
            ISO,
            CALL_LOOKBACK_DAYS,
            CALL_THRESHOLD_DAYS,
            0,
        );
        let long_after = at + 365 * DAY;
        finalize_through(storage, long_after);
        assert_eq!(scan(storage, scope, parent, long_after), 0);
        assert!(api::get_item(storage, scope, parent, item.nod_id)
            .unwrap()
            .is_some());
    });
}

/// A bucket whose sealed window outruns the current constant still gets its
/// whole span collected: the scan sizes the shared per-currency window off the
/// `max_call_window` high-water mark, not off the constant.
#[test]
fn a_window_wider_than_the_constant_is_collected_in_full() {
    harness(|storage, scope, parent| {
        const WIDE_DAYS: u32 = 40;
        let item = issue_qualified(storage, scope, parent, Address::repeat_byte(0x11), ISO);
        let at = START + 60 * DAY;
        fill_days(storage, last_closed_day(at), WIDE_DAYS, above_call());

        // 35 of the 40 days must breach, which no 28-day window can supply.
        reterm(storage, item.bucket_key, ISO, WIDE_DAYS, 35, 7);
        assert_eq!(scan(storage, scope, parent, at), 1);
        assert_eq!(called_at(storage, item.bucket_key), at);
    });
}

// --- Call arm --------------------------------------------------------------

#[test]
fn the_call_price_is_the_entry_price_times_the_call_rate() {
    harness(|storage, scope, parent| {
        let item = issue_qualified(storage, scope, parent, Address::repeat_byte(0x11), ISO);
        let stored = NodContract::new(storage.clone())
            .callable_bucket_call_price
            .read(&item.bucket_key)
            .unwrap();
        assert_eq!(
            stored,
            entry_price() * U256::from(100 + CALL_RATE_PCT) / U256::from(100)
        );
        assert_eq!(stored, at_call(), "2.0 x 3.56 == 7.12");
    });
}

#[test]
fn a_full_window_above_the_call_price_calls_the_bucket() {
    harness(|storage, scope, parent| {
        let item = issue_qualified(storage, scope, parent, Address::repeat_byte(0x11), ISO);
        let at = START + 30 * DAY;
        fill_days(
            storage,
            last_closed_day(at),
            CALL_LOOKBACK_DAYS,
            above_call(),
        );

        assert_eq!(scan(storage, scope, parent, at), 1);
        assert_eq!(called_at(storage, item.bucket_key), at);

        // Idempotent: a called bucket is not called twice.
        assert_eq!(scan(storage, scope, parent, at + DAY), 0);
        assert_eq!(called_at(storage, item.bucket_key), at);
    });
}

#[test]
fn one_breach_day_short_of_the_threshold_does_not_call() {
    harness(|storage, scope, parent| {
        let item = issue_qualified(storage, scope, parent, Address::repeat_byte(0x11), ISO);
        let at = START + 30 * DAY;
        let latest = last_closed_day(at);
        fill_days(storage, latest, CALL_LOOKBACK_DAYS, below_call());
        fill_days(storage, latest, CALL_THRESHOLD_DAYS - 1, above_call());

        assert_eq!(scan(storage, scope, parent, at), 0);
        assert_eq!(called_at(storage, item.bucket_key), 0);
    });
}

#[test]
fn the_window_absorbs_below_call_days_up_to_the_slack() {
    harness(|storage, scope, parent| {
        let item = issue_qualified(storage, scope, parent, Address::repeat_byte(0x11), ISO);
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
            day = previous_date_key(day);
            offset += 1;
        }

        assert_eq!(scan(storage, scope, parent, at), 1);
        assert_eq!(called_at(storage, item.bucket_key), at);
    });
}

#[test]
fn a_day_exactly_at_the_call_price_does_not_count_as_a_breach() {
    harness(|storage, scope, parent| {
        let item = issue_qualified(storage, scope, parent, Address::repeat_byte(0x11), ISO);
        let at = START + 30 * DAY;
        fill_days(storage, last_closed_day(at), CALL_LOOKBACK_DAYS, at_call());

        assert_eq!(scan(storage, scope, parent, at), 0);
        assert_eq!(called_at(storage, item.bucket_key), 0);
    });
}

#[test]
fn missing_days_do_not_count_as_breaches() {
    harness(|storage, scope, parent| {
        let item = issue_qualified(storage, scope, parent, Address::repeat_byte(0x11), ISO);
        let at = START + 30 * DAY;
        let latest = last_closed_day(at);
        // Only one day short of the threshold is published; the rest are absent.
        fill_days(storage, latest, CALL_THRESHOLD_DAYS - 1, above_call());
        finalize_through(storage, at);

        assert_eq!(scan(storage, scope, parent, at), 0);
        assert_eq!(called_at(storage, item.bucket_key), 0);
    });
}

#[test]
fn a_breach_run_that_predates_the_bucket_does_not_call_it() {
    harness(|storage, scope, parent| {
        let item = issue_qualified(storage, scope, parent, Address::repeat_byte(0x11), ISO);
        // Scan two days after issuance: only a couple of closed UTC days sit at
        // or after `first_full_day(issued_at)`, so the pre-existence run beyond
        // them is ignored even though the whole series breaches.
        let at = START + 2 * DAY;
        fill_days(
            storage,
            last_closed_day(at),
            CALL_LOOKBACK_DAYS,
            above_call(),
        );

        assert_eq!(scan(storage, scope, parent, at), 0);
        assert_eq!(called_at(storage, item.bucket_key), 0);
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
        oldest_breach = previous_date_key(oldest_breach);
    }
    let midnight = date_key_to_utc_timestamp(oldest_breach);

    for (issued_at, expected) in [(midnight, 1u32), (midnight + 1, 0u32)] {
        harness(|storage, scope, parent| {
            let wwd = WorldwideDay::from_timestamp(issued_at).value();
            let item = issue_qualified_item(
                storage,
                scope,
                parent,
                nod_item_issued(
                    Address::repeat_byte(0x11),
                    ISO,
                    U256::from(13),
                    wwd,
                    issued_at,
                ),
            );
            fill_days(storage, latest, CALL_LOOKBACK_DAYS, below_call());
            fill_days(storage, latest, CALL_THRESHOLD_DAYS, above_call());
            assert_eq!(
                scan(storage, scope, parent, scan_at),
                expected,
                "issued at {issued_at}"
            );
            assert_eq!(
                called_at(storage, item.bucket_key) != 0,
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
    harness(|storage, scope, parent| {
        // WWD a month before issuance. 21 closed UTC days ending on the
        // partial issuance day all post-date that WWD and all predate
        // `first_full_day(START)` = 2027-01-16.
        let delayed_wwd = 20_261_216;
        let item = issue_qualified_item(
            storage,
            scope,
            parent,
            nod_item_issued(
                Address::repeat_byte(0x11),
                ISO,
                U256::from(13),
                delayed_wwd,
                START,
            ),
        );
        let issuance_utc_day = timestamp_to_date_key(START);
        fill_days(storage, issuance_utc_day, CALL_THRESHOLD_DAYS, above_call());
        let scan_at = date_key_to_utc_timestamp(first_full_day(START));
        finalize_through(storage, scan_at);
        assert_eq!(scan(storage, scope, parent, scan_at), 0);
        assert_eq!(called_at(storage, item.bucket_key), 0);
    });
}

/// A bucket issued before the stamp existed carries zero. Zero is "unsealed",
/// not epoch-midnight; it cannot inherit a full-history breach. Delete and
/// reissue through the existing empty-bucket path to arm it.
#[test]
fn a_zero_issued_at_stamp_does_not_call() {
    harness(|storage, scope, parent| {
        let item = issue_qualified(storage, scope, parent, Address::repeat_byte(0x11), ISO);
        NodContract::new(storage.clone())
            .callable_bucket_issued_at
            .clear(&item.bucket_key)
            .unwrap();
        let at = START + 30 * DAY;
        fill_days(
            storage,
            last_closed_day(at),
            CALL_LOOKBACK_DAYS,
            above_call(),
        );
        assert_eq!(scan(storage, scope, parent, at), 0);
        assert_eq!(called_at(storage, item.bucket_key), 0);
    });
}

#[test]
fn an_unfinalized_day_skips_the_run_without_touching_state() {
    harness(|storage, scope, parent| {
        let item = issue_qualified(storage, scope, parent, Address::repeat_byte(0x11), ISO);
        let at = START + 30 * DAY;
        fill_days(
            storage,
            last_closed_day(at),
            CALL_LOOKBACK_DAYS,
            above_call(),
        );

        // Ask a day later than anything the oracle has finalized.
        let ahead = at + 2 * DAY;
        assert_eq!(scan(storage, scope, parent, ahead), 0);
        assert_eq!(called_at(storage, item.bucket_key), 0);
    });
}

#[test]
fn each_bucket_reads_its_own_currency_series() {
    harness(|storage, scope, parent| {
        register(storage, OTHER_ISO);
        let usd = issue_qualified(storage, scope, parent, Address::repeat_byte(0x11), ISO);
        let eur = issue_qualified(
            storage,
            scope,
            parent,
            Address::repeat_byte(0x22),
            OTHER_ISO,
        );

        let at = START + 30 * DAY;
        let latest = last_closed_day(at);
        // Only the EUR series breaches.
        fill_days_for(storage, ISO, latest, CALL_LOOKBACK_DAYS, below_call());
        fill_days_for(storage, OTHER_ISO, latest, CALL_LOOKBACK_DAYS, above_call());

        assert_eq!(scan(storage, scope, parent, at), 1);
        assert_eq!(called_at(storage, usd.bucket_key), 0);
        assert_eq!(called_at(storage, eur.bucket_key), at);
    });
}

// --- Forfeit arm -----------------------------------------------------------

/// Calls a bucket at `at`, returning the issued Nod.
fn call_bucket(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &NodRepositoryReader,
    owner: Address,
    at: u64,
) -> NodItemState {
    let item = issue_qualified(storage, scope, parent, owner, ISO);
    fill_days(
        storage,
        last_closed_day(at),
        CALL_LOOKBACK_DAYS,
        above_call(),
    );
    assert_eq!(scan(storage, scope, parent, at), 1);
    item
}

#[test]
fn the_notice_period_expires_strictly_after_the_deadline() {
    harness(|storage, scope, parent| {
        let at = START + 30 * DAY;
        let item = call_bucket(storage, scope, parent, Address::repeat_byte(0x11), at);
        let deadline = at + NOTICE;

        // Exactly at the deadline the Nod survives.
        finalize_through(storage, deadline);
        assert_eq!(scan(storage, scope, parent, deadline), 0);
        assert!(api::get_item(storage, scope, parent, item.nod_id)
            .unwrap()
            .is_some());

        // One second past it, the Nod is forfeit-burned.
        finalize_through(storage, deadline + 1);
        assert_eq!(scan(storage, scope, parent, deadline + 1), 1);
        assert!(api::get_item(storage, scope, parent, item.nod_id)
            .unwrap()
            .is_none());
    });
}

#[test]
fn forfeiting_the_last_member_drops_the_bucket_from_the_callable_index() {
    harness(|storage, scope, parent| {
        let at = START + 30 * DAY;
        let item = call_bucket(storage, scope, parent, Address::repeat_byte(0x11), at);
        assert_eq!(
            NodContract::new(storage.clone())
                .callable_buckets
                .len()
                .unwrap(),
            1
        );

        let past = at + NOTICE + 1;
        finalize_through(storage, past);
        assert_eq!(scan(storage, scope, parent, past), 1);

        let nod = NodContract::new(storage.clone());
        assert_eq!(nod.callable_buckets.len().unwrap(), 0);
        assert_eq!(nod.bucket_called_at.read(&item.bucket_key).unwrap(), 0);
        assert_eq!(nod.bucket_nod_count.read(&item.bucket_key).unwrap(), 0);
        assert_eq!(nod.total_supply().unwrap(), 0);
        // The bucket body is gone with its last member.
        let bucket_id = WwdEntityId::from_day_and_digest(item.worldwide_day, item.bucket_key.0);
        assert!(api::get_bucket(storage, scope, parent, bucket_id)
            .unwrap()
            .is_none());
    });
}

// --- Member index ----------------------------------------------------------

#[test]
fn the_member_index_tracks_issuance_and_removal_in_a_qualified_bucket() {
    harness(|storage, scope, parent| {
        // Two owners on the same worldwide day share one bucket: identical floor
        // price and currency.
        let a = issue_qualified(storage, scope, parent, Address::repeat_byte(0x11), ISO);
        let b_item = nod_item(Address::repeat_byte(0x22), ISO);
        api::add_nod(storage, scope, parent, &b_item, entry_price()).unwrap();
        assert_eq!(a.bucket_key, b_item.bucket_key, "same bucket");

        let nod = NodContract::new(storage.clone());
        assert_eq!(nod.bucket_nod_count.read(&a.bucket_key).unwrap(), 2);
        let bucket_id = WwdEntityId::from_day_and_digest(a.worldwide_day, a.bucket_key.0);
        let original = nod
            .get_bucket_verified(scope, parent, bucket_id)
            .unwrap()
            .unwrap();

        // Removing one keeps the bucket unchanged and clears only its own entry.
        let item = api::load_item(storage, scope, parent, a.nod_id)
            .unwrap()
            .unwrap();
        let bucket = api::load_bucket(storage, scope, parent, bucket_id)
            .unwrap()
            .unwrap();
        api::remove_nod(storage, scope, item, bucket).unwrap();

        let nod = NodContract::new(storage.clone());
        assert_eq!(nod.bucket_nod_count.read(&a.bucket_key).unwrap(), 1);
        assert_eq!(
            nod.get_bucket_verified(scope, parent, bucket_id)
                .unwrap()
                .unwrap(),
            original
        );
        assert_eq!(
            nod.bucket_nod_index.read(&a.nod_id).unwrap(),
            0,
            "the removed Nod's reverse index entry is cleared"
        );
        // The survivor is still reachable at slot 0.
        assert_eq!(
            nod.bucket_nods
                .read(&NodContract::bucket_nod_key(a.bucket_key, 0))
                .unwrap(),
            b_item.nod_id
        );
    });
}

#[test]
fn an_unqualified_bucket_is_never_visited_by_the_call_scan() {
    harness(|storage, scope, parent| {
        let item = nod_item(Address::repeat_byte(0x11), ISO);
        api::add_nod(storage, scope, parent, &item, entry_price()).unwrap();

        let at = START + 30 * DAY;
        fill_days(
            storage,
            last_closed_day(at),
            CALL_LOOKBACK_DAYS,
            above_call(),
        );

        assert_eq!(scan(storage, scope, parent, at), 0);
        assert_eq!(called_at(storage, item.bucket_key), 0);
    });
}

#[test]
fn every_member_of_a_lapsed_bucket_burns_in_one_pass() {
    harness(|storage, scope, parent| {
        // Three owners on the same day, floor and currency share one bucket.
        let owners = [0x11u8, 0x22, 0x33].map(Address::repeat_byte);
        let items: Vec<NodItemState> = owners
            .iter()
            .map(|owner| {
                let item = nod_item(*owner, ISO);
                api::add_nod(storage, scope, parent, &item, entry_price()).unwrap();
                item
            })
            .collect();
        let bucket_key = items[0].bucket_key;
        NodContract::new(storage.clone())
            .qualify_bucket(scope, parent, bucket_key)
            .unwrap();

        let at = START + 30 * DAY;
        fill_days(
            storage,
            last_closed_day(at),
            CALL_LOOKBACK_DAYS,
            above_call(),
        );
        assert_eq!(scan(storage, scope, parent, at), 1);

        let past = at + NOTICE + 1;
        finalize_through(storage, past);
        assert_eq!(scan(storage, scope, parent, past), 3, "all three burn");

        for item in &items {
            assert!(
                api::get_item(storage, scope, parent, item.nod_id)
                    .unwrap()
                    .is_none(),
                "nod {} survived the forfeit",
                item.nod_id
            );
        }
        assert_eq!(NodContract::new(storage.clone()).total_supply().unwrap(), 0);
    });
}

#[test]
fn a_lapsed_bucket_returns_every_forfeited_load_to_the_promis_reserve() {
    harness(|storage, scope, parent| {
        let owners = [0x11u8, 0x22, 0x33].map(Address::repeat_byte);
        let items: Vec<NodItemState> = owners
            .iter()
            .map(|owner| {
                let item = nod_item(*owner, ISO);
                api::add_nod(storage, scope, parent, &item, entry_price()).unwrap();
                item
            })
            .collect();
        let bucket_key = items[0].bucket_key;
        NodContract::new(storage.clone())
            .qualify_bucket(scope, parent, bucket_key)
            .unwrap();

        let at = START + 30 * DAY;
        fill_days(
            storage,
            last_closed_day(at),
            CALL_LOOKBACK_DAYS,
            above_call(),
        );
        assert_eq!(scan(storage, scope, parent, at), 1);

        // The call alone forfeits nothing, so nothing is returned yet.
        assert_eq!(reserve(storage), U256::ZERO);

        let past = at + NOTICE + 1;
        finalize_through(storage, past);
        assert_eq!(scan(storage, scope, parent, past), 3);

        let expected: U256 = items.iter().map(|item| item.gratis_load_minor).sum();
        assert_eq!(reserve(storage), expected);
    });
}

#[test]
fn forfeiting_a_bucket_mid_list_does_not_skip_its_neighbours() {
    harness(|storage, scope, parent| {
        // Three distinct buckets: distinct floor prices on one worldwide day.
        let specs = [
            (Address::repeat_byte(0x11), U256::from(11)),
            (Address::repeat_byte(0x22), U256::from(22)),
            (Address::repeat_byte(0x33), U256::from(33)),
        ];
        let items: Vec<NodItemState> = specs
            .iter()
            .map(|(owner, floor)| {
                let item = nod_item_at(*owner, ISO, *floor);
                api::add_nod(storage, scope, parent, &item, entry_price()).unwrap();
                NodContract::new(storage.clone())
                    .qualify_bucket(scope, parent, item.bucket_key)
                    .unwrap();
                item
            })
            .collect();

        // Call only the middle bucket, by breaching while the others are unarmed.
        // Arming order is issuance order, so index 1 is the middle of the list.
        let at = START + 30 * DAY;
        fill_days(
            storage,
            last_closed_day(at),
            CALL_LOOKBACK_DAYS,
            above_call(),
        );
        assert_eq!(scan(storage, scope, parent, at), 3, "all three call");

        // Drop the outer two back to uncalled so only the middle one lapses; the
        // pass must still visit the neighbours the swap-pop moves around.
        let nod = NodContract::new(storage.clone());
        nod.bucket_called_at.write(&items[0].bucket_key, 0).unwrap();
        nod.bucket_called_at.write(&items[2].bucket_key, 0).unwrap();

        let past = at + NOTICE + 1;
        fill_days(
            storage,
            last_closed_day(past),
            CALL_LOOKBACK_DAYS,
            above_call(),
        );
        // One forfeit for the middle bucket plus two fresh calls for the others.
        assert_eq!(scan(storage, scope, parent, past), 3);

        assert!(api::get_item(storage, scope, parent, items[1].nod_id)
            .unwrap()
            .is_none());
        for index in [0usize, 2] {
            assert!(
                api::get_item(storage, scope, parent, items[index].nod_id)
                    .unwrap()
                    .is_some(),
                "neighbour bucket {index} was wrongly burned"
            );
            assert_eq!(
                called_at(storage, items[index].bucket_key),
                past,
                "neighbour bucket {index} was skipped by the pass"
            );
        }
        assert_eq!(
            NodContract::new(storage.clone())
                .callable_buckets
                .len()
                .unwrap(),
            2
        );
    });
}

#[test]
fn mixed_bucket_forfeits_only_unpaid_loads_and_preserves_paid_terms_until_exercise() {
    harness(|storage, scope, parent| {
        let items: Vec<_> = [0x91, 0x92, 0x93, 0x95]
            .into_iter()
            .map(|seed| {
                let item = nod_item(Address::repeat_byte(seed), ISO);
                api::add_nod(storage, scope, parent, &item, entry_price()).unwrap();
                item
            })
            .collect();
        let key = items[0].bucket_key;
        let id = WwdEntityId::from_day_and_digest(items[0].worldwide_day, key);
        let mut nod = NodContract::new(storage.clone());
        nod.qualify_bucket(scope, parent, key).unwrap();
        // Settle the middle member, exercising swap-remove of the unpaid tail.
        api::settle_nod(
            storage,
            scope,
            api::load_item(storage, scope, parent, items[1].nod_id)
                .unwrap()
                .unwrap(),
            api::load_bucket(storage, scope, parent, id)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        api::settle_nod(
            storage,
            scope,
            api::load_item(storage, scope, parent, items[3].nod_id)
                .unwrap()
                .unwrap(),
            api::load_bucket(storage, scope, parent, id)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        api::remove_nod(
            storage,
            scope,
            api::load_item(storage, scope, parent, items[3].nod_id)
                .unwrap()
                .unwrap(),
            api::load_bucket(storage, scope, parent, id)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        let bucket = api::get_bucket(storage, scope, parent, id)
            .unwrap()
            .unwrap();
        assert_eq!(bucket.settled_nods, 1);
        assert_eq!(nod.bucket_nod_count.read(&key).unwrap(), 2);
        let at = START + 30 * DAY;
        fill_days(
            storage,
            last_closed_day(at),
            CALL_LOOKBACK_DAYS,
            above_call(),
        );
        assert_eq!(scan(storage, scope, parent, at), 1);
        let past = at + NOTICE + 1;
        finalize_through(storage, past);
        // A failed Promis credit must restore every unpaid body and index before retry.
        let mut limit = outbe_promislimit::PromisLimitContract::new(storage.clone());
        limit.set_total_unallocated(U256::MAX).unwrap();
        assert_eq!(scan(storage, scope, parent, past), 0);
        assert_eq!(nod.total_supply().unwrap(), 3);
        assert_eq!(nod.bucket_nod_count.read(&key).unwrap(), 2);
        assert!(api::get_item(storage, scope, parent, items[0].nod_id)
            .unwrap()
            .is_some());
        assert!(api::get_item(storage, scope, parent, items[2].nod_id)
            .unwrap()
            .is_some());
        assert_eq!(reserve(storage), U256::MAX);
        limit.set_total_unallocated(U256::ZERO).unwrap();
        assert_eq!(scan(storage, scope, parent, past), 2);
        let expected = items[0].gratis_load_minor + items[2].gratis_load_minor;
        assert_eq!(reserve(storage), expected);
        assert_eq!(scan(storage, scope, parent, past), 0);
        assert_eq!(reserve(storage), expected);
        let bucket = api::get_bucket(storage, scope, parent, id)
            .unwrap()
            .unwrap();
        assert_eq!(bucket.settled_nods, 1);
        assert_eq!(nod.bucket_nod_count.read(&key).unwrap(), 0);
        assert_eq!(nod.total_supply().unwrap(), 1);
        assert_eq!(called_at(storage, key), at);
        assert_eq!(
            nod.read_call_terms(key).unwrap().call_notice_period,
            CALL_NOTICE_PERIOD
        );
        api::remove_nod(
            storage,
            scope,
            api::load_item(storage, scope, parent, items[1].nod_id)
                .unwrap()
                .unwrap(),
            api::load_bucket(storage, scope, parent, id)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert!(api::get_bucket(storage, scope, parent, id)
            .unwrap()
            .is_none());
        assert_eq!(nod.total_supply().unwrap(), 0);
        assert_eq!(nod.callable_buckets.len().unwrap(), 0);
        assert_eq!(reserve(storage), expected);
    });
}

#[test]
fn a_fully_paid_bucket_is_not_called_and_corrupt_paid_membership_is_not_forfeited() {
    harness(|storage, scope, parent| {
        let item = issue_qualified(storage, scope, parent, Address::repeat_byte(0x94), ISO);
        let id = WwdEntityId::from_day_and_digest(item.worldwide_day, item.bucket_key);
        api::settle_nod(
            storage,
            scope,
            api::load_item(storage, scope, parent, item.nod_id)
                .unwrap()
                .unwrap(),
            api::load_bucket(storage, scope, parent, id)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        let at = START + 30 * DAY;
        fill_days(
            storage,
            last_closed_day(at),
            CALL_LOOKBACK_DAYS,
            above_call(),
        );
        assert_eq!(scan(storage, scope, parent, at), 0);
        assert_eq!(called_at(storage, item.bucket_key), 0);
        // Issuance into a paid-only bucket must retain its committed paid state.
        let newcomer = nod_item(Address::repeat_byte(0x96), ISO);
        let paid_bucket = api::get_bucket(storage, scope, parent, id)
            .unwrap()
            .unwrap();
        api::add_nod(storage, scope, parent, &newcomer, entry_price()).unwrap();
        assert_eq!(
            crate::repository::canonical_bucket(
                &api::get_bucket(storage, scope, parent, id)
                    .unwrap()
                    .unwrap()
            ),
            crate::repository::canonical_bucket(&paid_bucket),
        );
        api::remove_nod(
            storage,
            scope,
            api::load_item(storage, scope, parent, newcomer.nod_id)
                .unwrap()
                .unwrap(),
            api::load_bucket(storage, scope, parent, id)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            api::get_bucket(storage, scope, parent, id)
                .unwrap()
                .unwrap()
                .settled_nods,
            1
        );
        let nod = NodContract::new(storage.clone());
        nod.bucket_called_at.write(&item.bucket_key, at).unwrap();
        nod.bucket_nod_count.write(&item.bucket_key, 1).unwrap();
        nod.bucket_nods
            .write(
                &NodContract::bucket_nod_key(item.bucket_key, 0),
                item.nod_id,
            )
            .unwrap();
        let past = at + NOTICE + 1;
        finalize_through(storage, past);
        assert_eq!(scan(storage, scope, parent, past), 0);
        assert!(
            api::get_item(storage, scope, parent, item.nod_id)
                .unwrap()
                .unwrap()
                .is_settled
        );
        assert_eq!(nod.total_supply().unwrap(), 1);
        assert_eq!(reserve(storage), U256::ZERO);
    });
}

/// An unfinished call sweep keeps the day it opened on: a later clock whose
/// own window would not call still force-calls against the pinned day's
/// trailing VWAP.
#[test]
fn a_running_call_sweep_keeps_its_day() {
    harness(|storage, scope, parent| {
        let item = issue_qualified(storage, scope, parent, Address::repeat_byte(0x21), ISO);
        let at = START + 30 * DAY;
        let day = last_closed_day(at);
        let later = at + 21 * DAY;
        let later_day = last_closed_day(later);

        fill_days(storage, later_day, CALL_LOOKBACK_DAYS, below_call());
        fill_days(storage, day, CALL_THRESHOLD_DAYS, above_call());

        let nod = NodContract::new(storage.clone());
        nod.call_sweep_day.write(day).unwrap();
        nod.call_scan_cursor.write(0).unwrap();

        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(BLOCK_NUMBER, later, CHAIN_ID),
            storage.clone(),
        );
        crate::called::run_call_slice(&ctx, scope, parent).unwrap();
        assert_eq!(called_at(storage, item.bucket_key), later);
        assert_eq!(nod.call_sweep_day.read().unwrap(), 0);
    });
}

/// A closed day behind a running call sweep waits, and a newer one takes its
/// place and names the day it pushed out.
#[test]
fn a_newer_day_pushes_out_the_waiting_call_day_and_names_it() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let parent = NodRepositoryReader::new(Arc::new(MemoryStorage::new()));
    let scope = ExecutionScope::new();
    let (in_flight, skipped) = StorageHandle::enter(&mut provider, |storage| {
        seed_compressed_entities_genesis(&storage);
        begin_block(storage.clone(), &scope).unwrap();
        register(&storage, ISO);
        issue_qualified(&storage, &scope, &parent, Address::repeat_byte(0x22), ISO);

        let at = START + 30 * DAY;
        let closed = [
            last_closed_day(at),
            last_closed_day(at + DAY),
            last_closed_day(at + 2 * DAY),
        ];
        fill_days(&storage, closed[2], CALL_LOOKBACK_DAYS, below_call());

        let nod = NodContract::new(storage.clone());
        nod.call_sweep_day.write(closed[0]).unwrap();
        nod.call_scan_cursor.write(1).unwrap();

        crate::called::scan_and_call(
            &BlockRuntimeContext::new(
                BlockContext::empty_for_tests(BLOCK_NUMBER, at + DAY, CHAIN_ID),
                storage.clone(),
            ),
            &scope,
            &parent,
        )
        .unwrap();
        crate::called::scan_and_call(
            &BlockRuntimeContext::new(
                BlockContext::empty_for_tests(BLOCK_NUMBER, at + 2 * DAY, CHAIN_ID),
                storage.clone(),
            ),
            &scope,
            &parent,
        )
        .unwrap();
        assert_eq!(nod.call_sweep_day.read().unwrap(), closed[0]);
        assert_eq!(nod.call_pending_day.read().unwrap(), closed[2]);
        assert_eq!(nod.call_scan_cursor.read().unwrap(), 1);
        (closed[0], closed[1])
    });

    let events: Vec<_> = provider
        .get_events(NOD_ADDRESS)
        .iter()
        .filter_map(|log| INod::SweepDaySkipped::decode_log_data(log).ok())
        .collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].sweep, CALL_SWEEP);
    assert_eq!(events[0].skippedDay, skipped);
    assert_eq!(events[0].inFlightDay, in_flight);
}

#[test]
fn a_call_pass_announces_one_batch_metadata_update() {
    use alloy_sol_types::SolEvent;

    let parent = NodRepositoryReader::new(Arc::new(MemoryStorage::new()));
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let scope = ExecutionScope::new();
    StorageHandle::enter(&mut provider, |storage| {
        seed_compressed_entities_genesis(&storage);
        begin_block(storage.clone(), &scope).unwrap();
        register(&storage, ISO);
        register(&storage, OTHER_ISO);
        issue_qualified(&storage, &scope, &parent, Address::repeat_byte(0x11), ISO);
        issue_qualified(
            &storage,
            &scope,
            &parent,
            Address::repeat_byte(0x22),
            OTHER_ISO,
        );
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

        assert_eq!(scan(&storage, &scope, &parent, at), 2);
        assert_eq!(scan(&storage, &scope, &parent, at + DAY), 0);
    });

    let batches = provider
        .get_events(outbe_primitives::addresses::NOD_ADDRESS)
        .iter()
        .filter(|log| crate::precompile::INod::BatchMetadataUpdate::decode_log_data(log).is_ok())
        .count();
    assert_eq!(batches, 1);
}

#[test]
fn token_uri_reads_called_then_expired_across_the_settlement_deadline() {
    use crate::precompile::{dispatch, INod};
    use alloy_sol_types::SolCall;
    use base64::Engine;

    let parent = NodRepositoryReader::new(Arc::new(MemoryStorage::new()));
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let scope = ExecutionScope::new();
    let at = START + 30 * DAY;
    let item = StorageHandle::enter(&mut provider, |storage| {
        seed_compressed_entities_genesis(&storage);
        begin_block(storage.clone(), &scope).unwrap();
        register(&storage, ISO);
        call_bucket(&storage, &scope, &parent, Address::repeat_byte(0x71), at)
    });
    let json_at = |provider: &mut HashMapStorageProvider, timestamp: u64| {
        provider.set_timestamp(U256::from(timestamp));
        StorageHandle::enter(provider, |storage| {
            let data = INod::tokenURICall {
                nodId: item.nod_id.to_u256(),
            }
            .abi_encode();
            let out = dispatch(storage, &scope, &parent, &data, Address::ZERO, U256::ZERO).unwrap();
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
    assert!(json.contains(r#"{"trait_type":"State","value":"Expired"}"#));
}
