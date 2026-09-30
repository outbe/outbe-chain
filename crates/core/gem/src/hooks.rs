use alloy_primitives::{B256, U256};
use outbe_oracle::{api::get_all_reference_currencies, schema::OracleContract};
use outbe_primitives::{
    address_pair::AddressPair,
    block::{BlockLifecycle, BlockRuntimeContext},
    daily_sweep::{Scheduled, SweepDays},
    error::{PrecompileError, Result},
    math::{constants::MAX_BIN_ID, tree_math},
    time::{previous_date_key, timestamp_to_date_key},
};

use crate::buckets::BucketBins;
use crate::constants::{CALL_SWEEP, MAX_BUCKET_VISITS_PER_BLOCK, MAX_EXPIRY_STEPS_PER_BLOCK};
use crate::precompile::IGem::{BatchMetadataUpdate, CallScanSkipped, SweepDaySkipped};
use crate::schema::GemContract;

pub struct GemLifecycle;

impl BlockLifecycle for GemLifecycle {
    type Context<'a, 'storage> = BlockRuntimeContext<'storage>;
    type EndBlockResult = ();

    fn begin_block(ctx: &BlockRuntimeContext) -> Result<()> {
        // A call sweep the daily trigger could not finish in one go carries on
        // here, block by block, rather than waiting a day for the next trigger.
        run_call_slice(ctx)?;
        sweep_expired(ctx)?;
        Ok(())
    }

    fn end_block(_ctx: &BlockRuntimeContext) -> Result<Self::EndBlockResult> {
        Ok(())
    }
}

/// The most recent fully-closed UTC day, or `None` while its VWAPs are not final.
fn closed_day(ctx: &BlockRuntimeContext) -> Result<Option<u32>> {
    let last_closed_day = previous_date_key(timestamp_to_date_key(ctx.block.timestamp));
    let finalized = OracleContract::new(ctx.storage.clone())
        .utc_day_vwap_last_finalized
        .read()?;
    if finalized < last_closed_day {
        tracing::warn!(target: "outbe::gem", last_closed_day, finalized, "utc-day VWAP not finalized yet, skipping the day's sweeps");
        return Ok(None);
    }
    Ok(Some(last_closed_day))
}

/// Index of the currency the cursor names, or the head when the registry dropped it.
pub(crate) fn currency_position(currencies: &[u16], cursor: u32) -> usize {
    u16::try_from(cursor)
        .ok()
        .and_then(|iso| currencies.iter().position(|&code| code == iso))
        .unwrap_or(0)
}

/// Trailing finalized daily VWAPs of one pair, newest first. `None` marks a day
/// the pair had no data for.
type VwapWindow = Vec<(u32, Option<U256>)>;

/// Cycle daily-trigger entry: open the day's Called sweep, discarding the count.
pub fn run_daily(ctx: &BlockRuntimeContext) -> Result<()> {
    scan_and_call(ctx)?;
    Ok(())
}

/// Cycle daily-trigger entry: schedule the day the Oracle has just finalized, opening
/// a Called sweep over it and running its first slice, or queueing it behind the
/// sweep still in flight.
pub fn scan_and_call(ctx: &BlockRuntimeContext) -> Result<u32> {
    let Some(last_closed_day) = closed_day(ctx)? else {
        return Ok(0);
    };

    let mut gem = GemContract::new(ctx.storage.clone());
    let days = SweepDays {
        current: gem.call_sweep_day.read()?,
        pending: gem.call_pending_day.read()?,
    };
    match days.schedule(last_closed_day) {
        (next, Scheduled::Opened) => {
            start_call_sweep(ctx, &gem, next)?;
            run_call_slice(ctx)
        }
        (next, Scheduled::Queued) => {
            gem.call_pending_day.write(next.pending)?;
            Ok(0)
        }
        (next, Scheduled::Replaced { skipped }) => {
            gem.call_pending_day.write(next.pending)?;
            gem.emit(SweepDaySkipped {
                sweep: CALL_SWEEP,
                skippedDay: skipped,
                inFlightDay: next.current,
            })?;
            Ok(0)
        }
        (_, Scheduled::Ignored) => Ok(0),
    }
}

/// Pin the sweep's current day and walk it from the first currency's lowest bin.
fn start_call_sweep(ctx: &BlockRuntimeContext, gem: &GemContract, days: SweepDays) -> Result<()> {
    gem.call_sweep_day.write(days.current)?;
    gem.call_pending_day.write(days.pending)?;
    gem.call_currency_cursor.write(0)?;
    for iso_code in get_all_reference_currencies(ctx)? {
        gem.bucket_scan_cursor.write(&iso_code, 0)?;
    }
    Ok(())
}

/// Advance an open sweep by one slice, pinned to the day it opened on so blocks
/// of it decide against the same prices. Returns how many buckets were called.
pub fn run_call_slice(ctx: &BlockRuntimeContext) -> Result<u32> {
    let called = call_slice(ctx)?;
    // Gem ids carry no order, so a call can only refresh the whole range.
    if called != 0 {
        GemContract::new(ctx.storage.clone()).emit(BatchMetadataUpdate {
            _fromTokenId: U256::ZERO,
            _toTokenId: U256::MAX,
        })?;
    }
    Ok(called)
}

fn call_slice(ctx: &BlockRuntimeContext) -> Result<u32> {
    let mut gem = GemContract::new(ctx.storage.clone());
    let pinned_day = gem.call_sweep_day.read()?;
    if pinned_day == 0 {
        return Ok(0);
    }
    let currencies = get_all_reference_currencies(ctx)?;
    let oracle = OracleContract::new(ctx.storage.clone());
    let start = currency_position(&currencies, gem.call_currency_cursor.read()?);
    let live_window = crate::config::read_from(&gem, ctx.block.chain_id)?.call_window_seconds;

    let mut budget = MAX_BUCKET_VISITS_PER_BLOCK;
    let mut windows: Vec<(u16, VwapWindow)> = Vec::new();
    let mut called: u32 = 0;
    // One pass down the list: a currency closed behind the cursor is never walked
    // again, so every sweep ends however much it leaves undecided.
    for &iso_code in currencies.iter().skip(start) {
        if budget == 0 {
            gem.call_currency_cursor.write(u32::from(iso_code))?;
            return Ok(called);
        }
        // A currency this day's pass could not price is settled for the day.
        if gem.call_scan_failed_day.read(&iso_code)? == pinned_day {
            continue;
        }
        // Peek the trie before pricing the currency: a drained one costs three
        // reads here instead of a whole VWAP window.
        let (cursor_bin, _) = unpack_cursor(gem.bucket_scan_cursor.read(&iso_code)?);
        if tree_math::find_first_left_inclusive(&BucketBins(&gem, iso_code), cursor_bin)?.is_none()
        {
            gem.bucket_scan_cursor.write(&iso_code, 0)?;
            continue;
        }
        let index = window_for(
            &gem,
            &oracle,
            &mut windows,
            iso_code,
            pinned_day,
            live_window,
        )?;
        let window = windows[index].1.as_slice();
        // Nothing priced above the window's high can have breached.
        let Some(high) = window.iter().filter_map(|(_, vwap)| *vwap).max() else {
            continue;
        };
        let ceiling = match GemContract::price_to_bin(high) {
            Ok(bin) => bin,
            Err(error) => {
                tracing::warn!(target: "outbe::gem", iso_code, error = ?error, "call scan: window price out of range, skipping currency for the day");
                gem.call_scan_failed_day.write(&iso_code, pinned_day)?;
                gem.emit(CallScanSkipped {
                    referenceCurrency: iso_code,
                    utcDay: pinned_day,
                })?;
                continue;
            }
        };
        let (calls, finished) = call_currency(ctx, iso_code, window, ceiling, &mut budget)?;
        called = called.saturating_add(calls);
        if !finished {
            gem.call_currency_cursor.write(u32::from(iso_code))?;
            return Ok(called);
        }
    }

    // The next day starts on the next block, so no slice mixes two days' prices.
    let next = SweepDays {
        current: pinned_day,
        pending: gem.call_pending_day.read()?,
    }
    .finish();
    if next.current == 0 {
        gem.call_sweep_day.write(0)?;
    } else {
        start_call_sweep(ctx, &gem, next)?;
    }
    Ok(called)
}

/// Walk one currency's bucket bins up to `ceiling`, resuming where it gave out.
/// Returns the calls made and whether the eligible range was walked to the end.
///
/// Each bin is walked from the top, so a call's swap-pop only moves a bucket already
/// visited, and a bin wider than the budget resumes inside itself.
pub(crate) fn call_currency(
    ctx: &BlockRuntimeContext,
    iso_code: u16,
    window: &[(u32, Option<U256>)],
    ceiling: u32,
    budget: &mut u32,
) -> Result<(u32, bool)> {
    let mut gem = GemContract::new(ctx.storage.clone());
    let now = ctx.block.timestamp;
    let (mut from_bin, mut remaining) = unpack_cursor(gem.bucket_scan_cursor.read(&iso_code)?);
    let mut called: u32 = 0;
    loop {
        let bin = match tree_math::find_first_left_inclusive(&BucketBins(&gem, iso_code), from_bin)?
        {
            Some(bin) if bin <= ceiling => bin,
            _ => {
                gem.bucket_scan_cursor.write(&iso_code, 0)?;
                return Ok((called, true));
            }
        };
        let count = gem
            .bucket_bin_count
            .read(&GemContract::scoped(iso_code, bin))?;
        remaining = if bin == from_bin && remaining != 0 {
            remaining.min(count)
        } else {
            count
        };
        while remaining > 0 {
            if *budget == 0 {
                gem.bucket_scan_cursor
                    .write(&iso_code, pack_cursor(bin, remaining))?;
                return Ok((called, false));
            }
            *budget -= 1;
            remaining -= 1;
            let bucket = gem
                .bucket_bin_at
                .read(&GemContract::bin_index_key(iso_code, bin, remaining))?;
            match ctx
                .storage
                .with_checkpoint(|| gem.trigger_bucket_call(window, bucket, now))
            {
                Ok(true) => called = called.saturating_add(1),
                Ok(false) => {}
                Err(error) if is_node_local(&error) => return Err(error),
                Err(error) => {
                    tracing::warn!(target: "outbe::gem", %bucket, error = ?error, "call scan: skipping bucket");
                }
            }
        }
        from_bin = match bin.checked_add(1) {
            Some(next) if next <= MAX_BIN_ID => next,
            _ => {
                gem.bucket_scan_cursor.write(&iso_code, 0)?;
                return Ok((called, true));
            }
        };
    }
}

const fn pack_cursor(bin: u32, remaining: u32) -> u64 {
    ((bin as u64) << 32) | remaining as u64
}

const fn unpack_cursor(packed: u64) -> (u32, u32) {
    ((packed >> 32) as u32, packed as u32)
}

/// Forfeit-burn the gems whose notice period closed; a head not due ends the pass.
fn sweep_expired(ctx: &BlockRuntimeContext) -> Result<u32> {
    let now = ctx.block.timestamp;
    let mut budget = MAX_EXPIRY_STEPS_PER_BLOCK;
    let mut burned: u32 = 0;

    while budget > 0 {
        let mut gem = GemContract::new(ctx.storage.clone());
        let Some(day) = gem.first_expiry_day()? else {
            break;
        };
        // A deadline lies inside its own hour, so an open one holds nobody due.
        if now < GemContract::hour_end(day) {
            break;
        }
        let len = gem.expiry_bucket_len.read(&day)?;
        let resume = match gem.expiry_sweep_day.read()? == day {
            true => gem.expiry_cursor.read()?.min(len),
            false => 0,
        };

        let mut slot = resume;
        while slot < len && budget > 0 {
            let Some(entry) = gem.expiry_slot(day, slot)? else {
                budget -= 1;
                slot += 1;
                continue;
            };
            if now <= gem.called_deadline.read(&entry)? {
                budget -= 1;
                slot += 1;
                continue;
            }
            if let Some(bucket) = gem.called_bucket(entry)? {
                if !forfeit_bucket(ctx, &mut gem, bucket, now, &mut budget, &mut burned)? {
                    break;
                }
                slot += 1;
                if gem.expiry_bucket_live.read(&day)? == 0 {
                    break;
                }
                continue;
            }
            budget -= 1;
            // Out of this bucket either way, so an entry that does not burn cannot hold the day
            // back; a Called gem among them is retried later rather than lost.
            match ctx.storage.with_checkpoint(|| gem.forfeit(entry, now)) {
                Ok(true) => burned = burned.saturating_add(1),
                Ok(false) => {
                    if !gem.requeue_or_drop(entry, now)? {
                        tracing::warn!(target: "outbe::gem", %entry, "expiry sweep: queued gem is not Called");
                    }
                }
                Err(error) if is_node_local(&error) => return Err(error),
                Err(error) => {
                    let deferred = gem.requeue_or_drop(entry, now)?;
                    tracing::warn!(target: "outbe::gem", %entry, deferred, error = ?error, "expiry sweep: forfeit failed");
                }
            }
            slot += 1;
            if gem.expiry_bucket_live.read(&day)? == 0 {
                break;
            }
        }

        // The last live entry left and retired the hour, cursor included.
        if gem.expiry_bucket_live.read(&day)? == 0 {
            continue;
        }

        if slot < len {
            gem.expiry_sweep_day.write(day)?;
            gem.expiry_cursor.write(slot)?;
            break;
        }
        gem.expiry_sweep_day.write(0)?;
        gem.expiry_cursor.write(0)?;
        // Anything left broke the invariant above; retiring it keeps the tree moving.
        if gem.expiry_bucket_live.read(&day)? != 0 {
            let (deferred, dropped) = gem.force_retire_hour(day, now)?;
            tracing::warn!(target: "outbe::gem", day, deferred, dropped, "expiry sweep: hour outlived itself, retiring it");
        }
    }
    Ok(burned)
}

/// Burn a due bucket's gems from its last member down, one budget step each. Returns
/// whether the bucket left its slot: emptied, or moved on after a gem failed to burn.
fn forfeit_bucket(
    ctx: &BlockRuntimeContext,
    gem: &mut GemContract<'_>,
    bucket: B256,
    now: u64,
    budget: &mut u32,
    burned: &mut u32,
) -> Result<bool> {
    let entry = crate::buckets::bucket_entry(bucket);
    loop {
        let count = gem.bucket_gem_count.read(&bucket)?;
        if count == 0 {
            return Ok(true);
        }
        if *budget == 0 {
            return Ok(false);
        }
        *budget -= 1;
        let gem_id = gem
            .bucket_gems
            .read(&GemContract::bucket_member_key(bucket, count - 1))?;
        let error = match ctx.storage.with_checkpoint(|| gem.forfeit(gem_id, now)) {
            Ok(true) => {
                *burned = burned.saturating_add(1);
                continue;
            }
            Ok(false) => None,
            Err(error) if is_node_local(&error) => return Err(error),
            Err(error) => Some(error),
        };
        match ctx
            .storage
            .with_checkpoint(|| gem.detach_called_member(gem_id, now))
        {
            Ok(()) => {
                tracing::warn!(target: "outbe::gem", %bucket, %gem_id, error = ?error, "expiry sweep: bucket member not forfeited, queued on its own");
            }
            Err(detach) if is_node_local(&detach) => return Err(detach),
            Err(detach) => {
                gem.requeue_or_drop(entry, now)?;
                tracing::warn!(target: "outbe::gem", %bucket, %gem_id, error = ?error, detach = ?detach, "expiry sweep: bucket member not forfeited, bucket deferred");
                return Ok(true);
            }
        }
    }
}

/// A failure of this node's own storage or readers must fail the block rather than turn
/// into a state change only this node makes; any other error is the same on every node.
fn is_node_local(error: &PrecompileError) -> bool {
    matches!(
        error,
        PrecompileError::Storage(_)
            | PrecompileError::BodyReadUnavailable(_)
            | PrecompileError::BodyReadRequestDeadline
            | PrecompileError::TreeUnavailable(_)
    )
}

/// Index into `cache` of the trailing finalized-VWAP window for `COEN/<iso>`,
/// newest first, filling it on first use.
///
/// An unregistered pair caches an empty window: a gem in that currency can
/// never register a breach, but it must still reach `forfeit`, so this is a
/// skip of the call check rather than a skip of the gem.
fn window_for(
    gem: &GemContract<'_>,
    oracle: &OracleContract<'_>,
    cache: &mut Vec<(u16, VwapWindow)>,
    iso_code: u16,
    last_closed_day: u32,
    live_window: u32,
) -> Result<usize> {
    if let Some(index) = cache.iter().position(|(code, _)| *code == iso_code) {
        return Ok(index);
    }
    let pair_index = oracle.pair_index_of(AddressPair::new_coen_to(iso_code))?;
    let mut window = Vec::new();
    if pair_index != 0 {
        // Widest of the live profile and anything ever issued: a gem keeps the window
        // it was issued with, so a narrowed profile must not shorten the span.
        let window_days = gem
            .max_call_window_seconds
            .read(&iso_code)?
            .max(live_window)
            / 86_400;
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
