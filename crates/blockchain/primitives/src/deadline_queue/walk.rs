//! This module traverses the queue within its budget and expires members within checkpoints.

use super::{
    day_end, deadline_day, entry_at, first_day, pack_cursor, remove, retarget, unpack_cursor, Due,
    DeadlineHandler, DeadlineQueueStore, Progress,
};
use crate::error::{decide, Decided, PrecompileError, Result};
use crate::math::constants::MAX_BIN_ID;
use crate::storage::StorageHandle;
use crate::pass_budget::PassBudget;

/// This structure stores the actual block time and the selected cutoff day for scheduling.
/// Processing missed days must not use the cutoff as the clock for retries.
/// The cutoff must not change as block time advances.
#[derive(Debug, Clone, Copy)]
pub struct PassTime {
    pub now: u64,
    pub through_day: u32,
}

/// This function processes closed scheduling days at or before the cutoff, earliest first.
/// Each slot costs one visit, including an empty slot or a remaining entry.
/// Each attempted member costs one write.
pub fn process_through<'s, Q, H>(
    queue: &Q,
    storage: &StorageHandle<'s>,
    time: PassTime,
    budget: &mut PassBudget,
    handler: &mut H,
) -> Result<Progress>
where
    Q: DeadlineQueueStore<'s>,
    H: DeadlineHandler<Q::Entry>,
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
    /// The pass resumes at this slot, below the stored member index.
    Hold(u32),
}

struct Walk<'w, 's, Q, H> {
    queue: &'w Q,
    storage: &'w StorageHandle<'s>,
    now: u64,
    budget: &'w mut PassBudget,
    handler: &'w mut H,
}

impl<'s, Q, H> Walk<'_, 's, Q, H>
where
    Q: DeadlineQueueStore<'s>,
    H: DeadlineHandler<Q::Entry>,
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
        // Each charged visit removes its entry or moves the entry to another day.
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
            Due::Process => {}
            Due::Drop => {
                self.drop_entry(entry)?;
                return Ok(Step::Done);
            }
            Due::Wait => {
                self.defer(entry)?;
                return Ok(Step::Done);
            }
        }
        // Descending traversal keeps previously attempted members above the current index when removal swaps members.
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
                .with_checkpoint(|| handler.process_member(entry, member));
            match decide(outcome, |error| self.handler.classify(error))? {
                Decided::Done(()) => {}
                Decided::Stopped => {
                    self.budget.stop();
                    return Ok(Step::Hold(index + 1));
                }
                Decided::Skipped(error) => {
                    tracing::warn!(target: "outbe::expiry", error = ?error, "deadline pass: member left for a later day");
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

    /// This method handles an entry with a misplaced future deadline within the same charged visit.
    fn retire_leftover(&mut self, entry: Q::Entry) -> Result<()> {
        match self.handler.due(entry)? {
            Due::Drop => self.drop_entry(entry),
            Due::Process | Due::Wait => self.defer(entry),
        }
    }

    fn drop_entry(&mut self, entry: Q::Entry) -> Result<()> {
        let queue = self.queue;
        self.storage.with_checkpoint(|| remove(queue, entry))
    }

    /// This method schedules the retry when the actual current UTC day closes, but never before the deadline day closes.
    /// Processing missed days must not schedule the retry in the past.
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
