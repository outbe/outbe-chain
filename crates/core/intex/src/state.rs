//! Storage CRUD and dense enumeration helpers for the Intex module.
//!
//! All functions take a short-lived `&IntexContract` (or `&mut` for
//! writes) constructed via `IntexContract::new(storage)`. They only
//! touch local storage; orchestration and validation live in `api.rs`.

use alloy_primitives::{Address, U256};
use outbe_primitives::error::Result;

use crate::errors::IntexError;
use crate::schema::{DistProgress, IntexContract, SeriesRecord};

impl IntexContract<'_> {
    // ---------------------------------------------------------------------
    // Series CRUD
    // ---------------------------------------------------------------------

    pub(crate) fn series_exists(&self, series_id: u32) -> Result<bool> {
        self.series.exists(series_id)
    }

    pub(crate) fn get_series(&self, series_id: u32) -> Result<Option<SeriesRecord>> {
        self.series.get(series_id)
    }

    pub(crate) fn load_series(&self, series_id: u32) -> Result<SeriesRecord> {
        self.series
            .get(series_id)?
            .ok_or_else(|| IntexError::SeriesNotFound.into())
    }

    /// Create a new series record and append it to the global enumeration.
    /// The underlying record `create` rejects a duplicate `series_id`.
    pub(crate) fn create_series_record(&mut self, record: &SeriesRecord) -> Result<()> {
        self.series.create(record)?;
        self.append_to_global_index(record.series_id)
    }

    pub(crate) fn update_series_record(&mut self, record: &SeriesRecord) -> Result<()> {
        self.series.update(record)
    }

    // ---------------------------------------------------------------------
    // Global dense index for enumeration
    // ---------------------------------------------------------------------

    fn append_to_global_index(&mut self, series_id: u32) -> Result<()> {
        let total = self.total_series.read()?;
        self.series_id_at_index.write(&total, series_id)?;
        self.total_series.write(total + 1)?;
        Ok(())
    }

    pub(crate) fn read_total_series(&self) -> Result<u64> {
        self.total_series.read()
    }

    pub(crate) fn read_series_id_at(&self, index: u64) -> Result<u32> {
        self.series_id_at_index.read(&index)
    }

    // ---------------------------------------------------------------------
    // Creator-reward: per-series contributors (owner -> nominal share)
    // ---------------------------------------------------------------------

    /// Persist the (pre-deduplicated) contributor list for a series: per-index
    /// owner + nominal, the count, and the nominal total. Called once per
    /// series (lysis aggregates per owner upstream).
    pub(crate) fn write_contributors(
        &mut self,
        series_id: u32,
        contributors: &[(Address, U256)],
    ) -> Result<()> {
        let mut total = U256::ZERO;
        for (i, (owner, nominal)) in contributors.iter().enumerate() {
            let key = Self::contributor_index_key(series_id, i as u32);
            self.contributor_owner_at.write(&key, *owner)?;
            self.contributor_nominal_at.write(&key, *nominal)?;
            total += *nominal;
        }
        self.contributor_count
            .write(&series_id, contributors.len() as u32)?;
        self.contributor_total.write(&series_id, total)?;
        Ok(())
    }

    pub(crate) fn read_contributor_count(&self, series_id: u32) -> Result<u32> {
        self.contributor_count.read(&series_id)
    }

    pub(crate) fn read_contributor_total(&self, series_id: u32) -> Result<U256> {
        self.contributor_total.read(&series_id)
    }

    pub(crate) fn read_contributor_at(
        &self,
        series_id: u32,
        index: u32,
    ) -> Result<(Address, U256)> {
        let key = Self::contributor_index_key(series_id, index);
        let owner = self.contributor_owner_at.read(&key)?;
        let nominal = self.contributor_nominal_at.read(&key)?;
        Ok((owner, nominal))
    }

    /// Clear all contributor storage for a series (count, per-index entries, total).
    pub(crate) fn clear_contributors(&mut self, series_id: u32) -> Result<()> {
        let count = self.contributor_count.read(&series_id)?;
        for i in 0..count {
            let key = Self::contributor_index_key(series_id, i);
            self.contributor_owner_at.clear(&key)?;
            self.contributor_nominal_at.clear(&key)?;
        }
        self.contributor_count.clear(&series_id)?;
        self.contributor_total.clear(&series_id)?;
        Ok(())
    }

    // ---------------------------------------------------------------------
    // Creator-reward: paginated distribution progress + active set
    // ---------------------------------------------------------------------

    pub(crate) fn get_dist_progress(&self, series_id: u32) -> Result<Option<DistProgress>> {
        self.dist_progress.get(series_id)
    }

    pub(crate) fn create_dist_progress(&mut self, record: &DistProgress) -> Result<()> {
        self.dist_progress.create(record)
    }

    pub(crate) fn update_dist_progress(&mut self, record: &DistProgress) -> Result<()> {
        self.dist_progress.update(record)
    }

    pub(crate) fn delete_dist_progress(&mut self, series_id: u32) -> Result<()> {
        self.dist_progress.delete(series_id)
    }

    pub(crate) fn read_active_dist_count(&self) -> Result<u32> {
        self.active_dist_count.read()
    }

    pub(crate) fn read_active_dist_at(&self, index: u32) -> Result<u32> {
        self.active_dist_at.read(&index)
    }

    /// Append a series to the active-distribution set (idempotent).
    pub(crate) fn push_active_dist(&mut self, series_id: u32) -> Result<()> {
        if self.active_dist_slot.read(&series_id)? != 0 {
            return Ok(());
        }
        let count = self.active_dist_count.read()?;
        self.active_dist_at.write(&count, series_id)?;
        // store index + 1 so that 0 unambiguously means "absent".
        self.active_dist_slot.write(&series_id, count + 1)?;
        self.active_dist_count.write(count + 1)?;
        Ok(())
    }

    /// Remove a series from the active-distribution set via swap-remove (idempotent).
    pub(crate) fn remove_active_dist(&mut self, series_id: u32) -> Result<()> {
        let slot1 = self.active_dist_slot.read(&series_id)?;
        if slot1 == 0 {
            return Ok(());
        }
        let idx = slot1 - 1;
        let last = self.active_dist_count.read()? - 1;
        if idx != last {
            let last_series = self.active_dist_at.read(&last)?;
            self.active_dist_at.write(&idx, last_series)?;
            self.active_dist_slot.write(&last_series, idx + 1)?;
        }
        self.active_dist_at.clear(&last)?;
        self.active_dist_slot.clear(&series_id)?;
        self.active_dist_count.write(last)?;
        Ok(())
    }

    // ---------------------------------------------------------------------
    // Creator-reward: proceeds fan-in (awaiting set, dense swap-pop)
    // ---------------------------------------------------------------------

    /// Append a series to the awaiting-proceeds set (idempotent).
    pub(crate) fn push_awaiting_proceeds(&mut self, series_id: u32) -> Result<()> {
        if self.awaiting_proceeds_slot.read(&series_id)? != 0 {
            return Ok(());
        }
        let count = self.awaiting_proceeds_count.read()?;
        self.awaiting_proceeds_at.write(&count, series_id)?;
        // store index + 1 so that 0 unambiguously means "absent".
        self.awaiting_proceeds_slot.write(&series_id, count + 1)?;
        self.awaiting_proceeds_count.write(count + 1)?;
        Ok(())
    }

    /// Remove a series from the awaiting-proceeds set via swap-remove (idempotent).
    pub(crate) fn remove_awaiting_proceeds(&mut self, series_id: u32) -> Result<()> {
        let slot1 = self.awaiting_proceeds_slot.read(&series_id)?;
        if slot1 == 0 {
            return Ok(());
        }
        let idx = slot1 - 1;
        let last = self.awaiting_proceeds_count.read()? - 1;
        if idx != last {
            let last_series = self.awaiting_proceeds_at.read(&last)?;
            self.awaiting_proceeds_at.write(&idx, last_series)?;
            self.awaiting_proceeds_slot.write(&last_series, idx + 1)?;
        }
        self.awaiting_proceeds_at.clear(&last)?;
        self.awaiting_proceeds_slot.clear(&series_id)?;
        self.awaiting_proceeds_count.write(last)?;
        Ok(())
    }
}
