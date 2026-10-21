//! Storage CRUD, the live lists and the call groups of the Credis contract.
//!
//! All functions take a short-lived `&mut CredisContract` (or `&CredisContract`
//! for reads) constructed via `CredisContract::new(storage)`. They only touch
//! local storage. The rules that drive them live in CredisFactory.

use alloy_primitives::{Address, B256, U256};

use outbe_primitives::call_bins;
use outbe_primitives::error::Result;
use outbe_primitives::expiry_queue;
use outbe_primitives::storage::dsl::Map;
use outbe_primitives::storage::indexed_list::{create_indexed, delete_indexed, IndexedList};

use crate::errors::CredisError;
use crate::schema::{Credis, CredisContract, CredisState};

impl<'storage> CredisContract<'storage> {
    // ---------------------------------------------------------------------
    // Credis CRUD
    // ---------------------------------------------------------------------

    pub fn credis_exists(&self, credis_id: U256) -> Result<bool> {
        self.records.exists(credis_id)
    }

    /// Loads a Credis as its group sees it: a live Credis reads its call and call terms
    /// from the group. Reverts on missing.
    pub fn load_credis(&self, credis_id: U256) -> Result<Credis> {
        let mut record = self
            .records
            .get(credis_id)?
            .ok_or(CredisError::CredisNotFound)?;
        if record.lifecycle_state()? != CredisState::Settled {
            let group = record.group_key();
            record.call_window_seconds = self.group_call_window_seconds.read(&group)?;
            record.call_threshold_seconds = self.group_call_threshold_seconds.read(&group)?;
            record.call_notice_period_seconds =
                self.group_call_notice_period_seconds.read(&group)?;
            record.called_at = self.group_called_at.read(&group)?;
            record.state = match record.called_at {
                0 => CredisState::Issued,
                _ => CredisState::Called,
            } as u8;
        }
        Ok(record)
    }

    /// Records a new Credis, lists it, counts it in its owner's sums and puts it in its
    /// group.
    pub fn create_credis_record(&mut self, record: &Credis) -> Result<()> {
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
        self.join_group(record)
    }

    pub fn update_credis_record(&mut self, record: &Credis) -> Result<()> {
        self.records.update(record)
    }

    /// Deletes a Credis, unlists it and takes it out of its owner's sums. Its group is the
    /// caller's.
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
        })
    }

    /// Takes repaid principal out of its owner's outstanding sum.
    pub fn take_owner_outstanding(&mut self, owner: Address, repaid: U256) -> Result<()> {
        take_from(&self.owner_outstanding_minor, owner, repaid)
    }

    fn record_indexes(&self, owner: Address) -> [IndexedList<'storage, U256>; 2] {
        [
            IndexedList::new(self.owner_ids.list(&owner), self.owner_id_position.clone()),
            IndexedList::new(self.all_ids.clone(), self.global_id_position.clone()),
        ]
    }

    /// Widens the currency's scan terms to cover a newly opened Credis.
    pub fn widen_scan_terms(&mut self, record: &Credis) -> Result<()> {
        outbe_primitives::call_breach::widen_scan_terms(
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

    pub fn owner_credis_id(&self, account: Address, index: u32) -> Result<U256> {
        self.owner_ids
            .list(&account)
            .get(index)?
            .ok_or_else(|| CredisError::IndexOutOfBounds.into())
    }

    pub fn owner_credis_ids(&self, account: Address) -> Result<Vec<U256>> {
        self.owner_ids.list(&account).read_all()
    }

    pub fn live_credis_count(&self) -> Result<u32> {
        self.all_ids.len()
    }

    pub fn live_credis_id(&self, index: u32) -> Result<U256> {
        self.all_ids
            .get(index)?
            .ok_or_else(|| CredisError::IndexOutOfBounds.into())
    }

    // ---------------------------------------------------------------------
    // Call groups
    // ---------------------------------------------------------------------

    fn group_index(&self, group: B256) -> IndexedList<'storage, U256> {
        IndexedList::new(
            self.group_members.list(&group),
            self.group_member_position.clone(),
        )
    }

    /// Puts a Credis in its group. The first member opens the group with its terms and
    /// puts it in the call bin of its call price.
    fn join_group(&mut self, record: &Credis) -> Result<()> {
        let group = record.group_key();
        if self.group_member_count(group)? == 0 {
            self.group_currency
                .write(&group, record.reference_currency)?;
            self.group_full_day.write(
                &group,
                outbe_primitives::time::first_full_day(record.issued_at),
            )?;
            self.group_call_price_minor
                .write(&group, record.call_price_minor)?;
            self.group_call_window_seconds
                .write(&group, record.call_window_seconds)?;
            self.group_call_threshold_seconds
                .write(&group, record.call_threshold_seconds)?;
            self.group_call_notice_period_seconds
                .write(&group, record.call_notice_period_seconds)?;
            let bin = call_bins::price_to_bin(record.call_price_minor)?;
            call_bins::insert(&CallBins(self, record.reference_currency), group, bin)?;
        }
        self.group_index(group).insert(record.credis_id)
    }

    /// Takes a Credis out of its group, and closes the group with its last member.
    pub fn leave_group(&mut self, group: B256, credis_id: U256) -> Result<()> {
        self.group_index(group).remove(credis_id)?;
        if self.group_member_count(group)? == 0 {
            self.close_group(group)?;
        }
        Ok(())
    }

    /// Forgets a group nothing is left in: its bin, its queues, its call and its terms.
    fn close_group(&mut self, group: B256) -> Result<()> {
        let currency = self.group_currency.read(&group)?;
        call_bins::remove(&CallBins(self, currency), group)?;
        expiry_queue::remove(&ExpiryDays(self), group)?;
        expiry_queue::remove(&CleanupDays(self), group)?;
        self.group_currency.clear(&group)?;
        self.group_full_day.clear(&group)?;
        self.group_call_price_minor.clear(&group)?;
        self.group_call_window_seconds.clear(&group)?;
        self.group_call_threshold_seconds.clear(&group)?;
        self.group_call_notice_period_seconds.clear(&group)?;
        self.group_called_at.clear(&group)
    }

    pub fn group_member_count(&self, group: B256) -> Result<u32> {
        self.group_members.list(&group).len()
    }

    pub fn group_member(&self, group: B256, index: u32) -> Result<U256> {
        self.group_members
            .list(&group)
            .get(index)?
            .ok_or_else(|| CredisError::IndexOutOfBounds.into())
    }

    /// Calls every member at once: the group leaves its bin for the expiry queue, where
    /// it waits on its settlement deadline as one entry. Returns the deadline.
    pub fn record_group_called(&mut self, group: B256, now: u64) -> Result<u64> {
        let currency = self.group_currency.read(&group)?;
        call_bins::remove(&CallBins(self, currency), group)?;
        self.group_called_at.write(&group, now)?;
        let deadline = outbe_primitives::settlement::settlement_deadline_of(
            now,
            self.group_call_notice_period_seconds.read(&group)?,
        );
        expiry_queue::push(&ExpiryDays(self), group, deadline)?;
        Ok(deadline)
    }

    /// Moves a forfeited group from the expiry queue to the cleanup, which deletes its
    /// Credis.
    pub fn hand_to_cleanup(&mut self, group: B256) -> Result<()> {
        let deadline = self.called_deadline.read(&group)?;
        expiry_queue::remove(&ExpiryDays(self), group)?;
        expiry_queue::push(&CleanupDays(self), group, deadline)
    }

    pub fn push_called(&mut self, group: B256, deadline: u64) -> Result<()> {
        expiry_queue::push(&ExpiryDays(self), group, deadline)
    }

    pub fn remove_called(&mut self, group: B256) -> Result<()> {
        expiry_queue::remove(&ExpiryDays(self), group)
    }
}

fn add_to(sums: &Map<'_, Address, U256>, owner: Address, amount: U256) -> Result<()> {
    let total = sums
        .read(&owner)?
        .checked_add(amount)
        .ok_or(CredisError::ArithmeticOverflow)?;
    sums.write(&owner, total)
}

fn take_from(sums: &Map<'_, Address, U256>, owner: Address, amount: U256) -> Result<()> {
    let total = sums
        .read(&owner)?
        .checked_sub(amount)
        .ok_or(CredisError::ArithmeticOverflow)?;
    sums.write(&owner, total)
}

/// This queue orders called groups by the UTC day that contains their settlement deadline.
pub struct ExpiryDays<'a, 'storage>(pub &'a CredisContract<'storage>);

outbe_primitives::impl_expiry_queue!(ExpiryDays<B256> {
    root: expiry_tree_root,
    mid: expiry_tree_mid,
    leaf: expiry_tree_leaf,
    len: expiry_day_len,
    live: expiry_day_live,
    at: expiry_day_at,
    slot: called_group_slot,
    deadline: called_deadline,
    sweep_day: expiry_sweep_day,
    cursor: expiry_cursor,
});

/// Forfeited groups whose Credis wait to be deleted.
pub struct CleanupDays<'a, 'storage>(pub &'a CredisContract<'storage>);

outbe_primitives::impl_expiry_queue!(CleanupDays<B256> {
    root: cleanup_tree_root,
    mid: cleanup_tree_mid,
    leaf: cleanup_tree_leaf,
    len: cleanup_day_len,
    live: cleanup_day_live,
    at: cleanup_day_at,
    slot: cleanup_group_slot,
    deadline: cleanup_deadline,
    sweep_day: cleanup_sweep_day,
    cursor: cleanup_cursor,
});

/// One reference currency's uncalled groups, by call price.
pub struct CallBins<'a, 'storage>(pub &'a CredisContract<'storage>, pub u16);

outbe_primitives::impl_call_bins!(CallBins<B256> {
    root: call_bin_tree_root,
    mid: call_bin_tree_mid,
    leaf: call_bin_tree_leaf,
    count: call_bin_count,
    at: call_bin_groups,
    slot: call_group_slot,
    cursor: call_bin_cursor,
    failed: call_scan_failed_day,
});
