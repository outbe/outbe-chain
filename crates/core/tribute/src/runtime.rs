mod forfeiture;

use alloy_primitives::U256;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::wwd_entity_id::WwdEntityId;

use crate::errors::{date_step_error, TributeError};
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
    fn is_valid_lysis_partition(
        &self,
        day: WorldwideDay,
        totals: &crate::schema::DayTotals,
        verified_count: u32,
        verified_nominal: U256,
    ) -> Result<bool> {
        if self.retired_days.read(&day)? {
            return Ok(false);
        }
        let ready = totals.initialized && totals.is_sealed && verified_count > 0;
        let matches = totals.tribute_count == verified_count
            && totals.tribute_nominal_total_minor == verified_nominal;
        Ok(ready && matches)
    }

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
            if !self.is_valid_lysis_partition(day, &totals, verified_count, verified_nominal)? {
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
        self.storage_handle().require_metadosis_process_ready()?;
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
        self.reset_day_nominal(&totals)?;
        totals.tribute_count = 0;
        totals.tribute_nominal_total_minor = alloy_primitives::U256::ZERO;
        self.store_day_totals(&totals)
    }

    /// Retires a sealed empty day in the same state checkpoint as completion.
    pub fn retire_completed_partition(&mut self, day: WorldwideDay) -> Result<RetirementOutcome> {
        let storage = self.storage_handle();
        storage.require_metadosis_process_ready()?;
        storage.with_checkpoint(|| self.retire_completed_partition_inner(day))
    }

    /// Retires a sealed empty day when OFFERING was never opened.
    pub fn retire_empty_missed_offering_partition(
        &mut self,
        day: WorldwideDay,
    ) -> Result<RetirementOutcome> {
        let storage = self.storage_handle();
        storage.require_metadosis_process_ready()?;
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
        storage.require_metadosis_process_ready()?;
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

    /// Test helper. Production deletion is only `issue_nod` after Nod issuance.
    #[cfg(test)]
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

    /// Test helper. Production deletion is only `issue_nod` after Nod issuance.
    #[cfg(test)]
    pub fn burn_loaded(&mut self, loaded: LoadedTribute) -> Result<()> {
        crate::state_api::burn_loaded(&self.storage_handle(), loaded)
    }

    /// Test helper. Production deletion is only `issue_nod` after Nod issuance.
    #[cfg(test)]
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
        require_day_seal_lease(&self.storage_handle(), day)?;
        self.set_day_sealed(day, true)?;
        self.emit(ITribute::WorldwideDaySealed {
            worldwideDay: day.into(),
        })?;
        Ok(())
    }

    /// Records the Stage 3 Metadosis league fractions for `day` as a sealed enclave record.
    pub fn record_metadosis_fractions(
        &mut self,
        day: WorldwideDay,
        fractions: &std::collections::BTreeMap<u16, u64>,
        completed_at: u64,
    ) -> Result<()> {
        self.storage_handle().require_metadosis_process_ready()?;
        let chain_id = self.storage_handle().chain_id()?;
        let mut context = b"outbe/tribute/record-fractions/v1".to_vec();
        context.extend_from_slice(&day.value().to_be_bytes());
        context.extend_from_slice(&completed_at.to_be_bytes());
        let sealed = crate::enclave_client::seal_day_fractions(
            chain_id,
            day,
            completed_at,
            alloy_primitives::keccak256(context),
            fractions,
        )
        .map_err(TributeError::DayEnclave)?;
        self.day_metadosis_completed_at.write(&day, completed_at)?;
        self.store_day_fractions_record(day, &sealed)?;
        Ok(())
    }

    /// Resolves the Oracle UTC day (`emission_day`) that prices `day`.
    pub fn resolve_price_day(&self, day: WorldwideDay, completed_at: u64) -> Result<u32> {
        forfeiture::resolve_price_day(self, day, completed_at)
    }

    /// Computes league fractions from `fi_group_nominal_ct[day]` and `lysis_limit_minor`
    /// inside the enclave, stores the sealed fraction map along with `completed_at`,
    /// seals series unclaimed loads, and returns `(total_nominal_minor, total_allocated_minor)`.
    pub fn compute_and_record_metadosis_fractions(
        &mut self,
        day: WorldwideDay,
        lysis_limit_minor: U256,
        completed_at: u64,
    ) -> Result<(U256, U256)> {
        self.storage_handle().require_metadosis_process_ready()?;
        forfeiture::compute_and_record_metadosis_fractions(
            self,
            day,
            lysis_limit_minor,
            completed_at,
        )
    }

    /// Stage 5: Creates a `Nod` for `tribute_id` after Metadosis completion
    /// and deletes that Tribute only when Nod issuance succeeds.
    pub fn issue_nod(
        &mut self,
        caller: alloy_primitives::Address,
        tribute_id: WwdEntityId,
    ) -> Result<U256> {
        forfeiture::issue_nod_from_tribute(self, caller, tribute_id)
    }

    /// Permissionless cleanup of a forfeited Tribute (`ITribute.clean(uint256)`).
    pub fn clean(&mut self, tribute_id: WwdEntityId) -> Result<()> {
        forfeiture::clean_forfeited_tribute(self, tribute_id)
    }
}

#[derive(Clone, Copy)]
enum DayLease {
    InitGenesis,
    CycleAllocation(WorldwideDay),
    ProcessReady,
    AdvanceDue,
}

fn require_day_lease(storage: &StorageHandle<'_>, lease: DayLease) -> Result<()> {
    match lease {
        DayLease::InitGenesis => storage.require_metadosis_init_genesis(),
        DayLease::CycleAllocation(day) => storage.require_metadosis_cycle_allocation(
            day.to_timestamp_utc()
                .map_err(crate::errors::date_step_error)?,
        ),
        DayLease::ProcessReady => storage.require_metadosis_process_ready(),
        DayLease::AdvanceDue => storage.require_metadosis_advance_due(),
    }
}

/// Tries each caller binding. Only a lease identity mismatch selects the next
/// binding. Storage, borrow, and time errors return immediately.
fn accept_day_lease(storage: &StorageHandle<'_>, leases: &[DayLease]) -> Result<()> {
    let mut mismatch = None;
    for lease in leases {
        match require_day_lease(storage, *lease) {
            Ok(()) => return Ok(()),
            Err(error) if lease_identity_mismatch(&error) => mismatch = Some(error),
            Err(error) => return Err(error),
        }
    }
    Err(mismatch.unwrap_or_else(|| PrecompileError::Fatal("day lease list is empty".into())))
}

fn lease_identity_mismatch(error: &PrecompileError) -> bool {
    let PrecompileError::Revert(message) = error else {
        return false;
    };
    message == "test provider Metadosis mutation lease identity mismatch"
        || message == "Metadosis mutation frame identity mismatch"
}

/// Init-genesis, cycle allocation, advance-due, and process-ready create worldwide days.
pub fn require_worldwide_day_creation_lease(
    storage: &StorageHandle<'_>,
    day: WorldwideDay,
) -> Result<()> {
    accept_day_lease(
        storage,
        &[
            DayLease::InitGenesis,
            DayLease::CycleAllocation(day),
            DayLease::ProcessReady,
            DayLease::AdvanceDue,
        ],
    )
}

/// Creation, offering sync, failure, and settlement each seal under their own binding.
pub(crate) fn require_day_seal_lease(storage: &StorageHandle<'_>, day: WorldwideDay) -> Result<()> {
    accept_day_lease(
        storage,
        &[
            DayLease::InitGenesis,
            DayLease::CycleAllocation(day),
            DayLease::ProcessReady,
            DayLease::AdvanceDue,
        ],
    )
}
