//! Daily Called scan: force-calls a series once its COEN VWAP exceeded
//! the call trigger on `call_threshold_seconds` of the last `call_window_seconds`. Candidates
//! come from the call-trigger bin index. Each run recomputes the counts from the
//! Oracle's finalized per-UTC-day VWAPs. The Cycle daily trigger schedules the closed
//! day, and every CycleTick walks a slice of it.

use crate::transport::outbox::Label;
use crate::transport::wire::MSG_INTEX_SERIES_CALL_NOTICE;
use alloy_primitives::{FixedBytes, U256};
use outbe_intex::SeriesId;
use outbe_oracle::call_pass::{self, CallPass};
use outbe_oracle::call_window::CallWindow;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::Result,
    storage::{dsl::Value, StorageHandle},
};
use outbe_system_common::call_bins::{self, Visit};
use outbe_system_common::day_pass::PinnedDay;
use outbe_system_common::pass_budget::PassBudget;
use outbe_system_common::time::EmissionDate;

use crate::constants::MAX_SERIES_PER_MARK;
use crate::schema::IntexFactoryContract;
use outbe_intex::{CallBins, IntexContract};

pub(crate) mod group;

pub(crate) use group::{breach_terms, call_series_if_breached, GroupCall};

/// Cycle daily-trigger entry: schedules the day the Oracle has just finalized.
pub fn schedule_call_pass(ctx: &BlockRuntimeContext) -> Result<()> {
    call_pass::schedule(ctx, &mut IntexCallPass::new(ctx))
}

/// This function runs the coupled daily pass, including series forfeits after calls.
/// It returns the number of called series, not the pass completion state.
pub fn scan_and_call(ctx: &BlockRuntimeContext) -> Result<u32> {
    call_pass::continue_day(ctx, &mut IntexCallPass::new(ctx))
}

/// This function resumes the coupled daily pass with the same observation day and shared block allowance.
/// It returns the number of called series.
pub fn run_call_pass(ctx: &BlockRuntimeContext) -> Result<u32> {
    call_pass::continue_day(ctx, &mut IntexCallPass::new(ctx))
}

struct IntexCallPass<'storage> {
    intex: IntexContract<'storage>,
}

impl<'storage> IntexCallPass<'storage> {
    fn new(ctx: &BlockRuntimeContext<'storage>) -> Self {
        Self {
            intex: IntexContract::new(ctx.storage.clone()),
        }
    }
}

impl<'storage> CallPass<'storage> for IntexCallPass<'storage> {
    const CONSUMER: &'static str = "intex";
    const FORFEIT_PHASES: u8 = 1;

    fn phase(&self) -> &Value<'storage, u8> {
        &self.intex.daily_phase
    }

    fn block_budget(&self) -> &Value<'storage, U256> {
        &self.intex.daily_budget
    }

    fn forfeit(
        &mut self,
        ctx: &BlockRuntimeContext,
        pinned_day: u32,
        _phase: u8,
        budget: &mut PassBudget,
    ) -> Result<outbe_system_common::day_pass::Progress> {
        let pinned_ts = outbe_system_common::time::date_key_to_utc_timestamp(pinned_day)
            .map_err(crate::errors::date_step_error)?;
        let through_day = outbe_core_common::deadline_queue::deadline_day(pinned_ts)?;
        crate::close::close_through(ctx, through_day, budget)
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
        budget: &mut PassBudget,
    ) -> Result<(u32, bool)> {
        let params = outbe_intex::config::read(&ctx.storage)?;
        let index = &self.intex;
        let mut called: u32 = 0;
        let finished = call_pass::walk_day(
            ctx,
            pinned_day,
            budget,
            call_pass::CallIndex {
                currency_cursor: &index.call_currency_cursor,
                bins: |iso_code| CallBins(index, iso_code),
                scan_terms: |iso_code| {
                    index.scan_call_terms(
                        iso_code,
                        params.call_window_seconds,
                        params.call_threshold_seconds,
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
    budget: &mut PassBudget,
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
            call_pass::call_entry::<IntexCallPass>(&ctx.storage, budget, group, || {
                let call = GroupCall {
                    storage: &ctx.storage,
                    intex: &mut intex,
                    factory: &factory,
                };
                let applied = call_series_if_breached(call, group, window, ctx.block.timestamp)?;
                called += applied;
                Ok(applied)
            })
        },
    )?;
    Ok((called, finished))
}

/// Puts the call notices of one run into the outbox for every chain that is not Off, one
/// message per group, split only where the wire's cap forces it. `called_at` travels so every
/// target derives the same deadline the origin did.
pub(crate) fn notify_called(
    storage: &StorageHandle<'_>,
    emission_day: EmissionDate,
    called_at: u64,
    members: &[SeriesId],
) -> Result<()> {
    let chains = crate::targets::series_target_chains(storage)?;
    for chunk in members.chunks(MAX_SERIES_PER_MARK) {
        let series: Vec<FixedBytes<14>> = chunk.iter().map(|id| (*id).into()).collect();
        crate::outbox::push_to(
            storage,
            &chains,
            Label::new(
                MSG_INTEX_SERIES_CALL_NOTICE,
                emission_day.value(),
                [series.len() as u32, 0, 0],
            ),
            crate::transport::gas::intex_series_call_notice(series.len()),
            &crate::transport::wire::encode_intex_series_call_notice(
                emission_day.value(),
                called_at,
                &series,
            )?,
        )?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) use outbe_system_common::day_pass::currency_position;
