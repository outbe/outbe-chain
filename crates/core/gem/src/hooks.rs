use alloy_primitives::U256;
use outbe_oracle::{
    api::get_all_reference_currencies,
    call_sweep::{self, CallSweep, CALL_SWEEP},
    call_window::{CallWindow, CallWindows},
};
use outbe_primitives::{
    block::BlockRuntimeContext, call_bins, call_breach::ScanTerms, daily_sweep::PinnedDay,
    error::Result, storage::dsl::Value, sweep_budget::SweepBudget,
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

    type Bins<'a>
        = BucketBins<'a, 'storage>
    where
        Self: 'a;

    fn days(&self) -> PinnedDay<'_, 'storage> {
        PinnedDay {
            current: &self.gem.call_sweep_day,
            pending: &self.gem.call_pending_day,
        }
    }

    fn bins(&self, reference_currency: u16) -> BucketBins<'_, 'storage> {
        BucketBins(&self.gem, reference_currency)
    }

    fn currency_cursor(&self) -> &Value<'storage, u32> {
        &self.gem.call_currency_cursor
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
        let params = crate::config::read_from(&self.gem, ctx.block.chain_id)?;
        let mut windows = CallWindows::new(ctx.storage.clone(), pinned_day);
        let mut budget = SweepBudget::per_block();
        let mut called: u32 = 0;
        let gem = &self.gem;
        let finished = call_bins::walk_currencies(
            &currencies,
            &gem.call_currency_cursor,
            &mut budget,
            |iso_code, budget| {
                let skipped = || {
                    GemContract::new(ctx.storage.clone()).emit(CallScanSkipped {
                        referenceCurrency: iso_code,
                        utcDay: pinned_day,
                    })
                };
                let Some((window, ceiling)) = call_sweep::currency_ceiling(
                    &BucketBins(gem, iso_code),
                    &mut windows,
                    || scan_terms(gem, iso_code, &params),
                    skipped,
                )?
                else {
                    return Ok(true);
                };
                let (calls, finished) = call_currency(ctx, iso_code, window, ceiling, budget)?;
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
        |bucket, budget| {
            call_sweep::call_entry::<GemCallSweep>(&ctx.storage, budget, bucket, || {
                let calls =
                    u32::from(gem.trigger_bucket_call(window, bucket, ctx.block.timestamp)?);
                called += calls;
                Ok(calls)
            })
        },
    )?;
    Ok((called, finished))
}
