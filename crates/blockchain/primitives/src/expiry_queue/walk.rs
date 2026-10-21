//! Bounded queue traversal and checkpointed member expiry.

use super::{
    day_end, deadline_day, entry_at, first_day, pack_cursor, remove, retarget, unpack_cursor, Due,
    ExpiryHandler, ExpiryQueueStore, Progress,
};
use crate::error::{decide, Decided, PrecompileError, Result};
use crate::math::constants::MAX_BIN_ID;
use crate::storage::StorageHandle;
use crate::sweep_budget::SweepBudget;

/// Actual block time and the pinned scheduling cutoff. Catch-up must not use the
/// cutoff as the clock for retry scheduling or expand it as block time advances.
#[derive(Debug, Clone, Copy)]
pub struct SweepTime {
    pub now: u64,
    pub through_day: u32,
}

/// Sweeps closed scheduling days at or before the cutoff, earliest first.
/// Every visited slot (including a hole or a leftover) costs a visit; every
/// attempted member costs a write.
pub fn sweep_through<'s, Q, H>(
    queue: &Q,
    storage: &StorageHandle<'s>,
    time: SweepTime,
    budget: &mut SweepBudget,
    handler: &mut H,
) -> Result<Progress>
where
    Q: ExpiryQueueStore<'s>,
    H: ExpiryHandler<Q::Entry>,
{
    if time.through_day > MAX_BIN_ID {
        return Err(PrecompileError::Revert(
            "expiry cutoff exceeds 24-bit range".into(),
        ));
    }
    let mut walk = Walk {
        queue,
        storage,
        now: time.now,
        budget,
        handler,
    };
    loop {
        let Some(day) = first_day(queue)? else {
            return Ok(Progress::Complete);
        };
        if day > time.through_day {
            return Ok(Progress::Complete);
        }
        if time.now < day_end(day) || walk.budget.visits_left() == 0 || !walk.day(day)? {
            return Ok(Progress::Pending);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Done,
    /// Resume at this slot, below this member.
    Hold(u32),
}

struct Walk<'w, 's, Q, H> {
    queue: &'w Q,
    storage: &'w StorageHandle<'s>,
    now: u64,
    budget: &'w mut SweepBudget,
    handler: &'w mut H,
}

impl<'s, Q, H> Walk<'_, 's, Q, H>
where
    Q: ExpiryQueueStore<'s>,
    H: ExpiryHandler<Q::Entry>,
{
    fn day(&mut self, day: u32) -> Result<bool> {
        let len = self.queue.day_len().read(&day)?;
        let (slot, member) = self.walk_slots(day, len)?;
        if self.queue.day_live().read(&day)? == 0 {
            return Ok(true);
        }
        if slot < len {
            self.queue.sweep_day().write(day)?;
            self.queue.sweep_cursor().write(pack_cursor(slot, member))?;
            return Ok(false);
        }
        // Every live slot is removed or retargeted during its charged visit.
        Err(PrecompileError::Fatal(
            "expiry day retains entries after its last slot".into(),
        ))
    }

    fn walk_slots(&mut self, day: u32, len: u32) -> Result<(u32, u32)> {
        let (mut slot, mut member) = match self.queue.sweep_day().read()? == day {
            true => unpack_cursor(self.queue.sweep_cursor().read()?),
            false => (0, 0),
        };
        while slot < len && self.budget.visit() {
            if let Some(entry) = entry_at(self.queue, day, slot)? {
                let step = if self.now > self.queue.entry_deadline().read(&entry)? {
                    self.expire(entry, member)?
                } else {
                    self.retire_leftover(entry)?;
                    Step::Done
                };
                if let Step::Hold(held) = step {
                    return Ok((slot, held));
                }
                if self.queue.day_live().read(&day)? == 0 {
                    return Ok((len, 0));
                }
            }
            // slot < len <= u32::MAX, so this increment cannot overflow.
            slot += 1;
            member = 0;
        }
        Ok((slot, 0))
    }

    fn expire(&mut self, entry: Q::Entry, resume: u32) -> Result<Step> {
        match self.handler.due(entry)? {
            Due::Expire => {}
            Due::Drop => {
                self.drop_entry(entry)?;
                return Ok(Step::Done);
            }
            Due::Wait => {
                self.defer(entry)?;
                return Ok(Step::Done);
            }
        }
        // Reverse traversal keeps swap-removed members already tried above index.
        let count = self.handler.member_count(entry)?;
        let mut index = match resume {
            0 => count,
            held => held.min(count),
        };
        let mut failed = count - index;
        while index > 0 {
            if !self.budget.write() {
                return Ok(Step::Hold(index));
            }
            index -= 1;
            let member = self.handler.member_at(entry, index)?;
            let handler = &mut *self.handler;
            let outcome = self
                .storage
                .with_checkpoint(|| handler.expire_member(entry, member));
            match decide(outcome, |error| self.handler.classify(error))? {
                Decided::Done(()) => {}
                Decided::Stopped => {
                    self.budget.stop();
                    return Ok(Step::Hold(index + 1));
                }
                Decided::Skipped(error) => {
                    tracing::warn!(target: "outbe::expiry", error = ?error, "expiry sweep: member left for a later day");
                    failed += 1;
                }
            }
        }
        match failed {
            0 => self.drop_entry(entry)?,
            _ => self.defer(entry)?,
        }
        Ok(Step::Done)
    }

    /// Handles a misplaced future deadline in the same charged traversal.
    fn retire_leftover(&mut self, entry: Q::Entry) -> Result<()> {
        match self.handler.due(entry)? {
            Due::Drop => self.drop_entry(entry),
            Due::Expire | Due::Wait => self.defer(entry),
        }
    }

    fn drop_entry(&mut self, entry: Q::Entry) -> Result<()> {
        let queue = self.queue;
        self.storage.with_checkpoint(|| remove(queue, entry))
    }

    /// Retry at the closing timestamp of the actual current UTC day, never before
    /// the deadline day. Historical catch-up must not backdate the retry.
    fn defer(&mut self, entry: Q::Entry) -> Result<()> {
        let deadline = self.queue.entry_deadline().read(&entry)?;
        let day = deadline_day(deadline)?.max(deadline_day(self.now)?);
        let (queue, handler) = (self.queue, &mut *self.handler);
        self.storage.with_checkpoint(|| {
            retarget(queue, entry, day)?;
            handler.deferred(entry, day_end(day))
        })
    }
}
