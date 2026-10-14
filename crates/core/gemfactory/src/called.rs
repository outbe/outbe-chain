//! Daily call sweep: calls the Gem buckets whose reference price breached their call
//! price on enough days of their window. The daily trigger schedules the closed day,
//! and every CycleTick walks a slice of it.

use alloy_primitives::{B256, U256};
use outbe_gem::precompile::IGem::{BatchMetadataUpdate, GemBucketCalled};

use crate::precompile::IGemFactory::{CallScanSkipped, SweepDaySkipped};
use crate::runtime::emit_event;
use outbe_gem::{CallBins, CallTerms, GemContract, GemParams};
use outbe_oracle::{
    call_sweep::{self, CallSweep, CALL_SWEEP},
    call_window::CallWindow,
};
use outbe_primitives::{
    block::BlockRuntimeContext,
    call_bins,
    call_breach::{BreachTerms, ScanTerms},
    daily_sweep::PinnedDay,
    error::{PrecompileError, Result, SweepFailure},
    storage::dsl::Value,
    sweep_budget::SweepBudget,
    time::first_full_day,
};

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

pub(crate) struct GemCallSweep<'storage> {
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
        = CallBins<'a, 'storage>
    where
        Self: 'a;

    fn days(&self) -> PinnedDay<'_, 'storage> {
        PinnedDay {
            current: &self.gem.call_sweep_day,
            pending: &self.gem.call_pending_day,
        }
    }

    fn bins(&self, reference_currency: u16) -> CallBins<'_, 'storage> {
        CallBins(&self.gem, reference_currency)
    }

    fn currency_cursor(&self) -> &Value<'storage, u32> {
        &self.gem.call_currency_cursor
    }

    fn classify(error: &PrecompileError) -> SweepFailure {
        error.body_sweep_failure()
    }

    fn day_skipped(&mut self, skipped: u32, in_flight: u32) -> Result<()> {
        emit_event(
            &self.gem.storage,
            SweepDaySkipped {
                sweep: CALL_SWEEP,
                skippedDay: skipped,
                inFlightDay: in_flight,
            },
        )
    }

    fn slice(&mut self, ctx: &BlockRuntimeContext, pinned_day: u32) -> Result<(u32, bool)> {
        let params = outbe_gem::config::read(&ctx.storage)?;
        let mut called: u32 = 0;
        let gem = &self.gem;
        let finished = call_sweep::walk_day(
            ctx,
            pinned_day,
            &gem.call_currency_cursor,
            |iso_code| CallBins(gem, iso_code),
            |iso_code| scan_terms(gem, iso_code, &params),
            |iso_code| {
                emit_event(
                    &ctx.storage,
                    CallScanSkipped {
                        referenceCurrency: iso_code,
                        utcDay: pinned_day,
                    },
                )
            },
            |iso_code, window, ceiling, budget| {
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
        &CallBins(&index, iso_code),
        ceiling,
        budget,
        |bucket, budget| {
            call_sweep::call_entry::<GemCallSweep>(&ctx.storage, budget, bucket, || {
                let calls = u32::from(trigger_bucket_call(
                    &mut gem,
                    window,
                    bucket,
                    ctx.block.timestamp,
                )?);
                called += calls;
                Ok(calls)
            })
        },
    )?;
    Ok((called, finished))
}

/// Calls an uncalled bucket whose reference-currency daily VWAP exceeded its call price
/// on at least its threshold days of its trailing window. Returns true if called.
///
/// The terms were sealed when the bucket opened, so a later profile change cannot
/// re-term it. Days before the first full day of the first member never count: the
/// bucket did not exist yet.
pub(crate) fn trigger_bucket_call(
    gem: &mut GemContract<'_>,
    window: &CallWindow,
    bucket: B256,
    now: u64,
) -> Result<bool> {
    if gem.bucket_gem_count.read(&bucket)? == 0 || gem.bucket_called_at.read(&bucket)? != 0 {
        return Ok(false);
    }
    let terms = gem.read_call_terms(bucket)?;
    let issued_at = gem.bucket_issued_at.read(&bucket)?;
    let breached = window.breached(&BreachTerms {
        call_price: terms.call_price_minor,
        window_seconds: terms.call_window_seconds,
        threshold_seconds: terms.call_threshold_seconds,
        start_day: first_full_day(issued_at),
    });
    if !breached {
        return Ok(false);
    }
    call_bucket(gem, bucket, &terms, now)?;
    Ok(true)
}

/// `Issued -> Called` for every unpaid member at once: the bucket leaves the trie for the
/// expiry queue, where it waits on its sealed notice period as one entry.
pub(crate) fn call_bucket(
    gem: &mut GemContract<'_>,
    bucket: B256,
    terms: &CallTerms,
    now: u64,
) -> Result<()> {
    let deadline = now.saturating_add(u64::from(terms.call_notice_period_seconds));
    gem.record_bucket_called(bucket, now, deadline)?;
    gem.emit(GemBucketCalled {
        bucketKey: bucket,
        calledAt: now,
        settlementDeadline: deadline,
    })
}
