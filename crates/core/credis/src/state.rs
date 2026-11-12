//! Storage CRUD, the live lists and the series of the Credis contract.
//!
//! All functions take a short-lived `&mut CredisContract` (or `&CredisContract`
//! for reads) constructed via `CredisContract::new(storage)`. They only touch
//! local storage. The rules that drive them live in CredisFactory.

use alloy_primitives::{Address, U256};

use outbe_core_common::deadline_queue;
use outbe_primitives::error::Result;
use outbe_primitives::series_id::SeriesId;
use outbe_primitives::storage::dsl::Map;
use outbe_primitives::storage::indexed_list::{create_indexed, delete_indexed, IndexedList};
use outbe_system_common::call_bins;

use crate::errors::CredisError;
use crate::precompile::ICredis;
use crate::schema::{Credis, CredisContract};

impl<'storage> CredisContract<'storage> {
    // ---------------------------------------------------------------------
    // Credis CRUD
    // ---------------------------------------------------------------------

    pub fn credis_exists(&self, credis_id: U256) -> Result<bool> {
        self.records.exists(credis_id)
    }

    /// Loads a Credis as its series sees it: a live Credis reads its call and call terms
    /// from the series. Reverts on missing.
    pub fn load_credis(&self, credis_id: U256) -> Result<Credis> {
        let mut record = self
            .records
            .get(credis_id)?
            .ok_or(CredisError::CredisNotFound)?;
        if !record.is_settled() {
            let series = record.series_id;
            record.call_window_seconds = self.series_call_window_seconds.read(&series)?;
            record.call_threshold_seconds = self.series_call_threshold_seconds.read(&series)?;
            record.call_notice_period_seconds =
                self.series_call_notice_period_seconds.read(&series)?;
            record.called_at = self.series_called_at.read(&series)?;
        }
        Ok(record)
    }

    /// Records a new Credis, lists it, counts it in its owner's sums, puts it in its
    /// series, and emits the ERC-721 mint `Transfer`. Returns `true` when this Credis
    /// opens the series for the first time.
    pub fn create_credis_record(&mut self, record: &Credis, call_start_day: u32) -> Result<bool> {
        create_indexed(&self.records, record, self.record_indexes(record.owner))?;
        add_to(
            &self.owner_principal_minor,
            record.owner,
            record.principal_minor,
        )?;
        add_to(
            &self.owner_outstanding_minor,
            record.owner,
            record.outstanding_principal_minor,
        )?;
        let newly_opened = self.join_series(record, call_start_day)?;
        self.emit(ICredis::Transfer {
            from: Address::ZERO,
            to: record.owner,
            tokenId: record.credis_id,
        })?;
        Ok(newly_opened)
    }

    pub fn update_credis_record(&mut self, record: &Credis) -> Result<()> {
        self.records.update(record)?;
        self.emit(ICredis::MetadataUpdate {
            _tokenId: record.credis_id,
        })
    }

    /// Deletes a Credis, unlists it, takes it out of its owner's sums and emits the ERC-721 burn
    /// `Transfer`. Its series membership is handled by the caller.
    pub fn delete_credis_record(&mut self, credis_id: U256) -> Result<()> {
        let record = self
            .records
            .get(credis_id)?
            .ok_or(CredisError::CredisNotFound)?;
        take_from(
            &self.owner_principal_minor,
            record.owner,
            record.principal_minor,
        )?;
        take_from(
            &self.owner_outstanding_minor,
            record.owner,
            record.outstanding_principal_minor,
        )?;
        delete_indexed(&self.records, credis_id, |record: &Credis| {
            self.record_indexes(record.owner)
        })?;
        self.emit(ICredis::Transfer {
            from: record.owner,
            to: Address::ZERO,
            tokenId: credis_id,
        })
    }

    /// Takes repaid principal out of its owner's outstanding sum.
    pub fn take_owner_outstanding(&mut self, owner: Address, repaid: U256) -> Result<()> {
        take_from(&self.owner_outstanding_minor, owner, repaid)
    }

    /// Subtracts released or forfeited Gratis from the series's unpaid Gratis total.
    pub fn take_series_unpaid_gratis(&mut self, series: SeriesId, amount: U256) -> Result<()> {
        take_from(&self.series_unpaid_gratis_minor, series, amount)
    }

    fn record_indexes(&self, owner: Address) -> [IndexedList<'storage, U256>; 1] {
        [IndexedList::new(
            self.owner_ids.list(&owner),
            self.owner_id_position.clone(),
        )]
    }

    /// Widens the currency's scan terms to cover a newly opened Credis.
    pub fn widen_scan_terms(&mut self, record: &Credis) -> Result<()> {
        outbe_system_common::call_breach::widen_scan_terms(
            &self.max_call_window_seconds,
            &self.min_call_threshold_seconds,
            record.reference_currency,
            record.call_window_seconds,
            record.call_threshold_seconds,
        )
    }

    // ---------------------------------------------------------------------
    // Live lists
    // ---------------------------------------------------------------------

    pub fn owner_credis_count(&self, account: Address) -> Result<u32> {
        self.owner_ids.list(&account).len()
    }

    pub fn owner_credis_ids(&self, account: Address) -> Result<Vec<U256>> {
        self.owner_ids.list(&account).read_all()
    }

    // ---------------------------------------------------------------------
    // Series
    // ---------------------------------------------------------------------

    fn series_index(&self, series: SeriesId) -> IndexedList<'storage, U256> {
        IndexedList::new(
            self.series_members.list(&series),
            self.series_member_position.clone(),
        )
    }

    /// Puts a Credis in its series. The first member ever opened in the series seals its terms,
    /// and whenever an uncalled series transitions from 0 live members to 1 it enters the call bin
    /// of its call price. Returns `true` iff this Credis opened the series for the first time.
    fn join_series(&mut self, record: &Credis, call_start_day: u32) -> Result<bool> {
        let series = record.series_id;
        let was_empty = self.series_member_count(series)? == 0;
        let newly_opened = !self.series_opened.read(&series)?;
        if newly_opened {
            self.series_opened.write(&series, true)?;
            self.series_issuance_currency
                .write(&series, record.issuance_currency)?;
            self.series_currency
                .write(&series, record.reference_currency)?;
            self.series_full_day.write(&series, call_start_day)?;
            self.series_call_anchor_price_minor
                .write(&series, record.call_anchor_price_minor)?;
            self.series_call_price_minor
                .write(&series, record.call_price_minor)?;
            self.series_policy_rate.write(&series, record.policy_rate)?;
            self.series_call_window_seconds
                .write(&series, record.call_window_seconds)?;
            self.series_call_threshold_seconds
                .write(&series, record.call_threshold_seconds)?;
            self.series_call_notice_period_seconds
                .write(&series, record.call_notice_period_seconds)?;
        }
        if was_empty && self.series_called_at.read(&series)? == 0 {
            let call_price = self.series_call_price_minor.read(&series)?;
            let currency = self.series_currency.read(&series)?;
            let bin = call_bins::price_to_bin(call_price)?;
            call_bins::insert(&CallBins(self, currency), series, bin)?;
        }
        add_to(
            &self.series_unpaid_gratis_minor,
            series,
            record.gratis_minor,
        )?;
        self.series_index(series).insert(record.credis_id)?;
        Ok(newly_opened)
    }

    /// Takes a Credis out of its series. An uncalled series leaves the active queues when its
    /// last unsettled member leaves. A called series stays in the forfeit queue until its
    /// settlement deadline, so the close event still fires. Series terms and `series_opened`
    /// remain in storage.
    pub fn leave_series(&mut self, series: SeriesId, credis_id: U256) -> Result<()> {
        self.series_index(series).remove(credis_id)?;
        if self.series_member_count(series)? == 0 && self.series_called_at.read(&series)? == 0 {
            self.close_series_queues(series)?;
        }
        Ok(())
    }

    /// Removes an empty series from its call bin, forfeit queue, and cleanup queue while keeping
    /// its sealed terms and `series_opened` flag intact.
    fn close_series_queues(&mut self, series: SeriesId) -> Result<()> {
        let currency = self.series_currency.read(&series)?;
        call_bins::remove(&CallBins(self, currency), series)?;
        deadline_queue::remove(&ForfeitQueue(self), series)?;
        deadline_queue::remove(&CleanupQueue(self), series)
    }

    pub fn series_member_count(&self, series: SeriesId) -> Result<u32> {
        self.series_members.list(&series).len()
    }

    pub fn series_member(&self, series: SeriesId, index: u32) -> Result<U256> {
        self.series_members
            .list(&series)
            .get(index)?
            .ok_or_else(|| CredisError::IndexOutOfBounds.into())
    }

    /// Calls every member at once: the series leaves its bin for the forfeit queue, where
    /// it waits on its settlement deadline as one entry. Returns the deadline.
    pub fn record_series_called(&mut self, series: SeriesId, now: u64) -> Result<u64> {
        let currency = self.series_currency.read(&series)?;
        call_bins::remove(&CallBins(self, currency), series)?;
        self.series_called_at.write(&series, now)?;
        let deadline = outbe_core_common::settlement::settlement_deadline_of(
            now,
            self.series_call_notice_period_seconds.read(&series)?,
        );
        self.series_settlement_deadline.write(&series, deadline)?;
        deadline_queue::push(&ForfeitQueue(self), series, deadline)?;
        Ok(deadline)
    }

    /// Moves a forfeited series from the forfeit queue to the cleanup, which deletes its
    /// Credis.
    pub fn hand_to_cleanup(&mut self, series: SeriesId) -> Result<()> {
        let deadline = self.called_deadline.read(&series)?;
        deadline_queue::remove(&ForfeitQueue(self), series)?;
        deadline_queue::push(&CleanupQueue(self), series, deadline)
    }

    pub fn push_called(&mut self, series: SeriesId, deadline: u64) -> Result<()> {
        self.series_settlement_deadline.write(&series, deadline)?;
        deadline_queue::push(&ForfeitQueue(self), series, deadline)
    }

    pub fn remove_called(&mut self, series: SeriesId) -> Result<()> {
        deadline_queue::remove(&ForfeitQueue(self), series)
    }
}

fn add_to<K: outbe_primitives::storage::types::StorageKey>(
    sums: &Map<'_, K, U256>,
    key: K,
    amount: U256,
) -> Result<()> {
    let total = sums
        .read(&key)?
        .checked_add(amount)
        .ok_or(CredisError::ArithmeticOverflow)?;
    sums.write(&key, total)
}

fn take_from<K: outbe_primitives::storage::types::StorageKey>(
    sums: &Map<'_, K, U256>,
    key: K,
    amount: U256,
) -> Result<()> {
    let total = sums
        .read(&key)?
        .checked_sub(amount)
        .ok_or(CredisError::ArithmeticOverflow)?;
    sums.write(&key, total)
}

/// This queue orders called series by the UTC day that contains their settlement deadline.
pub struct ForfeitQueue<'a, 'storage>(pub &'a CredisContract<'storage>);

outbe_core_common::impl_deadline_queue!(ForfeitQueue<SeriesId> {
    root: forfeit_tree_root,
    mid: forfeit_tree_mid,
    leaf: forfeit_tree_leaf,
    len: forfeit_day_len,
    live: forfeit_day_live,
    at: forfeit_day_at,
    slot: called_series_slot,
    deadline: called_deadline,
    sweep_day: forfeit_pass_day,
    cursor: forfeit_cursor,
});

/// Forfeited series whose Credis wait to be deleted.
pub struct CleanupQueue<'a, 'storage>(pub &'a CredisContract<'storage>);

outbe_core_common::impl_deadline_queue!(CleanupQueue<SeriesId> {
    root: cleanup_tree_root,
    mid: cleanup_tree_mid,
    leaf: cleanup_tree_leaf,
    len: cleanup_day_len,
    live: cleanup_day_live,
    at: cleanup_day_at,
    slot: cleanup_series_slot,
    deadline: cleanup_deadline,
    sweep_day: cleanup_sweep_day,
    cursor: cleanup_cursor,
});

/// One reference currency's uncalled series, by call price.
pub struct CallBins<'a, 'storage>(pub &'a CredisContract<'storage>, pub u16);

outbe_system_common::impl_call_bins!(CallBins<SeriesId> {
    root: call_bin_tree_root,
    mid: call_bin_tree_mid,
    leaf: call_bin_tree_leaf,
    count: call_bin_count,
    at: call_bin_series,
    slot: call_series_slot,
    cursor: call_bin_cursor,
    failed: call_scan_failed_day,
});
