//! Daily price-path scan: latches, calls and voids positions off the Oracle's
//! finalized per-UTC-day VWAPs. Driven by the Cycle daily trigger.
//!
//! One pass over the dense active-position index applies up to three transitions
//! per position, in lifecycle order:
//!
//! - `Open -> Settleable` when the last closed day's reference price exceeded the
//!   position's floor. One-way, per §4.
//! - `Settleable -> Called` when the reference price sat at or above the call
//!   price for [`CALL_STREAK_DAYS`] consecutive days.
//! - `Called -> Void` when the settlement window has lapsed with principal still
//!   outstanding.
//!
//! The consecutive-day rule needs no per-position streak state: "the last 21
//! daily reference prices were all at or above `C`" is exactly
//! `min(vwap[d-20 ..= d]) >= C`. The series is global per currency, so one
//! rolling minimum per currency decides every position denominated in it.

use std::collections::BTreeMap;

use alloy_primitives::U256;
use outbe_credis::constants::CALL_STREAK_DAYS;
use outbe_credis::{CredisContract, CredisState};
use outbe_oracle::schema::OracleContract;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::Result,
    storage::StorageHandle,
    time::{previous_date_key, timestamp_to_date_key},
};

use crate::runtime;
use crate::schema::CredisFactoryContract;

/// Max positions visited per daily run; the cursor resumes the rest on the next
/// run so one scan can never outgrow a block. An entry displaced past the cursor
/// is picked up a day later, which cannot change an outcome: the call is a
/// 21-day sustained event and the void follows a 14-day window.
pub(crate) const MAX_CREDIS_DAILY_VISITS: u32 = 4096;

/// Cycle daily-trigger entry: runs the scan, discarding the count.
pub fn run_daily(ctx: &BlockRuntimeContext) -> Result<()> {
    scan_and_call(ctx)?;
    Ok(())
}

/// Runs the daily price-path scan. Returns the number of positions mutated.
///
/// Total by construction — it never returns `Err` for missing market data. The
/// Cycle dispatcher propagates a handler error out of the `CycleTick` system
/// transaction, which would fail the block, so an unregistered pair, an unpriced
/// currency or an unfinalized day each degrade to "no transition" instead.
pub fn scan_and_call(ctx: &BlockRuntimeContext) -> Result<u32> {
    let oracle = OracleContract::new(ctx.storage.clone());

    // Most recent fully-closed UTC day. The paper counts plain UTC days, so this
    // is `timestamp_to_date_key`, NOT the UTC+14 `WorldwideDay` key.
    let last_closed_day = previous_date_key(timestamp_to_date_key(ctx.block.timestamp));

    // The Oracle begin-block hook finalizes that day earlier in this same block;
    // a lagging watermark means the ordering broke — skip loudly instead of
    // misreading an unfinalized day as a missing one and resetting every streak.
    let finalized = oracle.utc_day_vwap_last_finalized.read()?;
    if finalized < last_closed_day {
        tracing::warn!(
            target: "outbe::credisfactory",
            last_closed_day,
            finalized,
            "credis scan: utc-day VWAP not finalized yet, skipping run"
        );
        return Ok(0);
    }

    let credis = CredisContract::new(ctx.storage.clone());
    let len = credis.active_len()?;
    if len == 0 {
        return Ok(0);
    }

    let factory = CredisFactoryContract::new(ctx.storage.clone());
    // Stored as `index + 1`; 0 means "start a fresh pass from the top".
    let mut cursor = match factory.call_scan_cursor.read()? {
        0 => len - 1,
        resume => resume.saturating_sub(1).min(len - 1),
    };

    let mut windows = CurrencyWindows::new(last_closed_day);
    let now = ctx.block.timestamp;
    let mut mutated: u32 = 0;
    let mut visited: u32 = 0;

    // Descending walk: `remove_active` swap-pops the tail into the hole, and the
    // tail is already behind a descending cursor, so no live entry is skipped.
    let completed = loop {
        if visited >= MAX_CREDIS_DAILY_VISITS {
            break false;
        }
        if let Some(position_id) = credis.active_at(cursor)? {
            // Isolate per position: a deterministic error rolls back that
            // position's checkpoint and is logged, so one bad position can never
            // halt the daily run. Structural reads above keep `?` so infra
            // errors still propagate.
            let res = ctx
                .storage
                .with_checkpoint(|| visit(ctx, &oracle, &mut windows, position_id, now));
            match res {
                Ok(true) => mutated = mutated.saturating_add(1),
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!(
                        target: "outbe::credisfactory",
                        %position_id,
                        error = ?e,
                        "credis scan: skipping position"
                    );
                }
            }
        }
        visited = visited.saturating_add(1);
        if cursor == 0 {
            break true;
        }
        cursor -= 1;
    };

    // `cursor` is the next index to visit when the budget cut the pass short.
    factory.call_scan_cursor.write(if completed {
        0
    } else {
        cursor.saturating_add(1)
    })?;
    Ok(mutated)
}

/// Applies whichever transitions are due for one position. Returns whether it
/// moved.
fn visit(
    ctx: &BlockRuntimeContext,
    oracle: &OracleContract,
    windows: &mut CurrencyWindows,
    position_id: U256,
    now: u64,
) -> Result<bool> {
    let mut credis = CredisContract::new(ctx.storage.clone());
    let position = credis.get_position(position_id)?;
    let entry_state = position.lifecycle_state()?;
    let window = windows.get(ctx.storage.clone(), oracle, position.issuance_currency)?;
    let mut state = entry_state;
    let mut moved = false;

    // A currency this chain cannot price yields no latch and no call; the void
    // arm needs no price and still runs.
    if state == CredisState::Open
        && window
            .last_closed_vwap
            .is_some_and(|vwap| vwap > position.floor_price)
        && credis.mark_settleable(position_id)?
    {
        state = CredisState::Settleable;
        moved = true;
    }

    // A position cannot own a streak that predates it. Mirrors the issuance
    // guard in gem's and intexfactory's call scans.
    if state == CredisState::Settleable
        && window
            .streak_min
            .is_some_and(|min| min >= position.call_price)
        && timestamp_to_date_key(position.originated_at) <= window.oldest_day
        && credis.mark_called(position_id, now)?
    {
        moved = true;
    }

    // Gated on the state at entry, not the running one: a call stamped in this
    // same visit sets `called_at = now`, and reading the deadline off the record
    // loaded before that would compare `now` against `0 + CALL_WINDOW_SECS`.
    if entry_state == CredisState::Called
        && !position.outstanding.is_zero()
        && now >= outbe_credis::settlement_deadline(&position)
    {
        runtime::void_remainder(ctx.storage.clone(), position_id)?;
        moved = true;
    }

    Ok(moved)
}

/// One currency's view of the finalized daily series, read once per run.
#[derive(Clone, Copy)]
struct CurrencyWindow {
    /// VWAP of the most recent closed day; drives the floor latch.
    last_closed_vwap: Option<U256>,
    /// Minimum across the whole streak window, or `None` when any day in it has
    /// no published reference price.
    streak_min: Option<U256>,
    /// Earliest day the window reached.
    oldest_day: u32,
}

/// Lazily-built per-currency windows. Only the currencies actually present in
/// the active book are read — there is no registry walk.
struct CurrencyWindows {
    last_closed_day: u32,
    by_iso: BTreeMap<u16, CurrencyWindow>,
}

impl CurrencyWindows {
    fn new(last_closed_day: u32) -> Self {
        Self {
            last_closed_day,
            by_iso: BTreeMap::new(),
        }
    }

    fn get(
        &mut self,
        storage: StorageHandle<'_>,
        oracle: &OracleContract,
        iso: u16,
    ) -> Result<CurrencyWindow> {
        if let Some(window) = self.by_iso.get(&iso) {
            return Ok(*window);
        }
        let window = self.build(storage, oracle, iso)?;
        self.by_iso.insert(iso, window);
        Ok(window)
    }

    fn build(
        &self,
        storage: StorageHandle<'_>,
        oracle: &OracleContract,
        iso: u16,
    ) -> Result<CurrencyWindow> {
        let mut oldest_day = self.last_closed_day;
        let Some(pair_index) = outbe_oracle::api::coen_pair_index_opt(storage, iso)? else {
            return Ok(CurrencyWindow {
                last_closed_vwap: None,
                streak_min: None,
                oldest_day,
            });
        };

        let mut last_closed_vwap = None;
        let mut streak_min: Option<U256> = None;
        let mut day = self.last_closed_day;
        for i in 0..CALL_STREAK_DAYS {
            let vwap = oracle.get_utc_day_vwap_for_pair(day, pair_index)?;
            if i == 0 {
                last_closed_vwap = vwap;
            }
            oldest_day = day;
            let Some(value) = vwap else {
                // ponytail: §11.3 leaves missing-data days undecided; a missing
                // day resets the streak here. Conservative — it can only delay a
                // call, never trigger one spuriously. To make such a day "pause"
                // the run instead, skip it rather than collapsing the minimum.
                streak_min = None;
                break;
            };
            streak_min = Some(streak_min.map_or(value, |min: U256| min.min(value)));
            day = previous_date_key(day);
        }

        Ok(CurrencyWindow {
            last_closed_vwap,
            streak_min,
            oldest_day,
        })
    }
}
