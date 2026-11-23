mod forfeiture;
pub use forfeiture::{LysisOutcome, PricedCurrencies};

use alloy_primitives::U256;
use outbe_core_common::wwd_entity_id::WwdEntityId;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_system_common::metadosis_bindings::MetadosisStorageExt;
use outbe_system_common::time::{EmissionDate, WorldwideDay};

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

    pub fn seal_worldwide_day(&mut self, day: WorldwideDay) -> Result<()> {
        outbe_worldwideday::seal_worldwide_day(self.storage_handle(), day)
    }

    /// Seals the Lysis of emission day `emission_day` over the priced Tributes of `day`.
    pub fn compute_and_record_metadosis_fractions(
        &mut self,
        day: WorldwideDay,
        emission_day: EmissionDate,
        priced: &PricedCurrencies,
        metadosis_limit_minor: U256,
        completed_at: u64,
    ) -> Result<LysisOutcome> {
        self.storage_handle().require_metadosis_process_ready()?;
        forfeiture::compute_and_record_metadosis_fractions(
            self,
            (day, emission_day),
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
