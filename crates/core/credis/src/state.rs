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
use crate::schema::{Credis, CredisContract};

impl CredisContract<'_> {
    // ---------------------------------------------------------------------
    // Credis CRUD
    // ---------------------------------------------------------------------

    pub(crate) fn credis_exists(&self, credis_id: U256) -> Result<bool> {
        self.records.exists(credis_id)
    }

    pub(crate) fn load_credis(&self, credis_id: U256) -> Result<Credis> {
        self.records
            .get(credis_id)?
            .ok_or_else(|| CredisError::CredisNotFound.into())
    }

    pub(crate) fn create_credis_record(&mut self, record: &Credis) -> Result<()> {
        self.records.create(record)
    }

    pub(crate) fn update_credis_record(&mut self, record: &Credis) -> Result<()> {
        self.records.update(record)
    }

    /// Widens the currency's scan terms to cover a newly opened Credis.
    pub(crate) fn widen_scan_terms(&mut self, record: &Credis) -> Result<()> {
        outbe_primitives::call_breach::widen_scan_terms(
            &self.max_call_window_seconds,
            &self.min_call_threshold_seconds,
            record.reference_currency,
            record.call_window_seconds,
            record.call_threshold_seconds,
        )
    }

    // ---------------------------------------------------------------------
    // Per-address dense index (mirrors outbe-nod owner_nod_* shape)
    // ---------------------------------------------------------------------

    pub(crate) fn append_to_owner_index(
        &mut self,
        account: Address,
        credis_id: U256,
    ) -> Result<()> {
        let count = self.owner_credis_counts.read(&account)?;
        let key = CredisContract::address_index_key(account, count);
        self.owner_credis_ids.write(&key, credis_id)?;
        self.owner_credis_counts.write(&account, count + 1)?;
        Ok(())
    }

    pub(crate) fn read_owner_credis_count(&self, account: Address) -> Result<u32> {
        self.owner_credis_counts.read(&account)
    }

    pub(crate) fn read_owner_credis_id(&self, account: Address, index: u32) -> Result<U256> {
        let key = CredisContract::address_index_key(account, index);
        self.owner_credis_ids.read(&key)
    }

    // ---------------------------------------------------------------------
    // Global dense index backing `totalSupply` / `positionByIndex`
    // ---------------------------------------------------------------------

    pub(crate) fn append_to_global_index(&mut self, credis_id: U256) -> Result<()> {
        let total = self.total_credis.read()?;
        self.credis_id_at_index.write(&total, credis_id)?;
        self.total_credis.write(total + 1)?;
        Ok(())
    }

    pub(crate) fn read_total_credis(&self) -> Result<u64> {
        self.total_credis.read()
    }

    pub(crate) fn read_credis_id_at(&self, index: u64) -> Result<U256> {
        self.credis_id_at_index.read(&index)
    }

    // ---------------------------------------------------------------------
    // Call-price index (Open Credis only)
    // ---------------------------------------------------------------------

    /// Puts an Open Credis in the bin of its sealed call price.
    pub(crate) fn index_for_call(&mut self, record: &Credis) -> Result<()> {
        let bin = call_bins::price_to_bin(record.call_price_minor)?;
        call_bins::insert(
            &CallBins(self, record.reference_currency),
            record.credis_id,
            bin,
        )
    }

    /// No-op for a Credis the call index no longer holds.
    pub(crate) fn unindex_for_call(&mut self, record: &Credis) -> Result<()> {
        call_bins::remove(&CallBins(self, record.reference_currency), record.credis_id)?;
        Ok(())
    }

    // ---------------------------------------------------------------------
    // Settlement-deadline queue
    // ---------------------------------------------------------------------

    pub(crate) fn queue_called(&mut self, credis_id: U256, deadline: u64) -> Result<()> {
        expiry_queue::push(&ExpiryHours(self), credis_id, deadline)
    }

    pub(crate) fn unqueue_called(&mut self, credis_id: U256) -> Result<()> {
        expiry_queue::remove(&ExpiryHours(self), credis_id)
    }
}

/// Called Credis, queued by the hour their settlement deadline falls in.
pub struct ExpiryHours<'a, 'storage>(pub &'a CredisContract<'storage>);

outbe_primitives::impl_expiry_queue!(ExpiryHours<U256> {
    root: expiry_tree_root,
    mid: expiry_tree_mid,
    leaf: expiry_tree_leaf,
    len: expiry_bucket_len,
    live: expiry_bucket_live,
    at: expiry_bucket_at,
    slot: called_credis_slot,
    deadline: called_deadline,
    sweep_bucket: expiry_sweep_hour,
    cursor: expiry_cursor,
});

/// One reference currency's Open Credis, by call price.
pub struct CallBins<'a, 'storage>(pub &'a CredisContract<'storage>, pub u16);

outbe_primitives::impl_call_bins!(CallBins<U256> {
    root: call_bin_tree_root,
    mid: call_bin_tree_mid,
    leaf: call_bin_tree_leaf,
    count: call_bin_count,
    at: call_bin_credis,
    slot: call_credis_slot,
    cursor: call_bin_cursor,
    failed: call_scan_failed_day,
});
