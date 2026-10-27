//! Daily call scan: force-calls Nod groups off the Oracle's finalized
//! per-UTC-day VWAPs. The Cycle daily trigger schedules the closed UTC day, and every
//! CycleTick walks a slice of it.
//!
//! A group is called, walking each currency's call-price trie, when the reference
//! price exceeded its call price on at least its `call_threshold_seconds` of the
//! trailing `call_window_seconds`. The call queues it on its deadline, and every
//! CycleTick forfeits the groups whose `call_notice_period_seconds` lapsed
//! (`expired::sweep_expired`).
//!
//! Issuance seals all four terms onto the group, and this scan reads them back
//! from it. Retuning a constant therefore leaves every issued group on the
//! terms it was issued with. Gem and intex give the same guarantee.
//!
//! The breach rule needs no per-group streak state. The daily series is global
//! per currency, so one trailing window per currency decides every group
//! denominated in it. Every run recomputes the count from oracle history rather
//! than carrying it. Gem, Credis and Intex run the same shape from their factories.
//!
//! Calls count only days from the group's `full_day_after_issue`.

use alloy_primitives::{B256, U256};
use outbe_oracle::call_pass::{self, CallPass};
use outbe_oracle::call_window::CallWindow;
use outbe_primitives::{
    block::BlockRuntimeContext,
    call_bins,
    call_breach::{BreachTerms, ScanTerms},
    day_pass::{PinnedDay, Progress},
    error::{PrecompileError, Result, PassFailure},
    storage::dsl::Value,
    pass_budget::PassBudget,
};

use outbe_nod::state::CallBins;
use outbe_nod::{api, precompile::INod, schema::NodContract};

use crate::precompile::INodFactory;
use crate::runtime::emit_event;

/// Cycle daily-trigger entry: schedules the day the Oracle has just finalized.
///
/// Never returns `Err` for missing market data. The Cycle dispatcher propagates
/// a handler error out of the `CycleTick` system transaction, which fails the
/// block. An unregistered pair, an unpriced currency or an unfinalized day
/// therefore each degrade to "no transition" instead.
pub fn run_daily(ctx: &BlockRuntimeContext) -> Result<()> {
    call_pass::schedule(ctx, &mut NodCallPass::new(ctx))
}

/// This function initializes the first eligible daily pass from the chain initialization day.
pub fn initialize_daily(ctx: &BlockRuntimeContext) -> Result<()> {
    call_pass::initialize(ctx, &mut NodCallPass::new(ctx))
}

/// Schedules the closed day and walks a slice of the day in flight. Returns the
/// groups called.
pub fn scan_and_call(ctx: &BlockRuntimeContext) -> Result<u32> {
    call_pass::continue_day(ctx, &mut NodCallPass::new(ctx))
}

/// This function runs the next coupled call and forfeit slice against its pinned observation day.
/// The return value counts called groups, not forfeited groups.
pub fn run_call_slice(ctx: &BlockRuntimeContext) -> Result<u32> {
    call_pass::continue_day(ctx, &mut NodCallPass::new(ctx))
}

struct NodCallPass<'storage> {
    nod: NodContract<'storage>,
}

impl<'storage> NodCallPass<'storage> {
    fn new(ctx: &BlockRuntimeContext<'storage>) -> Self {
        Self {
            nod: NodContract::new(ctx.storage.clone()),
        }
    }
}

impl<'storage> CallPass<'storage> for NodCallPass<'storage> {
    const CONSUMER: &'static str = "nod";
    const FORFEIT_PHASES: u8 = 1;

    fn phase(&self) -> &Value<'storage, u8> {
        &self.nod.daily_phase
    }
    fn block_budget(&self) -> &Value<'storage, U256> {
        &self.nod.daily_budget
    }
    fn genesis_day(&self) -> &Value<'storage, u32> {
        &self.nod.daily_genesis_day
    }

    fn forfeit(
        &mut self,
        ctx: &BlockRuntimeContext,
        pinned_day: u32,
        _phase: u8,
        budget: &mut PassBudget,
    ) -> Result<Progress> {
        let through_day = outbe_primitives::deadline_queue::deadline_day(
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
            current: &self.nod.call_sweep_day,
            latest: &self.nod.call_latest_day,
        }
    }

    fn bins(&self, reference_currency: u16) -> CallBins<'_, 'storage> {
        CallBins(&self.nod, reference_currency)
    }

    fn currency_cursor(&self) -> &Value<'storage, u32> {
        &self.nod.call_currency_cursor
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
        let (called, finished) = call_slice(ctx, &self.nod, pinned_day, budget)?;
        // A group holds Nods of several Worldwide Days, so a call refreshes the whole range.
        if called != 0 {
            self.nod.emit(INod::BatchMetadataUpdate {
                _fromTokenId: U256::ZERO,
                _toTokenId: U256::MAX,
            })?;
        }
        Ok((called, finished))
    }
}

/// One currency's call walk: its trailing VWAP window, and the highest bin a
/// group that window breached can sit in.
pub(crate) struct CurrencyScan<'w> {
    pub(crate) iso_code: u16,
    pub(crate) window: &'w CallWindow,
    pub(crate) ceiling: u32,
}

/// Walks every currency's bins up to its window's ceiling. Returns the groups
/// called and whether every currency was walked.
fn call_slice(
    ctx: &BlockRuntimeContext,
    nod: &NodContract<'_>,
    pinned_day: u32,
    budget: &mut PassBudget,
) -> Result<(u32, bool)> {
    let params = outbe_nod::config::read(&ctx.storage)?;
    let mut caller = NodContract::new(ctx.storage.clone());
    let mut called: u32 = 0;
    let finished = call_pass::walk_day(
        ctx,
        pinned_day,
        budget,
        call_pass::CallIndex {
            currency_cursor: &nod.call_currency_cursor,
            bins: |iso_code| CallBins(nod, iso_code),
            scan_terms: |iso_code| scan_terms(nod, iso_code, &params),
            skipped: |iso_code| {
                emit_event(
                    &ctx.storage,
                    INodFactory::CallScanSkipped {
                        referenceCurrency: iso_code,
                        utcDay: pinned_day,
                    },
                )
            },
        },
        |iso_code, window, ceiling, budget| {
            let scan = CurrencyScan {
                iso_code,
                window,
                ceiling,
            };
            let (calls, finished) = call_currency(ctx, &mut caller, scan, budget)?;
            called = called.saturating_add(calls);
            Ok(finished)
        },
    )?;
    Ok((called, finished))
}

/// The live profile is the terms the next group is sealed with.
fn scan_terms(
    nod: &NodContract<'_>,
    iso_code: u16,
    params: &outbe_nod::config::NodParams,
) -> Result<ScanTerms> {
    outbe_primitives::call_breach::scan_terms(
        &nod.max_call_window_seconds,
        &nod.min_call_threshold_seconds,
        iso_code,
        params.call_window_seconds,
        params.call_threshold_seconds,
    )
}

/// Walks the currency's bins up to the window's ceiling, resuming where it stopped.
pub(crate) fn call_currency(
    ctx: &BlockRuntimeContext,
    nod: &mut NodContract<'_>,
    scan: CurrencyScan<'_>,
    budget: &mut PassBudget,
) -> Result<(u32, bool)> {
    let CurrencyScan {
        iso_code,
        window,
        ceiling,
    } = scan;
    let index = NodContract::new(ctx.storage.clone());
    let mut called: u32 = 0;
    let finished = call_bins::walk(
        &CallBins(&index, iso_code),
        ceiling,
        budget,
        |group_key, budget| {
            call_pass::call_entry::<NodCallPass>(&ctx.storage, budget, group_key, || {
                if retire_paid_group(nod, group_key, ctx.block.timestamp)? {
                    return Ok(1);
                }
                if !call_if_breached(nod, window, group_key, ctx.block.timestamp)? {
                    return Ok(0);
                }
                called += 1;
                Ok(1)
            })
        },
    )?;
    Ok((called, finished))
}

/// Takes a group every Nod of which is paid out of its bin once its first full day is
/// under way: a Nod issued at its 00:00:00 still joins it, none after. Returns whether
/// it did.
fn retire_paid_group(nod: &mut NodContract<'_>, group_key: B256, now: u64) -> Result<bool> {
    if nod.group_nod_count.read(&group_key)? != 0 || nod.group_called_at.read(&group_key)? != 0 {
        return Ok(false);
    }
    let start_day = nod
        .groups
        .get(group_key)?
        .ok_or(crate::errors::NodFactoryError::MissingGroupForCall { group_key })?
        .full_day_after_issue;
    if now <= outbe_primitives::time::date_key_to_utc_timestamp(start_day.value()) {
        return Ok(false);
    }
    nod.remove_call_bin(group_key)?;
    Ok(true)
}

/// Calls the group if the window breached its sealed terms. Returns whether it did.
fn call_if_breached(
    nod: &mut NodContract<'_>,
    window: &CallWindow,
    group_key: B256,
    now: u64,
) -> Result<bool> {
    if nod.group_nod_count.read(&group_key)? == 0 || nod.group_called_at.read(&group_key)? != 0 {
        return Ok(false);
    }
    let start_day = nod
        .groups
        .get(group_key)?
        .ok_or(crate::errors::NodFactoryError::MissingGroupForCall { group_key })?
        .full_day_after_issue;
    let terms = nod.read_call_terms(group_key)?;
    let breached = window.breached(&BreachTerms {
        call_price: terms.call_price_minor,
        window_seconds: terms.call_window_seconds,
        threshold_seconds: terms.call_threshold_seconds,
        start_day: start_day.value(),
    });
    if !breached {
        return Ok(false);
    }
    mark_called(nod, group_key, now, terms.call_notice_period_seconds)?;
    Ok(true)
}

/// Stamps the call and opens the settlement window the group sealed.
fn mark_called(
    nod: &mut NodContract<'_>,
    group_key: B256,
    now: u64,
    notice_period: u32,
) -> Result<()> {
    let deadline = api::settlement_deadline_of(now, notice_period);
    nod.record_group_called(group_key, now, deadline)?;
    nod.emit(INod::NodGroupCalled {
        groupKey: group_key,
        calledAt: now,
        settlementDeadline: deadline,
    })
}
