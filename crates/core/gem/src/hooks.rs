use alloy_primitives::{B256, U256};
use outbe_oracle::{
    api::get_all_reference_currencies,
    call_window::{CallWindow, CallWindows},
};
use outbe_primitives::{
    block::{BlockLifecycle, BlockRuntimeContext},
    call_breach::ScanTerms,
    daily_sweep::{PinnedDay, Scheduled, SweepDays},
    error::Result,
    math::tree_math,
};

use outbe_primitives::call_bins::{self, unpack_cursor, Visit};
use outbe_primitives::sweep_budget::SweepBudget;

use crate::config::GemParams;
use crate::constants::{CALL_SWEEP, MAX_BUCKET_VISITS_PER_BLOCK};
use crate::precompile::IGem::{BatchMetadataUpdate, CallScanSkipped, SweepDaySkipped};
use crate::schema::GemContract;
use crate::state::BucketBins;

mod expiry;

pub struct GemLifecycle;

impl BlockLifecycle for GemLifecycle {
    type Context<'a, 'storage> = BlockRuntimeContext<'storage>;
    type EndBlockResult = ();

    fn begin_block(ctx: &BlockRuntimeContext) -> Result<()> {
        // A call sweep that the daily trigger could not finish in one pass continues
        // here. It continues block by block and does not wait a day for the next trigger.
        run_call_slice(ctx)?;
        expiry::sweep_expired(ctx)?;
        Ok(())
    }

    fn end_block(_ctx: &BlockRuntimeContext) -> Result<Self::EndBlockResult> {
        Ok(())
    }
}

/// The most recent fully-closed UTC day, or `None` while its VWAPs are not final.
fn closed_day(ctx: &BlockRuntimeContext) -> Result<Option<u32>> {
    outbe_oracle::closed_day::finalized_closed_day(ctx.storage.clone(), ctx.block.timestamp, "gem")
}

#[cfg(test)]
pub(crate) use outbe_primitives::daily_sweep::currency_position;

fn pinned_call_day<'a, 'storage>(gem: &'a GemContract<'storage>) -> PinnedDay<'a, 'storage> {
    PinnedDay {
        current: &gem.call_sweep_day,
        pending: &gem.call_pending_day,
    }
}

/// Cycle daily-trigger entry: open the day's Called sweep, discarding the count.
pub fn run_daily(ctx: &BlockRuntimeContext) -> Result<()> {
    scan_and_call(ctx)?;
    Ok(())
}

/// Cycle daily-trigger entry: schedule the day that the Oracle has just finalized.
/// This opens a Called sweep over the day and runs its first slice, or queues the
/// day behind the sweep still in flight.
pub fn scan_and_call(ctx: &BlockRuntimeContext) -> Result<u32> {
    let Some(last_closed_day) = closed_day(ctx)? else {
        return Ok(0);
    };

    let mut gem = GemContract::new(ctx.storage.clone());
    let scheduled = pinned_call_day(&gem).schedule(last_closed_day)?;
    match scheduled {
        (next, Scheduled::Opened) => {
            start_call_sweep(ctx, &gem, next)?;
            run_call_slice(ctx)
        }
        (next, Scheduled::Replaced { skipped }) => {
            gem.emit(SweepDaySkipped {
                sweep: CALL_SWEEP,
                skippedDay: skipped,
                inFlightDay: next.current,
            })?;
            Ok(0)
        }
        (_, Scheduled::Queued | Scheduled::Ignored) => Ok(0),
    }
}

/// Pin the sweep's current day and walk it from the first currency's lowest bin.
fn start_call_sweep(ctx: &BlockRuntimeContext, gem: &GemContract, days: SweepDays) -> Result<()> {
    pinned_call_day(gem).open(days)?;
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
    let gem = GemContract::new(ctx.storage.clone());
    let pinned_day = gem.call_sweep_day.read()?;
    if pinned_day == 0 {
        return Ok(0);
    }
    let currencies = get_all_reference_currencies(ctx)?;
    let params = crate::config::read_from(&gem, ctx.block.chain_id)?;
    let cursor = GemContract::new(ctx.storage.clone());
    let mut sweep = CallSweep {
        ctx,
        gem,
        pinned_day,
        params,
        windows: CallWindows::new(pinned_day),
    };
    let mut budget = SweepBudget::new(MAX_BUCKET_VISITS_PER_BLOCK, u32::MAX, 0);
    let mut called: u32 = 0;
    let finished = call_bins::walk_currencies(
        &currencies,
        &cursor.call_currency_cursor,
        &mut budget,
        |iso_code, budget| {
            let (calls, finished) = sweep.scan_currency(iso_code, budget)?;
            called = called.saturating_add(calls);
            Ok(finished)
        },
    )?;
    if finished {
        sweep.finish()?;
    }
    Ok(called)
}

/// Prices and progress pinned to one day's call sweep.
struct CallSweep<'a, 'storage> {
    ctx: &'a BlockRuntimeContext<'storage>,
    gem: GemContract<'storage>,
    pinned_day: u32,
    params: GemParams,
    windows: CallWindows,
}

impl CallSweep<'_, '_> {
    fn scan_currency(&mut self, iso_code: u16, budget: &mut SweepBudget) -> Result<(u32, bool)> {
        // A currency this day's pass could not price is settled for the day.
        if self.gem.call_scan_failed_day.read(&iso_code)? == self.pinned_day {
            return Ok((0, true));
        }
        // Peek the trie before pricing the currency: a drained one costs three
        // reads here instead of a whole VWAP window.
        let (cursor_bin, _) = unpack_cursor(self.gem.bucket_scan_cursor.read(&iso_code)?);
        if tree_math::find_first_left_inclusive(&BucketBins(&self.gem, iso_code), cursor_bin)?
            .is_none()
        {
            self.gem.bucket_scan_cursor.write(&iso_code, 0)?;
            return Ok((0, true));
        }
        let gem = &self.gem;
        let params = &self.params;
        let window = self.windows.window(&self.ctx.storage, iso_code, || {
            scan_terms(gem, iso_code, params)
        })?;
        let Some(high) = window.ceiling() else {
            return Ok((0, true));
        };
        let ceiling = match GemContract::price_to_bin(high) {
            Ok(bin) => bin,
            Err(error) => {
                tracing::warn!(target: "outbe::gem", iso_code, error = ?error, "call scan: window price out of range, skipping currency for the day");
                self.gem
                    .call_scan_failed_day
                    .write(&iso_code, self.pinned_day)?;
                self.gem.emit(CallScanSkipped {
                    referenceCurrency: iso_code,
                    utcDay: self.pinned_day,
                })?;
                return Ok((0, true));
            }
        };
        call_currency(self.ctx, iso_code, window, ceiling, budget)
    }

    fn finish(&self) -> Result<()> {
        // The next day starts on the next block, so no slice mixes two days' prices.
        let next = pinned_call_day(&self.gem).finish(self.pinned_day)?;
        match next {
            Some(next) => start_call_sweep(self.ctx, &self.gem, next),
            None => Ok(()),
        }
    }
}

/// The live profile is the terms the next gem is sealed with.
fn scan_terms(gem: &GemContract<'_>, iso_code: u16, params: &GemParams) -> Result<ScanTerms> {
    outbe_primitives::call_breach::scan_terms(
        &gem.max_call_window_seconds,
        &gem.min_call_threshold_seconds,
        iso_code,
        params.call_window_seconds,
        params.call_threshold_seconds,
    )
}

/// Walk one currency's bucket bins up to `ceiling`, resuming where it stopped.
/// Returns the calls made and whether the eligible range was walked to the end.
pub(crate) fn call_currency(
    ctx: &BlockRuntimeContext,
    iso_code: u16,
    window: &CallWindow,
    ceiling: u32,
    budget: &mut SweepBudget,
) -> Result<(u32, bool)> {
    let index = GemContract::new(ctx.storage.clone());
    let mut gem = GemContract::new(ctx.storage.clone());
    let mut called: u32 = 0;
    let finished = call_bins::walk(
        &BucketBins(&index, iso_code),
        ceiling,
        budget,
        |bucket, _| {
            if call_bucket(ctx, &mut gem, window, bucket)? {
                called = called.saturating_add(1);
            }
            Ok(Visit::Next)
        },
    )?;
    Ok((called, finished))
}

/// Protocol errors roll back only this bucket. Node-local failures fail the block.
fn call_bucket(
    ctx: &BlockRuntimeContext,
    gem: &mut GemContract<'_>,
    window: &CallWindow,
    bucket: B256,
) -> Result<bool> {
    match ctx
        .storage
        .with_checkpoint(|| gem.trigger_bucket_call(window, bucket, ctx.block.timestamp))
    {
        Ok(called) => Ok(called),
        Err(error) if error.is_node_local() => Err(error),
        Err(error) => {
            tracing::warn!(target: "outbe::gem", %bucket, error = ?error, "call scan: skipping bucket");
            Ok(false)
        }
    }
}
