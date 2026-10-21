//! Daily Called scan: force-calls a series once its COEN VWAP exceeded
//! the call trigger on `call_threshold_seconds` of the last `call_window_seconds`. Candidates
//! come from the call-trigger bin index. Each run recomputes the counts from the
//! Oracle's finalized per-UTC-day VWAPs. The Cycle daily trigger schedules the closed
//! day, and every CycleTick walks a slice of it.

use alloy_primitives::U256;
use alloy_sol_types::SolCall;
use outbe_intex::SeriesId;
use outbe_oracle::call_sweep::{self, CallSweep};
use outbe_oracle::call_window::CallWindow;
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
use outbe_intex::{CallBins, IntexContract};

pub(crate) mod group;

pub(crate) use group::{call_if_breached, GroupCall};

/// Cycle daily-trigger entry: schedules the day the Oracle has just finalized.
pub fn run_daily(ctx: &BlockRuntimeContext) -> Result<()> {
    call_sweep::schedule(ctx, &mut IntexCallSweep::new(ctx))
}

pub fn initialize_daily(ctx: &BlockRuntimeContext) -> Result<()> {
    call_sweep::initialize(ctx, &mut IntexCallSweep::new(ctx))
}

/// Runs the coupled daily pass, including series forfeits after calls.
/// Returns the series called, not the pass completion state.
pub fn scan_and_call(ctx: &BlockRuntimeContext) -> Result<u32> {
    let mut sweep = IntexCallSweep::new(ctx);
    call_sweep::schedule(ctx, &mut sweep)?;
    call_sweep::continue_day(ctx, &mut sweep)
}

/// Resumes the coupled daily pass, pinned to the same observation day and shared
/// block allowance. Returns how many series were called.
pub fn run_call_slice(ctx: &BlockRuntimeContext) -> Result<u32> {
    call_sweep::continue_day(ctx, &mut IntexCallSweep::new(ctx))
}

struct IntexCallSweep<'storage> {
    intex: IntexContract<'storage>,
}

impl<'storage> IntexCallSweep<'storage> {
    fn new(ctx: &BlockRuntimeContext<'storage>) -> Self {
        Self {
            intex: IntexContract::new(ctx.storage.clone()),
        }
    }
}

impl<'storage> CallSweep<'storage> for IntexCallSweep<'storage> {
    const CONSUMER: &'static str = "intex";
    const FORFEIT_PHASES: u8 = 1;

    fn phase(&self) -> &Value<'storage, u8> {
        &self.intex.daily_phase
    }

    fn block_budget(&self) -> &Value<'storage, U256> {
        &self.intex.daily_budget
    }

    fn genesis_day(&self) -> &Value<'storage, u32> {
        &self.intex.daily_genesis_day
    }

    fn forfeit(
        &mut self,
        ctx: &BlockRuntimeContext,
        pinned_day: u32,
        _phase: u8,
        budget: &mut SweepBudget,
    ) -> Result<outbe_primitives::daily_sweep::Progress> {
        let through_day = outbe_primitives::expiry_queue::deadline_day(
            outbe_primitives::time::date_key_to_utc_timestamp(pinned_day),
        )?;
        crate::expired::sweep_through(ctx, through_day, budget)
    }

    type Bins<'a>
        = CallBins<'a, 'storage>
    where
        Self: 'a;

    fn days(&self) -> PinnedDay<'_, 'storage> {
        PinnedDay {
            current: &self.intex.call_sweep_day,
            latest: &self.intex.call_latest_day,
        }
    }

    fn bins(&self, reference_currency: u16) -> CallBins<'_, 'storage> {
        CallBins(&self.intex, reference_currency)
    }

    fn currency_cursor(&self) -> &Value<'storage, u32> {
        &self.intex.call_currency_cursor
    }

    fn slice(
        &mut self,
        ctx: &BlockRuntimeContext,
        pinned_day: u32,
        budget: &mut SweepBudget,
    ) -> Result<(u32, bool)> {
        let params = outbe_intex::config::read(&ctx.storage)?;
        let index = &self.intex;
        let mut called: u32 = 0;
        let finished = call_sweep::walk_day(
            ctx,
            pinned_day,
            budget,
            call_sweep::CallIndex {
                currency_cursor: &index.call_currency_cursor,
                bins: |iso_code| CallBins(index, iso_code),
                scan_terms: |iso_code| {
                    index.scan_call_terms(
                        iso_code,
                        params.call_window_seconds,
                        params.call_threshold_seconds,
                    )
                },
                skipped: |iso_code| {
                    crate::runtime::emit_event(
                        &ctx.storage,
                        crate::precompile::IIntexFactory::CallScanSkipped {
                            referenceCurrency: iso_code,
                            utcDay: pinned_day,
                        },
                    )
                },
            },
            |iso_code, window, ceiling, budget| {
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
    let index = IntexContract::new(ctx.storage.clone());
    let mut intex = IntexContract::new(ctx.storage.clone());
    let factory = IntexFactoryContract::new(ctx.storage.clone());
    let mut called: u32 = 0;
    let finished = call_bins::walk(
        &CallBins(&index, iso_code),
        ceiling,
        budget,
        |group, budget| {
            if !budget.fits_writes(intex.group_member_count(group)?) {
                return Ok(Visit::Stop);
            }
            call_sweep::call_entry::<IntexCallSweep>(&ctx.storage, budget, group, || {
                let call = GroupCall {
                    storage: &ctx.storage,
                    intex: &mut intex,
                    factory: &factory,
                };
                let applied = call_if_breached(call, group, window, ctx.block.timestamp)?;
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
