//! Storage CRUD and dense enumeration helpers for the Intex module.
//!
//! All functions take a short-lived `&IntexContract` (or `&mut` for
//! writes) constructed via `IntexContract::new(storage)`. They only
//! touch local storage. Orchestration and validation live in `api.rs`.

use alloy_primitives::{B256, U256};
use outbe_primitives::call_bins;
use outbe_primitives::call_breach::{self, ScanTerms};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::expiry_queue;
use outbe_primitives::storage::dsl::Map;
use outbe_primitives::storage::types::Storable;
use outbe_primitives::time::WorldwideDay;

use crate::errors::IntexError;
use crate::schema::{IntexContract, SeriesId, SeriesRecord};

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

    /// Create a new series record and append it to the global enumeration.
    /// The underlying record `create` rejects a duplicate `series_id`.
    pub(crate) fn create_series_record(&mut self, record: &SeriesRecord) -> Result<()> {
        self.series.create(record)?;
        let day = record.series_id.worldwide_day();
        let seen = self.day_series_count.read(&day)?;
        self.day_series_count.write(&day, seen.saturating_add(1))?;
        self.append_to_global_index(record.series_id)
    }

    pub(crate) fn update_series_record(&mut self, record: &SeriesRecord) -> Result<()> {
        self.series.update(record)
    }

    // ---------------------------------------------------------------------
    // Global dense index for enumeration
    // ---------------------------------------------------------------------

    fn append_to_global_index(&mut self, series_id: SeriesId) -> Result<()> {
        let total = self.total_series.read()?;
        self.series_id_at_index.write(&total, series_id.to_word())?;
        self.total_series.write(total + 1)?;
        Ok(())
    }

    pub(crate) fn read_total_series(&self) -> Result<u64> {
        self.total_series.read()
    }

    pub(crate) fn read_series_id_at(&self, index: u64) -> Result<SeriesId> {
        Ok(SeriesId::from_word(self.series_id_at_index.read(&index)?))
    }

    // ---------------------------------------------------------------------
    // Proceeds fan-in (awaiting set, dense swap-pop)
    // ---------------------------------------------------------------------

    /// Append a WorldwideDay to the awaiting-proceeds set (idempotent).
    pub(crate) fn push_awaiting_proceeds(&mut self, worldwide_day: WorldwideDay) -> Result<()> {
        if self.awaiting_proceeds_slot.read(&worldwide_day)? != 0 {
            return Ok(());
        }
        let count = self.awaiting_proceeds_count.read()?;
        self.awaiting_proceeds_at
            .write(&count, worldwide_day.value())?;
        // store index + 1 so that 0 unambiguously means "absent".
        self.awaiting_proceeds_slot
            .write(&worldwide_day, count + 1)?;
        self.awaiting_proceeds_count.write(count + 1)?;
        Ok(())
    }

    /// Remove a WorldwideDay from the awaiting-proceeds set via swap-remove (idempotent).
    pub(crate) fn remove_awaiting_proceeds(&mut self, worldwide_day: WorldwideDay) -> Result<()> {
        let slot1 = self.awaiting_proceeds_slot.read(&worldwide_day)?;
        if slot1 == 0 {
            return Ok(());
        }
        let idx = slot1 - 1;
        let last = self.awaiting_proceeds_count.read()? - 1;
        if idx != last {
            let last_day = WorldwideDay::new(self.awaiting_proceeds_at.read(&last)?);
            self.awaiting_proceeds_at.write(&idx, last_day.value())?;
            self.awaiting_proceeds_slot.write(&last_day, idx + 1)?;
        }
        self.awaiting_proceeds_at.clear(&last)?;
        self.awaiting_proceeds_slot.clear(&worldwide_day)?;
        self.awaiting_proceeds_count.write(last)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------
// Call lifecycle: price bins of worldwide-day groups and the called-group queue
// ---------------------------------------------------------------------

impl IntexContract<'_> {
    // --- bin keys ---

    pub fn bin_index_key(reference_currency: u16, bin_id: u32, index: u32) -> B256 {
        call_bins::bin_index_key(reference_currency, bin_id, index)
    }

    /// Composite key for a group's member list. It uses the layout of `bin_index_key`,
    /// over a separate column keyed by worldwide day instead of bin id.
    pub fn group_member_key(
        reference_currency: u16,
        worldwide_day: WorldwideDay,
        index: u32,
    ) -> B256 {
        Self::bin_index_key(reference_currency, worldwide_day.value(), index)
    }

    /// Enroll a series in its call-price bin. Its day's group is created with the first member.
    pub fn insert_call_bin(
        &mut self,
        series_id: SeriesId,
        reference_currency: u16,
        call_price: U256,
    ) -> Result<()> {
        let bin_id = call_bins::price_to_bin(call_price)?;
        self.call_bin_index(reference_currency).insert(
            &CallBins(&*self, reference_currency),
            series_id,
            bin_id,
        )
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

    // --- call-price bin index the Called scan walks ---

    pub fn remove_call_bin(
        &mut self,
        reference_currency: u16,
        worldwide_day: WorldwideDay,
    ) -> Result<()> {
        self.call_bin_index(reference_currency)
            .remove_group(&CallBins(&*self, reference_currency), worldwide_day)
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub fn call_bin_groups(
        &self,
        reference_currency: u16,
        bin_id: u32,
    ) -> Result<Vec<WorldwideDay>> {
        let bins = CallBins(self, reference_currency);
        (0..call_bins::len(&bins, bin_id)?)
            .map(|index| {
                let group = call_bins::entry_at(&bins, bin_id, index)?;
                Ok(Self::unscoped(group).1)
            })
            .collect()
    }

    pub fn call_bin_group_members(
        &self,
        reference_currency: u16,
        worldwide_day: WorldwideDay,
    ) -> Result<Vec<SeriesId>> {
        self.call_bin_index(reference_currency)
            .members(worldwide_day)
    }

    pub fn call_bin_group(
        &self,
        reference_currency: u16,
        worldwide_day: WorldwideDay,
    ) -> Result<Group> {
        Ok(Group {
            iso_code: reference_currency,
            worldwide_day,
            members: self.call_bin_group_members(reference_currency, worldwide_day)?,
        })
    }

    // --- called groups awaiting their deadline ---

    /// Queues a called group on its deadline. It has left the bins, and its members stay
    /// in the group until each one expires.
    pub fn push_called(
        &mut self,
        reference_currency: u16,
        worldwide_day: WorldwideDay,
        deadline: u64,
    ) -> Result<()> {
        let key = Self::scoped(reference_currency, worldwide_day.value());
        if self.call_group_count.read(&key)? == 0 {
            return Ok(());
        }
        // A second push would orphan the first slot and credit the members twice.
        if self.called_deadline.read(&key)? != 0 {
            return Err(IntexError::GroupAlreadyIndexed {
                iso: reference_currency,
                worldwide_day,
            }
            .into());
        }
        expiry_queue::push(&ExpiryHours(self), key, deadline)
    }

    /// Hour since the epoch a deadline falls in: plain UTC, not a WorldwideDay.
    #[cfg(any(test, feature = "test-utils"))]
    pub const fn deadline_bucket(deadline: u64) -> u32 {
        expiry_queue::bucket_of(deadline)
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub const fn bucket_end(day: u32) -> u64 {
        expiry_queue::bucket_end(day)
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub fn bucket_slot_key(day: u32, slot: u32) -> B256 {
        expiry_queue::slot_key(day, slot)
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub fn first_expiry_day(&self) -> Result<Option<u32>> {
        expiry_queue::first_bucket(&ExpiryHours(self))
    }

    /// Takes a called group off the queue. Its members stay.
    #[cfg(feature = "e2e-test")]
    pub fn remove_called(
        &mut self,
        reference_currency: u16,
        worldwide_day: WorldwideDay,
    ) -> Result<()> {
        let key = Self::scoped(reference_currency, worldwide_day.value());
        expiry_queue::remove(&ExpiryHours(self), key)
    }

    /// The `index`-th member of a group.
    pub fn group_member(
        &self,
        reference_currency: u16,
        worldwide_day: WorldwideDay,
        index: u32,
    ) -> Result<SeriesId> {
        Ok(SeriesId::from_word(self.call_group_members.read(
            &Self::group_member_key(reference_currency, worldwide_day, index),
        )?))
    }

    /// Swap-removes the `index`-th member of a called group, so a re-walk never meets a
    /// series whose load already went back to the pool.
    pub fn remove_group_member(
        &mut self,
        reference_currency: u16,
        worldwide_day: WorldwideDay,
        index: u32,
    ) -> Result<()> {
        let key = Self::scoped(reference_currency, worldwide_day.value());
        let last = self
            .call_group_count
            .read(&key)?
            .checked_sub(1)
            .filter(|last| index <= *last)
            .ok_or_else(|| PrecompileError::Revert("called group member out of range".into()))?;
        let last_key = Self::group_member_key(reference_currency, worldwide_day, last);
        if index != last {
            let moved = self.call_group_members.read(&last_key)?;
            self.call_group_members.write(
                &Self::group_member_key(reference_currency, worldwide_day, index),
                moved,
            )?;
        }
        self.call_group_members.clear(&last_key)?;
        self.call_group_count.write(&key, last)
    }
}

impl<'storage> IntexContract<'storage> {
    fn call_bin_index(&self, reference_currency: u16) -> GroupIndex<'storage> {
        GroupIndex {
            group_count: self.call_group_count.clone(),
            group_members: self.call_group_members.clone(),
            iso: reference_currency,
        }
    }
}

/// One day's series in one reference currency: the unit a lifecycle decision is
/// taken over, since all of them share the inputs it reads.
pub struct Group {
    pub iso_code: u16,
    pub worldwide_day: WorldwideDay,
    pub members: Vec<SeriesId>,
}

/// One currency's two-level index: price bins hold worldwide-day groups, and
/// each group holds the series that share its decision inputs.
struct GroupIndex<'storage> {
    group_count: Map<'storage, u64, u32>,
    group_members: Map<'storage, B256, U256>,
    iso: u16,
}

impl GroupIndex<'_> {
    fn group_key(&self, worldwide_day: WorldwideDay) -> u64 {
        IntexContract::scoped(self.iso, worldwide_day.value())
    }

    fn member_key(&self, worldwide_day: WorldwideDay, index: u32) -> B256 {
        IntexContract::group_member_key(self.iso, worldwide_day, index)
    }

    fn members(&self, worldwide_day: WorldwideDay) -> Result<Vec<SeriesId>> {
        let count = self.group_count.read(&self.group_key(worldwide_day))?;
        let mut members = Vec::with_capacity(count as usize);
        for index in 0..count {
            members.push(SeriesId::from_word(
                self.group_members
                    .read(&self.member_key(worldwide_day, index))?,
            ));
        }
        Ok(members)
    }

    /// Append `series_id` to its day's group, creating it in `bin_id` when first.
    /// A member priced into another bin would split the group's decision: refused.
    fn insert(&self, bins: &CallBins<'_, '_>, series_id: SeriesId, bin_id: u32) -> Result<()> {
        let worldwide_day = series_id.worldwide_day();
        let group_key = self.group_key(worldwide_day);
        let count = self.group_count.read(&group_key)?;
        if count == 0 {
            call_bins::insert(bins, group_key, bin_id)?;
        } else {
            let expected = call_bins::bin_of(bins, group_key)?.unwrap_or_default();
            if expected != bin_id {
                return Err(IntexError::GroupBinMismatch {
                    iso: self.iso,
                    worldwide_day,
                    expected,
                    got: bin_id,
                }
                .into());
            }
        }
        self.group_members
            .write(&self.member_key(worldwide_day, count), series_id.to_word())?;
        self.group_count.write(&group_key, count + 1)?;
        Ok(())
    }

    /// Takes the group out of its bin. Its members stay for the expiry sweep.
    fn remove_group(&self, bins: &CallBins<'_, '_>, worldwide_day: WorldwideDay) -> Result<()> {
        call_bins::remove(bins, self.group_key(worldwide_day))?;
        Ok(())
    }
}

/// Buckets holding a called group whose settlement window has not closed yet.
pub struct ExpiryHours<'a, 'b>(pub &'a IntexContract<'b>);

outbe_primitives::impl_expiry_queue!(ExpiryHours<u64> {
    root: expiry_tree_root,
    mid: expiry_tree_mid,
    leaf: expiry_tree_leaf,
    len: expiry_bucket_len,
    live: expiry_bucket_live,
    at: expiry_bucket_at,
    slot: called_group_slot,
    deadline: called_deadline,
    sweep_bucket: expiry_sweep_hour,
    cursor: expiry_cursor,
});

/// The call-price bins of one reference currency, holding its worldwide-day groups.
pub struct CallBins<'a, 'b>(pub &'a IntexContract<'b>, pub u16);

outbe_primitives::impl_call_bins!(CallBins<u64> {
    root: call_bin_tree_root,
    mid: call_bin_tree_mid,
    leaf: call_bin_tree_leaf,
    count: call_bin_count,
    at: call_bin_groups,
    slot: call_group_slot,
    cursor: call_bin_cursor,
    failed: call_scan_failed_day,
});
