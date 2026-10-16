mod forfeiture;

use alloy_primitives::U256;
use outbe_primitives::error::Result;
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::wwd_entity_id::WwdEntityId;

use crate::errors::TributeError;
use crate::precompile::ITribute;
use crate::schema::TributeContract;

pub use crate::state_api::LoadedTribute;

/// Result of a logical Tribute day retirement. Stored ciphertext remains available.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetirementOutcome {
    NotPresent,
    Requested,
}

/// Constant-size owner receipt for a sealed Tribute partition forfeited by
/// Metadosis retained-cap policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TributeForfeitureReceipt {
    pub worldwide_day: WorldwideDay,
    pub forfeited_count: u32,
    pub forfeited_nominal: U256,
    pub source_generation: u64,
    pub retired_generation: u64,
    pub retirement_outcome: RetirementOutcome,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TributeLysisReceipt {
    pub worldwide_day: WorldwideDay,
    pub consumed_count: u32,
    pub consumed_nominal: U256,
    pub source_generation: u64,
    pub retired_generation: u64,
    pub retirement_outcome: RetirementOutcome,
}

impl TributeContract<'_> {
    pub fn consume_and_retire_lysis_partition(
        &mut self,
        day: WorldwideDay,
        verified_count: u32,
        verified_nominal: U256,
    ) -> Result<TributeLysisReceipt> {
        let storage = self.storage_handle();
        storage.require_metadosis_process_ready()?;
        storage.with_checkpoint(|| {
            let totals = self.get_day_totals(day)?;
            if self.retired_days.read(&day)?
                || !totals.initialized
                || !totals.is_sealed
                || verified_count == 0
                || totals.tribute_count != verified_count
                || totals.tribute_nominal_total_minor != verified_nominal
            {
                return Err(TributeError::LysisInputMismatch.into());
            }
            self.freeze_day_nominal(&totals)?;
            self.consume_lysis_partition_inner(day, verified_count, verified_nominal)?;
            let retirement_outcome = self.retire_completed_partition_inner(day)?;
            if retirement_outcome != RetirementOutcome::Requested {
                return Err(TributeError::LysisInputMismatch.into());
            }
            Ok(TributeLysisReceipt {
                worldwide_day: day,
                consumed_count: verified_count,
                consumed_nominal: verified_nominal,
                source_generation: 0,
                retired_generation: 1,
                retirement_outcome,
            })
        })
    }

    /// Applies ADR-011's one bulk accounting transition after every verified
    /// Tribute in the sealed WWD has produced exactly one Nod.
    pub fn consume_lysis_partition(
        &mut self,
        day: WorldwideDay,
        verified_count: u32,
        verified_nominal: alloy_primitives::U256,
    ) -> Result<()> {
        self.consume_lysis_partition_inner(day, verified_count, verified_nominal)
    }

    pub(crate) fn consume_lysis_partition_inner(
        &mut self,
        day: WorldwideDay,
        verified_count: u32,
        verified_nominal: alloy_primitives::U256,
    ) -> Result<()> {
        let mut totals = self.get_day_totals(day)?;
        if !totals.initialized
            || !totals.is_sealed
            || totals.tribute_count != verified_count
            || totals.tribute_nominal_total_minor != verified_nominal
        {
            return Err(TributeError::LysisInputMismatch.into());
        }
        let supply = self
            .total_supply
            .read()?
            .checked_sub(u64::from(verified_count))
            .ok_or(TributeError::TotalSupplyUnderflowDuringLysis)?;
        self.total_supply.write(supply)?;
        self.reset_day_nominal(&totals)?;
        totals.tribute_count = 0;
        totals.tribute_nominal_total_minor = alloy_primitives::U256::ZERO;
        self.store_day_totals(&totals)
    }

    /// Retires a sealed empty day in the same state checkpoint as completion.
    pub fn retire_completed_partition(&mut self, day: WorldwideDay) -> Result<RetirementOutcome> {
        let storage = self.storage_handle();
        storage.with_checkpoint(|| self.retire_completed_partition_inner(day))
    }

    /// Retires a sealed empty day when OFFERING was never opened.
    pub fn retire_empty_missed_offering_partition(
        &mut self,
        day: WorldwideDay,
    ) -> Result<RetirementOutcome> {
        let storage = self.storage_handle();
        storage.with_checkpoint(|| {
            let totals = self.get_day_totals(day)?;
            if !totals.initialized
                || !totals.is_sealed
                || totals.tribute_count != 0
                || !totals.tribute_nominal_total_minor.is_zero()
            {
                return Err(TributeError::MissedOfferingRequiresSealedEmptyPartition.into());
            }
            self.retire_completed_partition_inner(day)
        })
    }

    /// Forfeits one sealed partition from its frozen state aggregates.
    /// The operation retains all ciphertext records and indexes.
    pub fn forfeit_sealed_partition(
        &mut self,
        day: WorldwideDay,
    ) -> Result<TributeForfeitureReceipt> {
        let storage = self.storage_handle();
        storage.with_checkpoint(|| forfeiture::apply(self, day))
    }

    pub(crate) fn retire_completed_partition_inner(
        &mut self,
        day: WorldwideDay,
    ) -> Result<RetirementOutcome> {
        Ok(
            if crate::state_api::retire_completed_day(&self.storage_handle(), day)? {
                RetirementOutcome::Requested
            } else {
                RetirementOutcome::NotPresent
            },
        )
    }

    pub fn burn(&mut self, tribute_id: WwdEntityId) -> Result<()> {
        let loaded = self
            .load_tribute(tribute_id)?
            .ok_or(TributeError::TributeNotFound)?;
        self.burn_loaded(loaded)
    }

    /// Loads one Tribute while retaining its verified generic mutation capability.
    pub fn load_tribute(&self, tribute_id: WwdEntityId) -> Result<Option<LoadedTribute>> {
        crate::state_api::load(&self.storage_handle(), tribute_id)
    }

    /// Burns the exact state record retained by the loaded capability.
    pub fn burn_loaded(&mut self, loaded: LoadedTribute) -> Result<()> {
        crate::state_api::burn_loaded(&self.storage_handle(), loaded)
    }

    pub fn burn_all_by_wwd(&mut self, day: WorldwideDay) -> Result<()> {
        let storage = self.storage_handle();
        storage.with_checkpoint(|| {
            for id in self.get_tribute_ids_by_day(day)? {
                self.burn(id)?;
            }
            Ok(())
        })
    }

    pub fn seal_day(&mut self, day: WorldwideDay) -> Result<()> {
        self.set_day_sealed(day, true)?;
        self.emit(ITribute::TributeWorldwideDaySealed {
            worldwideDay: day.into(),
            isSealed: true,
        })?;
        Ok(())
    }

    pub fn unseal_day(&mut self, day: WorldwideDay) -> Result<()> {
        if self.retired_days.read(&day)? || self.day_amount_record(day, true)?.is_some() {
            return Err(TributeError::WorldwideDaySealed.into());
        }
        self.set_day_sealed(day, false)?;
        self.emit(ITribute::TributeWorldwideDaySealed {
            worldwideDay: day.into(),
            isSealed: false,
        })?;
        Ok(())
    }
}
