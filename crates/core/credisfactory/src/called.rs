//! Daily price-path scan: calls positions off the Oracle's finalized per-UTC-day
//! VWAPs. The Cycle daily trigger pins the closed UTC day and runs the first slice.
//! Later CycleTicks continue the same day through [`continue_sweeps`], which also
//! voids the lapsed called positions through [`crate::expired`].
//!
//! A position moves `Open -> Called` when the COEN price in its REFERENCE currency
//! sat strictly above the call price on `call_threshold_seconds` of the trailing
//! `call_window_seconds`. Both terms are sealed onto the position at opening. The
//! issuance currency the position is denominated in never enters the threshold.
//!
//! The breach rule needs no per-position streak state. The daily series is
//! global per currency, so one trailing window per reference currency decides
//! every position anchored to it. Every run recomputes the count from oracle
//! history and does not carry it. Mirrors the Gem, Intex and Nod call sweeps.

use alloy_sol_types::SolEvent;

use outbe_credis::constants::{CALL_THRESHOLD, CALL_WINDOW};
use outbe_credis::{CredisContract, CredisState, Position};
use outbe_oracle::call_window::{CallWindow, CallWindows};
use outbe_oracle::schema::OracleContract;
use outbe_primitives::{
    addresses::CREDIS_FACTORY_ADDRESS,
    block::BlockRuntimeContext,
    call_breach::{BreachTerms, ScanTerms},
    daily_sweep::{PinnedDay, Scheduled, SweepDays},
    error::{Result, SweepFailure},
    time::first_full_day,
};

use crate::precompile::ICredisFactory::SweepDaySkipped;
use crate::schema::CredisFactoryContract;

/// Max positions one block's slice visits. The rest of the pass continues on the
/// next block, pinned to the same day.
pub(crate) const MAX_CREDIS_CALL_VISITS_PER_BLOCK: u32 = 4096;

/// `SweepDaySkipped.sweep` for the call sweep.
const CALL_SWEEP: u8 = 1;

/// Cycle daily-trigger entry: runs the scan, discarding the count.
pub fn run_daily(ctx: &BlockRuntimeContext) -> Result<()> {
    scan_and_call(ctx)?;
    Ok(())
}

/// Runs from CycleTick every block, before the daily trigger can queue a newer day.
pub fn continue_sweeps(ctx: &BlockRuntimeContext) -> Result<()> {
    run_call_slice(ctx)?;
    crate::expired::sweep_expired(ctx)?;
    Ok(())
}

/// Schedules the day the Oracle has just finalized: opens a sweep over it and runs
/// its first slice, or queues it behind the sweep still in flight.
///
/// Never returns `Err` for missing market data: a handler error fails the block.
pub fn scan_and_call(ctx: &BlockRuntimeContext) -> Result<u32> {
    let Some(last_closed_day) = outbe_oracle::closed_day::finalized_closed_day(
        ctx.storage.clone(),
        ctx.block.timestamp,
        "credis",
    )?
    else {
        return Ok(0);
    };
    let factory = CredisFactoryContract::new(ctx.storage.clone());
    if factory.call_sweep_day.read()? == 0
        && CredisContract::new(ctx.storage.clone()).active_len()? == 0
    {
        return Ok(0);
    }
    let scheduled = pinned_call_day(&factory).schedule(last_closed_day)?;
    match scheduled {
        (next, Scheduled::Opened) => {
            start_call_sweep(&factory, next)?;
            run_call_slice(ctx)
        }
        (next, Scheduled::Replaced { skipped }) => {
            ctx.storage.emit_event(
                CREDIS_FACTORY_ADDRESS,
                SolEvent::encode_log_data(&SweepDaySkipped {
                    sweep: CALL_SWEEP,
                    skippedDay: skipped,
                    inFlightDay: next.current,
                }),
            )?;
            Ok(0)
        }
        (_, Scheduled::Queued | Scheduled::Ignored) => Ok(0),
    }
}

fn pinned_call_day<'a, 'storage>(
    factory: &'a CredisFactoryContract<'storage>,
) -> PinnedDay<'a, 'storage> {
    PinnedDay {
        current: &factory.call_sweep_day,
        pending: &factory.call_pending_day,
    }
}

/// Pins the sweep's day and walks the active index from the top.
fn start_call_sweep(factory: &CredisFactoryContract, days: SweepDays) -> Result<()> {
    pinned_call_day(factory).open(days)?;
    factory.call_scan_cursor.write(0)
}

/// Advances an open sweep by one slice, pinned to the day it opened on so later
/// blocks decide against the same prices. Returns the number of positions mutated.
pub fn run_call_slice(ctx: &BlockRuntimeContext) -> Result<u32> {
    let factory = CredisFactoryContract::new(ctx.storage.clone());
    let pinned_day = factory.call_sweep_day.read()?;
    if pinned_day == 0 {
        return Ok(0);
    }
    let oracle = OracleContract::new(ctx.storage.clone());
    let finalized = oracle.utc_day_vwap_last_finalized.read()?;
    if finalized < pinned_day {
        tracing::warn!(
            target: "outbe::credisfactory",
            pinned_day,
            finalized,
            "credis scan: pinned utc-day VWAP not finalized, holding the sweep"
        );
        return Ok(0);
    }
    let mut slice = CallSlice {
        ctx,
        credis: CredisContract::new(ctx.storage.clone()),
        windows: CallWindows::new(pinned_day),
        mutated: 0,
    };
    if slice.walk(&factory)? {
        // The next day starts on the next block, so no slice mixes two days' prices.
        if let Some(next) = pinned_call_day(&factory).finish(pinned_day)? {
            start_call_sweep(&factory, next)?;
        }
    }
    Ok(slice.mutated)
}

/// One block's slice of the pass.
struct CallSlice<'a, 'storage> {
    ctx: &'a BlockRuntimeContext<'storage>,
    credis: CredisContract<'storage>,
    windows: CallWindows,
    mutated: u32,
}

impl CallSlice<'_, '_> {
    /// Walks the active index down from the cursor. Returns whether the pass ended.
    ///
    /// Descending walk: `remove_active` swap-pops the tail into the hole, and the
    /// tail is already behind a descending cursor, so the walk skips no live entry.
    fn walk(&mut self, factory: &CredisFactoryContract) -> Result<bool> {
        let len = self.credis.active_len()?;
        if len == 0 {
            factory.call_scan_cursor.write(0)?;
            return Ok(true);
        }
        // Stored as `index + 1`. 0 means "start a fresh pass from the top".
        let mut cursor = match factory.call_scan_cursor.read()? {
            0 => len - 1,
            resume => resume.saturating_sub(1).min(len - 1),
        };
        let mut visited: u32 = 0;
        loop {
            if visited >= MAX_CREDIS_CALL_VISITS_PER_BLOCK || !self.visit(cursor)? {
                factory.call_scan_cursor.write(cursor.saturating_add(1))?;
                return Ok(false);
            }
            visited = visited.saturating_add(1);
            if cursor == 0 {
                factory.call_scan_cursor.write(0)?;
                return Ok(true);
            }
            cursor -= 1;
        }
    }

    /// Visits the position at `index`. Returns false when the slice is out of gas
    /// and must resume at this position.
    fn visit(&mut self, index: u32) -> Result<bool> {
        let Some(position_id) = self.credis.active_at(index)? else {
            return Ok(true);
        };
        // Structural reads stay on `?` so infra errors still propagate.
        let position = self.credis.get_position(position_id)?;
        let window = self
            .windows
            .window(&self.ctx.storage, position.reference_currency, || {
                scan_terms(&self.credis, position.reference_currency)
            })?;
        let now = self.ctx.block.timestamp;
        let credis = &mut self.credis;
        // The call is pure storage and arithmetic. A deterministic error is isolated
        // to this position. A node-local error fails the block.
        let outcome = self
            .ctx
            .storage
            .with_checkpoint(|| call_if_breached(credis, window, &position, now));
        match outcome {
            Ok(true) => self.mutated = self.mutated.saturating_add(1),
            Ok(false) => {}
            Err(error) => match error.sweep_failure() {
                SweepFailure::Propagate => return Err(error),
                SweepFailure::Stop => return Ok(false),
                SweepFailure::Skip => tracing::warn!(
                    target: "outbe::credisfactory",
                    %position_id,
                    error = ?error,
                    "credis scan: skipping position"
                ),
            },
        }
        Ok(true)
    }
}

/// Calls an Open position whose breach window filled. Returns whether it moved.
fn call_if_breached(
    credis: &mut CredisContract<'_>,
    window: &CallWindow,
    position: &Position,
    now: u64,
) -> Result<bool> {
    Ok(position.lifecycle_state()? == CredisState::Open
        && window.breached(&BreachTerms {
            call_price: position.call_price_minor,
            window_seconds: position.call_window_seconds,
            threshold_seconds: position.call_threshold_seconds,
            start_day: first_full_day(position.issued_at),
        })
        && credis.mark_called(position.position_id, now)?)
}

/// The constants are the live terms: the next position is opened with them.
fn scan_terms(credis: &CredisContract<'_>, reference_currency: u16) -> Result<ScanTerms> {
    outbe_primitives::call_breach::scan_terms(
        &credis.max_call_window_seconds,
        &credis.min_call_threshold_seconds,
        reference_currency,
        CALL_WINDOW,
        CALL_THRESHOLD,
    )
}
