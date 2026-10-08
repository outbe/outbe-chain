//! Daily Called scan: force-calls a series once its COEN VWAP exceeded
//! the call trigger on `call_threshold_seconds` of the last `call_window_seconds`. Candidates
//! come from the call-trigger bin index. Each run recomputes the counts from the
//! Oracle's finalized per-UTC-day VWAPs. The Oracle begin-block hook closes these
//! VWAPs before the CycleTick that drives this scan. The Cycle daily trigger drives
//! this scan.

use alloy_primitives::U256;
use alloy_sol_types::SolCall;
use outbe_intex::SeriesId;
use outbe_oracle::api::get_all_reference_currencies;
use outbe_oracle::call_sweep::{self, CallSweep, CALL_SWEEP};
use outbe_oracle::call_window::{CallWindow, CallWindows};
use outbe_primitives::daily_sweep::PinnedDay;
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{
    block::BlockRuntimeContext,
    call_bins::{self, Visit},
    error::{Result, SweepFailure},
    storage::{dsl::Value, StorageHandle},
    sweep_budget::SweepBudget,
};

use crate::constants::{MAX_SERIES_PER_MARK, ORIGIN_ROUTER_ADDRESS};
use crate::schema::IntexFactoryContract;
use crate::sol_ext::IOriginRouter;
use crate::state::CallBins;

mod group;

pub(crate) use group::{try_call_group, GroupCall};

/// Cycle daily-trigger entry: schedules the day the Oracle has just finalized.
pub fn run_daily(ctx: &BlockRuntimeContext) -> Result<()> {
    call_sweep::schedule(ctx, &mut IntexCallSweep::new(ctx))
}

/// Schedules the closed day and walks a slice of the day in flight. Returns the
/// series called.
pub fn scan_and_call(ctx: &BlockRuntimeContext) -> Result<u32> {
    let mut sweep = IntexCallSweep::new(ctx);
    call_sweep::schedule(ctx, &mut sweep)?;
    call_sweep::continue_day(ctx, &mut sweep)
}

/// Walks the next slice of the day in flight, pinned to the day it opened on so
/// blocks of it decide against the same prices. Returns how many series were called.
pub fn run_call_slice(ctx: &BlockRuntimeContext) -> Result<u32> {
    call_sweep::continue_day(ctx, &mut IntexCallSweep::new(ctx))
}

struct IntexCallSweep<'storage> {
    factory: IntexFactoryContract<'storage>,
}

impl<'storage> IntexCallSweep<'storage> {
    fn new(ctx: &BlockRuntimeContext<'storage>) -> Self {
        Self {
            factory: IntexFactoryContract::new(ctx.storage.clone()),
        }
    }
}

impl<'storage> CallSweep<'storage> for IntexCallSweep<'storage> {
    const CONSUMER: &'static str = "intex";

    type Bins<'a>
        = CallBins<'a, 'storage>
    where
        Self: 'a;

    fn days(&self) -> PinnedDay<'_, 'storage> {
        PinnedDay {
            current: &self.factory.call_sweep_day,
            pending: &self.factory.call_pending_day,
        }
    }

    fn bins(&self, reference_currency: u16) -> CallBins<'_, 'storage> {
        CallBins(&self.factory, reference_currency)
    }

    fn currency_cursor(&self) -> &Value<'storage, u32> {
        &self.factory.call_currency_cursor
    }

    fn day_skipped(&mut self, skipped: u32, in_flight: u32) -> Result<()> {
        crate::runtime::emit_event(
            &self.factory.storage,
            crate::precompile::IIntexFactory::SweepDaySkipped {
                sweep: CALL_SWEEP,
                skippedDay: skipped,
                inFlightDay: in_flight,
            },
        )
    }

    fn slice(&mut self, ctx: &BlockRuntimeContext, pinned_day: u32) -> Result<(u32, bool)> {
        let currencies = get_all_reference_currencies(ctx)?;
        let params = crate::config::read_from(&self.factory, ctx.block.chain_id)?;
        let mut windows = CallWindows::new(ctx.storage.clone(), pinned_day);
        let mut budget = SweepBudget::per_block();
        let index = &self.factory;
        let mut called: u32 = 0;
        let finished = call_bins::walk_currencies(
            &currencies,
            &index.call_currency_cursor,
            &mut budget,
            |iso_code, budget| {
                let skipped = || {
                    crate::runtime::emit_event(
                        &ctx.storage,
                        crate::precompile::IIntexFactory::CallScanSkipped {
                            referenceCurrency: iso_code,
                            utcDay: pinned_day,
                        },
                    )
                };
                let Some((window, ceiling)) = call_sweep::currency_ceiling(
                    &CallBins(index, iso_code),
                    &mut windows,
                    || {
                        index.scan_call_terms(
                            iso_code,
                            params.call_window_seconds,
                            params.call_threshold_seconds,
                        )
                    },
                    skipped,
                )?
                else {
                    return Ok(true);
                };
                let (calls, finished) = call_currency(ctx, iso_code, window, ceiling, budget)?;
                called = called.saturating_add(calls);
                Ok(finished)
            },
        )?;
        Ok((called, finished))
    }
}

/// Walks one currency's bins up to `ceiling`, calling each breached group whole.
/// Returns the series called and whether the eligible range was walked to the end.
fn call_currency(
    ctx: &BlockRuntimeContext,
    iso_code: u16,
    window: &CallWindow,
    ceiling: u32,
    budget: &mut SweepBudget,
) -> Result<(u32, bool)> {
    let index = IntexFactoryContract::new(ctx.storage.clone());
    let mut factory = IntexFactoryContract::new(ctx.storage.clone());
    let mut called: u32 = 0;
    let finished = call_bins::walk(
        &CallBins(&index, iso_code),
        ceiling,
        budget,
        |key, budget| {
            let (_, worldwide_day) = IntexFactoryContract::unscoped(key);
            let group = factory.call_bin_group(iso_code, worldwide_day)?;
            if !budget.fits_writes(group.members.len() as u32) {
                return Ok(Visit::Stop);
            }
            call_sweep::call_entry::<IntexCallSweep>(&ctx.storage, budget, key, || {
                let call = GroupCall {
                    storage: &ctx.storage,
                    factory: &mut factory,
                };
                let applied = try_call_group(call, &group, window, ctx.block.timestamp)?;
                called += applied;
                Ok(applied)
            })
        },
    )?;
    Ok((called, finished))
}

/// One message per group, split only where the wire's cap forces it. `called_at`
/// travels so every target derives the same deadline the origin did. Returns the
/// series whose message the router refused. A node-local failure fails the block instead.
pub(crate) fn notify_called(
    storage: &StorageHandle<'_>,
    worldwide_day: WorldwideDay,
    called_at: u32,
    members: &[SeriesId],
) -> Result<Vec<SeriesId>> {
    let mut refused = Vec::new();
    for chunk in members.chunks(MAX_SERIES_PER_MARK) {
        // The batch is the unit: a refusal returns every series in it.
        let sent = storage.with_checkpoint(|| {
            // Relay-float-funded: value 0, so the router self-quotes and pays the fee from its float.
            storage.call(
                ORIGIN_ROUTER_ADDRESS,
                U256::ZERO,
                IOriginRouter::sendMarkCalledCall {
                    worldwideDay: worldwide_day.value(),
                    calledAt: called_at,
                    seriesIds: chunk.iter().map(|id| (*id).into()).collect(),
                }
                .abi_encode()
                .into(),
            )?;
            Ok(())
        });
        match sent {
            Ok(()) => {}
            Err(error) if error.sweep_failure() == SweepFailure::Propagate => return Err(error),
            Err(error) => {
                tracing::warn!(
                    target: "outbe::intexfactory",
                    worldwide_day = worldwide_day.value(),
                    called_at,
                    series = ?chunk.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
                    error = ?error,
                    "called notice: refused"
                );
                refused.extend_from_slice(chunk);
            }
        }
    }
    Ok(refused)
}

#[cfg(test)]
pub(crate) use outbe_primitives::daily_sweep::currency_position;
