//! Storage CRUD and dense enumeration helpers for the Intex module.
//!
//! All functions take a short-lived `&IntexContract` (or `&mut` for
//! writes) constructed via `IntexContract::new(storage)`. They only
//! touch local storage; orchestration and validation live in `api.rs`.

use alloy_primitives::U256;
use outbe_primitives::error::Result;
use outbe_primitives::storage::types::Storable;
use outbe_primitives::time::WorldwideDay;

use crate::errors::IntexError;
use crate::schema::{CertifiedPayoutRound, IntexContract, SeriesId, SeriesRecord};

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

    /// Whether the day has produced any series yet.
    pub(crate) fn day_has_series(&self, worldwide_day: WorldwideDay) -> Result<bool> {
        Ok(self.day_series_count.read(&worldwide_day)? != 0)
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
    // Creator-reward: certified payout round + paid-leaf bitmap
    // ---------------------------------------------------------------------

    pub(crate) fn get_payout_round(&self, wwd: u32) -> Result<Option<CertifiedPayoutRound>> {
        self.ocomp_payout_round.get(wwd)
    }

    pub(crate) fn create_payout_round(&mut self, record: &CertifiedPayoutRound) -> Result<()> {
        self.ocomp_payout_round.create(record)
    }

    pub(crate) fn update_payout_round(&mut self, record: &CertifiedPayoutRound) -> Result<()> {
        self.ocomp_payout_round.update(record)
    }

    pub(crate) fn read_paid_word(&self, wwd: u32, word_index: u32) -> Result<U256> {
        self.ocomp_paid_leaves
            .read(&Self::paid_bitmap_key(wwd, word_index))
    }

    pub(crate) fn write_paid_word(&mut self, wwd: u32, word_index: u32, word: U256) -> Result<()> {
        self.ocomp_paid_leaves
            .write(&Self::paid_bitmap_key(wwd, word_index), word)
    }

    // ---------------------------------------------------------------------
    // Creator-reward: proceeds fan-in (awaiting set, dense swap-pop)
    // ---------------------------------------------------------------------

    /// Append a series to the awaiting-proceeds set (idempotent).
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

    /// Remove a series from the awaiting-proceeds set via swap-remove (idempotent).
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
