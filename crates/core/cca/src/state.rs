//! Storage CRUD and index helpers for the CCA registry.
//!
//! Local storage only; orchestration and the multiplier arithmetic live in
//! `runtime.rs`.

use alloy_primitives::{Address, U256};
use outbe_common::WorldwideDay;
use outbe_primitives::error::Result;

use crate::errors::CcaError;
use crate::schema::{CcaContract, CcaRecord};

impl CcaContract<'_> {
    // ---------------------------------------------------------------------
    // Registry CRUD
    // ---------------------------------------------------------------------

    pub(crate) fn cca_exists(&self, cca: Address) -> Result<bool> {
        self.ccas.exists(cca)
    }

    pub(crate) fn load_cca(&self, cca: Address) -> Result<CcaRecord> {
        self.ccas
            .get(cca)?
            .ok_or_else(|| CcaError::NotRegistered.into())
    }

    pub(crate) fn create_cca_record(&mut self, record: &CcaRecord) -> Result<()> {
        self.ccas.create(record)
    }

    pub(crate) fn update_cca_record(&mut self, record: &CcaRecord) -> Result<()> {
        self.ccas.update(record)
    }

    // ---------------------------------------------------------------------
    // Per-day origination units
    // ---------------------------------------------------------------------

    pub(crate) fn read_day_units(&self, day: WorldwideDay, cca: Address) -> Result<U256> {
        self.day_units.read(&CcaContract::day_unit_key(day, cca))
    }

    /// Adds `units` to a CCA's tally for `day`, appending it to that day's dense
    /// index the first time it earns anything.
    pub(crate) fn add_day_units(
        &mut self,
        day: WorldwideDay,
        cca: Address,
        units: U256,
    ) -> Result<()> {
        let key = CcaContract::day_unit_key(day, cca);
        let current = self.day_units.read(&key)?;
        if current.is_zero() {
            let count = self.day_cca_count.read(&day)?;
            self.day_ccas
                .write(&CcaContract::day_index_key(day, count), cca)?;
            self.day_cca_count.write(&day, count.saturating_add(1))?;
        }
        let next = current
            .checked_add(units)
            .ok_or(CcaError::ArithmeticOverflow)?;
        self.day_units.write(&key, next)
    }

    pub(crate) fn read_day_cca_count(&self, day: WorldwideDay) -> Result<u32> {
        self.day_cca_count.read(&day)
    }

    pub(crate) fn read_day_cca_at(&self, day: WorldwideDay, index: u32) -> Result<Address> {
        self.day_ccas.read(&CcaContract::day_index_key(day, index))
    }

    // ---------------------------------------------------------------------
    // Repeat-owner decay and the one-unit-per-owner-per-day guard
    // ---------------------------------------------------------------------

    pub(crate) fn read_owner_origination_count(&self, cca: Address, owner: Address) -> Result<u32> {
        self.owner_origination_counts
            .read(&CcaContract::owner_key(cca, owner))
    }

    pub(crate) fn bump_owner_origination_count(
        &mut self,
        cca: Address,
        owner: Address,
    ) -> Result<()> {
        let key = CcaContract::owner_key(cca, owner);
        let count = self.owner_origination_counts.read(&key)?;
        self.owner_origination_counts
            .write(&key, count.saturating_add(1))
    }

    pub(crate) fn read_owner_day_seen(
        &self,
        day: WorldwideDay,
        cca: Address,
        owner: Address,
    ) -> Result<bool> {
        self.owner_day_seen
            .read(&CcaContract::owner_day_key(day, cca, owner))
    }

    pub(crate) fn mark_owner_day_seen(
        &mut self,
        day: WorldwideDay,
        cca: Address,
        owner: Address,
    ) -> Result<()> {
        self.owner_day_seen
            .write(&CcaContract::owner_day_key(day, cca, owner), true)
    }
}
