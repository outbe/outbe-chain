//! The daily call sweep every right runs: the Cycle trigger schedules the closed UTC
//! day, and every block walks one slice of it, pinned to that day's prices.

use outbe_primitives::{
    block::BlockRuntimeContext,
    call_bins,
    daily_sweep::{PinnedDay, Scheduled},
    error::Result,
    storage::dsl::Map,
};

pub use outbe_primitives::error::{decide, Decided};

use crate::call_window::CallWindow;
use crate::schema::OracleContract;

/// `SweepDaySkipped.sweep` of the call sweep, the same in every right's ABI.
pub const CALL_SWEEP: u8 = 1;

/// One right's call sweep.
pub trait CallSweep<'storage> {
    /// Names the right in the logs of a day the Oracle has not finalized.
    const CONSUMER: &'static str;

    fn days(&self) -> PinnedDay<'_, 'storage>;
    /// Whether anything is left to call. An idle right schedules nothing.
    fn has_work(&self, ctx: &BlockRuntimeContext) -> Result<bool>;
    /// Points every cursor at the start of a freshly opened day.
    fn reset_cursors(&self, ctx: &BlockRuntimeContext) -> Result<()>;
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

/// The highest bin a call can sit in for the day, or `None` when the currency is
/// settled for it: too few priced days, or a price no bin covers. The latter marks
/// the day in `failed_day` and calls `skipped` once, so later slices pass it by.
pub fn ceiling_bin(
    window: &CallWindow,
    failed_day: &Map<'_, u16, u32>,
    reference_currency: u16,
    pinned_day: u32,
    skipped: impl FnOnce() -> Result<()>,
) -> Result<Option<u32>> {
    let Some(ceiling) = window.ceiling() else {
        return Ok(None);
    };
    match call_bins::price_to_bin(ceiling) {
        Ok(bin) => Ok(Some(bin)),
        Err(error) => {
            tracing::warn!(target: "outbe::oracle", reference_currency, error = ?error, "call sweep: window price out of range, skipping currency for the day");
            failed_day.write(&reference_currency, pinned_day)?;
            skipped()?;
            Ok(None)
        }
    }
}
