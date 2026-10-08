use alloy_primitives::{B256, U256};
use outbe_oracle::{
    api::get_all_reference_currencies,
    call_sweep::{self, CallSweep, Decided, CALL_SWEEP},
    call_window::{CallWindow, CallWindows},
};
use outbe_primitives::{
    block::BlockRuntimeContext,
    call_bins::{self, Visit},
    call_breach::ScanTerms,
    daily_sweep::PinnedDay,
    error::{PrecompileError, Result},
    sweep_budget::SweepBudget,
};

use crate::config::GemParams;
use crate::precompile::IGem::{BatchMetadataUpdate, CallScanSkipped, SweepDaySkipped};
use crate::schema::GemContract;
use crate::state::BucketBins;

mod expiry;

/// Burns the called buckets whose notice period lapsed.
pub fn sweep_forfeits(ctx: &BlockRuntimeContext) -> Result<()> {
    expiry::sweep_expired(ctx)?;
    Ok(())
}

/// One block of every Gem sweep: what fell due, then a slice of the call sweep.
pub fn continue_sweeps(ctx: &BlockRuntimeContext) -> Result<()> {
    sweep_forfeits(ctx)?;
    run_call_slice(ctx)?;
    Ok(())
}

#[cfg(test)]
pub(crate) use outbe_primitives::daily_sweep::currency_position;

/// Cycle daily-trigger entry: schedules the day the Oracle has just finalized.
pub fn run_daily(ctx: &BlockRuntimeContext) -> Result<()> {
    call_sweep::schedule(ctx, &mut GemCallSweep::new(ctx))
}

/// Schedules the closed day and walks a slice of the day in flight. Returns the
/// buckets called.
pub fn scan_and_call(ctx: &BlockRuntimeContext) -> Result<u32> {
    let mut sweep = GemCallSweep::new(ctx);
    call_sweep::schedule(ctx, &mut sweep)?;
    call_sweep::continue_day(ctx, &mut sweep)
}

/// Walks the next slice of the day in flight. Returns how many buckets were called.
pub fn run_call_slice(ctx: &BlockRuntimeContext) -> Result<u32> {
    call_sweep::continue_day(ctx, &mut GemCallSweep::new(ctx))
}

struct GemCallSweep<'storage> {
    gem: GemContract<'storage>,
}

impl<'storage> GemCallSweep<'storage> {
    fn new(ctx: &BlockRuntimeContext<'storage>) -> Self {
        Self {
            gem: GemContract::new(ctx.storage.clone()),
        }
    }
}

impl<'storage> CallSweep<'storage> for GemCallSweep<'storage> {
    const CONSUMER: &'static str = "gem";

    fn days(&self) -> PinnedDay<'_, 'storage> {
        PinnedDay {
            current: &self.gem.call_sweep_day,
            pending: &self.gem.call_pending_day,
        }
    }

    fn has_work(&self, ctx: &BlockRuntimeContext) -> Result<bool> {
        for iso_code in get_all_reference_currencies(ctx)? {
            if !self.gem.bucket_bin_tree_root.read(&iso_code)?.is_zero() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn reset_cursors(&self, ctx: &BlockRuntimeContext) -> Result<()> {
        self.gem.call_currency_cursor.write(0)?;
        for iso_code in get_all_reference_currencies(ctx)? {
            self.gem.bucket_scan_cursor.write(&iso_code, 0)?;
        }
        Ok(())
    }

    fn day_skipped(&mut self, skipped: u32, in_flight: u32) -> Result<()> {
        self.gem.emit(SweepDaySkipped {
            sweep: CALL_SWEEP,
            skippedDay: skipped,
            inFlightDay: in_flight,
        })
    }

    fn slice(&mut self, ctx: &BlockRuntimeContext, pinned_day: u32) -> Result<(u32, bool)> {
        let currencies = get_all_reference_currencies(ctx)?;
        let mut slice = CallSlice {
            ctx,
            gem: GemContract::new(ctx.storage.clone()),
            pinned_day,
            params: crate::config::read_from(&self.gem, ctx.block.chain_id)?,
            windows: CallWindows::new(pinned_day),
        };
        let mut budget = SweepBudget::per_block();
        let mut called: u32 = 0;
        let finished = call_bins::walk_currencies(
            &currencies,
            &self.gem.call_currency_cursor,
            &mut budget,
            |iso_code, budget| {
                let (calls, finished) = slice.scan_currency(iso_code, budget)?;
                called = called.saturating_add(calls);
                Ok(finished)
            },
        )?;
        // Gem ids carry no order, so a call can only refresh the whole range.
        if called != 0 {
            self.gem.emit(BatchMetadataUpdate {
                _fromTokenId: U256::ZERO,
                _toTokenId: U256::MAX,
            })?;
        }
        Ok((called, finished))
    }
}

/// Prices pinned to one day's call sweep.
struct CallSlice<'a, 'storage> {
    ctx: &'a BlockRuntimeContext<'storage>,
    gem: GemContract<'storage>,
    pinned_day: u32,
    params: GemParams,
    windows: CallWindows,
}

impl CallSlice<'_, '_> {
    fn scan_currency(&mut self, iso_code: u16, budget: &mut SweepBudget) -> Result<(u32, bool)> {
        // A currency this day's pass could not price is settled for the day.
        if self.gem.call_scan_failed_day.read(&iso_code)? == self.pinned_day {
            return Ok((0, true));
        }
        if !call_bins::pending(&BucketBins(&self.gem, iso_code))? {
            return Ok((0, true));
        }
        let (gem, params, ctx, pinned_day) = (&self.gem, &self.params, self.ctx, self.pinned_day);
        let window = self
            .windows
            .window(&ctx.storage, iso_code, || scan_terms(gem, iso_code, params))?;
        let skipped = || {
            GemContract::new(ctx.storage.clone()).emit(CallScanSkipped {
                referenceCurrency: iso_code,
                utcDay: pinned_day,
            })
        };
        let Some(ceiling) = call_sweep::ceiling_bin(
            window,
            &gem.call_scan_failed_day,
            iso_code,
            pinned_day,
            skipped,
        )?
        else {
            return Ok((0, true));
        };
        call_currency(ctx, iso_code, window, ceiling, budget)
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
        |bucket, budget| match call_bucket(ctx, &mut gem, window, bucket)? {
            Some(true) => {
                budget.write();
                called = called.saturating_add(1);
                Ok(Visit::Next)
            }
            Some(false) => Ok(Visit::Next),
            None => Ok(Visit::Stop),
        },
    )?;
    Ok((called, finished))
}

/// Whether the bucket was called, or `None` when the gas ran out before it. A
/// deterministic failure rolls back only this bucket. A node-local one fails the block.
fn call_bucket(
    ctx: &BlockRuntimeContext,
    gem: &mut GemContract<'_>,
    window: &CallWindow,
    bucket: B256,
) -> Result<Option<bool>> {
    let outcome = ctx
        .storage
        .with_checkpoint(|| gem.trigger_bucket_call(window, bucket, ctx.block.timestamp));
    match call_sweep::decide(outcome, PrecompileError::sweep_failure)? {
        Decided::Done(called) => Ok(Some(called)),
        Decided::Stopped => Ok(None),
        Decided::Skipped(error) => {
            tracing::warn!(target: "outbe::gem", %bucket, error = ?error, "call scan: skipping bucket");
            Ok(Some(false))
        }
    }
}
