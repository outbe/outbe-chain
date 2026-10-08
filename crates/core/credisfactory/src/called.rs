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

use alloy_primitives::U256;
use alloy_sol_types::SolEvent;

use outbe_credis::constants::{CALL_THRESHOLD, CALL_WINDOW};
use outbe_credis::{CallBins, CredisContract, CredisState, Position};
use outbe_oracle::api::get_all_reference_currencies;
use outbe_oracle::call_window::{CallWindow, CallWindows};
use outbe_oracle::schema::OracleContract;
use outbe_primitives::{
    addresses::CREDIS_FACTORY_ADDRESS,
    block::BlockRuntimeContext,
    call_bins::{self, Visit},
    call_breach::{BreachTerms, ScanTerms},
    daily_sweep::{PinnedDay, Scheduled, SweepDays},
    error::{Result, SweepFailure},
    sweep_budget::SweepBudget,
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
    if factory.call_sweep_day.read()? == 0 && !has_call_work(ctx)? {
        return Ok(0);
    }
    let scheduled = pinned_call_day(&factory).schedule(last_closed_day)?;
    match scheduled {
        (next, Scheduled::Opened) => {
            start_call_sweep(ctx, &factory, next)?;
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

/// Whether any reference currency still holds an Open position.
fn has_call_work(ctx: &BlockRuntimeContext) -> Result<bool> {
    let credis = CredisContract::new(ctx.storage.clone());
    for iso_code in get_all_reference_currencies(ctx)? {
        if !credis.call_bin_tree_root.read(&iso_code)?.is_zero() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Pins the sweep's day and walks it from the first currency's lowest bin.
fn start_call_sweep(
    ctx: &BlockRuntimeContext,
    factory: &CredisFactoryContract,
    days: SweepDays,
) -> Result<()> {
    pinned_call_day(factory).open(days)?;
    factory.call_currency_cursor.write(0)?;
    let credis = CredisContract::new(ctx.storage.clone());
    for iso_code in get_all_reference_currencies(ctx)? {
        credis.call_bin_cursor.write(&iso_code, 0)?;
    }
    Ok(())
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
    let currencies = get_all_reference_currencies(ctx)?;
    let mut slice = CallSlice {
        ctx,
        index: CredisContract::new(ctx.storage.clone()),
        credis: CredisContract::new(ctx.storage.clone()),
        windows: CallWindows::new(pinned_day),
        mutated: 0,
    };
    let mut budget = SweepBudget::new(MAX_CREDIS_CALL_VISITS_PER_BLOCK, u32::MAX, 0);
    let finished = call_bins::walk_currencies(
        &currencies,
        &factory.call_currency_cursor,
        &mut budget,
        |iso_code, budget| slice.currency(iso_code, budget),
    )?;
    if finished {
        // The next day starts on the next block, so no slice mixes two days' prices.
        if let Some(next) = pinned_call_day(&factory).finish(pinned_day)? {
            start_call_sweep(ctx, &factory, next)?;
        }
    }
    Ok(slice.mutated)
}

/// One block's slice of the pass.
struct CallSlice<'a, 'storage> {
    ctx: &'a BlockRuntimeContext<'storage>,
    index: CredisContract<'storage>,
    credis: CredisContract<'storage>,
    windows: CallWindows,
    mutated: u32,
}

impl CallSlice<'_, '_> {
    /// Walks one currency's bins up to its window's ceiling. Returns whether it ended.
    fn currency(&mut self, iso_code: u16, budget: &mut SweepBudget) -> Result<bool> {
        if self.index.call_bin_tree_root.read(&iso_code)?.is_zero() {
            return Ok(true);
        }
        let index = &self.index;
        let window = self
            .windows
            .window(&self.ctx.storage, iso_code, || scan_terms(index, iso_code))?;
        let Some(high) = window.ceiling() else {
            return Ok(true);
        };
        let ceiling = match call_bins::price_to_bin(high) {
            Ok(bin) => bin,
            Err(error) => {
                tracing::warn!(target: "outbe::credisfactory", iso_code, error = ?error, "credis scan: window price out of range, skipping currency for the day");
                return Ok(true);
            }
        };
        let (ctx, credis, mutated) = (self.ctx, &mut self.credis, &mut self.mutated);
        call_bins::walk(
            &CallBins(index, iso_code),
            ceiling,
            budget,
            |position_id, _| visit(ctx, credis, window, position_id, mutated),
        )
    }
}

/// Calls the position if its breach window filled. A deterministic error is isolated
/// to this position. A node-local error fails the block.
fn visit(
    ctx: &BlockRuntimeContext,
    credis: &mut CredisContract<'_>,
    window: &CallWindow,
    position_id: U256,
    mutated: &mut u32,
) -> Result<Visit> {
    // Structural reads stay on `?` so infra errors still propagate.
    let position = credis.get_position(position_id)?;
    let now = ctx.block.timestamp;
    let outcome = ctx
        .storage
        .with_checkpoint(|| call_if_breached(credis, window, &position, now));
    match outcome {
        Ok(true) => *mutated = mutated.saturating_add(1),
        Ok(false) => {}
        Err(error) => match error.sweep_failure() {
            SweepFailure::Propagate => return Err(error),
            SweepFailure::Stop => return Ok(Visit::Stop),
            SweepFailure::Skip => tracing::warn!(
                target: "outbe::credisfactory",
                %position_id,
                error = ?error,
                "credis scan: skipping position"
            ),
        },
    }
    Ok(Visit::Next)
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
