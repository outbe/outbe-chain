//! Daily qualification: an Issued group qualifies when the finalized VWAP of a closed
//! UTC day it held in full stands above its floor. The daily trigger opens the sweep
//! and `begin_block` carries it on, each currency down its own trie with its own cursor.

use alloy_primitives::U256;
use alloy_sol_types::SolCall;
use outbe_intex::SeriesId;
use outbe_oracle::api::{coen_pair_index_opt, get_all_reference_currencies, get_utc_day_vwap};
use outbe_primitives::daily_sweep::{Scheduled, SweepDays};
use outbe_primitives::storage::types::Storable;
use outbe_primitives::time::{first_full_day, WorldwideDay};
use outbe_primitives::{
    block::{BlockLifecycle, BlockRuntimeContext},
    error::Result,
    math::{constants::MAX_BIN_ID, tree_math},
    storage::StorageHandle,
};

use outbe_intex::IntexState;

use crate::constants::{
    MAX_GROUP_DECISIONS_PER_BLOCK, MAX_ROUTER_CALLS_PER_FIRING, MAX_SERIES_ACTIONS_PER_BLOCK,
    MAX_SERIES_PER_MARK, ORIGIN_ROUTER_ADDRESS, QUALIFY_SWEEP,
};
use crate::precompile::IIntexFactory;
use crate::schema::IntexFactoryContract;
use crate::sol_ext::IOriginRouter;
use crate::state::{Group, UnqualifiedBinTree};

pub struct IntexLifecycle;

impl BlockLifecycle for IntexLifecycle {
    type Context<'a, 'storage> = BlockRuntimeContext<'storage>;
    type EndBlockResult = ();

    fn begin_block(ctx: &BlockRuntimeContext) -> Result<()> {
        run_qualify_slice(ctx)?;
        // A call sweep the daily trigger could not finish in one go carries on
        // here, block by block, rather than waiting a day for the next trigger.
        crate::called::run_call_slice(ctx)?;
        // Drain in-flight payouts first, then start rounds for any series whose
        // proceeds fan-in deadline has passed.
        crate::runtime::drain_distributions(&ctx.storage)?;
        crate::runtime::sweep_proceeds_deadlines(&ctx.storage, ctx.block.timestamp)?;
        crate::expired::sweep_expiry_deadlines(ctx)?;
        Ok(())
    }

    fn end_block(_ctx: &BlockRuntimeContext) -> Result<Self::EndBlockResult> {
        Ok(())
    }
}

/// Schedule the day the Oracle has just finalized: open a qualification sweep over it
/// and run its first slice, or queue it behind the sweep still in flight.
pub fn scan_and_qualify(ctx: &BlockRuntimeContext) -> Result<u32> {
    let Some(day) = crate::called::closed_day(ctx)? else {
        return Ok(0);
    };
    let factory = IntexFactoryContract::new(ctx.storage.clone());
    let days = SweepDays {
        current: factory.qualify_sweep_day.read()?,
        pending: factory.qualify_pending_day.read()?,
    };
    match days.schedule(day) {
        (next, Scheduled::Opened) => {
            start_qualify_sweep(ctx, &factory, next)?;
            run_qualify_slice(ctx)
        }
        (next, Scheduled::Queued) => {
            factory.qualify_pending_day.write(next.pending)?;
            Ok(0)
        }
        (next, Scheduled::Replaced { skipped }) => {
            factory.qualify_pending_day.write(next.pending)?;
            crate::runtime::emit_event(
                &ctx.storage,
                IIntexFactory::SweepDaySkipped {
                    sweep: QUALIFY_SWEEP,
                    skippedDay: skipped,
                    inFlightDay: next.current,
                },
            )?;
            Ok(0)
        }
        (_, Scheduled::Ignored) => Ok(0),
    }
}

/// Pin the sweep's current day and walk it from the first currency's lowest bin.
fn start_qualify_sweep(
    ctx: &BlockRuntimeContext,
    factory: &IntexFactoryContract,
    days: SweepDays,
) -> Result<()> {
    factory.qualify_sweep_day.write(days.current)?;
    factory.qualify_pending_day.write(days.pending)?;
    factory.qualify_currency_cursor.write(0)?;
    for iso_code in get_all_reference_currencies(ctx)? {
        factory.qualify_scan_cursor.write(&iso_code, 0)?;
    }
    Ok(())
}

/// Advance an open qualification sweep by one slice, pinned to its day. Returns how
/// many series were promoted.
pub fn run_qualify_slice(ctx: &BlockRuntimeContext) -> Result<u32> {
    let factory = IntexFactoryContract::new(ctx.storage.clone());
    let pinned_day = factory.qualify_sweep_day.read()?;
    if pinned_day == 0 {
        return Ok(0);
    }
    let currencies = get_all_reference_currencies(ctx)?;
    let start = currency_position(&currencies, factory.qualify_currency_cursor.read()?);

    let mut budget = ScanBudget::for_qualify();
    let mut promoted: u32 = 0;
    // One pass down the list, as in the Called sweep, so every sweep ends.
    for &iso_code in currencies.iter().skip(start) {
        let finished = if budget.is_spent() {
            false
        } else {
            match day_price(ctx, iso_code, pinned_day)? {
                // No pair or no trade that day: nothing to decide by.
                None => true,
                Some(vwap) => {
                    let (moved, finished) =
                        qualify_currency(ctx, iso_code, vwap, pinned_day, &mut budget)?;
                    promoted = promoted.saturating_add(moved);
                    finished
                }
            }
        };
        if !finished {
            factory.qualify_currency_cursor.write(u32::from(iso_code))?;
            return Ok(promoted);
        }
    }

    // The next day starts on the next block, so no slice mixes two days' prices.
    let next = SweepDays {
        current: pinned_day,
        pending: factory.qualify_pending_day.read()?,
    }
    .finish();
    if next.current == 0 {
        factory.qualify_sweep_day.write(0)?;
    } else {
        start_qualify_sweep(ctx, &factory, next)?;
    }
    Ok(promoted)
}

fn day_price(ctx: &BlockRuntimeContext, iso_code: u16, day: u32) -> Result<Option<U256>> {
    match coen_pair_index_opt(ctx.storage.clone(), iso_code)? {
        Some(index) => get_utc_day_vwap(ctx.storage.clone(), day, index),
        None => Ok(None),
    }
}

/// Index of the currency the cursor names, or the head when the registry dropped it.
pub(crate) fn currency_position(currencies: &[u16], cursor: u32) -> usize {
    u16::try_from(cursor)
        .ok()
        .and_then(|iso| currencies.iter().position(|&code| code == iso))
        .unwrap_or(0)
}

/// Qualifies one reference currency's groups against its `vwap` of `day`, drawing on
/// the shared `budget`. Returns how many series were promoted and whether its eligible
/// range was walked to the end.
fn qualify_currency(
    ctx: &BlockRuntimeContext,
    iso_code: u16,
    vwap: U256,
    day: u32,
    budget: &mut ScanBudget,
) -> Result<(u32, bool)> {
    // Deterministic out-of-range price: settle the currency for the day instead of halting the block.
    let r_bin = match IntexFactoryContract::price_to_bin(vwap) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(target: "outbe::intexfactory", iso_code, error = ?e, "qualify scan: day price out of range, skipping currency for the day");
            crate::runtime::emit_event(
                &ctx.storage,
                IIntexFactory::QualifyScanSkipped {
                    referenceCurrency: iso_code,
                    utcDay: day,
                },
            )?;
            return Ok((0, true));
        }
    };
    let mut factory = IntexFactoryContract::new(ctx.storage.clone());

    let mut promoted: u32 = 0;
    let mut cursor: u32 = factory.qualify_scan_cursor.read(&iso_code)?;
    loop {
        if budget.is_spent() {
            // Between bins, so the next slice resumes at a bin it has not opened.
            factory.qualify_scan_cursor.write(&iso_code, cursor)?;
            return Ok((promoted, false));
        }
        let next = match tree_math::find_first_left_inclusive(
            &UnqualifiedBinTree(&factory, iso_code),
            cursor,
        )? {
            Some(b) if b <= r_bin => b,
            _ => {
                factory.qualify_scan_cursor.write(&iso_code, 0)?;
                return Ok((promoted, true));
            }
        };

        // Snapshot the bin before mutating: a qualified group leaves it.
        for worldwide_day in factory.unqualified_groups_in_bin(iso_code, next)? {
            let group = factory.unqualified_group(iso_code, worldwide_day)?;
            if !budget.admits_actions(group.members.len() as u32) {
                // Qualified groups have left this bin, so resuming on it redoes nothing.
                factory.qualify_scan_cursor.write(&iso_code, next)?;
                return Ok((promoted, false));
            }
            budget.spend_decision();
            // Per-group isolation: a deterministic Err rolls back and is logged, so one bad
            // group cannot halt the block; the structural reads above keep `?`.
            let res = ctx.storage.with_checkpoint(|| {
                try_qualify_group(&ctx.storage, &mut factory, &group, vwap, day)
            });
            match res {
                Ok(applied) => {
                    budget.spend_actions(applied);
                    promoted = promoted.saturating_add(applied);
                }
                Err(e) => {
                    tracing::warn!(target: "outbe::intexfactory", iso_code, worldwide_day = %worldwide_day, error = ?e, "qualify scan: skipping group");
                }
            }
        }

        cursor = match next.checked_add(1) {
            Some(c) if c <= MAX_BIN_ID => c,
            _ => {
                factory.qualify_scan_cursor.write(&iso_code, 0)?;
                return Ok((promoted, true));
            }
        };
    }
}

/// Work one scan may do, split by cost: deciding a group is a single read,
/// applying it writes once per series and sends its notice.
pub(crate) struct ScanBudget {
    decisions: u32,
    actions: u32,
    actions_full: u32,
}

impl ScanBudget {
    pub(crate) fn for_qualify() -> Self {
        Self::new(MAX_SERIES_ACTIONS_PER_BLOCK)
    }

    fn new(actions: u32) -> Self {
        Self {
            decisions: MAX_GROUP_DECISIONS_PER_BLOCK,
            actions,
            actions_full: actions,
        }
    }

    pub(crate) fn is_spent(&self) -> bool {
        self.decisions == 0 || self.actions == 0
    }

    /// Whole groups only. A transition shrinks its bin, so stopping on actions
    /// resumes past the work done; stopping on decisions would restart on the
    /// same groups, so they bound the scan at the next bin boundary instead.
    pub(crate) fn admits_actions(&self, members: u32) -> bool {
        members <= self.actions || self.actions == self.actions_full
    }

    pub(crate) fn spend_decision(&mut self) {
        self.decisions = self.decisions.saturating_sub(1);
    }

    pub(crate) fn spend_actions(&mut self, series: u32) {
        self.actions = self.actions.saturating_sub(series);
    }
}

/// Qualify a whole group on `day`'s `vwap`: one clearing issued the day with one floor,
/// so a single read decides every member. Returns how many were promoted.
pub(crate) fn try_qualify_group(
    storage: &StorageHandle<'_>,
    factory: &mut IntexFactoryContract,
    group: &Group,
    vwap: U256,
    day: u32,
) -> Result<u32> {
    let Some(&first) = group.members.first() else {
        return Ok(0);
    };
    let series = outbe_intex::api::read_series(storage, first)?;
    if series.lifecycle_state()? != IntexState::Issued {
        return Ok(0);
    }
    if day < first_full_day(u64::from(series.issued_at)) || vwap <= series.floor_price_minor {
        return Ok(0);
    }

    for &series_id in &group.members {
        outbe_intex::api::mark_qualified(storage, series_id)?;
    }
    factory.remove_unqualified_group(group.iso_code, group.worldwide_day)?;
    factory.insert_qualified_group(
        group.iso_code,
        group.worldwide_day,
        series.call_price_minor,
        &group.members,
    )?;

    // A slice of this sweep runs in a block hook, which cannot call contracts: the
    // notice leaves from the `intex_notify` cycle trigger instead.
    enqueue_notice(
        factory,
        NOTICE_QUALIFIED,
        U256::from(IntexFactoryContract::scoped(
            group.iso_code,
            group.worldwide_day.value(),
        )),
    )?;

    for &series_id in &group.members {
        crate::runtime::emit_event(
            storage,
            crate::precompile::IIntexFactory::SeriesQualified {
                seriesId: series_id.into(),
            },
        )?;
    }
    Ok(group.members.len() as u32)
}

/// A notice carrying a whole Qualified group, read back from the index it sits in.
pub const NOTICE_QUALIFIED: u8 = 0;
/// A notice carrying one Called series, which its group no longer holds.
pub const NOTICE_CALLED: u8 = 1;

/// A Called entry packs its call time into the low bytes the 14-byte `SeriesId` leaves
/// free, so the origin's stamp reaches the target instead of its delivery time.
pub fn pack_called_notice(series_id: SeriesId, called_at: u32) -> U256 {
    series_id.to_word() | U256::from(called_at)
}

/// Whether a Called entry belongs to the run a message is being built for. The wire carries one day and
/// one call time for the whole batch, so both must match for a series to ride along.
pub fn joins_run(day: WorldwideDay, called_at: u32, id: SeriesId, ts: u32) -> bool {
    ts == called_at && id.worldwide_day() == day
}

fn unpack_called_notice(entry: U256) -> (SeriesId, u32) {
    (
        SeriesId::from_word(entry),
        (entry & U256::from(u32::MAX)).to::<u32>(),
    )
}

pub(crate) fn enqueue_notice(
    factory: &mut IntexFactoryContract,
    kind: u8,
    entry: U256,
) -> Result<()> {
    let tail = factory.notify_tail.read()?;
    factory.notify_at.write(&tail, entry)?;
    factory.notify_kind.write(&tail, kind)?;
    factory.notify_tail.write(tail.saturating_add(1))?;
    Ok(())
}

/// Cycle-trigger entry: send the queued notices, at most
/// [`MAX_ROUTER_CALLS_PER_FIRING`] router calls' worth. This is where every
/// outbound mark leaves from - the scans that queue them run in a block hook,
/// which cannot call contracts.
pub fn drain_notices(ctx: &BlockRuntimeContext) -> Result<()> {
    let storage = ctx.storage.clone();
    let factory = IntexFactoryContract::new(storage.clone());
    let head = factory.notify_head.read()?;
    let tail = factory.notify_tail.read()?;
    if head >= tail {
        return Ok(());
    }
    let stop = tail;
    let mut index = head;
    let mut messages: u32 = 0;
    while index < stop && messages < MAX_ROUTER_CALLS_PER_FIRING {
        let kind = factory.notify_kind.read(&index)?;
        let entry = factory.notify_at.read(&index)?;
        let calls_left = MAX_ROUTER_CALLS_PER_FIRING - messages;
        let consumed = if kind == NOTICE_CALLED {
            drain_called_run(
                &factory,
                &storage,
                index,
                stop,
                entry,
                &mut messages,
                calls_left,
            )?
        } else {
            factory.notify_at.clear(&index)?;
            factory.notify_kind.clear(&index)?;
            messages = messages.saturating_add(1);
            // Best-effort: a notice that cannot be sent is dropped, never left to wedge the drain.
            if let Err(error) =
                storage.with_checkpoint(|| send_notice(&storage, kind, entry, &mut messages))
            {
                tracing::warn!(target: "outbe::intexfactory", kind, error = ?error, "notice: dropping");
            }
            1
        };
        index += consumed;
    }
    if index >= tail {
        factory.notify_head.write(0)?;
        factory.notify_tail.write(0)?;
    } else {
        factory.notify_head.write(index)?;
    }
    Ok(())
}

/// Send the run of Called entries starting at `at` that shares its day and call time. `stop` bounds the
/// look-ahead to this firing's entries; `notify_called` splits the run where the wire's cap forces it.
fn drain_called_run(
    factory: &IntexFactoryContract,
    storage: &StorageHandle<'_>,
    at: u32,
    stop: u32,
    first: U256,
    messages: &mut u32,
    calls_left: u32,
) -> Result<u32> {
    let (first_id, called_at) = unpack_called_notice(first);
    // A target refuses a zero stamp and its refusal is acknowledged, not retried, so such a mark would
    // be lost silently. Only an entry written by an older binary carries one; drop it where it shows.
    if called_at == 0 {
        factory.notify_at.clear(&at)?;
        factory.notify_kind.clear(&at)?;
        *messages = messages.saturating_add(1);
        tracing::warn!(
            target: "outbe::intexfactory",
            series = %first_id,
            "called notice: dropping, no call time"
        );
        return Ok(1);
    }
    let worldwide_day = first_id.worldwide_day();
    let mut run = vec![first_id];

    // The run is what one message carries, so it is cut to the calls still budgeted
    // rather than to the whole firing's window.
    let run_cap = (calls_left as usize).saturating_mul(MAX_SERIES_PER_MARK);
    let mut index = at.saturating_add(1);
    while index < stop && run.len() < run_cap {
        if factory.notify_kind.read(&index)? != NOTICE_CALLED {
            break;
        }
        let (id, ts) = unpack_called_notice(factory.notify_at.read(&index)?);
        if !joins_run(worldwide_day, called_at, id, ts) {
            break;
        }
        run.push(id);
        index += 1;
    }

    for slot in at..index {
        factory.notify_at.clear(&slot)?;
        factory.notify_kind.clear(&slot)?;
    }
    *messages = messages.saturating_add(router_calls(run.len()));
    // Best-effort: a batch that cannot be sent is dropped, never left to wedge the
    // drain and with it the whole cycle trigger.
    if let Err(error) = storage
        .with_checkpoint(|| crate::called::notify_called(storage, worldwide_day, called_at, &run))
    {
        tracing::warn!(
            target: "outbe::intexfactory",
            worldwide_day = worldwide_day.value(),
            called_at,
            series = ?run.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
            error = ?error,
            "called notice: dropping"
        );
    }
    Ok(index - at)
}

/// Send one Qualified notice. Called entries never reach here - the drain routes them through
/// [`drain_called_run`] so a whole group leaves as one message.
fn send_notice(
    storage: &StorageHandle<'_>,
    kind: u8,
    entry: U256,
    messages: &mut u32,
) -> Result<()> {
    // Only the Qualified shape is readable here; anything else is a scoped key this cannot decode,
    // and narrowing it would panic rather than revert.
    if kind != NOTICE_QUALIFIED {
        return Ok(());
    }
    let Ok(scoped) = u64::try_from(entry) else {
        return Ok(());
    };
    // A group that has since been called is gone from the index, and a Called
    // series would refuse the Qualified mark anyway - so an empty read is the
    // answer, not an error.
    let (iso_code, worldwide_day) = IntexFactoryContract::unscoped(scoped);
    let factory = IntexFactoryContract::new(storage.clone());
    let members = factory.qualified_group_members(iso_code, worldwide_day)?;
    if members.is_empty() {
        return Ok(());
    }
    // The drain charged the first call on the way in. A group wider than the budget still
    // goes whole: it has no cursor to resume from, so the overshoot is one group at most.
    *messages = messages.saturating_add(router_calls(members.len()).saturating_sub(1));
    notify_qualified(storage, worldwide_day, &members)
}

/// Router calls a batch of this many series costs: the wire caps a mark at
/// [`MAX_SERIES_PER_MARK`], and each call fans out to the day's target chains.
fn router_calls(series: usize) -> u32 {
    series.div_ceil(MAX_SERIES_PER_MARK) as u32
}

/// One message per group, split only where the wire's cap forces it.
fn notify_qualified(
    storage: &StorageHandle<'_>,
    worldwide_day: WorldwideDay,
    members: &[SeriesId],
) -> Result<()> {
    for chunk in members.chunks(MAX_SERIES_PER_MARK) {
        // Best-effort: one checkpoint per message, so a failure takes only its own batch.
        let sent = storage.with_checkpoint(|| {
            // Relay-float-funded: value 0, so the router self-quotes and pays the bridge fee from its float.
            storage.call(
                ORIGIN_ROUTER_ADDRESS,
                U256::ZERO,
                IOriginRouter::sendMarkQualifiedCall {
                    worldwideDay: worldwide_day.value(),
                    seriesIds: chunk.iter().map(|id| (*id).into()).collect(),
                }
                .abi_encode()
                .into(),
            )?;
            Ok(())
        });
        if let Err(error) = sent {
            tracing::warn!(
                target: "outbe::intexfactory",
                worldwide_day = worldwide_day.value(),
                series = ?chunk.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
                error = ?error,
                "qualified notice: dropping"
            );
        }
    }
    Ok(())
}
