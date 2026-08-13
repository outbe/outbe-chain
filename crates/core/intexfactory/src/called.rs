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
use outbe_primitives::storage::types::Storable;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result},
    math::{constants::MAX_BIN_ID, tree_math},
    storage::StorageHandle,
    time::{previous_date_key, timestamp_to_date_key, SECONDS_PER_DAY},
};

use outbe_intex::IntexState;

use crate::constants::ORIGIN_ROUTER_ADDRESS;
use crate::qualified::MAX_SERIES_PER_BLOCK;
use crate::schema::IntexFactoryContract;
use crate::sol_ext::IOriginRouter;
use crate::state::QualifiedBinTree;

/// Run the daily Called scan. Returns the number of series force-called.
///
/// A call trigger is only comparable to the VWAP of its own reference currency,
/// so each currency is scanned against its own pair, in oracle registry order,
/// sharing one [`MAX_SERIES_PER_BLOCK`] budget. A currency without a registered
/// or finalized pair is skipped for the run rather than halting it.
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

    let currencies = outbe_oracle::api::get_all_reference_currencies(ctx)?;
    if currencies.is_empty() {
        return Ok(0);
    }
    let factory = IntexFactoryContract::new(ctx.storage.clone());
    let start = factory.call_currency_cursor.read()? as usize % currencies.len();

    let mut budget = MAX_SERIES_PER_BLOCK;
    let mut called: u32 = 0;
    let mut resume_at = start;
    for offset in 0..currencies.len() {
        let at = (start + offset) % currencies.len();
        if budget == 0 {
            // Out of budget: the next run starts here, so a heavy currency
            // cannot keep the ones behind it from ever being scanned.
            resume_at = at;
            break;
        }
        let iso_code = currencies[at];
        // A currency with no registered COEN pair has nothing to compare against; a real
        // read failure is not that, and must not be mistaken for it.
        let pair_index =
            match outbe_oracle::api::coen_pair_index_opt(ctx.storage.clone(), iso_code)? {
                Some(index) => index,
                None => continue,
            };
        let (calls, inspected) =
            call_currency(ctx, &oracle, iso_code, pair_index, last_closed_day, budget)?;
        called = called.saturating_add(calls);
        budget = budget.saturating_sub(inspected);
    }
    factory.call_currency_cursor.write(resume_at as u32)?;
    Ok(called)
}

/// Scans one reference currency's qualified series, visiting at most `budget` of
/// them. Returns `(called, visited)` so the caller can share one run budget.
fn call_currency(
    ctx: &BlockRuntimeContext,
    oracle: &OracleContract,
    iso_code: u16,
    pair_index: PairIndex,
    last_closed_day: u32,
    budget: u32,
) -> Result<(u32, u32)> {
    let last_closed_vwap = match oracle.get_utc_day_vwap_for_pair(last_closed_day, pair_index)? {
        Some(v) if !v.is_zero() => v,
        _ => return Ok((0, 0)),
    };

    // Deterministic out-of-range VWAP: skip this currency instead of halting the block.
    let v_bin = match IntexFactoryContract::price_to_bin(last_closed_vwap) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(target: "outbe::intexfactory", iso_code, error = ?e, "call scan: vwap out of range, skipping currency");
            return Ok((0, 0));
        }
    };
    let mut factory = IntexFactoryContract::new(ctx.storage.clone());

    let mut vwaps = DayVwaps::new(pair_index);
    vwaps.seed(last_closed_day, Some(last_closed_vwap));

    let mut called: u32 = 0;
    let mut visited: u32 = 0;
    let mut cursor: u32 = factory.call_scan_cursor.read(&iso_code)?;
    loop {
        if visited >= budget {
            factory.call_scan_cursor.write(&iso_code, cursor)?;
            break;
        }
        let next = match tree_math::find_first_left_inclusive(
            &QualifiedBinTree(&factory, iso_code),
            cursor,
        )? {
            Some(b) if b <= v_bin => b,
            _ => {
                // End of the eligible range: the next run sweeps this currency afresh.
                factory.call_scan_cursor.write(&iso_code, 0)?;
                break;
            }
        };

        // Snapshot before mutating: try_call removes Called series.
        let count = factory
            .qualified_bin_count
            .read(&IntexFactoryContract::scoped(iso_code, next))?;
        let mut series: Vec<SeriesId> = Vec::with_capacity(count as usize);
        for i in 0..count {
            series.push(SeriesId::from_word(
                factory
                    .qualified_bin_series
                    .read(&IntexFactoryContract::bin_index_key(iso_code, next, i))?,
            ));
        }
        for series_id in series {
            // Isolate per-series: a deterministic Err rolls back this series' checkpoint and is
            // skipped (logged); structural reads above keep `?` so infra errors still propagate.
            let res = ctx.storage.with_checkpoint(|| {
                try_call(
                    &ctx.storage,
                    &mut factory,
                    oracle,
                    &mut vwaps,
                    series_id,
                    last_closed_day,
                    ctx.block.timestamp,
                )
            });
            match res {
                Ok(true) => called = called.saturating_add(1),
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!(target: "outbe::intexfactory", series_id = %series_id, error = ?e, "call scan: skipping series");
                }
            }
        }
        visited = visited.saturating_add(count);

        cursor = match next.checked_add(1) {
            Some(c) if c <= MAX_BIN_ID => c,
            _ => {
                factory.call_scan_cursor.write(&iso_code, 0)?;
                break;
            }
        };
    }
    Ok((called, visited))
}

/// Cycle daily-trigger entry: runs the Called scan, discarding the count.
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

    pub(crate) fn seed(&mut self, day: u32, vwap: Option<U256>) {
        self.days.insert(day, vwap);
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

/// Force-call one series if Qualified and its VWAP breached the call trigger on
/// at least `call_threshold` of the last `call_window` completed days.
pub(crate) fn try_call(
    storage: &StorageHandle<'_>,
    factory: &mut IntexFactoryContract,
    oracle: &OracleContract,
    vwaps: &mut DayVwaps,
    series_id: SeriesId,
    last_closed_day: u32,
    now_ts: u64,
) -> Result<bool> {
    let series = outbe_intex::api::read_series(storage, series_id)?;
    if series.lifecycle_state()? != IntexState::Qualified {
        return Ok(false);
    }
    let trigger = series.call_price_minor;
    // The scan walks finalized daily VWAPs, so both bounds floor to whole days.
    let secs_per_day = SECONDS_PER_DAY as u32;
    let window = series.call_window / secs_per_day;
    let threshold = series.call_threshold / secs_per_day;
    if window == 0 || threshold == 0 {
        return Ok(false);
    }

    // Breach-days (VWAP > trigger) within the window, not before issuance.
    let issued_day = timestamp_to_date_key(u64::from(series.issued_at));
    let mut breaches: u32 = 0;
    let mut day = last_closed_day;
    for _ in 0..window {
        if day < issued_day {
            break;
        }
        if let Some(v) = vwaps.get(oracle, day)? {
            if v > trigger {
                breaches += 1;
            }
        }
        day = previous_date_key(day);
    }
    if breaches < threshold {
        return Ok(false);
    }

    // u32 timestamp; bounded until 2106 (matches issued_at).
    let called_at = u32::try_from(now_ts)
        .map_err(|_| PrecompileError::Revert("block timestamp exceeds u32".into()))?;
    outbe_intex::api::mark_called(storage, series_id, called_at)?;
    factory.remove_qualified(series_id, series.reference_currency, trigger)?;

    // Notify the target chain of the Called transition via ERC-7786; best-effort.
    // OriginRouter failure (e.g. exhausted relay float) does not revert the
    // state transition. The target chain can reconcile series state from the origin chain.
    let _ = notify_called(storage, series_id, series.worldwide_day.value());

    crate::runtime::emit_event(
        storage,
        crate::precompile::IIntexFactory::SeriesCalled {
            seriesId: series_id.into(),
            calledAt: called_at,
        },
    )?;
    Ok(true)
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
