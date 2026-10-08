//! Queue of Called notices, sent every block after the call slice. A refused notice
//! waits [`NOTICE_RETRY_SECONDS`] before its next attempt.

use alloy_primitives::U256;
use outbe_intex::SeriesId;
use outbe_primitives::storage::types::Storable;
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{
    block::BlockRuntimeContext, error::Result, storage::StorageHandle, sweep_budget::SweepBudget,
};

use crate::constants::{
    MAX_CALLED_NOTICE_ATTEMPTS, MAX_REFUSED_RUNS_PER_BLOCK, MAX_SERIES_PER_MARK,
    NOTICE_RETRY_SECONDS,
};
use crate::precompile::IIntexFactory::CalledNoticeDropped;
use crate::schema::IntexFactoryContract;

/// Bit offset of the refused-attempt count, in the byte above the call time.
const ATTEMPTS_SHIFT: usize = 32;
/// Bit offset of the time a refused entry may go out again, above the attempt count.
const RETRY_AT_SHIFT: usize = 40;

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

/// When a queued entry may be sent. 0 for a fresh one.
pub fn notice_retry_at(entry: U256) -> u64 {
    ((entry >> RETRY_AT_SHIFT) & U256::from(u64::MAX)).to::<u64>()
}

pub fn with_retry_at(entry: U256, retry_at: u64) -> U256 {
    (entry & !(U256::from(u64::MAX) << RETRY_AT_SHIFT)) | (U256::from(retry_at) << RETRY_AT_SHIFT)
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

/// Sends the queued notices, one write of the sweep budget per router call.
/// The walk stops at the first entry not yet due: a refused one waits its pause, and the
/// queue behind it waits no longer than one pause.
pub fn send_notices(ctx: &BlockRuntimeContext) -> Result<()> {
    let factory = IntexFactoryContract::new(ctx.storage.clone());
    let head = factory.notify_head.read()?;
    let tail = factory.notify_tail.read()?;
    if head >= tail {
        return Ok(());
    }
    let mut send = NoticeSend {
        factory: &factory,
        storage: &ctx.storage,
        now: ctx.block.timestamp,
        stop: tail,
        budget: SweepBudget::per_block(),
    };
    let mut index = head;
    let mut refused_runs: u32 = 0;
    while index < tail
        && !send.budget.spent()
        && refused_runs < MAX_REFUSED_RUNS_PER_BLOCK
        && notice_retry_at(factory.notify_at.read(&index)?) <= send.now
    {
        let (consumed, refused) = send.run(index)?;
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

/// One block's send: the queue window it walks and the router calls left.
struct NoticeSend<'a, 'storage> {
    factory: &'a IntexFactoryContract<'storage>,
    storage: &'a StorageHandle<'storage>,
    now: u64,
    /// End of this block's window. Entries requeued behind it wait for a later block.
    stop: u32,
    budget: SweepBudget,
}

impl NoticeSend<'_, '_> {
    /// Send the run of Called entries starting at `at` that shares its day and call time.
    /// `notify_called` splits the run where the wire's cap forces it. Returns the entries
    /// consumed and whether the router refused all of them.
    fn run(&mut self, at: u32) -> Result<(u32, bool)> {
        let factory = self.factory;
        let first = factory.notify_at.read(&at)?;
        let calls_left = self.budget.writes_left();
        let (first_id, called_at) = unpack_called_notice(first);
        let worldwide_day = first_id.worldwide_day();
        let mut run = vec![first_id];
        let mut entries = vec![first];

        // The run is what one message carries, so it is cut to the calls still budgeted
        // rather than to the whole block's window.
        let run_cap = (calls_left as usize).saturating_mul(MAX_SERIES_PER_MARK);
        let mut index = at.saturating_add(1);
        while index < self.stop && run.len() < run_cap {
            let entry = factory.notify_at.read(&index)?;
            let (id, ts) = unpack_called_notice(entry);
            if !joins_run(worldwide_day, called_at, id, ts) || notice_retry_at(entry) > self.now {
                break;
            }
            run.push(id);
            entries.push(entry);
            index += 1;
        }

        for slot in at..index {
            factory.notify_at.clear(&slot)?;
        }
        self.budget.admit_writes(router_calls(run.len()));
        let refused = crate::called::notify_called(self.storage, worldwide_day, called_at, &run)?;
        let all_refused = refused.len() == run.len();
        // A refused entry goes behind this block's window, so it never wedges the queue.
        for entry in entries {
            if refused.contains(&unpack_called_notice(entry).0) {
                self.requeue_refused(entry)?;
            }
        }
        Ok((index - at, all_refused))
    }

    fn requeue_refused(&self, entry: U256) -> Result<()> {
        let attempts = called_notice_attempts(entry).saturating_add(1);
        if attempts < MAX_CALLED_NOTICE_ATTEMPTS {
            let retry_at = self.now.saturating_add(NOTICE_RETRY_SECONDS);
            return enqueue_notice(
                self.factory,
                with_retry_at(with_attempts(entry, attempts), retry_at),
            );
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
            self.storage,
            CalledNoticeDropped {
                seriesId: series_id.into(),
                calledAt: called_at,
            },
        )
    }
}

/// Router calls a batch of this many series costs: the wire caps a mark at
/// [`MAX_SERIES_PER_MARK`], and each call fans out to the day's target chains.
fn router_calls(series: usize) -> u32 {
    series.div_ceil(MAX_SERIES_PER_MARK) as u32
}
