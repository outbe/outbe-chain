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
use outbe_oracle::call_sweep::{self, CallSweep, Decided, CALL_SWEEP};
use outbe_oracle::call_window::{CallWindow, CallWindows};
use outbe_primitives::{
    addresses::CREDIS_FACTORY_ADDRESS,
    block::BlockRuntimeContext,
    call_bins::{self, Visit},
    call_breach::{BreachTerms, ScanTerms},
    daily_sweep::PinnedDay,
    error::{PrecompileError, Result},
    storage::StorageHandle,
    sweep_budget::SweepBudget,
    time::first_full_day,
};

use crate::precompile::ICredisFactory::{CallScanSkipped, SweepDaySkipped};
use crate::schema::CredisFactoryContract;

/// Max positions one block's slice visits. The rest of the pass continues on the
/// next block, pinned to the same day.
pub(crate) const MAX_CREDIS_CALL_VISITS_PER_BLOCK: u32 = 4096;

/// Cycle daily-trigger entry: schedules the day the Oracle has just finalized.
pub fn run_daily(ctx: &BlockRuntimeContext) -> Result<()> {
    call_sweep::schedule(ctx, &mut CredisCallSweep::new(ctx))
}

/// Voids the called positions whose settlement window lapsed.
pub fn sweep_forfeits(ctx: &BlockRuntimeContext) -> Result<()> {
    crate::expired::sweep_expired(ctx)?;
    Ok(())
}

/// One block of every Credis sweep: what fell due, then a slice of the call sweep.
pub fn continue_sweeps(ctx: &BlockRuntimeContext) -> Result<()> {
    sweep_forfeits(ctx)?;
    run_call_slice(ctx)?;
    Ok(())
}

/// Schedules the closed day and walks a slice of the day in flight. Returns the
/// positions called.
pub fn scan_and_call(ctx: &BlockRuntimeContext) -> Result<u32> {
    let mut sweep = CredisCallSweep::new(ctx);
    call_sweep::schedule(ctx, &mut sweep)?;
    call_sweep::continue_day(ctx, &mut sweep)
}

/// Walks the next slice of the day in flight, pinned to the day it opened on so
/// later blocks decide against the same prices. Returns the positions called.
pub fn run_call_slice(ctx: &BlockRuntimeContext) -> Result<u32> {
    call_sweep::continue_day(ctx, &mut CredisCallSweep::new(ctx))
}

struct CredisCallSweep<'storage> {
    factory: CredisFactoryContract<'storage>,
    credis: CredisContract<'storage>,
}

impl<'storage> CredisCallSweep<'storage> {
    fn new(ctx: &BlockRuntimeContext<'storage>) -> Self {
        Self {
            factory: CredisFactoryContract::new(ctx.storage.clone()),
            credis: CredisContract::new(ctx.storage.clone()),
        }
    }
}

impl<'storage> CallSweep<'storage> for CredisCallSweep<'storage> {
    const CONSUMER: &'static str = "credis";

    fn days(&self) -> PinnedDay<'_, 'storage> {
        PinnedDay {
            current: &self.factory.call_sweep_day,
            pending: &self.factory.call_pending_day,
        }
    }

    fn has_work(&self, ctx: &BlockRuntimeContext) -> Result<bool> {
        for iso_code in get_all_reference_currencies(ctx)? {
            if !self.credis.call_bin_tree_root.read(&iso_code)?.is_zero() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn reset_cursors(&self, ctx: &BlockRuntimeContext) -> Result<()> {
        self.factory.call_currency_cursor.write(0)?;
        for iso_code in get_all_reference_currencies(ctx)? {
            self.credis.call_bin_cursor.write(&iso_code, 0)?;
        }
        Ok(())
    }

    fn day_skipped(&mut self, skipped: u32, in_flight: u32) -> Result<()> {
        emit(
            &self.credis.storage,
            &SweepDaySkipped {
                sweep: CALL_SWEEP,
                skippedDay: skipped,
                inFlightDay: in_flight,
            },
        )
    }

    fn slice(&mut self, ctx: &BlockRuntimeContext, pinned_day: u32) -> Result<(u32, bool)> {
        let currencies = get_all_reference_currencies(ctx)?;
        let mut slice = CallSlice {
            ctx,
            pinned_day,
            index: CredisContract::new(ctx.storage.clone()),
            credis: CredisContract::new(ctx.storage.clone()),
            windows: CallWindows::new(pinned_day),
            called: 0,
        };
        let mut budget = SweepBudget::new(MAX_CREDIS_CALL_VISITS_PER_BLOCK, u32::MAX, 0);
        let finished = call_bins::walk_currencies(
            &currencies,
            &self.factory.call_currency_cursor,
            &mut budget,
            |iso_code, budget| slice.currency(iso_code, budget),
        )?;
        Ok((slice.called, finished))
    }
}

fn emit(storage: &StorageHandle<'_>, event: &impl SolEvent) -> Result<()> {
    storage.emit_event(CREDIS_FACTORY_ADDRESS, SolEvent::encode_log_data(event))
}

/// One block's slice of the pass.
struct CallSlice<'a, 'storage> {
    ctx: &'a BlockRuntimeContext<'storage>,
    pinned_day: u32,
    index: CredisContract<'storage>,
    credis: CredisContract<'storage>,
    windows: CallWindows,
    called: u32,
}

impl CallSlice<'_, '_> {
    /// Walks one currency's bins up to its window's ceiling. Returns whether it ended.
    fn currency(&mut self, iso_code: u16, budget: &mut SweepBudget) -> Result<bool> {
        let (ctx, pinned_day, index) = (self.ctx, self.pinned_day, &self.index);
        if index.call_scan_failed_day.read(&iso_code)? == pinned_day
            || !call_bins::pending(&CallBins(index, iso_code))?
        {
            return Ok(true);
        }
        let window = self
            .windows
            .window(&ctx.storage, iso_code, || scan_terms(index, iso_code))?;
        let skipped = || {
            emit(
                &ctx.storage,
                &CallScanSkipped {
                    referenceCurrency: iso_code,
                    utcDay: pinned_day,
                },
            )
        };
        let Some(ceiling) = call_sweep::ceiling_bin(
            window,
            &index.call_scan_failed_day,
            iso_code,
            pinned_day,
            skipped,
        )?
        else {
            return Ok(true);
        };
        let (credis, mutated) = (&mut self.credis, &mut self.called);
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
    match call_sweep::decide(outcome, PrecompileError::sweep_failure)? {
        Decided::Done(called) => {
            if called {
                *mutated = mutated.saturating_add(1);
            }
            Ok(Visit::Next)
        }
        Decided::Stopped => Ok(Visit::Stop),
        Decided::Skipped(error) => {
            tracing::warn!(target: "outbe::credisfactory", %position_id, error = ?error, "credis scan: skipping position");
            Ok(Visit::Next)
        }
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
