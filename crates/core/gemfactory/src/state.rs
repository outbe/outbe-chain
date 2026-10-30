//! GemPosition NFT bookkeeping: issue (create record, owner and expiry-hour lists),
//! expiry and deletion, and the ERC-721-style read views. Positions are single-owner
//! and non-transferable, mirroring the `gem` crate's NFT ledger.

use alloy_primitives::{Address, U256};
use base64::Engine;
use outbe_primitives::deadline_queue;
use outbe_primitives::error::Result;
use outbe_primitives::storage::indexed_list::IndexedList;

use crate::errors::GemFactoryError;
use crate::schema::{GemFactoryContract, GemPosition};

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
    fn owner_index(&self, owner: Address) -> IndexedList<'s, U256> {
        IndexedList::new(
            self.owner_position_ids.list(&owner),
            self.owner_position_index.clone(),
        )
    }

    fn hour_index(&self, hour: u64) -> IndexedList<'s, U256> {
        IndexedList::new(
            self.hour_positions.list(&hour),
            self.hour_position_index.clone(),
        )
    }

    /// Issue a position NFT: create the record, list it under its merchant and its
    /// expiry hour, and queue the hour on its first position. Rejects a duplicate
    /// `position_id`.
    pub(crate) fn add_position(&mut self, pos: &GemPosition) -> Result<()> {
        if self.positions.exists(pos.position_id)? {
            return Err(GemFactoryError::PositionAlreadyExists.into());
        }
        self.positions.create(pos)?;
        self.owner_index(pos.merchant).insert(pos.position_id)?;
        let hour = pos.expiry_hour();
        if self.hour_position_count(hour)? == 0 {
            deadline_queue::push(
                &PositionExpiryQueue(self),
                position_queue_entry(hour)?,
                pos.expires_at,
            )?;
        }
        self.hour_index(hour).insert(pos.position_id)?;
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
    pub(crate) fn delete_position(&mut self, record: &GemPosition) -> Result<()> {
        let hour = record.expiry_hour();
        self.owner_index(record.merchant)
            .remove(record.position_id)?;
        self.hour_index(hour).remove(record.position_id)?;
        self.positions.delete(record.position_id)?;
        if self.hour_position_count(hour)? == 0 {
            self.hour_capacity_minor.clear(&hour)?;
            let entry = position_queue_entry(hour)?;
            deadline_queue::remove(&PositionExpiryQueue(self), entry)?;
            deadline_queue::remove(&PositionCleanupQueue(self), entry)?;
        }
        Ok(())
    }

    // --- Position NFT views (ERC-721-style, non-transferable) ---

    /// What the position can still issue: nothing once it expired, whether or not the
    /// cleanup has deleted it yet.
    pub(crate) fn remaining_capacity(&self, pos: &GemPosition) -> Result<U256> {
        let now = self.storage.now_secs()?;
        Ok(match now >= pos.expires_at {
            true => U256::ZERO,
            false => pos.remaining_capacity_minor,
        })
    }

    pub fn balance_of(&self, owner: Address) -> Result<u32> {
        self.owner_position_ids.list(&owner).len()
    }

    pub fn owner_of(&self, position_id: U256) -> Result<Address> {
        let pos = self
            .positions
            .get(position_id)?
            .ok_or(GemFactoryError::PositionNotFound)?;
        Ok(pos.merchant)
    }

    pub fn tokens_by_owner(&self, owner: Address) -> Result<Vec<U256>> {
        self.owner_position_ids.list(&owner).read_all()
    }

    pub fn token_uri(&self, position_id: U256) -> Result<String> {
        let pos = self
            .positions
            .get(position_id)?
            .ok_or(GemFactoryError::PositionNotFound)?;
        // TODO: replace hand-rolled JSON with type-safe serialization (serde struct).
        let json = format!(
            "{{\"name\":\"GemPosition #{}\",\"attributes\":[{{\"trait_type\":\"merchant\",\"value\":\"{}\"}},{{\"trait_type\":\"source_intex_id\",\"value\":{}}},{{\"trait_type\":\"remaining_capacity\",\"value\":\"{}\"}},{{\"trait_type\":\"source_entry_price\",\"value\":\"{}\"}},{{\"trait_type\":\"issuance_currency\",\"value\":{}}},{{\"trait_type\":\"reference_currency\",\"value\":{}}},{{\"trait_type\":\"issued_at\",\"value\":{}}}]}}",
            position_id,
            pos.merchant,
            pos.source_intex_id,
            self.remaining_capacity(&pos)?,
            pos.source_entry_price_minor,
            pos.issuance_currency,
            pos.reference_currency,
            pos.issued_at,
        );
        let encoded = base64::engine::general_purpose::STANDARD.encode(json.as_bytes());
        Ok(format!("data:application/json;base64,{}", encoded))
    }
}

/// Positions' expiry hours, each queued as one entry.
pub struct PositionExpiryQueue<'a, 'storage>(pub &'a GemFactoryContract<'storage>);

outbe_primitives::impl_deadline_queue!(PositionExpiryQueue<u64> {
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

outbe_primitives::impl_deadline_queue!(PositionCleanupQueue<u64> {
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
