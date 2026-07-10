use alloy_primitives::U256;
use outbe_common::WorldwideDay;
use outbe_oracle::contract::OracleContract;
use outbe_primitives::{
    block::{BlockLifecycle, BlockRuntimeContext},
    error::Result,
    math::{constants::MAX_BIN_ID, tree_math},
};

use crate::constants::{GEM_CALL_WINDOW_DAYS, QUALIFIER_REFERENCE_ISO};
use crate::schema::GemContract;

pub struct GemLifecycle;

impl BlockLifecycle for GemLifecycle {
    fn begin_block(ctx: &BlockRuntimeContext) -> Result<()> {
        // TODO refactor this. Oracle is called here for each block
        scan_and_qualify(ctx)?;
        Ok(())
    }
}

pub fn scan_and_qualify(ctx: &BlockRuntimeContext) -> Result<u32> {
    let oracle = OracleContract::new(ctx.storage.clone());

    // Resolve the COEN/<reference_currency> pair via the Oracle's ISO registry,
    // matching the lookup used by gemfactory::mint_gem.
    let pair_hash = oracle
        .settlement_iso_to_pair
        .read(&QUALIFIER_REFERENCE_ISO)?;
    if pair_hash.is_zero() {
        return Ok(0);
    }
    let rate = oracle.exchange_rate.read(&pair_hash)?;
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

    // Trailing window of finalized daily VWAPs, newest first. Read once: every
    // candidate shares pair 840, so only the per-gem Call Threshold differs.
    let today = WorldwideDay::from_timestamp(ctx.block.timestamp).previous_date_key();
    let mut window: Vec<(WorldwideDay, Option<U256>)> =
        Vec::with_capacity(GEM_CALL_WINDOW_DAYS as usize);
    let mut day = today;
    for _ in 0..GEM_CALL_WINDOW_DAYS {
        window.push((day, oracle.get_worldwide_day_vwap_for_pair_id(day, pair_id)?));
        day = day.previous_date_key();
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
            if gem.call(&window, gem_id, now)? {
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
