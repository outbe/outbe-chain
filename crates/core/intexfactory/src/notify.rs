//! Queue of Called notices, sent by the `intex_drain_notices` trigger.

use alloy_primitives::U256;
use outbe_intex::SeriesId;
use outbe_primitives::storage::types::Storable;
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{block::BlockRuntimeContext, error::Result, storage::StorageHandle};

use crate::constants::{
    MAX_CALLED_NOTICE_ATTEMPTS, MAX_REFUSED_RUNS_PER_FIRING, MAX_ROUTER_CALLS_PER_FIRING,
    MAX_SERIES_PER_MARK,
};
use crate::precompile::IIntexFactory::CalledNoticeDropped;
use crate::schema::IntexFactoryContract;

/// Bit offset of the refused-attempt count, in the byte above the call time.
const ATTEMPTS_SHIFT: usize = 32;

/// A Called entry packs its call time into the low bytes the 14-byte `SeriesId` leaves
/// free, so the origin's stamp reaches the target instead of its delivery time.
pub fn pack_called_notice(series_id: SeriesId, called_at: u32) -> U256 {
    series_id.to_word() | U256::from(called_at)
}

/// Router calls a queued Called entry has been refused so far.
pub fn called_notice_attempts(entry: U256) -> u8 {
    ((entry >> ATTEMPTS_SHIFT) & U256::from(u8::MAX)).to::<u8>()
}

fn with_attempts(entry: U256, attempts: u8) -> U256 {
    (entry & !(U256::from(u8::MAX) << ATTEMPTS_SHIFT)) | (U256::from(attempts) << ATTEMPTS_SHIFT)
}

/// Whether a Called entry belongs to the run a message is being built for. The wire carries
/// one day and one call time for the whole batch, so both must match for a series to ride along.
pub fn joins_run(day: WorldwideDay, called_at: u32, id: SeriesId, ts: u32) -> bool {
    ts == called_at && id.worldwide_day() == day
}

fn unpack_called_notice(entry: U256) -> (SeriesId, u32) {
    (
        SeriesId::from_word(entry),
        (entry & U256::from(u32::MAX)).to::<u32>(),
    )
}

pub(crate) fn enqueue_notice(factory: &IntexFactoryContract, entry: U256) -> Result<()> {
    let tail = factory.notify_tail.read()?;
    factory.notify_at.write(&tail, entry)?;
    factory.notify_tail.write(tail.saturating_add(1))?;
    Ok(())
}

/// Cycle-trigger entry: send the queued notices, at most
/// [`MAX_ROUTER_CALLS_PER_FIRING`] router calls' worth. This is where every
/// outbound mark leaves from. The scans that queue them run in a block hook,
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
    let mut refused_runs: u32 = 0;
    while index < stop
        && messages < MAX_ROUTER_CALLS_PER_FIRING
        && refused_runs < MAX_REFUSED_RUNS_PER_FIRING
    {
        let entry = factory.notify_at.read(&index)?;
        let calls_left = MAX_ROUTER_CALLS_PER_FIRING - messages;
        let (consumed, refused) = drain_called_run(
            &factory,
            &storage,
            index,
            stop,
            entry,
            &mut messages,
            calls_left,
        )?;
        index += consumed;
        refused_runs = if refused { refused_runs + 1 } else { 0 };
    }
    if index >= factory.notify_tail.read()? {
        factory.notify_head.write(0)?;
        factory.notify_tail.write(0)?;
    } else {
        factory.notify_head.write(index)?;
    }
    Ok(())
}

/// Send the run of Called entries starting at `at` that shares its day and call time. `stop`
/// bounds the look-ahead to this firing's entries. `notify_called` splits the run where the
/// wire's cap forces it. Returns the entries consumed and whether the router refused all of them.
fn drain_called_run(
    factory: &IntexFactoryContract,
    storage: &StorageHandle<'_>,
    at: u32,
    stop: u32,
    first: U256,
    messages: &mut u32,
    calls_left: u32,
) -> Result<(u32, bool)> {
    let (first_id, called_at) = unpack_called_notice(first);
    let worldwide_day = first_id.worldwide_day();
    let mut run = vec![first_id];
    let mut entries = vec![first];

    // The run is what one message carries, so it is cut to the calls still budgeted
    // rather than to the whole firing's window.
    let run_cap = (calls_left as usize).saturating_mul(MAX_SERIES_PER_MARK);
    let mut index = at.saturating_add(1);
    while index < stop && run.len() < run_cap {
        let entry = factory.notify_at.read(&index)?;
        let (id, ts) = unpack_called_notice(entry);
        if !joins_run(worldwide_day, called_at, id, ts) {
            break;
        }
        run.push(id);
        entries.push(entry);
        index += 1;
    }

    for slot in at..index {
        factory.notify_at.clear(&slot)?;
    }
    *messages = messages.saturating_add(router_calls(run.len()));
    let refused = crate::called::notify_called(storage, worldwide_day, called_at, &run)?;
    let all_refused = refused.len() == run.len();
    // A refused entry goes behind this firing's window, so it never wedges the drain.
    for entry in entries {
        if refused.contains(&unpack_called_notice(entry).0) {
            requeue_refused(factory, storage, entry)?;
        }
    }
    Ok((index - at, all_refused))
}

fn requeue_refused(
    factory: &IntexFactoryContract,
    storage: &StorageHandle<'_>,
    entry: U256,
) -> Result<()> {
    let attempts = called_notice_attempts(entry).saturating_add(1);
    if attempts < MAX_CALLED_NOTICE_ATTEMPTS {
        return enqueue_notice(factory, with_attempts(entry, attempts));
    }
    let (series_id, called_at) = unpack_called_notice(entry);
    tracing::warn!(
        target: "outbe::intexfactory",
        series = %series_id,
        called_at,
        attempts,
        "called notice: dropping"
    );
    crate::runtime::emit_event(
        storage,
        CalledNoticeDropped {
            seriesId: series_id.into(),
            calledAt: called_at,
        },
    )
}

/// Router calls a batch of this many series costs: the wire caps a mark at
/// [`MAX_SERIES_PER_MARK`], and each call fans out to the day's target chains.
fn router_calls(series: usize) -> u32 {
    series.div_ceil(MAX_SERIES_PER_MARK) as u32
}
