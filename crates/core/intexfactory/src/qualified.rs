//! Block-hook carry-on for the Intex sweeps, and the queue of Called notices the
//! `intex_drain_notices` trigger sends: the scans run in a block hook, which cannot
//! call contracts.

use alloy_primitives::U256;
use outbe_intex::SeriesId;
use outbe_primitives::storage::types::Storable;
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{
    block::{BlockLifecycle, BlockRuntimeContext},
    error::Result,
    storage::StorageHandle,
};

use crate::constants::{MAX_ROUTER_CALLS_PER_FIRING, MAX_SERIES_PER_MARK};
use crate::schema::IntexFactoryContract;

pub struct IntexLifecycle;

impl BlockLifecycle for IntexLifecycle {
    type Context<'a, 'storage> = BlockRuntimeContext<'storage>;
    type EndBlockResult = ();

    fn begin_block(ctx: &BlockRuntimeContext) -> Result<()> {
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

/// A notice carrying one Called series, which its group no longer holds. It is the only kind
/// sent; an entry of any other kind is dropped.
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

/// Router calls a batch of this many series costs: the wire caps a mark at
/// [`MAX_SERIES_PER_MARK`], and each call fans out to the day's target chains.
fn router_calls(series: usize) -> u32 {
    series.div_ceil(MAX_SERIES_PER_MARK) as u32
}
