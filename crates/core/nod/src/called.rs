//! Daily call scan: force-calls Nod buckets off the Oracle's finalized
//! per-UTC-day VWAPs, then forfeit-burns the Nods of a bucket whose notice
//! period lapsed. The Cycle daily trigger pins the closed UTC day and runs the
//! first slice; later CycleTicks continue the same day.
//!
//! One pass applies at most one transition per bucket, in lifecycle order, over
//! two arms sharing one visit budget:
//!
//! - *not called* -> *called*, walking each currency's call-price trie, when
//!   the reference price exceeded the bucket's call price on at least its
//!   `call_threshold` of the trailing `call_window`.
//! - *called* -> *forfeited*, walking the called-bucket list, when the bucket's
//!   `call_notice_period` has lapsed with Nods still unpaid. The two can never
//!   fire in one pass, since a bucket called now cannot also be a notice period
//!   past its call.
//!
//! All four terms are sealed onto the bucket at issuance and read back from it
//! here, so retuning a constant leaves every issued bucket on the terms it was
//! issued with. Gem and intex give the same guarantee.
//!
//! The breach rule needs no per-bucket streak state: the daily series is global
//! per currency, so one trailing window per currency decides every bucket
//! denominated in it, and the count is recomputed from oracle history on every
//! run rather than carried. Mirrors `outbe_gem::hooks::scan_and_call` and
//! `outbe_credisfactory::called::scan_and_call`, which evaluate the same shape.
//!
//! Calls count only days from `first_full_day` of the bucket's sealed `issued_at`.

use std::collections::BTreeSet;

use alloy_primitives::{B256, U256};
use outbe_compressed_entities::{ExecutionScope, ParentBodySource, WwdEntityId};
use outbe_oracle::{api::get_all_reference_currencies, schema::OracleContract};
use outbe_primitives::{
    block::BlockRuntimeContext,
    daily_sweep::{Scheduled, SweepDays},
    error::Result,
    math::{constants::MAX_BIN_ID, tree_math},
    storage::StorageHandle,
    time::{first_full_day, previous_date_key, timestamp_to_date_key},
};

use crate::{
    api,
    constants::{
        CALL_SWEEP, CALL_WINDOW, MAX_NOD_CALL_VISITS_PER_BLOCK, MAX_NOD_FORFEITS_PER_BLOCK,
        SECS_PER_DAY,
    },
    precompile::INod,
    schema::{CallTerms, NodContract},
    state::CallBins,
};

pub(crate) const CALL_ARM_DONE: u32 = u32::MAX;

/// Trailing finalized daily VWAPs of one `COEN/<iso>` pair, newest first.
/// `None` marks a day the pair published no reference price.
type VwapWindow = Vec<(u32, Option<U256>)>;

/// Schedule the day the Oracle has just finalized: open a Called sweep over it
/// and run its first slice, or queue it behind the sweep still in flight.
///
/// Never returns `Err` for missing market data: the Cycle dispatcher propagates
/// a handler error out of the `CycleTick` system transaction, which fails the
/// block, so an unregistered pair, an unpriced currency or an unfinalized day
/// each degrade to "no transition" instead.
pub fn scan_and_call(
    ctx: &BlockRuntimeContext,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
) -> Result<u32> {
    let Some(last_closed_day) = closed_day(ctx)? else {
        return Ok(0);
    };
    let mut nod = NodContract::new(ctx.storage.clone());
    if nod.call_sweep_day.read()? == 0 && !has_call_work(ctx, &nod)? {
        return Ok(0);
    }
    let days = SweepDays {
        current: nod.call_sweep_day.read()?,
        pending: nod.call_pending_day.read()?,
    };
    match days.schedule(last_closed_day) {
        (next, Scheduled::Opened) => {
            start_call_sweep(ctx, &nod, next)?;
            run_call_slice(ctx, scope, parent)
        }
        (next, Scheduled::Queued) => {
            nod.call_pending_day.write(next.pending)?;
            Ok(0)
        }
        (next, Scheduled::Replaced { skipped }) => {
            nod.call_pending_day.write(next.pending)?;
            nod.emit(INod::SweepDaySkipped {
                sweep: CALL_SWEEP,
                skippedDay: skipped,
                inFlightDay: next.current,
            })?;
            Ok(0)
        }
        (_, Scheduled::Ignored) => Ok(0),
    }
}

fn has_call_work(ctx: &BlockRuntimeContext, nod: &NodContract) -> Result<bool> {
    if nod.called_buckets.len()? != 0 {
        return Ok(true);
    }
    for iso_code in get_all_reference_currencies(ctx)? {
        if !nod.call_bin_tree_root.read(&iso_code)?.is_zero() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The most recent fully-closed UTC day, or `None` while its VWAPs are not final.
pub(crate) fn closed_day(ctx: &BlockRuntimeContext) -> Result<Option<u32>> {
    let last_closed_day = previous_date_key(timestamp_to_date_key(ctx.block.timestamp));
    let finalized = OracleContract::new(ctx.storage.clone())
        .utc_day_vwap_last_finalized
        .read()?;
    if finalized < last_closed_day {
        tracing::warn!(
            target: "outbe::nod",
            last_closed_day,
            finalized,
            "nod: utc-day VWAP not finalized yet, skipping the day's sweeps"
        );
        return Ok(None);
    }
    Ok(Some(last_closed_day))
}

/// Pin the sweep's current day and walk it from the first currency's lowest bin.
fn start_call_sweep(ctx: &BlockRuntimeContext, nod: &NodContract, days: SweepDays) -> Result<()> {
    nod.call_sweep_day.write(days.current)?;
    nod.call_pending_day.write(days.pending)?;
    nod.call_currency_cursor.write(0)?;
    nod.forfeit_cursor.write(0)?;
    for iso_code in get_all_reference_currencies(ctx)? {
        nod.call_bin_cursor.write(&iso_code, 0)?;
    }
    Ok(())
}

fn finish_call_sweep(ctx: &BlockRuntimeContext, nod: &NodContract, pinned_day: u32) -> Result<()> {
    let next = SweepDays {
        current: pinned_day,
        pending: nod.call_pending_day.read()?,
    }
    .finish();
    if next.current == 0 {
        nod.call_sweep_day.write(0)
    } else {
        start_call_sweep(ctx, nod, next)
    }
}

/// Advance an open sweep by one slice, pinned to the day it opened on so
/// later blocks decide against the same prices. Returns how many buckets
/// were called plus Nods forfeited.
pub fn run_call_slice(
    ctx: &BlockRuntimeContext,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
) -> Result<u32> {
    let mut nod = NodContract::new(ctx.storage.clone());
    let pinned_day = nod.call_sweep_day.read()?;
    if pinned_day == 0 {
        return Ok(0);
    }
    let oracle = OracleContract::new(ctx.storage.clone());
    let finalized = oracle.utc_day_vwap_last_finalized.read()?;
    if finalized < pinned_day {
        tracing::warn!(
            target: "outbe::nod",
            pinned_day,
            finalized,
            "nod call scan: pinned utc-day VWAP not finalized, holding the sweep"
        );
        return Ok(0);
    }

    let mut visits: u32 = 0;
    let mut mutated: u32 = 0;
    if nod.call_currency_cursor.read()? != CALL_ARM_DONE {
        let mut called_days = BTreeSet::new();
        let (called, finished) = call_arm(
            ctx,
            &mut nod,
            &oracle,
            pinned_day,
            &mut visits,
            &mut called_days,
        )?;
        nod.emit_days_metadata_update(&called_days)?;
        mutated = mutated.saturating_add(called);
        if !finished {
            return Ok(mutated);
        }
        nod.call_currency_cursor.write(CALL_ARM_DONE)?;
    }
    let (forfeited, finished) = forfeit_arm(ctx, scope, parent, &mut nod, &mut visits)?;
    mutated = mutated.saturating_add(forfeited);
    if finished {
        finish_call_sweep(ctx, &nod, pinned_day)?;
    }
    Ok(mutated)
}

/// Returns the buckets called and whether every currency was walked.
fn call_arm(
    ctx: &BlockRuntimeContext,
    nod: &mut NodContract<'_>,
    oracle: &OracleContract<'_>,
    pinned_day: u32,
    visits: &mut u32,
    called_days: &mut BTreeSet<u32>,
) -> Result<(u32, bool)> {
    let currencies = get_all_reference_currencies(ctx)?;
    let start = currency_position(&currencies, nod.call_currency_cursor.read()?);
    let mut windows: Vec<(u16, VwapWindow)> = Vec::new();
    let mut called: u32 = 0;
    for &iso_code in currencies.iter().skip(start) {
        if nod.call_bin_tree_root.read(&iso_code)?.is_zero() {
            continue;
        }
        let index = window_for(
            nod,
            &ctx.storage,
            oracle,
            &mut windows,
            iso_code,
            pinned_day,
        )?;
        let window = windows[index].1.as_slice();
        // Nothing priced above the window's high can have breached.
        let Some(high) = window.iter().filter_map(|(_, vwap)| *vwap).max() else {
            continue;
        };
        let ceiling = match NodContract::price_to_bin(high) {
            Ok(bin) => bin,
            Err(error) => {
                tracing::warn!(
                    target: "outbe::nod",
                    iso_code,
                    error = ?error,
                    "nod call scan: window price out of range, skipping currency for the day"
                );
                continue;
            }
        };
        let (calls, finished) =
            call_currency(ctx, nod, iso_code, window, ceiling, visits, called_days)?;
        called = called.saturating_add(calls);
        if !finished {
            nod.call_currency_cursor.write(u32::from(iso_code))?;
            return Ok((called, false));
        }
    }
    Ok((called, true))
}

/// Each bin is walked from the top, so a call's swap-pop only moves an entry already visited.
pub(crate) fn call_currency(
    ctx: &BlockRuntimeContext,
    nod: &mut NodContract<'_>,
    iso_code: u16,
    window: &[(u32, Option<U256>)],
    ceiling: u32,
    visits: &mut u32,
    called_days: &mut BTreeSet<u32>,
) -> Result<(u32, bool)> {
    let now = ctx.block.timestamp;
    let (mut from_bin, mut remaining) = unpack_cursor(nod.call_bin_cursor.read(&iso_code)?);
    let mut called: u32 = 0;
    loop {
        let bin_id = match tree_math::find_first_left_inclusive(&CallBins(nod, iso_code), from_bin)?
        {
            Some(bin) if bin <= ceiling => bin,
            _ => {
                nod.call_bin_cursor.write(&iso_code, 0)?;
                return Ok((called, true));
            }
        };
        let count = nod
            .call_bin_count
            .read(&NodContract::scoped(iso_code, bin_id))?;
        remaining = if bin_id == from_bin && remaining != 0 {
            remaining.min(count)
        } else {
            count
        };
        while remaining > 0 {
            if *visits >= MAX_NOD_CALL_VISITS_PER_BLOCK {
                nod.call_bin_cursor
                    .write(&iso_code, pack_cursor(bin_id, remaining))?;
                return Ok((called, false));
            }
            *visits += 1;
            remaining -= 1;
            let bucket_key = nod
                .call_bin_buckets
                .read(&NodContract::bin_index_key(iso_code, bin_id, remaining))?;
            if try_call(ctx, nod, window, bucket_key, now)? {
                called = called.saturating_add(1);
                called_days.insert(nod.bucket_worldwide_day.read(&bucket_key)?.value());
            }
        }
        from_bin = match bin_id.checked_add(1) {
            Some(next) if next <= MAX_BIN_ID => next,
            _ => {
                nod.call_bin_cursor.write(&iso_code, 0)?;
                return Ok((called, true));
            }
        };
    }
}

fn try_call(
    ctx: &BlockRuntimeContext,
    nod: &mut NodContract<'_>,
    window: &[(u32, Option<U256>)],
    bucket_key: B256,
    now: u64,
) -> Result<bool> {
    if nod.bucket_nod_count.read(&bucket_key)? == 0 || nod.bucket_called_at.read(&bucket_key)? != 0
    {
        return Ok(false);
    }
    let issued_at = nod.callable_bucket_issued_at.read(&bucket_key)?;
    let terms = nod.read_call_terms(bucket_key)?;
    if !breached_enough(window, &terms, first_full_day(issued_at)) {
        return Ok(false);
    }
    // A failing bucket rolls back alone, so it never halts the scan.
    Ok(ctx
        .storage
        .with_checkpoint(|| mark_called(nod, bucket_key, now, terms.call_notice_period))
        .is_ok())
}

/// Returns the Nods burned and whether the walk reached the bottom.
fn forfeit_arm(
    ctx: &BlockRuntimeContext,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    nod: &mut NodContract<'_>,
    visits: &mut u32,
) -> Result<(u32, bool)> {
    let len = nod.called_buckets.len()?;
    if len == 0 {
        return Ok((0, true));
    }
    // Stored as `index + 1`; 0 means "start a fresh pass from the top".
    let initial_cursor = nod.forfeit_cursor.read()?;
    let mut cursor = match initial_cursor {
        0 => len - 1,
        resume => resume.saturating_sub(1).min(len - 1),
    };
    let now = ctx.block.timestamp;
    let mut forfeited: u32 = 0;

    // Descending walk: removing a bucket swap-pops the tail into the hole, and
    // the tail is already behind a descending cursor, so no live entry is
    // skipped and none is visited twice.
    let completed = loop {
        if *visits >= MAX_NOD_CALL_VISITS_PER_BLOCK {
            break false;
        }
        if let Some(bucket_key) = nod.called_buckets.get(cursor)? {
            *visits += 1;
            let called_at = nod.bucket_called_at.read(&bucket_key)?;
            // Paid entitlements retain their bucket terms, but cannot be forfeited.
            let has_unpaid = nod.bucket_nod_count.read(&bucket_key)? != 0;
            if has_unpaid
                && now > api::settlement_deadline_of(called_at, notice_period(nod, bucket_key)?)
            {
                let budget = MAX_NOD_FORFEITS_PER_BLOCK.saturating_sub(forfeited);
                if budget == 0 {
                    break false;
                }
                let res = ctx.storage.with_checkpoint(|| {
                    forfeit_members(&ctx.storage, nod, scope, parent, bucket_key, budget)
                });
                if let Ok(burned) = res {
                    forfeited = forfeited.saturating_add(burned);
                    // The next slice resumes on this bucket.
                    if burned == budget && nod.bucket_nod_count.read(&bucket_key)? != 0 {
                        break false;
                    }
                }
            }
        }
        if cursor == 0 {
            break true;
        }
        cursor -= 1;
    };

    let next_cursor = if completed {
        0
    } else {
        cursor.saturating_add(1)
    };
    if next_cursor != initial_cursor {
        nod.forfeit_cursor.write(next_cursor)?;
    }
    Ok((forfeited, completed))
}

/// Index of the currency the cursor names, or the head when the registry dropped it.
pub(crate) fn currency_position(currencies: &[u16], cursor: u32) -> usize {
    u16::try_from(cursor)
        .ok()
        .and_then(|iso| currencies.iter().position(|&code| code == iso))
        .unwrap_or(0)
}

const fn pack_cursor(bin_id: u32, remaining: u32) -> u64 {
    ((bin_id as u64) << 32) | remaining as u64
}

const fn unpack_cursor(packed: u64) -> (u32, u32) {
    ((packed >> 32) as u32, packed as u32)
}

/// True when the bucket's trailing `call_window` carries at least its
/// `call_threshold` of days strictly above its `call_price`.
///
/// Every term comes off the bucket, not from the constants, so a retune cannot
/// re-term a bucket that is already armed. `window` is sized for the widest
/// window in the currency, so this takes only its own prefix.
///
/// Days at or below the call price, and days with no published price, both
/// simply fail to count, so the window absorbs up to `window - threshold` of
/// either. The walk stops at the first UTC day preceding `first_full_day` of
/// the bucket's sealed `issued_at`, so a delayed materialization cannot inherit
/// a breach run from before the right existed, and a partial issuance UTC day
/// does not count. The window is newest-first, so everything beyond that point
/// is older still.
fn breached_enough(window: &[(u32, Option<U256>)], terms: &CallTerms, start_day: u32) -> bool {
    let window_days = terms.call_window / SECS_PER_DAY;
    let threshold_days = terms.call_threshold / SECS_PER_DAY;
    if threshold_days > window_days {
        return false;
    }
    let mut breaches: u32 = 0;
    for (day, vwap) in window.iter().take(window_days as usize) {
        if *day < start_day {
            break;
        }
        if vwap.is_some_and(|value| value > terms.call_price) {
            breaches += 1;
            if breaches >= threshold_days {
                return true;
            }
        }
    }
    false
}

/// The bucket's sealed notice period. Read on its own in the forfeit arm, which
/// needs no other term.
fn notice_period(nod: &NodContract<'_>, bucket_key: B256) -> Result<u32> {
    nod.callable_bucket_call_notice_period.read(&bucket_key)
}

/// Stamps the call and opens the settlement window the bucket sealed.
fn mark_called(
    nod: &mut NodContract<'_>,
    bucket_key: B256,
    now: u64,
    notice_period: u32,
) -> Result<()> {
    nod.remove_call_bin(bucket_key)?;
    nod.push_called_bucket(bucket_key)?;
    nod.bucket_called_at.write(&bucket_key, now)?;
    nod.emit(INod::NodBucketCalled {
        bucketKey: bucket_key,
        calledAt: now,
        settlementDeadline: api::settlement_deadline_of(now, notice_period),
    })
}

/// Forfeit-burns up to `budget` of a lapsed bucket's remaining unpaid Nods, newest
/// first. Returns how many were burned.
///
/// A bucket holding more members than the budget resumes on the next run, which
/// cannot change an outcome: the deadline has already passed and settlement is
/// closed, so nothing can rescue the remainder. Removing the last member deletes
/// the bucket body and drops it from the called list.
///
/// Each burned load returns to the Promis Reserve. Lysis drew it out of the day
/// limit and only mining converts it into Gratis, so a load that is destroyed
/// unmined would otherwise leave the reserve with nothing minted against it.
/// The credit is one accumulated write per pass, and the caller's checkpoint
/// makes it atomic with the burns it accounts for.
pub(crate) fn forfeit_members(
    storage: &StorageHandle<'_>,
    nod: &mut NodContract<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    bucket_key: B256,
    budget: u32,
) -> Result<u32> {
    let worldwide_day = nod.bucket_worldwide_day.read(&bucket_key)?;
    let mut burned: u32 = 0;
    let mut credit = U256::ZERO;
    while burned < budget {
        let count = nod.bucket_nod_count.read(&bucket_key)?;
        let Some(last) = count.checked_sub(1) else {
            break;
        };
        let nod_id = nod
            .bucket_nods
            .read(&NodContract::bucket_nod_key(bucket_key, last))?;
        if nod_id.is_zero() {
            return Err(
                outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                    "Nod bucket {bucket_key} member slot {last} is empty during forfeit"
                )),
            );
        }
        let item = api::load_item(storage, scope, parent, nod_id)?.ok_or_else(|| {
            outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                "Nod bucket {bucket_key} member {nod_id} has no body during forfeit"
            ))
        })?;
        if item.body().is_settled || item.body().bucket_key != bucket_key {
            return Err(
                outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                    "Nod bucket {bucket_key} indexes an ineligible member {nod_id}"
                )),
            );
        }
        let owner = item.body().owner;
        let gratis_load_minor = item.body().gratis_load_minor;
        let bucket_id = WwdEntityId::from_day_and_digest(worldwide_day, bucket_key.0);
        let bucket = api::load_bucket(storage, scope, parent, bucket_id)?.ok_or_else(|| {
            outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                "Nod bucket {bucket_key} has no body during forfeit"
            ))
        })?;
        api::remove_nod(storage, scope, item, bucket)?;
        nod.emit(INod::NodForfeited {
            owner,
            nodId: nod_id.to_u256(),
            gratisLoadMinor: gratis_load_minor,
        })?;
        credit = credit.checked_add(gratis_load_minor).ok_or_else(|| {
            outbe_primitives::error::PrecompileError::Revert(
                "Nod forfeit Promis Reserve credit overflow".into(),
            )
        })?;
        burned = burned.saturating_add(1);
    }
    if !credit.is_zero() {
        outbe_promislimit::PromisLimitContract::new(storage.clone())
            .add_to_total_unallocated(credit)?;
    }
    Ok(burned)
}

/// Index into `cache` of the trailing finalized-VWAP window for `COEN/<iso>`,
/// newest first, filling it on first use.
///
/// An unregistered pair caches an empty window: a bucket in that currency never
/// registers a breach.
fn window_for(
    nod: &NodContract<'_>,
    storage: &StorageHandle<'_>,
    oracle: &OracleContract<'_>,
    cache: &mut Vec<(u16, VwapWindow)>,
    iso_code: u16,
    last_closed_day: u32,
) -> Result<usize> {
    if let Some(index) = cache.iter().position(|(code, _)| *code == iso_code) {
        return Ok(index);
    }
    let mut window = Vec::new();
    if let Some(pair_index) = outbe_oracle::api::coen_pair_index_opt(storage.clone(), iso_code)? {
        // Widest of the current constant and anything ever armed: a bucket keeps
        // the window it was armed with, so a narrowed constant must not shorten
        // the span the scan collects for it.
        let window_days = nod.max_call_window.read(&iso_code)?.max(CALL_WINDOW) / SECS_PER_DAY;
        window.reserve(window_days as usize);
        let mut day = last_closed_day;
        for _ in 0..window_days {
            window.push((day, oracle.get_utc_day_vwap_for_pair(day, pair_index)?));
            day = previous_date_key(day);
        }
    }
    cache.push((iso_code, window));
    Ok(cache.len() - 1)
}
