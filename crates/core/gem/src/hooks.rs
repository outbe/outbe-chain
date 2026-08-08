use alloy_primitives::U256;
use outbe_oracle::api::coen_rate_for;
use outbe_primitives::{
    block::{BlockLifecycle, BlockRuntimeContext},
    error::Result,
    math::{constants::MAX_BIN_ID, tree_math},
    time::{previous_date_key, timestamp_to_date_key},
};

use crate::constants::{CALL_WINDOW, QUALIFIER_REFERENCE_ISO};
use crate::schema::GemContract;

pub struct GemLifecycle;

impl BlockLifecycle for GemLifecycle {
    type Context<'a, 'storage> = BlockRuntimeContext<'storage>;
    type EndBlockResult = ();

    fn begin_block(ctx: &BlockRuntimeContext) -> Result<()> {
        // TODO refactor this. Oracle is called here for each block
        scan_and_qualify(ctx)?;
        Ok(())
    }

    fn end_block(_ctx: &BlockRuntimeContext) -> Result<Self::EndBlockResult> {
        Ok(())
    }
}

pub fn scan_and_qualify(ctx: &BlockRuntimeContext) -> Result<u32> {
    // No pair or no published rate: skip this block's scan rather than halt it.
    let Some(rate) = coen_rate_for(ctx.storage.clone(), QUALIFIER_REFERENCE_ISO)? else {
        return Ok(0);
    };
    if rate.is_zero() {
        return Ok(0);
    }

    let now = ctx.block.timestamp;
    let r_bin = GemContract::price_to_bin(rate)?;
    let mut gem = GemContract::new(ctx.storage.clone());

    let mut promoted: u32 = 0;
    let mut cursor: u32 = 0;
    loop {
        let next = match tree_math::find_first_left_inclusive(&gem, cursor)? {
            Some(b) if b <= r_bin => b,
            _ => break,
        };

        // Snapshot the bin's gem_ids before mutating; qualify() calls
        // remove_unqualified() on success which shifts entries in storage.
        let count = gem.unqualified_bin_count.read(&next)?;
        let mut bin_gems: Vec<U256> = Vec::with_capacity(count as usize);
        for i in 0..count {
            let id = gem
                .unqualified_bin_gems
                .read(&GemContract::bin_index_key(next, i))?;
            if !id.is_zero() {
                bin_gems.push(id);
            }
        }

        for gem_id in bin_gems {
            if gem.qualify(gem_id, now, rate)? {
                promoted = promoted.saturating_add(1);
            }
        }

        cursor = match next.checked_add(1) {
            Some(c) if c <= MAX_BIN_ID => c,
            _ => break,
        };
    }
    Ok(promoted)
}

/// Cycle daily-trigger entry: run the Called scan, discarding the count.
pub fn run_call_daily(ctx: &BlockRuntimeContext) -> Result<()> {
    scan_and_call(ctx)?;
    Ok(())
}

/// Force-call breached Qualified gems and forfeit-burn expired Called gems.
/// Only visits the `callable_gems` index (gems in Qualified/Called state);
/// breach counts are recomputed from the oracle VWAP history each run. Returns
/// the number of gems mutated (called or burned).
pub fn scan_and_call(ctx: &BlockRuntimeContext) -> Result<u32> {
    let oracle = OracleContract::new(ctx.storage.clone());
    let pair_hash = oracle
        .settlement_iso_to_pair
        .read(&QUALIFIER_REFERENCE_ISO)?;
    if pair_hash.is_zero() {
        return Ok(0);
    }
    let pair_id = oracle.pair_hash_to_id.read(&pair_hash)?;
    if pair_id == 0 {
        return Ok(0);
    }

    // Most recent fully-closed UTC day (finalized VWAP).
    let last_closed_day = previous_date_key(timestamp_to_date_key(ctx.block.timestamp));

    // The Oracle begin-block hook finalizes that day earlier in this same block;
    // a lagging watermark means the ordering broke — skip loudly instead of
    // misreading an unfinalized day as empty.
    let finalized = oracle.utc_day_vwap_last_finalized.read()?;
    if finalized < last_closed_day {
        tracing::warn!(target: "outbe::gem", last_closed_day, finalized, "call scan: utc-day VWAP not finalized yet, skipping run");
        return Ok(0);
    }

    // Trailing window of finalized daily VWAPs, newest first. Read once: every
    // candidate shares pair 840, so only the per-gem Call Threshold differs.
    // CALL_WINDOW is stored in seconds; the daily scan needs the day count.
    let window_days = CALL_WINDOW / 86_400;
    let mut window: Vec<(u32, Option<U256>)> = Vec::with_capacity(window_days as usize);
    let mut day = last_closed_day;
    for _ in 0..window_days {
        window.push((day, oracle.get_utc_day_vwap_for_pair_id(day, pair_id)?));
        day = previous_date_key(day);
    }

    // Snapshot the callable-gem ids before mutating: a forfeit burn swap-pops
    // the list, which would shift a live cursor mid-scan.
    let mut gem = GemContract::new(ctx.storage.clone());
    let count = gem.callable_gems.len()?;
    let mut ids: Vec<U256> = Vec::with_capacity(count as usize);
    for i in 0..count {
        if let Some(id) = gem.callable_gems.get(i)? {
            ids.push(id);
        }
    }

    let now = ctx.block.timestamp;
    let mut mutated: u32 = 0;
    for gem_id in ids {
        // Isolate per-gem: a deterministic Err rolls back this gem's checkpoint
        // and is skipped, so one bad gem never halts the daily scan. Structural
        // reads above keep `?` so infra errors still propagate. A gem is either
        // Qualified (call) or Called (forfeit); the inapplicable op is a no-op.
        let res = ctx.storage.with_checkpoint(|| {
            if gem.trigger_call(&window, gem_id, now)? {
                return Ok(true);
            }
            gem.forfeit(gem_id, now)
        });
        if matches!(res, Ok(true)) {
            mutated = mutated.saturating_add(1);
        }
    }
    Ok(mutated)
}
