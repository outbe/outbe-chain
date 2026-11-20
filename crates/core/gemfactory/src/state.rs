//! GemFactory position bookkeeping: creation (record and expiry-day list), expiry and
//! deletion. A position is a record with an owner, not a token.

use alloy_primitives::U256;
use outbe_core_common::deadline_queue;
use outbe_primitives::error::Result;
use outbe_primitives::storage::indexed_list::IndexedList;

use crate::errors::GemFactoryError;
use crate::schema::{GemFactoryContract, GemFactoryPosition};

/// Zero denotes an empty queue slot, so an expiry day is queued as `day + 1`.
pub(crate) fn position_queue_entry(day: u32) -> u64 {
    u64::from(day) + 1
}

pub(crate) fn position_queue_day(entry: u64) -> Result<u32> {
    entry
        .checked_sub(1)
        .and_then(|day| u32::try_from(day).ok())
        .ok_or_else(|| GemFactoryError::IndexOutOfBounds.into())
}

impl<'s> GemFactoryContract<'s> {
    fn day_index(&self, day: u32) -> IndexedList<'s, U256> {
        IndexedList::new(
            self.day_positions.list(&day),
            self.day_position_index.clone(),
        )
    }

    /// Creates the position record, lists it under its expiry day, and queues the day
    /// on its first position. Rejects a duplicate `gem_factory_position_id`.
    pub(crate) fn add_position(&mut self, pos: &GemFactoryPosition) -> Result<()> {
        if self.positions.exists(pos.gem_factory_position_id)? {
            return Err(GemFactoryError::GemFactoryPositionAlreadyExists.into());
        }
        self.positions.create(pos)?;
        let day = pos.expiry_day()?;
        if self.day_position_count(day)? == 0 {
            deadline_queue::push(
                &PositionExpiryQueue(self),
                position_queue_entry(day),
                pos.expires_at,
            )?;
        }
        self.day_index(day).insert(pos.gem_factory_position_id)?;
        let capacity = self
            .day_capacity_minor
            .read(&day)?
            .checked_add(pos.remaining_capacity_minor)
            .ok_or(GemFactoryError::Overflow)?;
        self.day_capacity_minor.write(&day, capacity)
    }

    /// Records a merchant issue drawing `load` from the position's expiry day.
    pub(crate) fn take_day_capacity(&mut self, day: u32, load: U256) -> Result<()> {
        let capacity = self
            .day_capacity_minor
            .read(&day)?
            .checked_sub(load)
            .ok_or(GemFactoryError::InsufficientCapacity)?;
        self.day_capacity_minor.write(&day, capacity)
    }

    pub(crate) fn day_position_count(&self, day: u32) -> Result<u32> {
        self.day_positions.list(&day).len()
    }

    pub(crate) fn day_position(&self, day: u32, index: u32) -> Result<U256> {
        self.day_positions
            .list(&day)
            .get(index)?
            .ok_or_else(|| GemFactoryError::IndexOutOfBounds.into())
    }

    /// Moves an expired day from the expiry queue to the deletion, which deletes its
    /// positions. Done with the day's credit, so a retry never credits it twice.
    pub(crate) fn hand_day_to_deletion(&mut self, day: u32) -> Result<()> {
        let entry = position_queue_entry(day);
        let deadline = self.expiry_deadline.read(&entry)?;
        deadline_queue::remove(&PositionExpiryQueue(self), entry)?;
        deadline_queue::push(&PositionCleanupQueue(self), entry, deadline)
    }

    /// Deletes an expired position, and forgets its day with the last one.
    pub(crate) fn delete_position(&mut self, record: &GemFactoryPosition) -> Result<()> {
        let day = record.expiry_day()?;
        self.day_index(day).remove(record.gem_factory_position_id)?;
        self.positions.delete(record.gem_factory_position_id)?;
        if self.day_position_count(day)? == 0 {
            self.day_capacity_minor.clear(&day)?;
            let entry = position_queue_entry(day);
            deadline_queue::remove(&PositionExpiryQueue(self), entry)?;
            deadline_queue::remove(&PositionCleanupQueue(self), entry)?;
        }
        Ok(())
    }

    /// What the position can still issue: nothing once it expired, whether or not the
    /// cleanup has deleted it yet.
    pub(crate) fn remaining_capacity(&self, pos: &GemFactoryPosition) -> Result<U256> {
        let now = self.storage.now_secs()?;
        Ok(match now >= pos.expires_at {
            true => U256::ZERO,
            false => pos.remaining_capacity_minor,
        })
    }
}

/// Positions' expiry days, each queued as one entry.
pub struct PositionExpiryQueue<'a, 'storage>(pub &'a GemFactoryContract<'storage>);

outbe_core_common::impl_deadline_queue!(PositionExpiryQueue<u64> {
    root: expiry_tree_root,
    mid: expiry_tree_mid,
    leaf: expiry_tree_leaf,
    len: expiry_day_len,
    live: expiry_day_live,
    at: expiry_day_at,
    slot: expiry_entry_slot,
    deadline: expiry_deadline,
    sweep_day: expiry_sweep_day,
    cursor: expiry_cursor,
});

/// Expired days whose positions wait to be deleted.
pub struct PositionCleanupQueue<'a, 'storage>(pub &'a GemFactoryContract<'storage>);

outbe_core_common::impl_deadline_queue!(PositionCleanupQueue<u64> {
    root: cleanup_tree_root,
    mid: cleanup_tree_mid,
    leaf: cleanup_tree_leaf,
    len: cleanup_day_len,
    live: cleanup_day_live,
    at: cleanup_day_at,
    slot: cleanup_entry_slot,
    deadline: cleanup_deadline,
    sweep_day: cleanup_sweep_day,
    cursor: cleanup_cursor,
});
