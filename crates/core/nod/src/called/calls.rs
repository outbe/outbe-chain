use std::collections::BTreeSet;

use alloy_primitives::B256;
use outbe_oracle::api::get_all_reference_currencies;
use outbe_oracle::call_sweep;
use outbe_oracle::call_window::{CallWindow, CallWindows};
use outbe_primitives::{
    block::BlockRuntimeContext,
    call_bins,
    call_breach::{BreachTerms, ScanTerms},
    error::Result,
    sweep_budget::SweepBudget,
    time::first_full_day,
};

use super::{materializing, NodCallSweep};
use crate::{api, precompile::INod, schema::NodContract, state::CallBins};

/// One currency's call walk: its trailing VWAP window, and the highest bin a
/// bucket that window breached can sit in.
pub(crate) struct CurrencyScan<'w> {
    pub(crate) iso_code: u16,
    pub(crate) window: &'w CallWindow,
    pub(crate) ceiling: u32,
}

/// Walks every currency's bins up to its window's ceiling. Returns the buckets
/// called and whether every currency was walked.
pub(super) fn call_slice(
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
