//! GemFactory position bookkeeping: creation (record and expiry-hour list), expiry and
//! deletion. A position is a record with an owner, not a token.

use alloy_primitives::U256;
use outbe_core_common::deadline_queue;
use outbe_primitives::error::Result;
use outbe_primitives::storage::indexed_list::IndexedList;

use crate::errors::GemFactoryError;
use crate::schema::{GemFactoryContract, GemFactoryPosition};

/// Zero denotes an empty queue slot. Hour zero is a valid position group.
pub(crate) fn position_queue_entry(hour: u64) -> Result<u64> {
    hour.checked_add(1)
        .ok_or_else(|| GemFactoryError::Overflow.into())
}

pub(crate) fn position_queue_hour(entry: u64) -> Result<u64> {
    entry
        .checked_sub(1)
        .ok_or_else(|| GemFactoryError::IndexOutOfBounds.into())
}

impl<'s> GemFactoryContract<'s> {
    fn hour_index(&self, hour: u64) -> IndexedList<'s, U256> {
        IndexedList::new(
            self.hour_positions.list(&hour),
            self.hour_position_index.clone(),
        )
    }

    /// Creates the position record, lists it under its expiry hour, and queues the hour
    /// on its first position. Rejects a duplicate `gem_factory_position_id`.
    pub(crate) fn add_position(&mut self, pos: &GemFactoryPosition) -> Result<()> {
        if self.positions.exists(pos.gem_factory_position_id)? {
            return Err(GemFactoryError::GemFactoryPositionAlreadyExists.into());
        }
        self.positions.create(pos)?;
        let hour = pos.expiry_hour();
        if self.hour_position_count(hour)? == 0 {
            deadline_queue::push(
                &PositionExpiryQueue(self),
                position_queue_entry(hour)?,
                pos.expires_at,
            )?;
        }
        self.hour_index(hour).insert(pos.gem_factory_position_id)?;
        let capacity = self
            .hour_capacity_minor
            .read(&hour)?
            .checked_add(pos.remaining_capacity_minor)
            .ok_or(GemFactoryError::Overflow)?;
        self.hour_capacity_minor.write(&hour, capacity)
    }

    /// Records a merchant issue drawing `load` from the position's hour.
    pub(crate) fn take_hour_capacity(&mut self, hour: u64, load: U256) -> Result<()> {
        let capacity = self
            .hour_capacity_minor
            .read(&hour)?
            .checked_sub(load)
            .ok_or(GemFactoryError::InsufficientCapacity)?;
        self.hour_capacity_minor.write(&hour, capacity)
    }

    pub(crate) fn hour_position_count(&self, hour: u64) -> Result<u32> {
        self.hour_positions.list(&hour).len()
    }

    pub(crate) fn hour_position(&self, hour: u64, index: u32) -> Result<U256> {
        self.hour_positions
            .list(&hour)
            .get(index)?
            .ok_or_else(|| GemFactoryError::IndexOutOfBounds.into())
    }

    /// Moves an expired hour from the expiry queue to the cleanup, which deletes its
    /// positions. Done with the hour's credit, so a retry never credits it twice.
    pub(crate) fn hand_hour_to_cleanup(&mut self, hour: u64) -> Result<()> {
        let entry = position_queue_entry(hour)?;
        let deadline = self.expiry_deadline.read(&entry)?;
        deadline_queue::remove(&PositionExpiryQueue(self), entry)?;
        deadline_queue::push(&PositionCleanupQueue(self), entry, deadline)
    }

    /// Deletes an expired position, and forgets its hour with the last one.
    pub(crate) fn delete_position(&mut self, record: &GemFactoryPosition) -> Result<()> {
        let hour = record.expiry_hour();
        self.hour_index(hour)
            .remove(record.gem_factory_position_id)?;
        self.positions.delete(record.gem_factory_position_id)?;
        if self.hour_position_count(hour)? == 0 {
            self.hour_capacity_minor.clear(&hour)?;
            let entry = position_queue_entry(hour)?;
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

/// Positions' expiry hours, each queued as one entry.
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

/// Expired hours whose positions wait to be deleted.
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
