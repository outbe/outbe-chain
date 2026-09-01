use alloy_primitives::U256;
use outbe_oracle::{
    api::{fresh_coen_rate_for_opt, get_all_reference_currencies},
    schema::OracleContract,
};
use outbe_primitives::{
    address_pair::AddressPair,
    block::{BlockLifecycle, BlockRuntimeContext},
    error::Result,
    math::{constants::MAX_BIN_ID, tree_math},
    time::{previous_date_key, timestamp_to_date_key},
};

use crate::constants::{
    CALL_WINDOW, MAX_GEM_CALLS_PER_BLOCK, MAX_GEM_FORFEITS_PER_RUN,
    MAX_GEM_QUALIFICATIONS_PER_BLOCK,
};
use crate::schema::GemContract;
use crate::state::{CurrencyBins, QualifiedBins};

pub struct GemLifecycle;

impl BlockLifecycle for GemLifecycle {
    type Context<'a, 'storage> = BlockRuntimeContext<'storage>;
    type EndBlockResult = ();

    fn begin_block(ctx: &BlockRuntimeContext) -> Result<()> {
        scan_and_qualify(ctx)?;
        // A call sweep the daily trigger could not finish in one go carries on
        // here, block by block, rather than waiting a day for the next trigger.
        run_call_slice(ctx)?;
        Ok(())
    }

    fn end_block(_ctx: &BlockRuntimeContext) -> Result<Self::EndBlockResult> {
        Ok(())
    }
}

/// Qualifies every reference currency the oracle knows about, each against its
/// own COEN rate, sharing one per-block budget. An uninitialized registry does
/// no work. A currency whose COEN pair is unregistered or unpriced is skipped
/// for this block rather than halting it. The scan resumes from a persisted
/// currency cursor, so a spent budget defers the rest of the list to the next
/// block instead of dropping it.
pub fn scan_and_qualify(ctx: &BlockRuntimeContext) -> Result<()> {
    let currencies = get_all_reference_currencies(ctx)?;
    if currencies.is_empty() {
        return Ok(());
    }
    let gem = GemContract::new(ctx.storage.clone());
    let start = gem.qualify_currency_cursor.read()? as usize % currencies.len();

    let mut budget = MAX_GEM_QUALIFICATIONS_PER_BLOCK;
    let mut resume_at = start;
    for offset in 0..currencies.len() {
        let at = (start + offset) % currencies.len();
        if budget == 0 {
            // Resume here, so a heavy currency cannot starve the ones behind it.
            resume_at = at;
            break;
        }
        let Some(rate) = fresh_coen_rate_for_opt(ctx.storage.clone(), currencies[at])? else {
            continue;
        };
        let inspected = qualify_with_rate(ctx, currencies[at], rate, budget)?;
        budget = budget.saturating_sub(inspected);
    }
    gem.qualify_currency_cursor.write(resume_at as u32)?;
    Ok(())
}

/// Drains the floor-bins crossed by one currency's `rate`, inspecting at most
/// `budget` gems. Returns how many it inspected so the caller can share one
/// per-block budget across currencies.
///
/// Only this currency's trie is walked, so no gem of another currency is ever
/// read here. Whole bins are processed atomically, so the resumption cursor is
/// bin-granular and a bin larger than the remaining budget overshoots it.
pub(crate) fn qualify_with_rate(
    ctx: &BlockRuntimeContext,
    iso_code: u16,
    rate: U256,
    budget: u32,
) -> Result<u32> {
    let now = ctx.block.timestamp;
    let r_bin = GemContract::price_to_bin(rate)?;
    let mut gem = GemContract::new(ctx.storage.clone());

    let mut inspected: u32 = 0;
    let mut cursor: u32 = gem.qualify_scan_cursor.read(&iso_code)?;
    loop {
        if inspected >= budget {
            gem.qualify_scan_cursor.write(&iso_code, cursor)?;
            break;
        }
        let next =
            match tree_math::find_first_left_inclusive(&CurrencyBins(&gem, iso_code), cursor)? {
                Some(b) if b <= r_bin => b,
                _ => {
                    // End of the eligible range: next block sweeps from the bottom.
                    gem.qualify_scan_cursor.write(&iso_code, 0)?;
                    break;
                }
            };

        // Snapshot the bin's gem_ids before mutating; qualify() calls
        // remove_unqualified() on success which shifts entries in storage.
        let count = gem
            .unqualified_bin_count
            .read(&GemContract::scoped(iso_code, next))?;
        let mut bin_gems: Vec<U256> = Vec::with_capacity(count as usize);
        for i in 0..count {
            let id = gem
                .unqualified_bin_gems
                .read(&GemContract::bin_index_key(iso_code, next, i))?;
            if !id.is_zero() {
                bin_gems.push(id);
            }
        }

        for gem_id in bin_gems {
            inspected = inspected.saturating_add(1);
            gem.qualify(gem_id, now, iso_code, rate)?;
        }

        cursor = match next.checked_add(1) {
            Some(c) if c <= MAX_BIN_ID => c,
            _ => {
                gem.qualify_scan_cursor.write(&iso_code, 0)?;
                break;
            }
        };
    }
    Ok(inspected)
}

/// Trailing finalized daily VWAPs of one pair, newest first. `None` marks a day
/// the pair had no data for.
type VwapWindow = Vec<(u32, Option<U256>)>;

/// Cycle daily-trigger entry: run the Called scan, discarding the count.
pub fn run_call_daily(ctx: &BlockRuntimeContext) -> Result<()> {
    scan_and_call(ctx)?;
    sweep_expired(ctx)?;
    Ok(())
}

/// Cycle daily-trigger entry: open a Called sweep over the day the Oracle has
/// just finalized and run its first slice.
pub fn scan_and_call(ctx: &BlockRuntimeContext) -> Result<u32> {
    let oracle = OracleContract::new(ctx.storage.clone());

    // Most recent fully-closed UTC day (finalized VWAP).
    let last_closed_day = previous_date_key(timestamp_to_date_key(ctx.block.timestamp));
    let finalized = oracle.utc_day_vwap_last_finalized.read()?;
    if finalized < last_closed_day {
        tracing::warn!(target: "outbe::gem", last_closed_day, finalized, "call scan: utc-day VWAP not finalized yet, skipping run");
        return Ok(0);
    }

    let gem = GemContract::new(ctx.storage.clone());
    gem.call_sweep_day.write(last_closed_day)?;
    // A sweep in flight is superseded: its mid-range cursors would let the new one
    // call itself done over bins it never walked.
    gem.call_currency_cursor.write(0)?;
    for iso_code in get_all_reference_currencies(ctx)? {
        gem.call_scan_cursor.write(&iso_code, 0)?;
    }
    run_call_slice(ctx)
}

/// Advance an open sweep by one slice, pinned to the day it opened on so blocks
/// of it decide against the same prices. Returns how many gems were called.
pub fn run_call_slice(ctx: &BlockRuntimeContext) -> Result<u32> {
    let gem = GemContract::new(ctx.storage.clone());
    let pinned_day = gem.call_sweep_day.read()?;
    if pinned_day == 0 {
        return Ok(0);
    }
    let currencies = get_all_reference_currencies(ctx)?;
    if currencies.is_empty() {
        gem.call_sweep_day.write(0)?;
        return Ok(0);
    }
    let oracle = OracleContract::new(ctx.storage.clone());
    let start = gem.call_currency_cursor.read()? as usize % currencies.len();

    let mut budget = MAX_GEM_CALLS_PER_BLOCK;
    let mut windows: Vec<(u16, VwapWindow)> = Vec::new();
    let mut called: u32 = 0;
    // The first currency left unfinished, so the next slice picks up where this
    // one gave out rather than re-walking the ones already closed behind it.
    let mut resume_at = start;
    let mut resumed = false;
    let mut swept = true;
    for offset in 0..currencies.len() {
        let at = (start + offset) % currencies.len();
        if budget == 0 {
            if !resumed {
                resume_at = at;
            }
            swept = false;
            break;
        }
        let iso_code = currencies[at];
        let index = window_for(&oracle, &mut windows, iso_code, pinned_day)?;
        let window = windows[index].1.as_slice();
        // Nothing priced above the window's high can have breached.
        let Some(high) = window.iter().filter_map(|(_, vwap)| *vwap).max() else {
            continue;
        };
        let ceiling = GemContract::price_to_bin(high)?;
        let (calls, finished) = call_currency(ctx, iso_code, window, ceiling, &mut budget)?;
        called = called.saturating_add(calls);
        if !finished && !resumed {
            resume_at = at;
            resumed = true;
        }
        swept &= finished;
    }
    gem.call_currency_cursor.write(resume_at as u32)?;
    if swept {
        // Nothing left to walk: the next daily trigger opens a fresh sweep.
        gem.call_sweep_day.write(0)?;
    }
    Ok(called)
}

/// Walk one currency's qualified bins up to `ceiling`, resuming where it gave out.
/// Returns the calls made and whether the eligible range was walked to the end.
pub(crate) fn call_currency(
    ctx: &BlockRuntimeContext,
    iso_code: u16,
    window: &[(u32, Option<U256>)],
    ceiling: u32,
    budget: &mut u32,
) -> Result<(u32, bool)> {
    let mut gem = GemContract::new(ctx.storage.clone());
    let now = ctx.block.timestamp;
    let mut called: u32 = 0;
    let mut cursor = gem.call_scan_cursor.read(&iso_code)?;
    let mut finished = false;
    loop {
        if *budget == 0 {
            gem.call_scan_cursor.write(&iso_code, cursor)?;
            break;
        }
        let next =
            match tree_math::find_first_left_inclusive(&QualifiedBins(&gem, iso_code), cursor)? {
                Some(bin) if bin <= ceiling => bin,
                _ => {
                    gem.call_scan_cursor.write(&iso_code, 0)?;
                    finished = true;
                    break;
                }
            };

        // Snapshot the bin: calling a gem removes it and shifts the rest. Whole
        // bins go at once, so the cursor never advances past a gem it skipped.
        for gem_id in gem.qualified_bin_gems_at(iso_code, next)? {
            *budget = budget.saturating_sub(1);
            match ctx
                .storage
                .with_checkpoint(|| gem.trigger_call(window, gem_id, now))
            {
                Ok(true) => called = called.saturating_add(1),
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(target: "outbe::gem", %gem_id, error = ?error, "call scan: skipping gem");
                }
            }
        }

        cursor = match next.checked_add(1) {
            Some(c) if c <= MAX_BIN_ID => c,
            _ => {
                gem.call_scan_cursor.write(&iso_code, 0)?;
                finished = true;
                break;
            }
        };
    }
    Ok((called, finished))
}

/// Forfeit-burn the gems whose notice period closed; a head not due ends the pass.
fn sweep_expired(ctx: &BlockRuntimeContext) -> Result<u32> {
    let mut gem = GemContract::new(ctx.storage.clone());
    let head = gem.called_head.read()?;
    let tail = gem.called_tail.read()?;
    if head >= tail {
        return Ok(0);
    }

    let now = ctx.block.timestamp;
    let mut budget = MAX_GEM_FORFEITS_PER_RUN;
    let mut burned: u32 = 0;
    // Slots, not just actions: an entry the sweep cannot retire pins the head, and
    // the run of emptied slots behind it would otherwise grow without bound.
    for index in head..tail.min(head.saturating_add(MAX_GEM_FORFEITS_PER_RUN)) {
        if budget == 0 {
            break;
        }
        let Some(gem_id) = gem.called_queue_slot(index)? else {
            continue;
        };
        if now <= gem.called_deadline.read(&gem_id)? {
            break;
        }
        budget -= 1;
        match ctx.storage.with_checkpoint(|| gem.forfeit(gem_id, now)) {
            Ok(true) => burned = burned.saturating_add(1),
            // Due and still not burning: the entry no longer matches its gem.
            Ok(false) => {
                tracing::warn!(target: "outbe::gem", %gem_id, "expiry sweep: queued gem is not Called")
            }
            Err(error) => {
                tracing::warn!(target: "outbe::gem", %gem_id, error = ?error, "expiry sweep: skipping gem")
            }
        }
    }
    gem.compact_called_queue()?;
    Ok(burned)
}

/// Index into `cache` of the trailing finalized-VWAP window for `COEN/<iso>`,
/// newest first, filling it on first use.
///
/// An unregistered pair caches an empty window: a gem in that currency can
/// never register a breach, but it must still reach `forfeit`, so this is a
/// skip of the call check rather than a skip of the gem.
fn window_for(
    oracle: &OracleContract<'_>,
    cache: &mut Vec<(u16, VwapWindow)>,
    iso_code: u16,
    last_closed_day: u32,
) -> Result<usize> {
    if let Some(index) = cache.iter().position(|(code, _)| *code == iso_code) {
        return Ok(index);
    }
    let pair_index = oracle.pair_index_of(AddressPair::new_coen_to(iso_code))?;
    let mut window = Vec::new();
    if pair_index != 0 {
        // CALL_WINDOW is stored in seconds; the daily scan needs the day count.
        let window_days = CALL_WINDOW / 86_400;
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
