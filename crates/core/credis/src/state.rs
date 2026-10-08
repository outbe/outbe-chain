//! Storage CRUD and per-address index helpers for the Credis contract.
//!
//! All functions take a short-lived `&mut CredisContract` (or `&CredisContract`
//! for reads) constructed via `CredisContract::new(storage)`. They only touch
//! local storage. Orchestration logic lives in `runtime.rs`.

use alloy_primitives::{Address, U256};

use outbe_primitives::call_bins;
use outbe_primitives::error::Result;
use outbe_primitives::expiry_queue;

use crate::errors::CredisError;
use crate::schema::{CredisContract, Position};

impl CredisContract<'_> {
    // ---------------------------------------------------------------------
    // Position CRUD
    // ---------------------------------------------------------------------

    pub(crate) fn position_exists(&self, position_id: U256) -> Result<bool> {
        self.positions.exists(position_id)
    }

    pub(crate) fn load_position(&self, position_id: U256) -> Result<Position> {
        self.positions
            .get(position_id)?
            .ok_or_else(|| CredisError::PositionNotFound.into())
    }

    pub(crate) fn create_position_record(&mut self, position: &Position) -> Result<()> {
        self.positions.create(position)
    }

    pub(crate) fn update_position_record(&mut self, position: &Position) -> Result<()> {
        self.positions.update(position)
    }

    /// Widens the currency's scan terms to cover a newly opened position.
    pub(crate) fn widen_scan_terms(&mut self, position: &Position) -> Result<()> {
        outbe_primitives::call_breach::widen_scan_terms(
            &self.max_call_window_seconds,
            &self.min_call_threshold_seconds,
            position.reference_currency,
            position.call_window_seconds,
            position.call_threshold_seconds,
        )
    }

    // ---------------------------------------------------------------------
    // Per-address dense index (mirrors outbe-nod owner_nod_* shape)
    // ---------------------------------------------------------------------

    pub(crate) fn append_to_address_index(
        &mut self,
        account: Address,
        position_id: U256,
    ) -> Result<()> {
        let count = self.address_position_counts.read(&account)?;
        let key = CredisContract::address_index_key(account, count);
        self.address_position_ids.write(&key, position_id)?;
        self.address_position_counts.write(&account, count + 1)?;
        Ok(())
    }

    pub(crate) fn read_address_position_count(&self, account: Address) -> Result<u32> {
        self.address_position_counts.read(&account)
    }

    pub(crate) fn read_address_position_id(&self, account: Address, index: u32) -> Result<U256> {
        let key = CredisContract::address_index_key(account, index);
        self.address_position_ids.read(&key)
    }

    // ---------------------------------------------------------------------
    // Global dense index backing `totalSupply` / `positionByIndex`
    // ---------------------------------------------------------------------

    pub(crate) fn append_to_global_index(&mut self, position_id: U256) -> Result<()> {
        let total = self.total_positions.read()?;
        self.position_id_at_index.write(&total, position_id)?;
        self.total_positions.write(total + 1)?;
        Ok(())
    }

    pub(crate) fn read_total_positions(&self) -> Result<u64> {
        self.total_positions.read()
    }

    pub(crate) fn read_position_id_at(&self, index: u64) -> Result<U256> {
        self.position_id_at_index.read(&index)
    }

    // ---------------------------------------------------------------------
    // Call-price index (Open positions only)
    // ---------------------------------------------------------------------

    /// Puts an Open position in the bin of its sealed call price.
    pub(crate) fn index_for_call(&mut self, position: &Position) -> Result<()> {
        let bin = call_bins::price_to_bin(position.call_price_minor)?;
        call_bins::insert(
            &CallBins(self, position.reference_currency),
            position.position_id,
            bin,
        )
    }

    /// No-op for a position the call index no longer holds.
    pub(crate) fn unindex_for_call(&mut self, position: &Position) -> Result<()> {
        call_bins::remove(
            &CallBins(self, position.reference_currency),
            position.position_id,
        )?;
        Ok(())
    }

    // ---------------------------------------------------------------------
    // Called-position counter
    // ---------------------------------------------------------------------

    pub(crate) fn bump_called_count(&mut self, account: Address) -> Result<()> {
        let count = self.called_position_counts.read(&account)?;
        self.called_position_counts
            .write(&account, count.saturating_add(1))
    }

    pub(crate) fn drop_called_count(&mut self, account: Address) -> Result<()> {
        let count = self.called_position_counts.read(&account)?;
        self.called_position_counts
            .write(&account, count.saturating_sub(1))
    }

    // ---------------------------------------------------------------------
    // Settlement-deadline queue
    // ---------------------------------------------------------------------

    pub(crate) fn queue_called(&mut self, position_id: U256, deadline: u64) -> Result<()> {
        expiry_queue::push(&ExpiryHours(self), position_id, deadline)
    }

    pub(crate) fn unqueue_called(&mut self, position_id: U256) -> Result<()> {
        expiry_queue::remove(&ExpiryHours(self), position_id)
    }
}

/// Called positions, queued by the hour their settlement deadline falls in.
pub struct ExpiryHours<'a, 'storage>(pub &'a CredisContract<'storage>);

outbe_primitives::impl_expiry_queue!(ExpiryHours<U256> {
    root: expiry_tree_root,
    mid: expiry_tree_mid,
    leaf: expiry_tree_leaf,
    len: expiry_bucket_len,
    live: expiry_bucket_live,
    at: expiry_bucket_at,
    slot: called_slot,
    deadline: called_deadline,
    sweep_bucket: expiry_sweep_hour,
    cursor: expiry_cursor,
});

/// One reference currency's Open positions, by call price.
pub struct CallBins<'a, 'storage>(pub &'a CredisContract<'storage>, pub u16);

outbe_primitives::impl_call_bins!(CallBins<U256> {
    root: call_bin_tree_root,
    mid: call_bin_tree_mid,
    leaf: call_bin_tree_leaf,
    count: call_bin_count,
    at: call_bin_positions,
    slot: call_position_slot,
    cursor: call_bin_cursor,
    failed: call_scan_failed_day,
});
