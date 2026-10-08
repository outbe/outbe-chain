//! Daily call scan: force-calls Nod buckets off the Oracle's finalized
//! per-UTC-day VWAPs. The Cycle daily trigger pins the closed UTC day and runs the
//! first slice. Later CycleTicks continue the same day.
//!
//! A bucket is called, walking each currency's call-price trie, when the reference
//! price exceeded its call price on at least its `call_threshold_seconds` of the
//! trailing `call_window_seconds`. The call queues it on its deadline, and every
//! CycleTick forfeit-burns the unpaid Nods of the buckets whose
//! `call_notice_period_seconds` lapsed ([`sweep_expired`]).
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

use std::collections::BTreeSet;

use alloy_primitives::B256;
use outbe_oracle::{
    api::get_all_reference_currencies,
    call_sweep::{self, CallSweep, CALL_SWEEP},
    call_window::CallWindows,
};
use outbe_primitives::{
    block::BlockRuntimeContext,
    daily_sweep::PinnedDay,
    error::{PrecompileError, Result, SweepFailure},
    sweep_budget::SweepBudget,
};

use crate::{constants::MAX_NOD_CALL_VISITS_PER_BLOCK, precompile::INod, schema::NodContract};

pub(crate) use forfeits::sweep_expired;

#[cfg(test)]
pub(crate) use calls::{call_currency, CurrencyScan};
#[cfg(test)]
pub(crate) use forfeits::{forfeit_members, Bodies};

/// Cycle daily-trigger entry: schedules the day the Oracle has just finalized.
///
/// Never returns `Err` for missing market data. The Cycle dispatcher propagates
/// a handler error out of the `CycleTick` system transaction, which fails the
/// block. An unregistered pair, an unpriced currency or an unfinalized day
/// therefore each degrade to "no transition" instead.
pub fn schedule(ctx: &BlockRuntimeContext) -> Result<()> {
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

    fn days(&self) -> PinnedDay<'_, 'storage> {
        PinnedDay {
            current: &self.nod.call_sweep_day,
            pending: &self.nod.call_pending_day,
        }
    }

    fn has_work(&self, ctx: &BlockRuntimeContext) -> Result<bool> {
        for iso_code in get_all_reference_currencies(ctx)? {
            if !self.nod.call_bin_tree_root.read(&iso_code)?.is_zero() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn reset_cursors(&self, ctx: &BlockRuntimeContext) -> Result<()> {
        self.nod.call_currency_cursor.write(0)?;
        for iso_code in get_all_reference_currencies(ctx)? {
            self.nod.call_bin_cursor.write(&iso_code, 0)?;
        }
        Ok(())
    }

    fn day_skipped(&mut self, skipped: u32, in_flight: u32) -> Result<()> {
        self.nod.emit(INod::SweepDaySkipped {
            sweep: CALL_SWEEP,
            skippedDay: skipped,
            inFlightDay: in_flight,
        })
    }

    fn slice(&mut self, ctx: &BlockRuntimeContext, pinned_day: u32) -> Result<(u32, bool)> {
        let mut budget = SweepBudget::new(MAX_NOD_CALL_VISITS_PER_BLOCK, u32::MAX, 0);
        let mut called_days = BTreeSet::new();
        let mut windows = CallWindows::new(pinned_day);
        let (called, finished) = calls::call_arm(
            ctx,
            &mut self.nod,
            &mut windows,
            &mut budget,
            &mut called_days,
        )?;
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
fn materializing(nod: &NodContract<'_>, bucket_key: B256) -> Result<bool> {
    let worldwide_day = nod.bucket_worldwide_day.read(&bucket_key)?;
    Ok(nod.ocomp_target_generation.read(&worldwide_day)? != 0)
}
