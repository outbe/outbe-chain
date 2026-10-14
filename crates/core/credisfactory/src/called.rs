//! Daily price-path scan: calls Credis off the Oracle's finalized per-UTC-day
//! VWAPs. The Cycle daily trigger schedules the closed UTC day, and every CycleTick
//! walks a slice of it.
//!
//! A Credis moves `Issued -> Called` when the COEN price in its REFERENCE currency
//! sat strictly above the call price on `call_threshold_seconds` of the trailing
//! `call_window_seconds`. Both terms are sealed onto the Credis at issuance. The
//! issuance currency the Credis is denominated in never enters the threshold.
//!
//! The breach rule needs no per-Credis streak state. The daily series is
//! global per currency, so one trailing window per reference currency decides
//! every Credis anchored to it. Every run recomputes the count from oracle
//! history and does not carry it. Mirrors the Gem, Intex and Nod call sweeps.

use alloy_sol_types::SolEvent;

use outbe_credis::config::CredisParams;
use outbe_credis::{CallBins, Credis, CredisContract, CredisState};

use crate::rules::CredisRules;
use outbe_oracle::call_sweep::{self, CallSweep, CALL_SWEEP};
use outbe_oracle::call_window::CallWindow;
use outbe_primitives::{
    addresses::CREDIS_FACTORY_ADDRESS,
    block::BlockRuntimeContext,
    call_bins,
    call_breach::{BreachTerms, ScanTerms},
    daily_sweep::PinnedDay,
    error::Result,
    storage::{dsl::Value, StorageHandle},
    time::first_full_day,
};

use crate::precompile::ICredisFactory::{CallScanSkipped, SweepDaySkipped};

/// Cycle daily-trigger entry: schedules the day the Oracle has just finalized.
pub fn run_daily(ctx: &BlockRuntimeContext) -> Result<()> {
    call_sweep::schedule(ctx, &mut CredisCallSweep::new(ctx))
}

/// Schedules the closed day and walks a slice of the day in flight. Returns the
/// Credis called.
pub fn scan_and_call(ctx: &BlockRuntimeContext) -> Result<u32> {
    let mut sweep = CredisCallSweep::new(ctx);
    call_sweep::schedule(ctx, &mut sweep)?;
    call_sweep::continue_day(ctx, &mut sweep)
}

/// Walks the next slice of the day in flight, pinned to the day it opened on so
/// later blocks decide against the same prices. Returns the Credis called.
pub fn run_call_slice(ctx: &BlockRuntimeContext) -> Result<u32> {
    call_sweep::continue_day(ctx, &mut CredisCallSweep::new(ctx))
}

struct CredisCallSweep<'storage> {
    credis: CredisContract<'storage>,
}

impl<'storage> CredisCallSweep<'storage> {
    fn new(ctx: &BlockRuntimeContext<'storage>) -> Self {
        Self {
            credis: CredisContract::new(ctx.storage.clone()),
        }
    }
}

impl<'storage> CallSweep<'storage> for CredisCallSweep<'storage> {
    const CONSUMER: &'static str = "credis";

    type Bins<'a>
        = CallBins<'a, 'storage>
    where
        Self: 'a;

    fn days(&self) -> PinnedDay<'_, 'storage> {
        PinnedDay {
            current: &self.credis.call_sweep_day,
            pending: &self.credis.call_pending_day,
        }
    }

    fn bins(&self, reference_currency: u16) -> CallBins<'_, 'storage> {
        CallBins(&self.credis, reference_currency)
    }

    fn currency_cursor(&self) -> &Value<'storage, u32> {
        &self.credis.call_currency_cursor
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
        let live = outbe_credis::config::read(&ctx.storage)?;
        let mut caller = CredisContract::new(ctx.storage.clone());
        let mut called: u32 = 0;
        let index = &self.credis;
        let finished = call_sweep::walk_day(
            ctx,
            pinned_day,
            call_sweep::CallIndex {
                currency_cursor: &index.call_currency_cursor,
                bins: |iso_code| CallBins(index, iso_code),
                scan_terms: |iso_code| scan_terms(index, iso_code, &live),
                skipped: |iso_code| {
                    emit(
                        &ctx.storage,
                        &CallScanSkipped {
                            referenceCurrency: iso_code,
                            utcDay: pinned_day,
                        },
                    )
                },
            },
            |iso_code, window, ceiling, budget| {
                call_bins::walk(
                    &CallBins(index, iso_code),
                    ceiling,
                    budget,
                    |credis_id, budget| {
                        call_sweep::call_entry::<Self>(&ctx.storage, budget, credis_id, || {
                            let record = caller.get_credis(credis_id)?;
                            let calls = u32::from(call_if_breached(
                                &mut caller,
                                window,
                                &record,
                                ctx.block.timestamp,
                            )?);
                            called += calls;
                            Ok(calls)
                        })
                    },
                )
            },
        )?;
        Ok((called, finished))
    }
}

pub(crate) fn emit(storage: &StorageHandle<'_>, event: &impl SolEvent) -> Result<()> {
    storage.emit_event(CREDIS_FACTORY_ADDRESS, SolEvent::encode_log_data(event))
}

/// Calls an Issued Credis whose breach window filled. Returns whether it moved.
fn call_if_breached(
    credis: &mut CredisContract<'_>,
    window: &CallWindow,
    record: &Credis,
    now: u64,
) -> Result<bool> {
    Ok(record.lifecycle_state()? == CredisState::Issued
        && window.breached(&BreachTerms {
            call_price: record.call_price_minor,
            window_seconds: record.call_window_seconds,
            threshold_seconds: record.call_threshold_seconds,
            start_day: first_full_day(record.issued_at),
        })
        && credis.mark_called(record.credis_id, now)?)
}

/// The live profile is the terms the next Credis is issued with.
fn scan_terms(
    credis: &CredisContract<'_>,
    reference_currency: u16,
    live: &CredisParams,
) -> Result<ScanTerms> {
    outbe_primitives::call_breach::scan_terms(
        &credis.max_call_window_seconds,
        &credis.min_call_threshold_seconds,
        reference_currency,
        live.call_window_seconds,
        live.call_threshold_seconds,
    )
}
