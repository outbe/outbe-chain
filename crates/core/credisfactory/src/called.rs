//! Daily price-path scan: calls and voids positions off the Oracle's finalized
//! per-UTC-day VWAPs. The Cycle daily trigger pins the closed UTC day and runs the
//! first slice. Later CycleTicks continue the same day through [`continue_sweeps`].
//!
//! One pass over the dense active-position index applies up to two transitions
//! per position, in lifecycle order:
//!
//! - `Open -> Called` when the COEN price in the position's REFERENCE currency sat
//!   strictly above the call price on `call_threshold_seconds` of the trailing
//!   `call_window_seconds`. Both terms are sealed onto the position at opening. The
//!   issuance currency the position is denominated in never enters the threshold.
//! - `Called -> Void` when the settlement window has lapsed with principal still
//!   outstanding.
//!
//! The breach rule needs no per-position streak state. The daily series is
//! global per currency, so one trailing window per reference currency decides
//! every position anchored to it. Every run recomputes the count from oracle
//! history and does not carry it. Mirrors the Gem, Intex and Nod call sweeps.

use alloy_primitives::U256;
use alloy_sol_types::SolEvent;

use outbe_credis::constants::{CALL_WINDOW, SECS_PER_DAY};
use outbe_credis::{CredisContract, CredisState, Position};
use outbe_oracle::schema::OracleContract;
use outbe_primitives::{
    addresses::CREDIS_FACTORY_ADDRESS,
    block::BlockRuntimeContext,
    daily_sweep::{PinnedDay, Scheduled, SweepDays},
    error::{Result, SweepFailure},
    storage::StorageHandle,
    time::{first_full_day, previous_date_key},
};

use crate::precompile::ICredisFactory::SweepDaySkipped;
use crate::runtime;
use crate::schema::CredisFactoryContract;

/// Max positions one block's slice visits. The rest of the pass continues on the
/// next block, pinned to the same day.
pub(crate) const MAX_CREDIS_CALL_VISITS_PER_BLOCK: u32 = 4096;

/// Max positions voided per slice. A void is far more expensive than a call: it
/// makes a blocking TEE round-trip to burn aggregate Credis collateral.
pub(crate) const MAX_CREDIS_VOIDS_PER_RUN: u32 = 64;

/// `SweepDaySkipped.sweep` for the call sweep.
const CALL_SWEEP: u8 = 1;

/// Trailing finalized daily VWAPs of one `COEN/<iso>` pair, newest first.
/// `None` marks a day the pair published no reference price.
type VwapWindow = Vec<(u32, Option<U256>)>;

/// Cycle daily-trigger entry: runs the scan, discarding the count.
pub fn run_daily(ctx: &BlockRuntimeContext) -> Result<()> {
    scan_and_call(ctx)?;
    Ok(())
}

/// Runs from CycleTick every block, before the daily trigger can queue a newer day.
pub fn continue_sweeps(ctx: &BlockRuntimeContext) -> Result<()> {
    run_call_slice(ctx)?;
    Ok(())
}

/// Schedules the day the Oracle has just finalized: opens a sweep over it and runs
/// its first slice, or queues it behind the sweep still in flight.
///
/// Never returns `Err` for missing market data: a handler error fails the block.
pub fn scan_and_call(ctx: &BlockRuntimeContext) -> Result<u32> {
    let Some(last_closed_day) = outbe_oracle::closed_day::finalized_closed_day(
        ctx.storage.clone(),
        ctx.block.timestamp,
        "credis",
    )?
    else {
        return Ok(0);
    };
    let factory = CredisFactoryContract::new(ctx.storage.clone());
    if factory.call_sweep_day.read()? == 0
        && CredisContract::new(ctx.storage.clone()).active_len()? == 0
    {
        return Ok(0);
    }
    let scheduled = pinned_call_day(&factory).schedule(last_closed_day)?;
    match scheduled {
        (next, Scheduled::Opened) => {
            start_call_sweep(&factory, next)?;
            run_call_slice(ctx)
        }
        (next, Scheduled::Replaced { skipped }) => {
            ctx.storage.emit_event(
                CREDIS_FACTORY_ADDRESS,
                SolEvent::encode_log_data(&SweepDaySkipped {
                    sweep: CALL_SWEEP,
                    skippedDay: skipped,
                    inFlightDay: next.current,
                }),
            )?;
            Ok(0)
        }
        (_, Scheduled::Queued | Scheduled::Ignored) => Ok(0),
    }
}

fn pinned_call_day<'a, 'storage>(
    factory: &'a CredisFactoryContract<'storage>,
) -> PinnedDay<'a, 'storage> {
    PinnedDay {
        current: &factory.call_sweep_day,
        pending: &factory.call_pending_day,
    }
}

/// Pins the sweep's day and walks the active index from the top.
fn start_call_sweep(factory: &CredisFactoryContract, days: SweepDays) -> Result<()> {
    pinned_call_day(factory).open(days)?;
    factory.call_scan_cursor.write(0)
}

/// Advances an open sweep by one slice, pinned to the day it opened on so later
/// blocks decide against the same prices. Returns the number of positions mutated.
pub fn run_call_slice(ctx: &BlockRuntimeContext) -> Result<u32> {
    let factory = CredisFactoryContract::new(ctx.storage.clone());
    let pinned_day = factory.call_sweep_day.read()?;
    if pinned_day == 0 {
        return Ok(0);
    }
    let oracle = OracleContract::new(ctx.storage.clone());
    let finalized = oracle.utc_day_vwap_last_finalized.read()?;
    if finalized < pinned_day {
        tracing::warn!(
            target: "outbe::credisfactory",
            pinned_day,
            finalized,
            "credis scan: pinned utc-day VWAP not finalized, holding the sweep"
        );
        return Ok(0);
    }
    let mut slice = CallSlice {
        ctx,
        credis: CredisContract::new(ctx.storage.clone()),
        windows: VwapWindows {
            storage: ctx.storage.clone(),
            oracle: &oracle,
            last_closed_day: pinned_day,
            cache: Vec::new(),
        },
        mutated: 0,
        voided: 0,
    };
    if slice.walk(&factory)? {
        // The next day starts on the next block, so no slice mixes two days' prices.
        if let Some(next) = pinned_call_day(&factory).finish(pinned_day)? {
            start_call_sweep(&factory, next)?;
        }
    }
    Ok(slice.mutated)
}

/// One block's slice of the pass.
struct CallSlice<'a, 'storage> {
    ctx: &'a BlockRuntimeContext<'storage>,
    credis: CredisContract<'storage>,
    windows: VwapWindows<'a, 'storage>,
    mutated: u32,
    voided: u32,
}

impl CallSlice<'_, '_> {
    /// Walks the active index down from the cursor. Returns whether the pass ended.
    ///
    /// Descending walk: `remove_active` swap-pops the tail into the hole, and the
    /// tail is already behind a descending cursor, so the walk skips no live entry.
    fn walk(&mut self, factory: &CredisFactoryContract) -> Result<bool> {
        let len = self.credis.active_len()?;
        if len == 0 {
            factory.call_scan_cursor.write(0)?;
            return Ok(true);
        }
        // Stored as `index + 1`. 0 means "start a fresh pass from the top".
        let mut cursor = match factory.call_scan_cursor.read()? {
            0 => len - 1,
            resume => resume.saturating_sub(1).min(len - 1),
        };
        let mut visited: u32 = 0;
        loop {
            if visited >= MAX_CREDIS_CALL_VISITS_PER_BLOCK || !self.visit(cursor)? {
                factory.call_scan_cursor.write(cursor.saturating_add(1))?;
                return Ok(false);
            }
            visited = visited.saturating_add(1);
            if cursor == 0 {
                factory.call_scan_cursor.write(0)?;
                return Ok(true);
            }
            cursor -= 1;
        }
    }

    /// Visits the position at `index`. Returns false when the slice is out of gas
    /// and must resume at this position.
    fn visit(&mut self, index: u32) -> Result<bool> {
        let Some(position_id) = self.credis.active_at(index)? else {
            return Ok(true);
        };
        // Structural reads stay on `?` so infra errors still propagate.
        let position = self.credis.get_position(position_id)?;
        let window = self
            .windows
            .window(&self.credis, position.reference_currency)?;
        let now = self.ctx.block.timestamp;
        let credis = &mut self.credis;
        // The price-path arms are pure storage and arithmetic. A deterministic error is
        // isolated to this position. A node-local error fails the block.
        let outcome = self
            .ctx
            .storage
            .with_checkpoint(|| visit_price_path(credis, window, &position, now));
        match outcome {
            Ok(visit) => self.apply(position_id, visit)?,
            Err(error) => match error.sweep_failure() {
                SweepFailure::Propagate => return Err(error),
                SweepFailure::Stop => return Ok(false),
                SweepFailure::Skip => tracing::warn!(
                    target: "outbe::credisfactory",
                    %position_id,
                    error = ?error,
                    "credis scan: skipping position"
                ),
            },
        }
        Ok(true)
    }

    /// Runs the void the visit found due, within the slice's void budget.
    ///
    /// The void is not isolated: its TEE round-trip fails on node-local faults, and
    /// swallowing one would fork the chain silently. A declined void keeps the
    /// position in the active index, and a later slice voids it.
    fn apply(&mut self, position_id: U256, visit: Visit) -> Result<()> {
        let did_void = visit.void_due && self.voided < MAX_CREDIS_VOIDS_PER_RUN;
        if did_void {
            runtime::void_position(self.ctx.storage.clone(), position_id)?;
            self.voided = self.voided.saturating_add(1);
        }
        if visit.moved || did_void {
            self.mutated = self.mutated.saturating_add(1);
        }
        Ok(())
    }
}

/// What one position's price-path arms decided.
struct Visit {
    /// Whether the call actually transitioned it.
    moved: bool,
    /// Whether the caller must now void the remainder.
    void_due: bool,
}

/// Applies the call, and reports whether the void is due.
///
/// A reference currency this chain cannot price yields an empty window: no call, but
/// the void arm still runs, so such a position is never stranded.
fn visit_price_path(
    credis: &mut CredisContract<'_>,
    window: &[(u32, Option<U256>)],
    position: &Position,
    now: u64,
) -> Result<Visit> {
    let entry_state = position.lifecycle_state()?;
    let moved = entry_state == CredisState::Open
        && breached_enough(window, position)
        && credis.mark_called(position.position_id, now)?;

    Ok(Visit {
        moved,
        // Gated on the state at entry, not the running one. A call stamped in
        // this same visit sets `called_at = now`, so its window cannot have
        // lapsed. Also, a read of the deadline off the record loaded before that
        // call would compare `now` against `0 + call_notice_period_seconds`.
        void_due: entry_state == CredisState::Called
            && !position.outstanding_principal_minor.is_zero()
            && now > outbe_credis::settlement_deadline(position),
    })
}

/// True when the daily COEN price in the position's reference currency sat
/// strictly above its call price on at least `call_threshold_seconds` days of its
/// trailing `call_window_seconds`.
///
/// This function reads both terms off the position, not from the constants, so
/// retuning them cannot re-term a position that is already live. `window` is
/// sized for the widest window in the currency, so this takes only its own prefix.
///
/// Days at or below the call price and days with no published price both simply
/// fail to count, so the window absorbs up to `window - threshold` of either.
/// Section 11.3 leaves missing-data days undecided. Treating them as
/// non-breaches is conservative: it can only delay a call, never trigger one.
///
/// A day before the position's first full UTC day ends the count. The window is
/// newest-first, so every remaining entry is older still. A position never counts
/// a day it did not exist for in full. Mirrors `outbe_gem::runtime::breached_enough`.
fn breached_enough(window: &[(u32, Option<U256>)], position: &Position) -> bool {
    let window_days = position.call_window_seconds / SECS_PER_DAY;
    let threshold_days = position.call_threshold_seconds / SECS_PER_DAY;
    // A position sealed before the terms existed carries zeroes. Zero days is
    // "no terms", not "every day breaches". Leave it uncallable. Same guard as
    // `outbe_gem::runtime::breached_enough`.
    if window_days == 0 || threshold_days == 0 {
        return false;
    }
    let first_day = first_full_day(position.issued_at);
    let mut breaches: u32 = 0;
    for (day, vwap) in window.iter().take(window_days as usize) {
        if *day < first_day {
            break;
        }
        if vwap.is_some_and(|value| value > position.call_price_minor) {
            breaches = breaches.saturating_add(1);
        }
    }
    breaches >= threshold_days
}

/// The trailing finalized-VWAP windows one slice reads, at most one per currency.
struct VwapWindows<'a, 'storage> {
    storage: StorageHandle<'storage>,
    oracle: &'a OracleContract<'storage>,
    last_closed_day: u32,
    cache: Vec<(u16, VwapWindow)>,
}

impl VwapWindows<'_, '_> {
    /// The window for `COEN/<iso>`, newest first, filled on first use. It reads only
    /// the currencies actually present in the active book.
    ///
    /// An unregistered pair caches an empty window: such a position can never
    /// register a breach, but it must still reach the void arm.
    fn window(
        &mut self,
        credis: &CredisContract<'_>,
        iso_code: u16,
    ) -> Result<&[(u32, Option<U256>)]> {
        let index = match self.cache.iter().position(|(code, _)| *code == iso_code) {
            Some(index) => index,
            None => {
                let window = self.load(credis, iso_code)?;
                self.cache.push((iso_code, window));
                self.cache.len() - 1
            }
        };
        Ok(&self.cache[index].1)
    }

    fn load(&self, credis: &CredisContract<'_>, iso_code: u16) -> Result<VwapWindow> {
        let Some(pair_index) =
            outbe_oracle::api::coen_pair_index_opt(self.storage.clone(), iso_code)?
        else {
            return Ok(Vec::new());
        };
        // Widest of the current constant and anything ever opened: a position
        // keeps the window it was opened with, so a narrowed constant must not
        // shorten the span the scan collects for it.
        let window_days = credis
            .max_call_window_seconds
            .read(&iso_code)?
            .max(CALL_WINDOW)
            / SECS_PER_DAY;
        let mut window = Vec::with_capacity(window_days as usize);
        let mut day = self.last_closed_day;
        for _ in 0..window_days {
            window.push((day, self.oracle.get_utc_day_vwap_for_pair(day, pair_index)?));
            day = previous_date_key(day);
        }
        Ok(window)
    }
}
