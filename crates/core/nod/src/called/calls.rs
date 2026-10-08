use std::collections::BTreeSet;

use alloy_primitives::B256;
use outbe_oracle::api::get_all_reference_currencies;
use outbe_oracle::call_window::{CallWindow, CallWindows};
use outbe_primitives::{
    block::BlockRuntimeContext,
    call_bins::{pack_cursor, unpack_cursor},
    call_breach::{BreachTerms, ScanTerms},
    error::{Result, SweepFailure},
    math::{constants::MAX_BIN_ID, tree_math},
    time::first_full_day,
};

use super::{materializing, sweep_failure};
use crate::{
    api, constants::MAX_NOD_CALL_VISITS_PER_BLOCK, precompile::INod, schema::NodContract,
    state::CallBins,
};

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
    visits: &mut u32,
    called_days: &mut BTreeSet<u32>,
) -> Result<(u32, bool)> {
    let currencies = get_all_reference_currencies(ctx)?;
    let start = currency_position(&currencies, nod.call_currency_cursor.read()?);
    let params = crate::config::read_from(nod, ctx.block.chain_id)?;
    let mut called: u32 = 0;
    for &iso_code in currencies.iter().skip(start) {
        if nod.call_bin_tree_root.read(&iso_code)?.is_zero() {
            continue;
        }
        let window = windows.window(&ctx.storage, iso_code, || {
            scan_terms(nod, iso_code, &params)
        })?;
        let Some(ceiling) = window_ceiling(window, iso_code) else {
            continue;
        };
        let scan = CurrencyScan {
            iso_code,
            window,
            ceiling,
        };
        let (calls, finished) = call_currency(ctx, nod, scan, visits, called_days)?;
        called = called.saturating_add(calls);
        if !finished {
            nod.call_currency_cursor.write(u32::from(iso_code))?;
            return Ok((called, false));
        }
    }
    Ok((called, true))
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

/// The bin of the window's ceiling, or `None` when nothing in it can have breached.
fn window_ceiling(window: &CallWindow, iso_code: u16) -> Option<u32> {
    match NodContract::price_to_bin(window.ceiling()?) {
        Ok(bin) => Some(bin),
        Err(error) => {
            tracing::warn!(
                target: "outbe::nod",
                iso_code,
                error = ?error,
                "nod call scan: window price out of range, skipping currency for the day"
            );
            None
        }
    }
}

/// Each bin is walked from the top, so a call's swap-pop only moves an entry already visited.
pub(crate) fn call_currency(
    ctx: &BlockRuntimeContext,
    nod: &mut NodContract<'_>,
    scan: CurrencyScan<'_>,
    visits: &mut u32,
    called_days: &mut BTreeSet<u32>,
) -> Result<(u32, bool)> {
    let CurrencyScan {
        iso_code,
        window,
        ceiling,
    } = scan;
    let now = ctx.block.timestamp;
    let (mut from_bin, mut remaining) = unpack_cursor(nod.call_bin_cursor.read(&iso_code)?);
    let mut called: u32 = 0;
    loop {
        let bin_id = match tree_math::find_first_left_inclusive(&CallBins(nod, iso_code), from_bin)?
        {
            Some(bin) if bin <= ceiling => bin,
            _ => {
                nod.call_bin_cursor.write(&iso_code, 0)?;
                return Ok((called, true));
            }
        };
        let count = nod
            .call_bin_count
            .read(&NodContract::scoped(iso_code, bin_id))?;
        remaining = if bin_id == from_bin && remaining != 0 {
            remaining.min(count)
        } else {
            count
        };
        while remaining > 0 {
            if *visits >= MAX_NOD_CALL_VISITS_PER_BLOCK {
                nod.call_bin_cursor
                    .write(&iso_code, pack_cursor(bin_id, remaining))?;
                return Ok((called, false));
            }
            *visits += 1;
            remaining -= 1;
            let bucket_key = nod
                .call_bin_buckets
                .read(&NodContract::bin_index_key(iso_code, bin_id, remaining))?;
            match try_call(ctx, nod, window, bucket_key, now)? {
                Some(true) => {
                    called = called.saturating_add(1);
                    called_days.insert(nod.bucket_worldwide_day.read(&bucket_key)?.value());
                }
                Some(false) => {}
                None => {
                    nod.call_bin_cursor
                        .write(&iso_code, pack_cursor(bin_id, remaining + 1))?;
                    return Ok((called, false));
                }
            }
        }
        from_bin = match bin_id.checked_add(1) {
            Some(next) if next <= MAX_BIN_ID => next,
            _ => {
                nod.call_bin_cursor.write(&iso_code, 0)?;
                return Ok((called, true));
            }
        };
    }
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
    match ctx
        .storage
        .with_checkpoint(|| mark_called(nod, bucket_key, now, terms.call_notice_period_seconds))
    {
        Ok(()) => Ok(Some(true)),
        Err(error) => match sweep_failure(&error) {
            SweepFailure::Skip => Ok(Some(false)),
            SweepFailure::Stop => Ok(None),
            SweepFailure::Propagate => Err(error),
        },
    }
}

pub(crate) use outbe_primitives::daily_sweep::currency_position;

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
