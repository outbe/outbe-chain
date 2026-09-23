//! Daily Nod qualification, call and forfeit hook.
//!
//! Each closed UTC day is pinned when the `NodDaily` trigger opens a sweep.
//! Later CycleTicks continue the same day with the same price, so a later
//! UTC rollover cannot reprice the remainder or skip a one-day crossing.
//! Qualification and calls both wait for Oracle finalization and never fall
//! back to a live rate, an older day, or a WorldwideDay VWAP. Both arms skip
//! pre-issuance history and the partial issuance UTC day: a bucket only
//! qualifies, and a call window only counts, on days at or after
//! `first_full_day` of the sealed `issued_at`. A zero stamp is unsealed, not
//! epoch-midnight, and never qualifies.
//!
//! Qualification promotes any unqualified bucket whose
//! `floor_price_minor < rate` on a UTC day it held in full. The comparison is
//! strict - a bucket priced exactly at the rate stays unqualified until the
//! rate moves strictly above its floor. Qualification is a monotonic latch -
//! once a bucket is qualified, it stays that way, so `mine_gratis` only has
//! to read the cached `is_qualified` bit.
//!
//! Implementation (PancakeSwap-Liquidity-Book bin index):
//! - `floor_price_minor` is mapped to a 24-bit `bin_id` on a log-spaced
//!   ladder (`BIN_STEP_BP = 25` => 0.25% per bin) via `state::price_to_bin`.
//! - Unqualified buckets are stored in `unqualified_bin_count` /
//!   `unqualified_bin_buckets`, and a 3-level radix-256 bitmap trie
//!   (`bin_tree_root`/`bin_tree_mid`/`bin_tree_leaf`) marks non-empty bins.
//! - Each run: walk set bins in ascending `bin_id` order via
//!   `bin_tree::find_first_left_inclusive`. Bins strictly below `r_bin` hold
//!   only floors `< rate` (any floor equal to the rate maps into `r_bin`), so
//!   they drain except buckets whose sealed `issued_at` has not yet reached
//!   `first_full_day` of the sweep day (those stay parked until a later day);
//!   the tail bin (`bin_id == r_bin`) checks each bucket's exact
//!   `floor_price_minor < rate` so a coarse bin neither qualifies a bucket
//!   above the rate nor one priced exactly at it.
//!
//! Multi-currency: a floor is only comparable to the rate of its own
//! `reference_currency`, so every bin column is namespaced by ISO code and
//! each reference currency walks an independent trie. A sweep walks each
//! currency once from `qualify_currency_cursor`, sharing one
//! `MAX_BUCKET_QUALIFICATIONS_PER_BLOCK` budget; a currency whose COEN pair
//! is unregistered or has no VWAP for that day is settled for the day.

use std::collections::BTreeSet;

use alloy_primitives::U256;
use outbe_compressed_entities::{ExecutionScope, ParentBodySource, WwdEntityId};
use outbe_oracle::api::{get_all_reference_currencies, get_utc_day_vwap_for_iso};
use outbe_primitives::{
    block::BlockRuntimeContext,
    daily_sweep::{Scheduled, SweepDays},
    error::Result,
    math::{constants::MAX_BIN_ID, tree_math},
    time::first_full_day,
};

use crate::{
    api,
    constants::{MAX_BUCKET_QUALIFICATIONS_PER_BLOCK, QUALIFY_SWEEP},
    precompile::INod,
    schema::NodContract,
    state::CurrencyBins,
};

/// Daily cycle-trigger entry. Opens the day's qualification and call sweeps
/// and runs their first slices. Later CycleTicks carry the remainder on
/// through [`continue_sweeps`].
pub fn run_daily(
    ctx: &BlockRuntimeContext,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
) -> Result<()> {
    scan_and_qualify(ctx, scope, parent)?;
    crate::called::scan_and_call(ctx, scope, parent)?;
    Ok(())
}

/// Advance in-flight qualification and call sweeps by one slice each. Runs
/// from CycleTick on every block, before the daily trigger can queue a newer
/// day, so an unfinished walk keeps the prices it opened with.
pub fn continue_sweeps(
    ctx: &BlockRuntimeContext,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
) -> Result<()> {
    run_qualify_slice(ctx, scope, parent)?;
    crate::called::run_call_slice(ctx, scope, parent)?;
    Ok(())
}

/// Schedule the day the Oracle has just finalized: open a qualification sweep
/// over it and run its first slice, or queue it behind the sweep still in
/// flight.
pub fn scan_and_qualify(
    ctx: &BlockRuntimeContext,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
) -> Result<u32> {
    let Some(day) = crate::called::closed_day(ctx)? else {
        return Ok(0);
    };
    let mut nod = NodContract::new(ctx.storage.clone());
    let days = SweepDays {
        current: nod.qualify_sweep_day.read()?,
        pending: nod.qualify_pending_day.read()?,
    };
    match days.schedule(day) {
        (next, Scheduled::Opened) => {
            start_qualify_sweep(ctx, &nod, next)?;
            run_qualify_slice(ctx, scope, parent)
        }
        (next, Scheduled::Queued) => {
            nod.qualify_pending_day.write(next.pending)?;
            Ok(0)
        }
        (next, Scheduled::Replaced { skipped }) => {
            nod.qualify_pending_day.write(next.pending)?;
            nod.emit(INod::SweepDaySkipped {
                sweep: QUALIFY_SWEEP,
                skippedDay: skipped,
                inFlightDay: next.current,
            })?;
            Ok(0)
        }
        (_, Scheduled::Ignored) => Ok(0),
    }
}

/// Pin the sweep's current day and walk it from the first currency's lowest bin.
fn start_qualify_sweep(
    ctx: &BlockRuntimeContext,
    nod: &NodContract,
    days: SweepDays,
) -> Result<()> {
    nod.qualify_sweep_day.write(days.current)?;
    nod.qualify_pending_day.write(days.pending)?;
    nod.qualify_currency_cursor.write(0)?;
    for iso_code in get_all_reference_currencies(ctx)? {
        nod.qualify_scan_cursor.write(&iso_code, 0)?;
    }
    Ok(())
}

/// Advance an open qualification sweep by one slice, pinned to its day.
pub fn run_qualify_slice(
    ctx: &BlockRuntimeContext,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
) -> Result<u32> {
    let mut nod = NodContract::new(ctx.storage.clone());
    let pinned_day = nod.qualify_sweep_day.read()?;
    if pinned_day == 0 {
        return Ok(0);
    }
    let currencies = get_all_reference_currencies(ctx)?;
    let start = currency_position(&currencies, nod.qualify_currency_cursor.read()?);
    let mut budget = MAX_BUCKET_QUALIFICATIONS_PER_BLOCK;
    let mut inspected_total = 0_u32;
    let mut qualified_days = BTreeSet::new();
    let mut unfinished = None;

    // One pass down the list, as in the Called sweep, so every sweep ends.
    for &iso_code in currencies.iter().skip(start) {
        let finished = if budget == 0 {
            false
        } else if nod.bin_tree_root.read(&iso_code)?.is_zero() {
            true
        } else {
            match get_utc_day_vwap_for_iso(ctx.storage.clone(), pinned_day, iso_code)? {
                None => true,
                Some(vwap) => match NodContract::price_to_bin(vwap) {
                    Err(error) => {
                        tracing::warn!(
                            target: "outbe::nod",
                            iso_code,
                            error = ?error,
                            "qualify scan: day price out of range, skipping currency for the day"
                        );
                        nod.emit(INod::QualifyScanSkipped {
                            referenceCurrency: iso_code,
                            utcDay: pinned_day,
                        })?;
                        true
                    }
                    Ok(_) => {
                        let (inspected, finished, days) = qualify_with_rate(
                            ctx, scope, parent, iso_code, vwap, pinned_day, budget,
                        )?;
                        budget = budget.saturating_sub(inspected);
                        inspected_total = inspected_total.saturating_add(inspected);
                        qualified_days.extend(days);
                        finished
                    }
                },
            }
        };
        if !finished {
            unfinished = Some(iso_code);
            break;
        }
    }
    nod.emit_days_metadata_update(&qualified_days)?;
    if let Some(iso_code) = unfinished {
        nod.qualify_currency_cursor.write(u32::from(iso_code))?;
        return Ok(inspected_total);
    }

    // The next day starts on the next block, so no slice mixes two days' prices.
    let next = SweepDays {
        current: pinned_day,
        pending: nod.qualify_pending_day.read()?,
    }
    .finish();
    if next.current == 0 {
        nod.qualify_sweep_day.write(0)?;
    } else {
        start_qualify_sweep(ctx, &nod, next)?;
    }
    Ok(inspected_total)
}

/// Qualifies Nod buckets using the same block scope and parent source as transactions.
///
/// Opens this block's closed UTC day (or queues it) and runs the first slice.
/// Tests that want one pass against "yesterday" call this; in-flight remainder
/// continues through [`run_qualify_slice`].
pub fn qualify_nods(
    ctx: &BlockRuntimeContext,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
) -> Result<()> {
    scan_and_qualify(ctx, scope, parent).map(|_| ())
}

/// Index of the currency the cursor names, or the head when the registry dropped it.
pub(crate) fn currency_position(currencies: &[u16], cursor: u32) -> usize {
    u16::try_from(cursor)
        .ok()
        .and_then(|iso| currencies.iter().position(|&code| code == iso))
        .unwrap_or(0)
}

/// Qualifies one reference currency's buckets, inspecting at most `budget`
/// bucket bodies. Returns how many it inspected so the caller can share one
/// per-run budget across currencies.
///
/// Used by the daily qualifier and behavioral tests.
pub fn qualify_buckets_with_rate(
    ctx: &BlockRuntimeContext,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    iso_code: u16,
    rate: U256,
    day: u32,
    budget: u32,
) -> Result<u32> {
    let (inspected, _, days) = qualify_with_rate(ctx, scope, parent, iso_code, rate, day, budget)?;
    NodContract::new(ctx.storage.clone()).emit_days_metadata_update(&days)?;
    Ok(inspected)
}

/// True when `day` is a full UTC day the bucket held. Zero stamp is unsealed
/// (predates the field), not epoch-midnight, so it never qualifies — same
/// policy as the call scan.
fn held_in_full(issued_at: u64, day: u32) -> bool {
    issued_at != 0 && day >= first_full_day(issued_at)
}

/// Drains the floor-bins crossed by one currency's `rate` on `day`, inspecting
/// at most `budget` buckets. Returns how many it inspected, whether the
/// eligible range was walked to the end, and the Worldwide Days of the buckets
/// it qualified. Buckets whose sealed `issued_at` has
/// not yet reached `first_full_day` of `day` stay in the trie for a later sweep.
fn qualify_with_rate(
    ctx: &BlockRuntimeContext,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    iso_code: u16,
    rate: U256,
    day: u32,
    budget: u32,
) -> Result<(u32, bool, BTreeSet<u32>)> {
    if budget == 0 {
        return Ok((0, false, BTreeSet::new()));
    }
    let r_bin = NodContract::price_to_bin(rate)?;
    let mut nod = NodContract::new(ctx.storage.clone());
    let mut bin_cursor = nod.qualify_scan_cursor.read(&iso_code)?;
    let mut inspected = 0_u32;
    let mut qualified_days = BTreeSet::new();
    loop {
        if inspected == budget {
            nod.qualify_scan_cursor.write(&iso_code, bin_cursor)?;
            return Ok((inspected, false, qualified_days));
        }
        let next = match tree_math::find_first_left_inclusive(
            &CurrencyBins(&nod, iso_code),
            bin_cursor,
        )? {
            Some(bin) if bin <= r_bin => bin,
            _ => {
                nod.qualify_scan_cursor.write(&iso_code, 0)?;
                return Ok((inspected, true, qualified_days));
            }
        };
        let scoped = NodContract::scoped(iso_code, next);
        let strict = next < r_bin;
        let initial_count = nod.unqualified_bin_count.read(&scoped)?;
        let mut count = initial_count;
        if count == 0 {
            return Err(
                outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                    "Nod bin tree references empty bin {iso_code}:{next}"
                )),
            );
        }
        let initial_cursor = nod.unqualified_bin_scan_cursor.read(&scoped)?;
        let mut index = initial_cursor;
        if index >= count {
            index = 0;
        }
        while index < count && inspected < budget {
            let key = NodContract::bin_index_key(iso_code, next, index);
            let bucket_key = nod.unqualified_bin_buckets.read(&key)?;
            if bucket_key.is_zero() {
                return Err(
                    outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                        "Nod unqualified-bin entry {iso_code}:{next}:{index} has an empty bucket key"
                    )),
                );
            }
            let worldwide_day = nod.bucket_worldwide_day.read(&bucket_key)?;
            let bucket_id = WwdEntityId::from_day_and_digest(worldwide_day, bucket_key.0);
            let loaded =
                api::load_bucket(&ctx.storage, scope, parent, bucket_id)?.ok_or_else(|| {
                    outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                    "Nod unqualified-bin entry {iso_code}:{next}:{index} references missing bucket {bucket_id}"
                ))
                })?;
            let bucket = loaded.body();
            if bucket.is_qualified
                || bucket.reference_currency != iso_code
                || NodContract::price_to_bin(bucket.floor_price_minor)? != next
            {
                return Err(
                    outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                        "Nod unqualified-bin entry {iso_code}:{next}:{index} mismatches bucket {bucket_key}"
                    )),
                );
            }
            let issued_at = nod.callable_bucket_issued_at.read(&bucket_key)?;
            if (!strict && bucket.floor_price_minor >= rate) || !held_in_full(issued_at, day) {
                index += 1;
                inspected += 1;
                continue;
            }
            nod.qualify_bucket_loaded(scope, loaded)?;
            qualified_days.insert(worldwide_day.value());
            let last = count.checked_sub(1).ok_or_else(|| {
                outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                    "Nod bin {iso_code}:{next} count underflow"
                ))
            })?;
            if index != last {
                let replacement = nod
                    .unqualified_bin_buckets
                    .read(&NodContract::bin_index_key(iso_code, next, last))?;
                if replacement.is_zero() {
                    return Err(
                        outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                            "Nod unqualified-bin entry {iso_code}:{next}:{last} has an empty bucket key"
                        )),
                    );
                }
                nod.unqualified_bin_buckets.write(&key, replacement)?;
            }
            nod.unqualified_bin_buckets.write(
                &NodContract::bin_index_key(iso_code, next, last),
                alloy_primitives::B256::ZERO,
            )?;
            count = last;
            inspected += 1;
        }

        if count != initial_count {
            nod.unqualified_bin_count.write(&scoped, count)?;
        }
        let next_cursor = if index >= count { 0 } else { index };
        if next_cursor != initial_cursor {
            nod.unqualified_bin_scan_cursor
                .write(&scoped, next_cursor)?;
        }
        if count == 0 {
            tree_math::remove(&CurrencyBins(&nod, iso_code), next)?;
        } else if index < count {
            nod.qualify_scan_cursor.write(&iso_code, next)?;
            return Ok((inspected, false, qualified_days));
        }

        bin_cursor = match next.checked_add(1) {
            Some(next) if next <= MAX_BIN_ID => next,
            _ => {
                nod.qualify_scan_cursor.write(&iso_code, 0)?;
                return Ok((inspected, true, qualified_days));
            }
        };
    }
}
