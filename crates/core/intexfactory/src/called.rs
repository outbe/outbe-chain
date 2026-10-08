//! Daily Called scan: force-calls a series once its COEN VWAP exceeded
//! the call trigger on `call_threshold_seconds` of the last `call_window_seconds`. Candidates
//! come from the call-trigger bin index. Each run recomputes the counts from the
//! Oracle's finalized per-UTC-day VWAPs. The Oracle begin-block hook closes these
//! VWAPs before the CycleTick that drives this scan. The Cycle daily trigger drives
//! this scan.

use alloy_primitives::U256;
use alloy_sol_types::SolCall;
use outbe_intex::SeriesId;
use outbe_oracle::call_window::CallWindow;
use outbe_primitives::daily_sweep::{PinnedDay, Scheduled, SweepDays};
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{
    block::BlockRuntimeContext,
    call_bins::{self, Visit},
    error::{Result, SweepFailure},
    storage::StorageHandle,
    sweep_budget::SweepBudget,
};

use crate::constants::{
    CALL_SWEEP, MAX_GROUP_DECISIONS_PER_BLOCK, MAX_SERIES_ACTIONS_PER_BLOCK, MAX_SERIES_PER_MARK,
    ORIGIN_ROUTER_ADDRESS,
};
use crate::schema::IntexFactoryContract;
use crate::sol_ext::IOriginRouter;
use crate::state::CallBins;

mod group;

pub(crate) use group::{try_call_group, GroupCall};

/// Schedule the day the Oracle just finalized: open a Called sweep over it and
/// run its first slice, or queue it behind the sweep still in flight.
pub fn scan_and_call(ctx: &BlockRuntimeContext) -> Result<u32> {
    let Some(last_closed_day) = closed_day(ctx)? else {
        return Ok(0);
    };
    let factory = IntexFactoryContract::new(ctx.storage.clone());
    let scheduled = pinned_call_day(&factory).schedule(last_closed_day)?;
    match scheduled {
        (next, Scheduled::Opened) => {
            start_call_sweep(ctx, &factory, next)?;
            run_call_slice(ctx)
        }
        (next, Scheduled::Replaced { skipped }) => {
            crate::runtime::emit_event(
                &ctx.storage,
                crate::precompile::IIntexFactory::SweepDaySkipped {
                    sweep: CALL_SWEEP,
                    skippedDay: skipped,
                    inFlightDay: next.current,
                },
            )?;
            Ok(0)
        }
        (_, Scheduled::Queued | Scheduled::Ignored) => Ok(0),
    }
}

fn pinned_call_day<'a, 'storage>(
    factory: &'a IntexFactoryContract<'storage>,
) -> PinnedDay<'a, 'storage> {
    PinnedDay {
        current: &factory.call_sweep_day,
        pending: &factory.call_pending_day,
    }
}

/// The most recent fully-closed UTC day, or `None` while its VWAPs are not final.
pub(crate) fn closed_day(ctx: &BlockRuntimeContext) -> Result<Option<u32>> {
    outbe_oracle::closed_day::finalized_closed_day(
        ctx.storage.clone(),
        ctx.block.timestamp,
        "intexfactory",
    )
}

/// Pin the sweep's current day and walk it from the first currency's lowest bin.
fn start_call_sweep(
    ctx: &BlockRuntimeContext,
    factory: &IntexFactoryContract,
    days: SweepDays,
) -> Result<()> {
    pinned_call_day(factory).open(days)?;
    factory.call_currency_cursor.write(0)?;
    for iso_code in outbe_oracle::api::get_all_reference_currencies(ctx)? {
        factory.call_scan_cursor.write(&iso_code, 0)?;
    }
    Ok(())
}

/// Advance an open sweep by one slice, pinned to the day it opened on so blocks
/// of it decide against the same prices. Returns how many series were called.
pub fn run_call_slice(ctx: &BlockRuntimeContext) -> Result<u32> {
    let factory = IntexFactoryContract::new(ctx.storage.clone());
    let pinned_day = factory.call_sweep_day.read()?;
    if pinned_day == 0 {
        return Ok(0);
    }
    let currencies = outbe_oracle::api::get_all_reference_currencies(ctx)?;

    let mut budget = SweepBudget::new(
        MAX_GROUP_DECISIONS_PER_BLOCK,
        MAX_SERIES_ACTIONS_PER_BLOCK,
        0,
    );
    let mut called: u32 = 0;
    let finished = call_bins::walk_currencies(
        &currencies,
        &factory.call_currency_cursor,
        &mut budget,
        |iso_code, budget| {
            let (calls, finished) = call_currency(ctx, iso_code, pinned_day, budget)?;
            called = called.saturating_add(calls);
            Ok(finished)
        },
    )?;
    if !finished {
        return Ok(called);
    }

    // The next day starts on the next block, so no slice mixes two days' prices.
    if let Some(next) = pinned_call_day(&factory).finish(pinned_day)? {
        start_call_sweep(ctx, &factory, next)?;
    }
    Ok(called)
}

/// Scans one currency's call-price bins on the shared `budget`. Returns the calls
/// made and whether its eligible range was walked to the end.
fn call_currency<'storage>(
    ctx: &BlockRuntimeContext<'storage>,
    iso_code: u16,
    last_closed_day: u32,
    budget: &mut SweepBudget,
) -> Result<(u32, bool)> {
    let factory = IntexFactoryContract::new(ctx.storage.clone());
    let params = crate::config::read_from(&factory, ctx.block.chain_id)?;
    // Use the widest terms ever issued here, not the live profile. A series keeps the
    // terms it was issued with, and a narrowed profile must not hide it from the search.
    let terms = factory.scan_call_terms(
        iso_code,
        params.call_window_seconds,
        params.call_threshold_seconds,
    )?;
    let window = CallWindow::load(&ctx.storage, iso_code, last_closed_day, terms)?;
    let Some(p_star) = window.ceiling() else {
        // Too few priced days for any trigger to be breached often enough.
        return Ok((0, true));
    };

    // Every trigger below `p_star` is breached often enough, so the range ends at
    // its bin. An out-of-range price skips the currency rather than halting.
    let p_bin = match IntexFactoryContract::price_to_bin(p_star) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(target: "outbe::intexfactory", iso_code, error = ?e, "call scan: window price out of range, skipping currency for the day");
            crate::runtime::emit_event(
                &ctx.storage,
                crate::precompile::IIntexFactory::CallScanSkipped {
                    referenceCurrency: iso_code,
                    utcDay: last_closed_day,
                },
            )?;
            return Ok((0, true));
        }
    };

    let index = IntexFactoryContract::new(ctx.storage.clone());
    let mut factory = factory;
    let mut called: u32 = 0;
    let finished = call_bins::walk(
        &CallBins(&index, iso_code),
        p_bin,
        budget,
        |group, budget| {
            let (_, worldwide_day) = IntexFactoryContract::unscoped(group);
            let group = factory.call_bin_group(iso_code, worldwide_day)?;
            let members = group.members.len() as u32;
            if !budget.fits_writes(members) {
                return Ok(Visit::Stop);
            }
            // Isolate per group. A deterministic Err rolls back the group's checkpoint, and the
            // scan logs it and skips the group. A node-local Err fails the block.
            let res = ctx.storage.with_checkpoint(|| {
                try_call_group(
                    GroupCall {
                        storage: &ctx.storage,
                        factory: &mut factory,
                    },
                    &group,
                    &window,
                    ctx.block.timestamp,
                )
            });
            match res {
                Ok(applied) => {
                    budget.admit_writes(applied);
                    called = called.saturating_add(applied);
                }
                Err(e) if e.sweep_failure() == SweepFailure::Propagate => return Err(e),
                Err(e) => {
                    tracing::warn!(target: "outbe::intexfactory", iso_code, worldwide_day = %worldwide_day, error = ?e, "call scan: skipping group");
                }
            }
            Ok(Visit::Next)
        },
    )?;
    Ok((called, finished))
}

/// Cycle daily-trigger entry: opens the day's Called sweep, discarding the count.
pub fn run_daily(ctx: &BlockRuntimeContext) -> Result<()> {
    scan_and_call(ctx)?;
    Ok(())
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
