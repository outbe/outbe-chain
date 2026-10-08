//! Daily call scan: force-calls Nod buckets off the Oracle's finalized
//! per-UTC-day VWAPs. The Cycle daily trigger schedules the closed UTC day, and every
//! CycleTick walks a slice of it.
//!
//! A bucket is called, walking each currency's call-price trie, when the reference
//! price exceeded its call price on at least its `call_threshold_seconds` of the
//! trailing `call_window_seconds`. The call queues it on its deadline, and every
//! CycleTick forfeit-burns the unpaid Nods of the buckets whose
//! `call_notice_period_seconds` lapsed ([`crate::expired::sweep_expired`]).
//!
//! Issuance seals all four terms onto the bucket, and this scan reads them back
//! from it. Retuning a constant therefore leaves every issued bucket on the
//! terms it was issued with. Gem and intex give the same guarantee.
//!
//! The breach rule needs no per-bucket streak state. The daily series is global
//! per currency, so one trailing window per currency decides every bucket
//! denominated in it. Every run recomputes the count from oracle history rather
//! than carrying it. Mirrors `outbe_gem::called::scan_and_call` and
//! `outbe_credisfactory::called::scan_and_call`, which evaluate the same shape.
//!
//! Calls count only days from `first_full_day` of the bucket's sealed `issued_at`.

use std::collections::BTreeSet;

use alloy_primitives::B256;
use outbe_oracle::api::get_all_reference_currencies;
use outbe_oracle::call_sweep::{self, CallSweep, CALL_SWEEP};
use outbe_oracle::call_window::{CallWindow, CallWindows};
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

use crate::{api, precompile::INod, schema::NodContract, state::CallBins};

/// Cycle daily-trigger entry: schedules the day the Oracle has just finalized.
///
/// Never returns `Err` for missing market data. The Cycle dispatcher propagates
/// a handler error out of the `CycleTick` system transaction, which fails the
/// block. An unregistered pair, an unpriced currency or an unfinalized day
/// therefore each degrade to "no transition" instead.
pub fn run_daily(ctx: &BlockRuntimeContext) -> Result<()> {
    call_sweep::schedule(ctx, &mut NodCallSweep::new(ctx))
}

/// Schedules the closed day and walks a slice of the day in flight. Returns the
/// buckets called.
pub fn scan_and_call(ctx: &BlockRuntimeContext) -> Result<u32> {
    let mut sweep = NodCallSweep::new(ctx);
    call_sweep::schedule(ctx, &mut sweep)?;
    call_sweep::continue_day(ctx, &mut sweep)
}

/// Walks the next slice of the day in flight, pinned to the day it opened on so
/// later blocks decide against the same prices. Returns how many buckets were called.
pub fn run_call_slice(ctx: &BlockRuntimeContext) -> Result<u32> {
    call_sweep::continue_day(ctx, &mut NodCallSweep::new(ctx))
}

struct NodCallSweep<'storage> {
    nod: NodContract<'storage>,
}

impl<'storage> NodCallSweep<'storage> {
    fn new(ctx: &BlockRuntimeContext<'storage>) -> Self {
        Self {
            nod: NodContract::new(ctx.storage.clone()),
        }
    }
}

impl<'storage> CallSweep<'storage> for NodCallSweep<'storage> {
    const CONSUMER: &'static str = "nod";

    type Bins<'a>
        = CallBins<'a, 'storage>
    where
        Self: 'a;

    fn days(&self) -> PinnedDay<'_, 'storage> {
        PinnedDay {
            current: &self.nod.call_sweep_day,
            pending: &self.nod.call_pending_day,
        }
    }

    fn bins(&self, reference_currency: u16) -> CallBins<'_, 'storage> {
        CallBins(&self.nod, reference_currency)
    }

    fn currency_cursor(&self) -> &Value<'storage, u32> {
        &self.nod.call_currency_cursor
    }

    fn classify(error: &PrecompileError) -> SweepFailure {
        sweep_failure(error)
    }

    fn day_skipped(&mut self, skipped: u32, in_flight: u32) -> Result<()> {
        self.nod.emit(INod::SweepDaySkipped {
            sweep: CALL_SWEEP,
            skippedDay: skipped,
            inFlightDay: in_flight,
        })
    }

    fn slice(&mut self, ctx: &BlockRuntimeContext, pinned_day: u32) -> Result<(u32, bool)> {
        let mut called_days = BTreeSet::new();
        let (called, finished) = call_slice(ctx, &self.nod, pinned_day, &mut called_days)?;
        self.nod.emit_days_metadata_update(&called_days)?;
        Ok((called, finished))
    }
}

/// Nod's own index checks revert, so a body corruption reaching a sweep is this
/// node's body store.
pub(crate) fn sweep_failure(error: &PrecompileError) -> SweepFailure {
    match error {
        PrecompileError::BodyReadCorruption(_) => SweepFailure::Propagate,
        other => other.sweep_failure(),
    }
}

/// Whether the certified generation of the bucket's Worldwide Day still has Nods to land.
pub(crate) fn materializing(nod: &NodContract<'_>, bucket_key: B256) -> Result<bool> {
    let worldwide_day = nod.bucket_worldwide_day.read(&bucket_key)?;
    Ok(nod.ocomp_target_generation.read(&worldwide_day)? != 0)
}

/// One currency's call walk: its trailing VWAP window, and the highest bin a
/// bucket that window breached can sit in.
pub(crate) struct CurrencyScan<'w> {
    pub(crate) iso_code: u16,
    pub(crate) window: &'w CallWindow,
    pub(crate) ceiling: u32,
}

/// Walks every currency's bins up to its window's ceiling. Returns the buckets
/// called and whether every currency was walked.
fn call_slice(
    ctx: &BlockRuntimeContext,
    nod: &NodContract<'_>,
    pinned_day: u32,
    called_days: &mut BTreeSet<u32>,
) -> Result<(u32, bool)> {
    let currencies = get_all_reference_currencies(ctx)?;
    let params = crate::config::read_from(nod, ctx.block.chain_id)?;
    let mut windows = CallWindows::new(ctx.storage.clone(), pinned_day);
    let mut budget = SweepBudget::per_block();
    let mut caller = NodContract::new(ctx.storage.clone());
    let mut called: u32 = 0;
    let finished = call_bins::walk_currencies(
        &currencies,
        &nod.call_currency_cursor,
        &mut budget,
        |iso_code, budget| {
            let skipped = || {
                NodContract::new(ctx.storage.clone()).emit(INod::CallScanSkipped {
                    referenceCurrency: iso_code,
                    utcDay: pinned_day,
                })
            };
            let Some((window, ceiling)) = call_sweep::currency_ceiling(
                &CallBins(nod, iso_code),
                &mut windows,
                || scan_terms(nod, iso_code, &params),
                skipped,
            )?
            else {
                return Ok(true);
            };
            let scan = CurrencyScan {
                iso_code,
                window,
                ceiling,
            };
            let (calls, finished) = call_currency(ctx, &mut caller, scan, budget, called_days)?;
            called = called.saturating_add(calls);
            Ok(finished)
        },
    )?;
    Ok((called, finished))
}

/// The live profile is the terms the next bucket is sealed with.
fn scan_terms(
    nod: &NodContract<'_>,
    iso_code: u16,
    params: &crate::config::NodParams,
) -> Result<ScanTerms> {
    outbe_primitives::call_breach::scan_terms(
        &nod.max_call_window_seconds,
        &nod.min_call_threshold_seconds,
        iso_code,
        params.call_window_seconds,
        params.call_threshold_seconds,
    )
}

/// Walks the currency's bins up to the window's ceiling, resuming where it stopped.
pub(crate) fn call_currency(
    ctx: &BlockRuntimeContext,
    nod: &mut NodContract<'_>,
    scan: CurrencyScan<'_>,
    budget: &mut SweepBudget,
    called_days: &mut BTreeSet<u32>,
) -> Result<(u32, bool)> {
    let CurrencyScan {
        iso_code,
        window,
        ceiling,
    } = scan;
    let index = NodContract::new(ctx.storage.clone());
    let mut called: u32 = 0;
    let finished = call_bins::walk(
        &CallBins(&index, iso_code),
        ceiling,
        budget,
        |bucket_key, budget| {
            call_sweep::call_entry::<NodCallSweep>(&ctx.storage, budget, bucket_key, || {
                if !call_bucket(nod, window, bucket_key, ctx.block.timestamp)? {
                    return Ok(0);
                }
                called += 1;
                called_days.insert(nod.bucket_worldwide_day.read(&bucket_key)?.value());
                Ok(1)
            })
        },
    )?;
    Ok((called, finished))
}

/// Calls the bucket if the window breached its sealed terms. Returns whether it did.
fn call_bucket(
    nod: &mut NodContract<'_>,
    window: &CallWindow,
    bucket_key: B256,
    now: u64,
) -> Result<bool> {
    if nod.bucket_nod_count.read(&bucket_key)? == 0 || nod.bucket_called_at.read(&bucket_key)? != 0
    {
        return Ok(false);
    }
    let issued_at = nod.callable_bucket_issued_at.read(&bucket_key)?;
    let terms = nod.read_call_terms(bucket_key)?;
    let breached = window.breached(&BreachTerms {
        call_price: terms.call_price_minor,
        window_seconds: terms.call_window_seconds,
        threshold_seconds: terms.call_threshold_seconds,
        start_day: first_full_day(issued_at),
    });
    if !breached || materializing(nod, bucket_key)? {
        return Ok(false);
    }
    mark_called(nod, bucket_key, now, terms.call_notice_period_seconds)?;
    Ok(true)
}

/// Stamps the call and opens the settlement window the bucket sealed.
fn mark_called(
    nod: &mut NodContract<'_>,
    bucket_key: B256,
    now: u64,
    notice_period: u32,
) -> Result<()> {
    let deadline = api::settlement_deadline_of(now, notice_period);
    nod.remove_call_bin(bucket_key)?;
    nod.push_called_bucket(bucket_key, deadline)?;
    nod.bucket_called_at.write(&bucket_key, now)?;
    nod.emit(INod::NodBucketCalled {
        bucketKey: bucket_key,
        calledAt: now,
        settlementDeadline: deadline,
    })
}
