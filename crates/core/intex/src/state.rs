//! Storage CRUD and dense enumeration helpers for the Intex module.
//!
//! All functions take a short-lived `&IntexContract` (or `&mut` for
//! writes) constructed via `IntexContract::new(storage)`. They only
//! touch local storage. Orchestration and validation live in `api.rs`.

use alloy_primitives::B256;
use outbe_primitives::call_bins;
use outbe_primitives::call_breach::{self, ScanTerms};
use outbe_primitives::error::Result;
use outbe_primitives::expiry_queue;
use outbe_primitives::storage::indexed_list::IndexedList;

use crate::errors::{date_step_error, IntexError};
use crate::schema::{CallStamp, IntexContract, SeriesId, SeriesRecord};

impl IntexContract<'_> {
    // ---------------------------------------------------------------------
    // Series CRUD
    // ---------------------------------------------------------------------

    pub(crate) fn series_exists(&self, series_id: SeriesId) -> Result<bool> {
        self.series.exists(series_id)
    }

    pub(crate) fn get_series(&self, series_id: SeriesId) -> Result<Option<SeriesRecord>> {
        self.series.get(series_id)
    }

    pub(crate) fn load_series(&self, series_id: SeriesId) -> Result<SeriesRecord> {
        self.series
            .get(series_id)?
            .ok_or_else(|| IntexError::SeriesNotFound.into())
    }

    /// Create a new series record and list it for enumeration.
    /// The underlying record `create` rejects a duplicate `series_id`.
    pub(crate) fn create_series_record(&mut self, record: &SeriesRecord) -> Result<()> {
        self.series.create(record)?;
        self.series_index().insert(record.series_id)
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub(crate) fn update_series_record(&mut self, record: &SeriesRecord) -> Result<()> {
        self.series.update(record)
    }

    /// Deletes a spent series: its record, its place in the enumeration and its ledgers.
    pub(crate) fn delete_series_record(&mut self, series_id: SeriesId) -> Result<()> {
        self.series_index().remove(series_id)?;
        self.settled_units.clear(&series_id)?;
        self.gem_factory_units.clear(&series_id)?;
        self.mined_units.clear(&series_id)?;
        self.series.delete(series_id)
    }

    pub(crate) fn read_total_series(&self) -> Result<u32> {
        self.series_ids.len()
    }

    pub(crate) fn read_series_id_at(&self, index: u32) -> Result<SeriesId> {
        self.series_ids
            .get(index)?
            .ok_or_else(|| IntexError::SeriesIndexOutOfRange.into())
    }
}

impl<'storage> IntexContract<'storage> {
    fn series_index(&self) -> IndexedList<'storage, SeriesId> {
        IndexedList::new(self.series_ids.clone(), self.series_position.clone())
    }

    fn group_index(&self, group: B256) -> IndexedList<'storage, SeriesId> {
        IndexedList::new(
            self.call_group_members.list(&group),
            self.call_group_position.clone(),
        )
    }
}

// ---------------------------------------------------------------------
// Call lifecycle: the price bins of call groups, the called-group queue and the
// cleanup queue of expired groups
// ---------------------------------------------------------------------

impl IntexContract<'_> {
    /// Enrolls a series in its call group. The first member opens the group in its
    /// call-price bin and seals its call terms.
    pub fn join_call_group(&mut self, record: &SeriesRecord) -> Result<()> {
        let group = record.group_key().map_err(date_step_error)?;
        if self.call_group_members.list(&group).len()? == 0 {
            let iso = record.reference_currency;
            self.call_group_currency.write(&group, iso)?;
            self.call_group_window_seconds
                .write(&group, record.call_window_seconds)?;
            self.call_group_threshold_seconds
                .write(&group, record.call_threshold_seconds)?;
            self.call_group_notice_seconds
                .write(&group, record.call_notice_period_seconds)?;
            self.call_group_transit_seconds
                .write(&group, record.transit_margin_seconds)?;
            let bin = call_bins::price_to_bin(record.call_price_minor)?;
            call_bins::insert(&CallBins(&*self, iso), group, bin)?;
        }
        self.group_index(group).insert(record.series_id)
    }

    /// Takes a series out of its group, and closes the group with its last member.
    pub fn leave_call_group(&mut self, group: B256, series_id: SeriesId) -> Result<()> {
        self.group_index(group).remove(series_id)?;
        if self.call_group_members.list(&group).len()? == 0 {
            self.close_call_group(group)?;
        }
        Ok(())
    }

    /// Forgets a group nothing is left in: its bin, its queues and its call.
    fn close_call_group(&mut self, group: B256) -> Result<()> {
        let iso = self.call_group_currency.read(&group)?;
        call_bins::remove(&CallBins(&*self, iso), group)?;
        expiry_queue::remove(&ExpiryDays(self), group)?;
        expiry_queue::remove(&CleanupDays(self), group)?;
        self.call_group_currency.clear(&group)?;
        self.call_group_window_seconds.clear(&group)?;
        self.call_group_threshold_seconds.clear(&group)?;
        self.call_group_notice_seconds.clear(&group)?;
        self.call_group_transit_seconds.clear(&group)?;
        self.call_group_called_at.clear(&group)
    }

    pub fn group_members(&self, group: B256) -> Result<Vec<SeriesId>> {
        self.call_group_members.list(&group).read_all()
    }

    pub fn group_member_count(&self, group: B256) -> Result<u32> {
        self.call_group_members.list(&group).len()
    }

    pub fn group_member(&self, group: B256, index: u32) -> Result<SeriesId> {
        self.call_group_members
            .list(&group)
            .get(index)?
            .ok_or_else(|| IntexError::CalledGroupMemberOutOfRange.into())
    }

    pub fn group_currency(&self, group: B256) -> Result<u16> {
        self.call_group_currency.read(&group)
    }

    /// The series' call, read from its group.
    pub fn call_stamp(&self, record: &SeriesRecord) -> Result<CallStamp> {
        let group = record.group_key().map_err(date_step_error)?;
        self.group_stamp(group, self.call_group_called_at.read(&group)?)
    }

    /// The group's call at `called_at`, on the notice period it sealed.
    pub fn group_stamp(&self, group: B256, called_at: u32) -> Result<CallStamp> {
        let deadline = match called_at {
            0 => 0,
            _ => outbe_primitives::settlement::settlement_deadline_of(
                u64::from(called_at),
                self.call_group_notice_seconds.read(&group)?,
            ),
        };
        Ok(CallStamp {
            called_at,
            deadline,
        })
    }

    /// Whether the group's expiry already returned what its members left unpaid.
    pub fn group_forfeited(&self, group: B256) -> Result<bool> {
        Ok(self.call_group_called_at.read(&group)? != 0
            && self.called_group_slot.read(&group)? == 0)
    }

    /// Calls the group: it leaves its bin and waits on `deadline` in the expiry queue.
    pub fn record_group_called(
        &mut self,
        group: B256,
        called_at: u32,
        deadline: u64,
    ) -> Result<()> {
        // A second call would orphan the first slot and return the load twice.
        if self.call_group_called_at.read(&group)? != 0 {
            return Err(IntexError::GroupAlreadyCalled.into());
        }
        let iso = self.call_group_currency.read(&group)?;
        call_bins::remove(&CallBins(&*self, iso), group)?;
        self.call_group_called_at.write(&group, called_at)?;
        expiry_queue::push(&ExpiryDays(self), group, deadline)
    }

    /// Moves an expired group from the expiry queue to the cleanup, which deletes its
    /// spent series. Done with the group's credit, so a retry never credits it twice.
    pub fn hand_to_cleanup(&mut self, group: B256) -> Result<()> {
        let deadline = self.called_deadline.read(&group)?;
        expiry_queue::remove(&ExpiryDays(self), group)?;
        expiry_queue::push(&CleanupDays(self), group, deadline)
    }

    /// Widen the currency's stored terms to cover a newly issued series.
    pub fn widen_call_terms(
        &mut self,
        reference_currency: u16,
        call_window_seconds: u32,
        call_threshold_seconds: u32,
    ) -> Result<()> {
        call_breach::widen_scan_terms(
            &self.max_call_window_seconds,
            &self.min_call_threshold_seconds,
            reference_currency,
            call_window_seconds,
            call_threshold_seconds,
        )
    }

    /// The terms the call scan must search to cover every live series.
    pub fn scan_call_terms(
        &self,
        reference_currency: u16,
        live_window: u32,
        live_threshold: u32,
    ) -> Result<ScanTerms> {
        call_breach::scan_terms(
            &self.max_call_window_seconds,
            &self.min_call_threshold_seconds,
            reference_currency,
            live_window,
            live_threshold,
        )
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub fn call_bin_groups(&self, reference_currency: u16, bin_id: u32) -> Result<Vec<B256>> {
        let bins = CallBins(self, reference_currency);
        (0..call_bins::len(&bins, bin_id)?)
            .map(|index| call_bins::entry_at(&bins, bin_id, index))
            .collect()
    }

    /// This function returns the deadline's UTC day since the epoch, not a WorldwideDay.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn deadline_day(deadline: u64) -> u32 {
        expiry_queue::deadline_day(deadline).unwrap()
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub const fn day_end(day: u32) -> u64 {
        expiry_queue::day_end(day)
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub fn day_slot_key(day: u32, slot: u32) -> B256 {
        expiry_queue::slot_key(day, slot)
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub fn first_expiry_day(&self) -> Result<Option<u32>> {
        expiry_queue::first_day(&ExpiryDays(self))
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub fn first_cleanup_day(&self) -> Result<Option<u32>> {
        expiry_queue::first_day(&CleanupDays(self))
    }

    /// Moves a called group onto `deadline` in the expiry queue.
    #[cfg(feature = "e2e-test")]
    pub fn requeue_called(&mut self, group: B256, deadline: u64) -> Result<()> {
        expiry_queue::remove(&ExpiryDays(self), group)?;
        expiry_queue::push(&ExpiryDays(self), group, deadline)
    }
}

/// This daily queue stores called groups whose settlement window remains open.
pub struct ExpiryDays<'a, 'b>(pub &'a IntexContract<'b>);

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

/// This daily queue stores expired groups whose spent series await deletion.
pub struct CleanupDays<'a, 'b>(pub &'a IntexContract<'b>);

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

/// The call-price bins of one reference currency, holding its call groups.
pub struct CallBins<'a, 'b>(pub &'a IntexContract<'b>, pub u16);

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
