//! Daily price-path scan: calls Credis off the Oracle's finalized per-UTC-day
//! VWAPs. The Cycle daily trigger schedules the closed UTC day, and every CycleTick
//! walks a slice of it.
//!
//! A group of Credis moves `Issued -> Called` when the COEN price in its REFERENCE
//! currency sat strictly above its call price on `call_threshold_seconds` of the
//! trailing `call_window_seconds`, counted from the group's first full day. Both terms
//! are sealed onto the group by its first member. The issuance currency a Credis is
//! denominated in never enters the threshold.
//!
//! The breach rule needs no per-group streak state. The daily series is
//! global per currency, so one trailing window per reference currency decides
//! every group anchored to it. Every run recomputes the count from oracle
//! history and does not carry it. Mirrors the Gem, Intex and Nod call sweeps.

use alloy_primitives::{B256, U256};
use alloy_sol_types::SolEvent;

use outbe_credis::config::CredisParams;
use outbe_credis::precompile::ICredis;
use outbe_credis::{CallBins, CredisContract};

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
};

use crate::precompile::ICredisFactory::{CallScanSkipped, SweepDaySkipped};

/// Cycle daily-trigger entry: schedules the day the Oracle has just finalized.
pub fn run_daily(ctx: &BlockRuntimeContext) -> Result<()> {
    call_sweep::schedule(ctx, &mut CredisCallSweep::new(ctx))
}

/// Schedules the closed day and walks a slice of the day in flight. Returns the
/// groups called.
pub fn scan_and_call(ctx: &BlockRuntimeContext) -> Result<u32> {
    let mut sweep = CredisCallSweep::new(ctx);
    call_sweep::schedule(ctx, &mut sweep)?;
    call_sweep::continue_day(ctx, &mut sweep)
}

/// Walks the next slice of the day in flight, pinned to the day it opened on so
/// later blocks decide against the same prices. Returns the groups called.
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
                    |group, budget| {
                        call_sweep::call_entry::<Self>(&ctx.storage, budget, group, || {
                            let calls = u32::from(call_if_breached(
                                &mut caller,
                                window,
                                group,
                                ctx.block.timestamp,
                            )?);
                            called += calls;
                            Ok(calls)
                        })
                    },
                )
            },
        )?;
        // A group holds Credis of any id, so a call refreshes the whole range.
        if called != 0 {
            self.credis.emit(ICredis::BatchMetadataUpdate {
                _fromTokenId: U256::ZERO,
                _toTokenId: U256::MAX,
            })?;
        }
        Ok((called, finished))
    }
}

pub(crate) fn emit(storage: &StorageHandle<'_>, event: &impl SolEvent) -> Result<()> {
    storage.emit_event(CREDIS_FACTORY_ADDRESS, SolEvent::encode_log_data(event))
}

/// Calls a group whose breach window filled. Returns whether it was called.
fn call_if_breached(
    credis: &mut CredisContract<'_>,
    window: &CallWindow,
    group: B256,
    now: u64,
) -> Result<bool> {
    let breached = window.breached(&BreachTerms {
        call_price: credis.group_call_price_minor.read(&group)?,
        window_seconds: credis.group_call_window_seconds.read(&group)?,
        threshold_seconds: credis.group_call_threshold_seconds.read(&group)?,
        start_day: credis.group_full_day.read(&group)?,
    });
    if !breached {
        return Ok(false);
    }
    let deadline = credis.record_group_called(group, now)?;
    credis.emit(ICredis::CredisGroupCalled {
        groupKey: group,
        calledAt: now,
        settlementDeadline: deadline,
    })?;
    Ok(true)
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
