//! Daily call sweep: calls the Gem groups whose reference price breached their call
//! price on enough days of their window. The daily trigger schedules the closed day,
//! and every CycleTick walks a slice of it.

use alloy_primitives::{B256, U256};
use outbe_gem::precompile::IGem::{BatchMetadataUpdate, GemGroupCalled};

use crate::errors::GemFactoryError;
use outbe_gem::{CallBins, CallTerms, GemContract, GemParams};
use outbe_oracle::{
    call_pass::{self, CallPass},
    call_window::CallWindow,
};
use outbe_primitives::{
    block::BlockRuntimeContext,
    call_bins,
    call_breach::{BreachTerms, ScanTerms},
    day_pass::PinnedDay,
    error::{PassFailure, PrecompileError, Result},
    pass_budget::PassBudget,
    storage::dsl::Value,
};

/// Cycle daily-trigger entry: schedules the day the Oracle has just finalized.
pub fn schedule_call_pass(ctx: &BlockRuntimeContext) -> Result<()> {
    call_pass::schedule(ctx, &mut GemCallPass::new(ctx))
}

pub fn initialize_daily(ctx: &BlockRuntimeContext) -> Result<()> {
    call_pass::initialize(ctx, &mut GemCallPass::new(ctx))
}

/// This function runs the coupled daily pass, including forfeits and position expiry after calls.
/// It returns the number of called groups, not the pass completion state.
pub fn scan_and_call(ctx: &BlockRuntimeContext) -> Result<u32> {
    call_pass::continue_day(ctx, &mut GemCallPass::new(ctx))
}

/// This function resumes the coupled daily pass under this block height's shared allowance.
/// It returns the number of called groups.
pub fn process_call_pass(ctx: &BlockRuntimeContext) -> Result<u32> {
    call_pass::continue_day(ctx, &mut GemCallPass::new(ctx))
}

pub(crate) struct GemCallPass<'storage> {
    gem: GemContract<'storage>,
}

impl<'storage> GemCallPass<'storage> {
    fn new(ctx: &BlockRuntimeContext<'storage>) -> Self {
        Self {
            gem: GemContract::new(ctx.storage.clone()),
        }
    }
}

impl<'storage> CallPass<'storage> for GemCallPass<'storage> {
    const CONSUMER: &'static str = "gem";
    const FORFEIT_PHASES: u8 = 2;

    fn phase(&self) -> &Value<'storage, u8> {
        &self.gem.daily_phase
    }

    fn block_budget(&self) -> &Value<'storage, U256> {
        &self.gem.daily_budget
    }

    fn genesis_day(&self) -> &Value<'storage, u32> {
        &self.gem.daily_genesis_day
    }

    fn forfeit(
        &mut self,
        ctx: &BlockRuntimeContext,
        pinned_day: u32,
        phase: u8,
        budget: &mut PassBudget,
    ) -> Result<outbe_primitives::day_pass::Progress> {
        let pinned_ts = outbe_primitives::time::date_key_to_utc_timestamp(pinned_day)
            .map_err(crate::errors::date_step_error)?;
        let through_day = outbe_primitives::deadline_queue::deadline_day(pinned_ts)?;
        match phase {
            1 => crate::forfeited::forfeit_through(ctx, through_day, budget),
            2 => crate::positions_expired::expire_through(ctx, through_day, budget),
            _ => Err(PrecompileError::revert("invalid Gem daily phase")),
        }
    }

    type Bins<'a>
        = CallBins<'a, 'storage>
    where
        Self: 'a;

    fn days(&self) -> PinnedDay<'_, 'storage> {
        PinnedDay {
            current: &self.gem.call_sweep_day,
            latest: &self.gem.call_latest_day,
        }
    }

    fn bins(&self, reference_currency: u16) -> CallBins<'_, 'storage> {
        CallBins(&self.gem, reference_currency)
    }

    fn currency_cursor(&self) -> &Value<'storage, u32> {
        &self.gem.call_currency_cursor
    }

    fn classify(error: &PrecompileError) -> PassFailure {
        error.record_pass_failure()
    }

    fn slice(
        &mut self,
        ctx: &BlockRuntimeContext,
        pinned_day: u32,
        budget: &mut PassBudget,
    ) -> Result<(u32, bool)> {
        let params = outbe_gem::config::read(&ctx.storage)?;
        let mut called: u32 = 0;
        let gem = &self.gem;
        let finished = call_pass::walk_day(
            ctx,
            pinned_day,
            budget,
            call_pass::CallIndex {
                currency_cursor: &gem.call_currency_cursor,
                bins: |iso_code| CallBins(gem, iso_code),
                scan_terms: |iso_code| scan_terms(gem, iso_code, &params),
            },
            |iso_code, window, ceiling, budget| {
                let (calls, finished) = call_currency(ctx, iso_code, window, ceiling, budget)?;
                called = called.saturating_add(calls);
                Ok(finished)
            },
        )?;
        // Gem ids carry no order, so a call can only refresh the whole range.
        if called != 0 {
            self.gem.emit(BatchMetadataUpdate {
                _fromTokenId: U256::ZERO,
                _toTokenId: U256::MAX,
            })?;
        }
        Ok((called, finished))
    }
}

/// The live profile is the terms the next gem is sealed with.
fn scan_terms(gem: &GemContract<'_>, iso_code: u16, params: &GemParams) -> Result<ScanTerms> {
    outbe_primitives::call_breach::scan_terms(
        &gem.max_call_window_seconds,
        &gem.min_call_threshold_seconds,
        iso_code,
        params.call_window_seconds,
        params.call_threshold_seconds,
    )
}

/// Walk one currency's group bins up to `ceiling`, resuming where it stopped.
/// Returns the calls made and whether the eligible range was walked to the end.
pub(crate) fn call_currency(
    ctx: &BlockRuntimeContext,
    iso_code: u16,
    window: &CallWindow,
    ceiling: u32,
    budget: &mut PassBudget,
) -> Result<(u32, bool)> {
    let index = GemContract::new(ctx.storage.clone());
    let mut gem = GemContract::new(ctx.storage.clone());
    let mut called: u32 = 0;
    let finished = call_bins::walk(
        &CallBins(&index, iso_code),
        ceiling,
        budget,
        |group, budget| {
            call_pass::call_entry::<GemCallPass>(&ctx.storage, budget, group, || {
                if retire_paid_group(&mut gem, group, ctx.block.timestamp)? {
                    return Ok(1);
                }
                let calls = u32::from(call_if_breached(
                    &mut gem,
                    window,
                    group,
                    ctx.block.timestamp,
                )?);
                called += calls;
                Ok(calls)
            })
        },
    )?;
    Ok((called, finished))
}

/// Takes a group every gem of which is paid out of its bin once its first full day is
/// under way: a gem issued at its 00:00:00 still joins it, none after. Returns whether
/// it did.
fn retire_paid_group(gem: &mut GemContract<'_>, group: B256, now: u64) -> Result<bool> {
    if gem.group_gem_count.read(&group)? != 0 || gem.group_called_at.read(&group)? != 0 {
        return Ok(false);
    }
    let start_day = gem
        .groups
        .get(group)?
        .ok_or(GemFactoryError::MissingGroupForCall { group })?
        .full_day_after_issue;
    let start_ts = start_day
        .to_timestamp_utc()
        .map_err(crate::errors::date_step_error)?;
    if now <= start_ts {
        return Ok(false);
    }
    gem.remove_call_bin(group)?;
    Ok(true)
}

/// Calls an uncalled group whose reference-currency daily VWAP exceeded its call price
/// on at least its threshold days of its trailing window. Returns true if called.
///
/// The terms were sealed when the group opened, so a later profile change cannot
/// re-term it. Days before the group's first full day never count: no member existed yet.
pub(crate) fn call_if_breached(
    gem: &mut GemContract<'_>,
    window: &CallWindow,
    group: B256,
    now: u64,
) -> Result<bool> {
    if gem.group_gem_count.read(&group)? == 0 || gem.group_called_at.read(&group)? != 0 {
        return Ok(false);
    }
    let terms = gem.read_call_terms(group)?;
    let start_day = gem
        .groups
        .get(group)?
        .ok_or(GemFactoryError::MissingGroupForCall { group })?
        .full_day_after_issue;
    if !window.breached(&breach_terms(&terms, start_day.value())) {
        return Ok(false);
    }
    mark_called(gem, group, &terms, now)?;
    Ok(true)
}

/// The terms a group is decided on: the ones it sealed, from its first full day.
pub(crate) fn breach_terms(terms: &CallTerms, full_day_after_issue: u32) -> BreachTerms {
    BreachTerms {
        call_price: terms.call_price_minor,
        window_seconds: terms.call_window_seconds,
        threshold_seconds: terms.call_threshold_seconds,
        start_day: full_day_after_issue,
    }
}

/// `Issued -> Called` for every unpaid member at once: the group leaves the trie for the
/// expiry queue, where it waits on its sealed notice period as one entry.
pub(crate) fn mark_called(
    gem: &mut GemContract<'_>,
    group: B256,
    terms: &CallTerms,
    now: u64,
) -> Result<()> {
    let deadline = outbe_gem::api::settlement_deadline_of(now, terms.call_notice_period_seconds);
    gem.record_group_called(group, now, deadline)?;
    gem.emit(GemGroupCalled {
        groupKey: group,
        calledAt: now,
        settlementDeadline: deadline,
    })
}
