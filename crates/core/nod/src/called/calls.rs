use std::collections::BTreeSet;

use alloy_primitives::B256;
use outbe_oracle::api::get_all_reference_currencies;
use outbe_oracle::call_sweep::{self, Decided};
use outbe_oracle::call_window::{CallWindow, CallWindows};
use outbe_primitives::{
    block::BlockRuntimeContext,
    call_bins::{self, Visit},
    call_breach::{BreachTerms, ScanTerms},
    error::Result,
    sweep_budget::SweepBudget,
    time::first_full_day,
};

use super::{materializing, sweep_failure};
use crate::{api, precompile::INod, schema::NodContract, state::CallBins};

/// One currency's call walk: its trailing VWAP window, and the highest bin a
/// bucket that window breached can sit in.
pub(crate) struct CurrencyScan<'w> {
    pub(crate) iso_code: u16,
    pub(crate) window: &'w CallWindow,
    pub(crate) ceiling: u32,
}

/// Returns the buckets called and whether every currency was walked.
pub(super) fn call_arm(
    ctx: &BlockRuntimeContext,
    nod: &mut NodContract<'_>,
    windows: &mut CallWindows,
    budget: &mut SweepBudget,
    called_days: &mut BTreeSet<u32>,
) -> Result<(u32, bool)> {
    let pinned_day = windows.last_day();
    let currencies = get_all_reference_currencies(ctx)?;
    let params = crate::config::read_from(nod, ctx.block.chain_id)?;
    let cursor = NodContract::new(ctx.storage.clone());
    let mut called: u32 = 0;
    let finished = call_bins::walk_currencies(
        &currencies,
        &cursor.call_currency_cursor,
        budget,
        |iso_code, budget| {
            if nod.call_scan_failed_day.read(&iso_code)? == pinned_day
                || !call_bins::pending(&CallBins(nod, iso_code))?
            {
                return Ok(true);
            }
            let window = windows.window(&ctx.storage, iso_code, || {
                scan_terms(nod, iso_code, &params)
            })?;
            let skipped = || {
                NodContract::new(ctx.storage.clone()).emit(INod::CallScanSkipped {
                    referenceCurrency: iso_code,
                    utcDay: pinned_day,
                })
            };
            let Some(ceiling) = call_sweep::ceiling_bin(
                window,
                &nod.call_scan_failed_day,
                iso_code,
                pinned_day,
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
            let (calls, finished) = call_currency(ctx, nod, scan, budget, called_days)?;
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
    let now = ctx.block.timestamp;
    let index = NodContract::new(ctx.storage.clone());
    let mut called: u32 = 0;
    let finished = call_bins::walk(
        &CallBins(&index, iso_code),
        ceiling,
        budget,
        |bucket_key, budget| match try_call(ctx, nod, window, bucket_key, now)? {
            Some(true) => {
                budget.write();
                called = called.saturating_add(1);
                called_days.insert(nod.bucket_worldwide_day.read(&bucket_key)?.value());
                Ok(Visit::Next)
            }
            Some(false) => Ok(Visit::Next),
            None => Ok(Visit::Stop),
        },
    )?;
    Ok((called, finished))
}

/// Whether the bucket was called, or `None` when the gas ran out before it.
fn try_call(
    ctx: &BlockRuntimeContext,
    nod: &mut NodContract<'_>,
    window: &CallWindow,
    bucket_key: B256,
    now: u64,
) -> Result<Option<bool>> {
    if nod.bucket_nod_count.read(&bucket_key)? == 0 || nod.bucket_called_at.read(&bucket_key)? != 0
    {
        return Ok(Some(false));
    }
    let issued_at = nod.callable_bucket_issued_at.read(&bucket_key)?;
    let terms = nod.read_call_terms(bucket_key)?;
    let breached = window.breached(&BreachTerms {
        call_price: terms.call_price_minor,
        window_seconds: terms.call_window_seconds,
        threshold_seconds: terms.call_threshold_seconds,
        start_day: first_full_day(issued_at),
    });
    if !breached {
        return Ok(Some(false));
    }
    if materializing(nod, bucket_key)? {
        return Ok(Some(false));
    }
    let outcome = ctx
        .storage
        .with_checkpoint(|| mark_called(nod, bucket_key, now, terms.call_notice_period_seconds));
    match call_sweep::decide(outcome, sweep_failure)? {
        Decided::Done(()) => Ok(Some(true)),
        Decided::Stopped => Ok(None),
        Decided::Skipped(error) => {
            tracing::warn!(target: "outbe::nod", %bucket_key, error = ?error, "call scan: skipping bucket");
            Ok(Some(false))
        }
    }
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
