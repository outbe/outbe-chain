mod forfeiture;
pub use forfeiture::{LysisOutcome, PricedCurrencies};

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

impl TributeContract<'_> {
    /// Retires an empty day in the same state checkpoint as completion.
    pub fn retire_completed_partition(&mut self, day: WorldwideDay) -> Result<RetirementOutcome> {
        let storage = self.storage_handle();
        storage.require_metadosis_process_ready()?;
        storage.with_checkpoint(|| self.retire_completed_partition_inner(day))
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
    /// Resolves the Oracle UTC day (`emission_day`) that prices `day`.
    pub fn resolve_price_day(&self, day: WorldwideDay, completed_at: u64) -> Result<u32> {
        forfeiture::resolve_price_day(self, day, completed_at)
    }

    /// Seals the Lysis of `day` in the enclave over its priced Tributes: the enclave takes the
    /// Lysis Limit of their nominal under `metadosis_limit_minor`, the fractions are stored with
    /// `completed_at`, and the series loads are sealed. Unpriced Tributes get no series.
    pub fn compute_and_record_metadosis_fractions(
        &mut self,
        day: WorldwideDay,
        priced: &PricedCurrencies,
        metadosis_limit_minor: U256,
        completed_at: u64,
    ) -> Result<LysisOutcome> {
        self.storage_handle().require_metadosis_process_ready()?;
        forfeiture::compute_and_record_metadosis_fractions(
            self,
            day,
            priced,
            metadosis_limit_minor,
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
