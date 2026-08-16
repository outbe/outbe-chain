//! Daily Called scan: force-calls a Qualified series once its COEN VWAP exceeded
//! the call trigger on `call_threshold` of the last `call_window`. Candidates
//! come from the call-trigger bin index; counts are recomputed each run from the
//! Oracle's finalized per-UTC-day VWAPs, which the Oracle begin-block hook
//! closes before the CycleTick that drives this scan. Driven by the Cycle daily
//! trigger.

use std::collections::BTreeMap;

use alloy_primitives::U256;
use alloy_sol_types::SolCall;
use outbe_intex::SeriesId;
use outbe_oracle::schema::{OracleContract, PairIndex};
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result},
    math::{constants::MAX_BIN_ID, tree_math},
    storage::StorageHandle,
    time::{previous_date_key, timestamp_to_date_key, SECONDS_PER_DAY},
};

use outbe_intex::IntexState;

use crate::constants::ORIGIN_ROUTER_ADDRESS;
use crate::qualified::ScanBudget;
use crate::schema::IntexFactoryContract;
use crate::sol_ext::IOriginRouter;
use crate::state::{Group, QualifiedBinTree};

/// Open a Called sweep over the day the Oracle has just finalized and run its
/// first slice. Returns the number of series force-called in that slice.
pub fn scan_and_call(ctx: &BlockRuntimeContext) -> Result<u32> {
    let oracle = OracleContract::new(ctx.storage.clone());

    // Most recent fully-closed UTC day (finalized VWAP).
    let last_closed_day = previous_date_key(timestamp_to_date_key(ctx.block.timestamp));

    // The Oracle begin-block hook finalizes that day earlier in this same
    // block; a lagging watermark means the ordering broke — skip loudly
    // instead of misreading an unfinalized day as empty.
    // todo use api.rs
    let finalized = oracle.utc_day_vwap_last_finalized.read()?;
    if finalized < last_closed_day {
        tracing::warn!(target: "outbe::intexfactory", last_closed_day, finalized, "call scan: utc-day VWAP not finalized yet, skipping run");
        return Ok(0);
    }

    let factory = IntexFactoryContract::new(ctx.storage.clone());
    factory.call_sweep_day.write(last_closed_day)?;
    run_call_slice(ctx)
}

/// Advance an open sweep by one slice, or do nothing when none is in flight.
/// The sweep stays pinned to the day it opened on, so a mass call spread over
/// several blocks decides every group against the same finalized prices.
/// Returns the number of series force-called.
pub fn run_call_slice(ctx: &BlockRuntimeContext) -> Result<u32> {
    let factory = IntexFactoryContract::new(ctx.storage.clone());
    let pinned_day = factory.call_sweep_day.read()?;
    if pinned_day == 0 {
        return Ok(0);
    }
    let currencies = outbe_oracle::api::get_all_reference_currencies(ctx)?;
    if currencies.is_empty() {
        factory.call_sweep_day.write(0)?;
        return Ok(0);
    }
    let oracle = OracleContract::new(ctx.storage.clone());
    let start = factory.call_currency_cursor.read()? as usize % currencies.len();

    let mut budget = ScanBudget::new();
    let mut called: u32 = 0;
    let mut resume_at = start;
    let mut swept = true;
    for offset in 0..currencies.len() {
        let at = (start + offset) % currencies.len();
        if budget.is_spent() {
            // Resume here, so a heavy currency cannot starve the ones behind it.
            resume_at = at;
            swept = false;
            break;
        }
        let iso_code = currencies[at];
        // No registered pair is an answer; a failed read is not.
        let pair_index =
            match outbe_oracle::api::coen_pair_index_opt(ctx.storage.clone(), iso_code)? {
                Some(index) => index,
                None => continue,
            };
        let (calls, finished) =
            call_currency(ctx, &oracle, iso_code, pair_index, pinned_day, &mut budget)?;
        called = called.saturating_add(calls);
        swept &= finished;
    }
    factory.call_currency_cursor.write(resume_at as u32)?;
    if swept {
        // Nothing left to walk: the next daily trigger opens a fresh sweep.
        factory.call_sweep_day.write(0)?;
    }
    Ok(called)
}

/// Scans one reference currency's qualified groups, drawing on the shared
/// `budget`. Returns how many series were called and whether the currency was
/// walked to the end of its eligible range.
fn call_currency(
    ctx: &BlockRuntimeContext,
    oracle: &OracleContract,
    iso_code: u16,
    pair_index: PairIndex,
    last_closed_day: u32,
    budget: &mut ScanBudget,
) -> Result<(u32, bool)> {
    let mut factory = IntexFactoryContract::new(ctx.storage.clone());
    let params = crate::config::read(&factory)?;
    let secs_per_day = SECONDS_PER_DAY as u32;

    let mut vwaps = DayVwaps::new(pair_index);
    let Some(window) = call_window(
        oracle,
        &mut vwaps,
        last_closed_day,
        params.call_window / secs_per_day,
        params.call_threshold / secs_per_day,
    )?
    else {
        // Too few priced days for any trigger to be breached often enough.
        return Ok((0, true));
    };

    // Every trigger below `p_star` is breached often enough, so the eligible
    // range ends at its bin. Deterministic out-of-range price: skip this
    // currency instead of halting the block.
    let p_bin = match IntexFactoryContract::price_to_bin(window.p_star) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(target: "outbe::intexfactory", iso_code, error = ?e, "call scan: window price out of range, skipping currency");
            return Ok((0, true));
        }
    };

    let mut called: u32 = 0;
    let mut finished = true;
    let mut cursor: u32 = factory.call_scan_cursor.read(&iso_code)?;
    'bins: loop {
        let next = match tree_math::find_first_left_inclusive(
            &QualifiedBinTree(&factory, iso_code),
            cursor,
        )? {
            Some(b) if b <= p_bin => b,
            _ => {
                // End of the eligible range: the next run sweeps this currency afresh.
                factory.call_scan_cursor.write(&iso_code, 0)?;
                break;
            }
        };

        // Snapshot the bin before mutating: a called group leaves it.
        for worldwide_day in factory.qualified_groups_in_bin(iso_code, next)? {
            let group = factory.qualified_group(iso_code, worldwide_day)?;
            if !budget.admits(group.members.len() as u32) {
                // Stop before the group, leaving the cursor on its bin: the groups
                // already called are gone from it, so the next slice resumes here
                // without redoing them.
                factory.call_scan_cursor.write(&iso_code, next)?;
                finished = false;
                break 'bins;
            }
            budget.spend_decision();
            // Isolate per-group: a deterministic Err rolls back the group's checkpoint and is
            // skipped (logged); structural reads above keep `?` so infra errors still propagate.
            let res = ctx.storage.with_checkpoint(|| {
                try_call_group(
                    &ctx.storage,
                    &mut factory,
                    oracle,
                    &mut vwaps,
                    &group,
                    &window,
                    ctx.block.timestamp,
                )
            });
            match res {
                Ok(applied) => {
                    budget.spend_actions(applied);
                    called = called.saturating_add(applied);
                }
                Err(e) => {
                    tracing::warn!(target: "outbe::intexfactory", iso_code, worldwide_day = %worldwide_day, error = ?e, "call scan: skipping group");
                }
            }
        }

        cursor = match next.checked_add(1) {
            Some(c) if c <= MAX_BIN_ID => c,
            _ => {
                factory.call_scan_cursor.write(&iso_code, 0)?;
                break;
            }
        };
    }
    Ok((called, finished))
}

/// Cycle daily-trigger entry: opens the day's Called sweep, discarding the count.
pub fn run_daily(ctx: &BlockRuntimeContext) -> Result<()> {
    scan_and_call(ctx)?;
    Ok(())
}

/// Finalized per-day VWAPs of one oracle pair, read once per scan.
pub(crate) struct DayVwaps {
    pair_index: PairIndex,
    days: BTreeMap<u32, Option<U256>>,
}

impl DayVwaps {
    pub(crate) fn new(pair_index: PairIndex) -> Self {
        Self {
            pair_index,
            days: BTreeMap::new(),
        }
    }

    fn get(&mut self, oracle: &OracleContract, day: u32) -> Result<Option<U256>> {
        if let Some(v) = self.days.get(&day) {
            return Ok(*v);
        }
        let v = oracle.get_utc_day_vwap_for_pair(day, self.pair_index)?;
        self.days.insert(day, v);
        Ok(v)
    }
}

/// The finalized VWAP window one call scan decides against, and the price that
/// summarises it.
pub(crate) struct CallWindow {
    /// Most recent fully-closed UTC day; the window ends here.
    pub(crate) last_day: u32,
    /// Window length and required breach count, both in whole days.
    pub(crate) days: u32,
    pub(crate) threshold: u32,
    /// The `threshold`-th largest VWAP of the window. `trigger < p_star` and
    /// "breached on at least `threshold` days" are the same statement, so a
    /// whole group's decision is this one comparison.
    pub(crate) p_star: U256,
}

impl CallWindow {
    /// The window's first day.
    fn first_day(&self) -> u32 {
        let mut day = self.last_day;
        for _ in 1..self.days {
            day = previous_date_key(day);
        }
        day
    }
}

/// Reads the window's finalized VWAPs and takes its `threshold`-th largest.
/// `None` when fewer days carry a price than the threshold requires: no trigger
/// can be breached often enough, so the currency has nothing to call.
pub(crate) fn call_window(
    oracle: &OracleContract,
    vwaps: &mut DayVwaps,
    last_day: u32,
    days: u32,
    threshold: u32,
) -> Result<Option<CallWindow>> {
    if days == 0 || threshold == 0 {
        return Ok(None);
    }
    let mut priced: Vec<U256> = Vec::with_capacity(days as usize);
    let mut day = last_day;
    for _ in 0..days {
        if let Some(vwap) = vwaps.get(oracle, day)? {
            priced.push(vwap);
        }
        day = previous_date_key(day);
    }
    if (priced.len() as u32) < threshold {
        return Ok(None);
    }
    priced.sort_unstable_by(|a, b| b.cmp(a));
    Ok(Some(CallWindow {
        last_day,
        days,
        threshold,
        p_star: priced[threshold as usize - 1],
    }))
}

/// Breach-days (VWAP > trigger) inside the window, not before issuance.
fn count_breaches(
    oracle: &OracleContract,
    vwaps: &mut DayVwaps,
    last_day: u32,
    days: u32,
    issued_day: u32,
    trigger: U256,
) -> Result<u32> {
    let mut breaches: u32 = 0;
    let mut day = last_day;
    for _ in 0..days {
        if day < issued_day {
            break;
        }
        if let Some(vwap) = vwaps.get(oracle, day)? {
            if vwap > trigger {
                breaches += 1;
            }
        }
        day = previous_date_key(day);
    }
    Ok(breaches)
}

/// Force-call a whole `(reference currency, worldwide day)` group: one day's
/// series in one reference currency carry the same trigger, issue time and call
/// parameters, so one read decides all of them. Returns how many were called.
pub(crate) fn try_call_group(
    storage: &StorageHandle<'_>,
    factory: &mut IntexFactoryContract,
    oracle: &OracleContract,
    vwaps: &mut DayVwaps,
    group: &Group,
    window: &CallWindow,
    now_ts: u64,
) -> Result<u32> {
    let Some(&first) = group.members.first() else {
        return Ok(0);
    };
    let series = outbe_intex::api::read_series(storage, first)?;
    if series.lifecycle_state()? != IntexState::Qualified {
        return Ok(0);
    }
    let trigger = series.call_price_minor;
    // The scan walks finalized daily VWAPs, so both bounds floor to whole days.
    let secs_per_day = SECONDS_PER_DAY as u32;
    let group_days = series.call_window / secs_per_day;
    let group_threshold = series.call_threshold / secs_per_day;
    if group_days == 0 || group_threshold == 0 {
        return Ok(0);
    }

    let issued_day = timestamp_to_date_key(u64::from(series.issued_at));
    let breached = if issued_day <= window.first_day()
        && group_days == window.days
        && group_threshold == window.threshold
    {
        trigger < window.p_star
    } else {
        // The group sees a shorter window than the scan's — it was issued inside
        // it, or under different call parameters — so its own days are counted.
        count_breaches(
            oracle,
            vwaps,
            window.last_day,
            group_days,
            issued_day,
            trigger,
        )? >= group_threshold
    };
    if !breached {
        return Ok(0);
    }

    // u32 timestamp; bounded until 2106 (matches issued_at).
    let called_at = u32::try_from(now_ts)
        .map_err(|_| PrecompileError::Revert("block timestamp exceeds u32".into()))?;
    for &series_id in &group.members {
        outbe_intex::api::mark_called(storage, series_id, called_at)?;
    }
    factory.remove_qualified_group(group.iso_code, group.worldwide_day)?;

    for &series_id in &group.members {
        // Notify the target chain of the Called transition via ERC-7786; best-effort.
        // OriginRouter failure (e.g. exhausted relay float) does not revert the
        // state transition. The target chain can reconcile series state from the origin chain.
        let _ = notify_called(storage, series_id, group.worldwide_day.value());

        crate::runtime::emit_event(
            storage,
            crate::precompile::IIntexFactory::SeriesCalled {
                seriesId: series_id.into(),
                calledAt: called_at,
            },
        )?;
    }
    Ok(group.members.len() as u32)
}

fn notify_called(
    storage: &StorageHandle<'_>,
    series_id: SeriesId,
    worldwide_day: u32,
) -> Result<()> {
    // Relay-float-funded: value 0, so the router self-quotes and pays the bridge fee from its float.
    storage.call(
        ORIGIN_ROUTER_ADDRESS,
        U256::ZERO,
        IOriginRouter::sendMarkCalledCall {
            seriesId: series_id.into(),
            worldwideDay: worldwide_day,
        }
        .abi_encode()
        .into(),
    )?;
    Ok(())
}
