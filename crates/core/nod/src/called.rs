//! Daily call scan: force-calls Nod buckets off the Oracle's finalized
//! per-UTC-day VWAPs, then forfeit-burns the Nods of a bucket whose notice
//! period lapsed. The Cycle daily trigger pins the closed UTC day and runs the
//! first slice. Later CycleTicks continue the same day.
//!
//! One pass applies at most one transition per bucket, in lifecycle order, over
//! two arms sharing one visit budget:
//!
//! - *not called* -> *called*, walking each currency's call-price trie, when
//!   the reference price exceeded the bucket's call price on at least its
//!   `call_threshold_seconds` of the trailing `call_window_seconds`.
//! - *called* -> *forfeited*, walking the called-bucket list, when the bucket's
//!   `call_notice_period_seconds` has lapsed with Nods still unpaid. The two can never
//!   fire in one pass, since a bucket called now cannot also be a notice period
//!   past its call.
//!
//! Issuance seals all four terms onto the bucket, and this scan reads them back
//! from it. Retuning a constant therefore leaves every issued bucket on the
//! terms it was issued with. Gem and intex give the same guarantee.
//!
//! The breach rule needs no per-bucket streak state. The daily series is global
//! per currency, so one trailing window per currency decides every bucket
//! denominated in it. Every run recomputes the count from oracle history rather
//! than carrying it. Mirrors `outbe_gem::hooks::scan_and_call` and
//! `outbe_credisfactory::called::scan_and_call`, which evaluate the same shape.
//!
//! Calls count only days from `first_full_day` of the bucket's sealed `issued_at`.

mod calls;
mod forfeits;
mod window;

use std::collections::BTreeSet;

use alloy_primitives::{B256, U256};
use outbe_compressed_entities::{ExecutionScope, ParentBodySource};
use outbe_oracle::{api::get_all_reference_currencies, schema::OracleContract};
use outbe_primitives::{
    block::BlockRuntimeContext,
    daily_sweep::{Scheduled, SweepDays},
    error::{PrecompileError, Result, SweepFailure},
};

use crate::{constants::CALL_SWEEP, precompile::INod, schema::NodContract};

#[cfg(test)]
pub(crate) use calls::{call_currency, CurrencyScan};
#[cfg(test)]
pub(crate) use forfeits::{forfeit_members, Bodies};

pub(crate) const CALL_ARM_DONE: u32 = u32::MAX;

/// Trailing finalized daily VWAPs of one `COEN/<iso>` pair, newest first.
/// `None` marks a day the pair published no reference price.
type VwapWindow = Vec<(u32, Option<U256>)>;

/// Schedule the day the Oracle has just finalized. Open a Called sweep over it
/// and run its first slice, or queue it behind the sweep still in flight.
///
/// Never returns `Err` for missing market data. The Cycle dispatcher propagates
/// a handler error out of the `CycleTick` system transaction, which fails the
/// block. An unregistered pair, an unpriced currency or an unfinalized day
/// therefore each degrade to "no transition" instead.
pub fn scan_and_call(
    ctx: &BlockRuntimeContext,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
) -> Result<u32> {
    let Some(last_closed_day) = closed_day(ctx)? else {
        return Ok(0);
    };
    let mut nod = NodContract::new(ctx.storage.clone());
    if nod.call_sweep_day.read()? == 0 && !has_call_work(ctx, &nod)? {
        return Ok(0);
    }
    let days = SweepDays {
        current: nod.call_sweep_day.read()?,
        pending: nod.call_pending_day.read()?,
    };
    match days.schedule(last_closed_day) {
        (next, Scheduled::Opened) => {
            start_call_sweep(ctx, &nod, next)?;
            run_call_slice(ctx, scope, parent)
        }
        (next, Scheduled::Queued) => {
            nod.call_pending_day.write(next.pending)?;
            Ok(0)
        }
        (next, Scheduled::Replaced { skipped }) => {
            nod.call_pending_day.write(next.pending)?;
            nod.emit(INod::SweepDaySkipped {
                sweep: CALL_SWEEP,
                skippedDay: skipped,
                inFlightDay: next.current,
            })?;
            Ok(0)
        }
        (_, Scheduled::Ignored) => Ok(0),
    }
}

fn has_call_work(ctx: &BlockRuntimeContext, nod: &NodContract) -> Result<bool> {
    if nod.called_buckets.len()? != 0 {
        return Ok(true);
    }
    for iso_code in get_all_reference_currencies(ctx)? {
        if !nod.call_bin_tree_root.read(&iso_code)?.is_zero() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The most recent fully-closed UTC day, or `None` while its VWAPs are not final.
pub(crate) fn closed_day(ctx: &BlockRuntimeContext) -> Result<Option<u32>> {
    outbe_oracle::closed_day::finalized_closed_day(ctx.storage.clone(), ctx.block.timestamp, "nod")
}

/// Pin the sweep's current day and walk it from the first currency's lowest bin.
fn start_call_sweep(ctx: &BlockRuntimeContext, nod: &NodContract, days: SweepDays) -> Result<()> {
    nod.call_sweep_day.write(days.current)?;
    nod.call_pending_day.write(days.pending)?;
    nod.call_currency_cursor.write(0)?;
    nod.forfeit_cursor.write(0)?;
    for iso_code in get_all_reference_currencies(ctx)? {
        nod.call_bin_cursor.write(&iso_code, 0)?;
    }
    Ok(())
}

fn finish_call_sweep(ctx: &BlockRuntimeContext, nod: &NodContract, pinned_day: u32) -> Result<()> {
    let next = SweepDays {
        current: pinned_day,
        pending: nod.call_pending_day.read()?,
    }
    .finish();
    if next.current == 0 {
        nod.call_sweep_day.write(0)
    } else {
        start_call_sweep(ctx, nod, next)
    }
}

/// Advance an open sweep by one slice, pinned to the day it opened on so
/// later blocks decide against the same prices. Returns how many buckets
/// were called plus Nods forfeited.
pub fn run_call_slice(
    ctx: &BlockRuntimeContext,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
) -> Result<u32> {
    let mut nod = NodContract::new(ctx.storage.clone());
    let pinned_day = nod.call_sweep_day.read()?;
    if pinned_day == 0 {
        return Ok(0);
    }
    let oracle = OracleContract::new(ctx.storage.clone());
    let finalized = oracle.utc_day_vwap_last_finalized.read()?;
    if finalized < pinned_day {
        tracing::warn!(
            target: "outbe::nod",
            pinned_day,
            finalized,
            "nod call scan: pinned utc-day VWAP not finalized, holding the sweep"
        );
        return Ok(0);
    }

    let mut visits: u32 = 0;
    let mut mutated: u32 = 0;
    if nod.call_currency_cursor.read()? != CALL_ARM_DONE {
        let mut called_days = BTreeSet::new();
        let mut windows = window::VwapWindows::new(&oracle, pinned_day);
        let (called, finished) =
            calls::call_arm(ctx, &mut nod, &mut windows, &mut visits, &mut called_days)?;
        nod.emit_days_metadata_update(&called_days)?;
        mutated = mutated.saturating_add(called);
        if !finished {
            return Ok(mutated);
        }
        nod.call_currency_cursor.write(CALL_ARM_DONE)?;
    }
    let (forfeited, finished) = forfeits::forfeit_arm(ctx, scope, parent, &mut nod, &mut visits)?;
    mutated = mutated.saturating_add(forfeited);
    if finished {
        finish_call_sweep(ctx, &nod, pinned_day)?;
    }
    Ok(mutated)
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
fn materializing(nod: &NodContract<'_>, bucket_key: B256) -> Result<bool> {
    let worldwide_day = nod.bucket_worldwide_day.read(&bucket_key)?;
    Ok(nod.ocomp_target_generation.read(&worldwide_day)? != 0)
}
