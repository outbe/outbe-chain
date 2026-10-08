//! The daily call sweep every right runs: the Cycle trigger schedules the closed UTC
//! day, and every block walks one slice of it, pinned to that day's prices.

use std::fmt::Display;

use outbe_primitives::{
    block::BlockRuntimeContext,
    call_bins::{self, CallBinStore, Visit},
    call_breach::ScanTerms,
    daily_sweep::{PinnedDay, Scheduled},
    error::{decide, Decided, PrecompileError, Result, SweepFailure},
    math::tree_math,
    storage::{dsl::Value, StorageHandle},
    sweep_budget::SweepBudget,
};

use crate::api::get_all_reference_currencies;
use crate::call_window::{CallWindow, CallWindows};
use crate::schema::OracleContract;

/// `SweepDaySkipped.sweep` of the call sweep, the same in every right's ABI.
pub const CALL_SWEEP: u8 = 1;

/// One right's call sweep.
pub trait CallSweep<'storage> {
    /// Names the right in the logs of a day the Oracle has not finalized.
    const CONSUMER: &'static str;

    type Bins<'a>: CallBinStore<'storage>
    where
        Self: 'a;

    fn days(&self) -> PinnedDay<'_, 'storage>;
    /// One reference currency's bins.
    fn bins(&self, reference_currency: u16) -> Self::Bins<'_>;
    /// The currency the walk in flight stands at.
    fn currency_cursor(&self) -> &Value<'storage, u32>;

    /// Sorts a failed call. A node-local failure fails the block.
    fn classify(error: &PrecompileError) -> SweepFailure {
        error.sweep_failure()
    }

    /// Whether anything is left to call. An idle right schedules nothing.
    fn has_work(&self, ctx: &BlockRuntimeContext) -> Result<bool> {
        for reference_currency in get_all_reference_currencies(ctx)? {
            if tree_math::find_first_left_inclusive(&self.bins(reference_currency), 0)?.is_some() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Points every cursor at the start of a freshly opened day.
    fn reset_cursors(&self, ctx: &BlockRuntimeContext) -> Result<()> {
        self.currency_cursor().write(0)?;
        for reference_currency in get_all_reference_currencies(ctx)? {
            self.bins(reference_currency)
                .scan_cursor()
                .write(&reference_currency, 0)?;
        }
        Ok(())
    }

    /// Emits `SweepDaySkipped` for a waiting day a newer one pushed out.
    fn day_skipped(&mut self, skipped: u32, in_flight: u32) -> Result<()>;
    /// Walks one slice of `pinned_day`. Returns the calls made and whether the day ended.
    fn slice(&mut self, ctx: &BlockRuntimeContext, pinned_day: u32) -> Result<(u32, bool)>;
}

/// Opens the day the Oracle has just finalized, or queues it behind the one in flight.
pub fn schedule<'s, S: CallSweep<'s>>(ctx: &BlockRuntimeContext, sweep: &mut S) -> Result<()> {
    let Some(day) = crate::closed_day::finalized_closed_day(
        ctx.storage.clone(),
        ctx.block.timestamp,
        S::CONSUMER,
    )?
    else {
        return Ok(());
    };
    if sweep.days().current.read()? == 0 && !sweep.has_work(ctx)? {
        return Ok(());
    }
    let scheduled = sweep.days().schedule(day)?;
    match scheduled {
        (next, Scheduled::Opened) => {
            sweep.days().open(next)?;
            sweep.reset_cursors(ctx)
        }
        (next, Scheduled::Replaced { skipped }) => sweep.day_skipped(skipped, next.current),
        (_, Scheduled::Queued | Scheduled::Ignored) => Ok(()),
    }
}

/// Walks the pinned day's next slice, and opens the waiting day once it ends.
/// Returns the calls made.
pub fn continue_day<'s, S: CallSweep<'s>>(ctx: &BlockRuntimeContext, sweep: &mut S) -> Result<u32> {
    let pinned_day = sweep.days().current.read()?;
    if pinned_day == 0 {
        return Ok(0);
    }
    let finalized = OracleContract::new(ctx.storage.clone())
        .utc_day_vwap_last_finalized
        .read()?;
    if finalized < pinned_day {
        tracing::warn!(
            target: "outbe::oracle",
            consumer = S::CONSUMER,
            pinned_day,
            finalized,
            "call sweep: pinned utc-day VWAP not finalized, holding the sweep"
        );
        return Ok(0);
    }
    let (called, finished) = sweep.slice(ctx, pinned_day)?;
    if finished {
        if let Some(next) = sweep.days().finish(pinned_day)? {
            sweep.days().open(next)?;
            sweep.reset_cursors(ctx)?;
        }
    }
    Ok(called)
}

/// The currency's window and the highest bin a breached entry can sit in. `None`
/// when the currency is settled for the pinned day: nothing left past its cursor, too
/// few priced days, or a price no bin covers. The last marks the day failed and calls
/// `skipped` once, so later slices pass the currency by.
pub fn currency_ceiling<'w, 's, S: CallBinStore<'s>>(
    bins: &S,
    windows: &'w mut CallWindows<'_>,
    terms: impl FnOnce() -> Result<ScanTerms>,
    skipped: impl FnOnce() -> Result<()>,
) -> Result<Option<(&'w CallWindow, u32)>> {
    let (reference_currency, pinned_day) = (bins.currency(), windows.last_day());
    if bins.failed_day().read(&reference_currency)? == pinned_day || !call_bins::pending(bins)? {
        return Ok(None);
    }
    let window = windows.window(reference_currency, terms)?;
    let Some(ceiling) = window.ceiling() else {
        return Ok(None);
    };
    match call_bins::price_to_bin(ceiling) {
        Ok(bin) => Ok(Some((window, bin))),
        Err(error) => {
            tracing::warn!(target: "outbe::oracle", reference_currency, error = ?error, "call sweep: window price out of range, skipping currency for the day");
            bins.failed_day().write(&reference_currency, pinned_day)?;
            skipped()?;
            Ok(None)
        }
    }
}

/// Calls one entry in its own checkpoint. `call` returns the writes it made, 0 when
/// the entry was not called. A deterministic failure rolls back only this entry, and
/// running out of gas resumes on it next block.
pub fn call_entry<'s, S: CallSweep<'s>>(
    storage: &StorageHandle<'_>,
    budget: &mut SweepBudget,
    entry: impl Display,
    call: impl FnOnce() -> Result<u32>,
) -> Result<Visit> {
    match decide(storage.with_checkpoint(call), S::classify)? {
        Decided::Done(writes) => {
            budget.admit_writes(writes);
            Ok(Visit::Next)
        }
        Decided::Stopped => Ok(Visit::Stop),
        Decided::Skipped(error) => {
            tracing::warn!(target: "outbe::oracle", consumer = S::CONSUMER, %entry, error = ?error, "call sweep: skipping entry");
            Ok(Visit::Next)
        }
    }
}
