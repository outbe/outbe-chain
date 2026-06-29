//! Storage CRUD and dense enumeration helpers for the Intex module.
//!
//! All functions take a short-lived `&IntexContract` (or `&mut` for
//! writes) constructed via `IntexContract::new(storage)`. They only
//! touch local storage; orchestration and validation live in `api.rs`.

use outbe_primitives::error::Result;

use crate::errors::IntexError;
use crate::schema::{IntexContract, SeriesRecord};

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
}
