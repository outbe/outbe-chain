//! Local storage helpers for the IntexFactory module (settlement bookkeeping
//! + the call-price bin index). Orchestration lives in `runtime.rs`.

use alloy_primitives::{Address, B256, U256};
use outbe_intex::SeriesId;
use outbe_primitives::call_bins;
use outbe_primitives::call_breach::{self, ScanTerms};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::expiry_queue;
use outbe_primitives::storage::dsl::Map;
use outbe_primitives::storage::types::Storable;
use outbe_primitives::time::WorldwideDay;

use crate::errors::IntexFactoryError;
use crate::schema::IntexFactoryContract;

impl IntexFactoryContract<'_> {
    // --- mineSeq ---

    pub(crate) fn read_mine_seq(&self, series_id: SeriesId, owner: Address) -> Result<u32> {
        let key = Self::mine_seq_key(series_id, owner);
        self.mine_seq.read(&key)
    }

    pub(crate) fn write_mine_seq(
        &mut self,
        series_id: SeriesId,
        owner: Address,
        value: u32,
    ) -> Result<()> {
        let key = Self::mine_seq_key(series_id, owner);
        self.mine_seq.write(&key, value)
    }

    // --- bin keys ---

    /// Map a six-decimal COEN/ISO price to its LB-style bin id (bounded by the codec).
    pub fn price_to_bin(price: U256) -> Result<u32> {
        call_bins::price_to_bin(price)
    }

    pub(crate) fn bin_index_key(reference_currency: u16, bin_id: u32, index: u32) -> B256 {
        call_bins::bin_index_key(reference_currency, bin_id, index)
    }

    /// Composite key for a group's member list. It uses the layout of `bin_index_key`,
    /// over a separate column keyed by worldwide day instead of bin id.
    pub(crate) fn group_member_key(
        reference_currency: u16,
        worldwide_day: WorldwideDay,
        index: u32,
    ) -> B256 {
        Self::bin_index_key(reference_currency, worldwide_day.value(), index)
    }

    /// Enroll a series in its call-price bin. Its day's group is created with the first member.
    pub(crate) fn insert_call_bin(
        &mut self,
        series_id: SeriesId,
        reference_currency: u16,
        call_price: U256,
    ) -> Result<()> {
        let bin_id = Self::price_to_bin(call_price)?;
        self.call_bin_index(reference_currency).insert(
            &CallBins(&*self, reference_currency),
            series_id,
            bin_id,
        )
    }

    /// Widen the currency's stored terms to cover a newly issued series.
    pub(crate) fn widen_call_terms(
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
    pub(crate) fn scan_call_terms(
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

    pub(crate) fn remove_call_bin_group(
        &mut self,
        reference_currency: u16,
        worldwide_day: WorldwideDay,
    ) -> Result<()> {
        self.call_bin_index(reference_currency)
            .remove_group(&CallBins(&*self, reference_currency), worldwide_day)
    }

    #[cfg(test)]
    pub(crate) fn call_bin_groups(
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

    pub(crate) fn call_bin_group_members(
        &self,
        reference_currency: u16,
        worldwide_day: WorldwideDay,
    ) -> Result<Vec<SeriesId>> {
        self.call_bin_index(reference_currency)
            .members(worldwide_day)
    }

    pub(crate) fn call_bin_group(
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
    pub(crate) fn push_called_group(
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
        if self.called_group_deadline.read(&key)? != 0 {
            return Err(IntexFactoryError::GroupAlreadyIndexed {
                iso: reference_currency,
                worldwide_day,
            }
            .into());
        }
        expiry_queue::push(&ExpiryHours(self), key, deadline)
    }

    /// Hour since the epoch a deadline falls in: plain UTC, not a WorldwideDay.
    #[cfg(test)]
    pub(crate) const fn deadline_bucket(deadline: u64) -> u32 {
        expiry_queue::bucket_of(deadline)
    }

    #[cfg(test)]
    pub(crate) const fn bucket_end(day: u32) -> u64 {
        expiry_queue::bucket_end(day)
    }

    #[cfg(test)]
    pub(crate) fn bucket_slot_key(day: u32, slot: u32) -> B256 {
        expiry_queue::slot_key(day, slot)
    }

    #[cfg(test)]
    pub(crate) fn first_expiry_day(&self) -> Result<Option<u32>> {
        expiry_queue::first_bucket(&ExpiryHours(self))
    }

    /// Takes a called group off the queue. Its members stay.
    #[cfg(feature = "e2e-test")]
    pub(crate) fn remove_called_group(
        &mut self,
        reference_currency: u16,
        worldwide_day: WorldwideDay,
    ) -> Result<()> {
        let key = Self::scoped(reference_currency, worldwide_day.value());
        expiry_queue::remove(&ExpiryHours(self), key)
    }

    /// The `index`-th member of a group.
    pub(crate) fn group_member(
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
    pub(crate) fn remove_group_member(
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

impl<'storage> IntexFactoryContract<'storage> {
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
pub(crate) struct Group {
    pub(crate) iso_code: u16,
    pub(crate) worldwide_day: WorldwideDay,
    pub(crate) members: Vec<SeriesId>,
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
        IntexFactoryContract::scoped(self.iso, worldwide_day.value())
    }

    fn member_key(&self, worldwide_day: WorldwideDay, index: u32) -> B256 {
        IntexFactoryContract::group_member_key(self.iso, worldwide_day, index)
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
                return Err(IntexFactoryError::GroupBinMismatch {
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
pub(crate) struct ExpiryHours<'a, 'b>(pub(crate) &'a IntexFactoryContract<'b>);

outbe_primitives::impl_expiry_queue!(ExpiryHours<u64> {
    root: expiry_tree_root,
    mid: expiry_tree_mid,
    leaf: expiry_tree_leaf,
    len: expiry_bucket_len,
    live: expiry_bucket_live,
    at: expiry_bucket_at,
    slot: called_group_slot,
    deadline: called_group_deadline,
    sweep_bucket: expiry_sweep_day,
    cursor: expiry_cursor,
});

/// The call-price bins of one reference currency, holding its worldwide-day groups.
pub(crate) struct CallBins<'a, 'b>(pub(crate) &'a IntexFactoryContract<'b>, pub(crate) u16);

outbe_primitives::impl_call_bins!(CallBins<u64> {
    root: call_bin_tree_root,
    mid: call_bin_tree_mid,
    leaf: call_bin_tree_leaf,
    count: call_bin_count,
    at: call_bin_groups,
    slot: call_group_slot,
    cursor: call_scan_cursor,
    failed: call_scan_failed_day,
});
