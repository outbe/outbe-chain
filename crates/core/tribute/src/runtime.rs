mod forfeiture;

use alloy_primitives::{B256, U256};
use outbe_compressed_entities::{
    delete, read, retire_partition, EntityRef, ExecutionScope, ParentBodySource, PartitionRef,
    RetirementOutcome, VerifiedBody, WwdEntityId,
};
use outbe_primitives::error::Result;
use outbe_primitives::time::WorldwideDay;

use crate::errors::TributeError;
use crate::precompile::ITribute;
use crate::schema::{TributeContract, TributeData};
use crate::state::{record_from_verified, tribute_from_verified};

/// A semantic Tribute paired with the exact generic mutation capability that verified it.
pub struct LoadedTribute {
    body: TributeData,
    current: VerifiedBody,
}

/// Constant-size owner receipt for a sealed Tribute generation forfeited by
/// Metadosis retained-cap policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TributeForfeitureReceipt {
    pub worldwide_day: WorldwideDay,
    pub sealed_root: B256,
    pub forfeited_count: u32,
    pub forfeited_nominal: U256,
    pub source_generation: u64,
    pub retired_generation: u64,
    pub retirement_outcome: RetirementOutcome,
}

impl LoadedTribute {
    /// Converts an authenticated generic Tribute body into the domain capability.
    pub fn from_verified(current: VerifiedBody) -> Result<Self> {
        let body = tribute_from_verified(&current)?;
        Ok(Self { body, current })
    }

    #[must_use]
    pub const fn body(&self) -> &TributeData {
        &self.body
    }
}

impl TributeContract<'_> {
    /// Applies ADR-011's one bulk accounting transition after every verified
    /// Tribute in the sealed WWD has produced exactly one Nod.
    pub fn consume_lysis_partition(
        &mut self,
        day: WorldwideDay,
        verified_count: u32,
        verified_nominal: alloy_primitives::U256,
    ) -> Result<()> {
        if self.ocomp_profile_ready.read()? {
            return Err(outbe_primitives::error::PrecompileError::Revert(
                "populated OCOMP Tribute input requires certified retirement".into(),
            ));
        }
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
            return Err(
                outbe_primitives::error::PrecompileError::BodyReadCorruption(
                    "Lysis input count/nominal does not match sealed Tribute DayTotals".into(),
                ),
            );
        }
        let supply = self
            .total_supply
            .read()?
            .checked_sub(u64::from(verified_count))
            .ok_or_else(|| {
                outbe_primitives::error::PrecompileError::BodyReadCorruption(
                    "Tribute total supply underflow during Lysis".into(),
                )
            })?;
        self.total_supply.write(supply)?;
        self.reset_day_nominal(&totals)?;
        totals.tribute_count = 0;
        totals.tribute_nominal_total_minor = alloy_primitives::U256::ZERO;
        self.store_day_totals(&totals)
    }

    /// Requests the one authenticated Catalog retirement after the sealed
    /// DayTotals are empty. The legacy terminal path calls this after
    /// completion; certified activation calls the private inner transition
    /// inside the same outer checkpoint as terminal completion.
    pub fn retire_completed_partition(
        &mut self,
        scope: &ExecutionScope,
        day: WorldwideDay,
    ) -> Result<RetirementOutcome> {
        if self.ocomp_profile_ready.read()? {
            let admission = self.pre_admission_projection(day)?;
            if admission.is_sealed && admission.tribute_count != 0 {
                return Err(outbe_primitives::error::PrecompileError::Revert(
                    "populated OCOMP Tribute input requires certified retirement".into(),
                ));
            }
        }
        let storage = self.storage_handle();
        storage.with_checkpoint(|| self.retire_completed_partition_inner(scope, day))
    }

    /// Requests retirement for the one terminal policy where OFFERING was
    /// never opened. The day must still be sealed and exactly empty before any
    /// CE work is touched.
    pub fn retire_empty_missed_offering_partition(
        &mut self,
        scope: &ExecutionScope,
        day: WorldwideDay,
    ) -> Result<RetirementOutcome> {
        let ce_checkpoint = scope.ce_work_checkpoint()?;
        let storage = self.storage_handle();
        let result = storage.with_checkpoint(|| {
            let totals = self.get_day_totals(day)?;
            if !totals.initialized
                || !totals.is_sealed
                || totals.tribute_count != 0
                || !totals.tribute_nominal_total_minor.is_zero()
            {
                return Err(
                    outbe_primitives::error::PrecompileError::BodyReadCorruption(
                        "MissedOffering requires a sealed empty Tribute partition".into(),
                    ),
                );
            }
            self.retire_completed_partition_inner(scope, day)
        });
        if result.is_err() {
            scope.restore_ce_work_checkpoint(ce_checkpoint)?;
        }
        result
    }

    /// Forfeits one complete sealed Tribute generation without enumerating
    /// individual bodies. The authenticated parent partition root and owner
    /// aggregates are bound before supply, retirement, or generation changes.
    pub fn forfeit_sealed_partition(
        &mut self,
        scope: &ExecutionScope,
        day: WorldwideDay,
    ) -> Result<TributeForfeitureReceipt> {
        let ce_checkpoint = scope.ce_work_checkpoint()?;
        let storage = self.storage_handle();
        let result = storage.with_checkpoint(|| forfeiture::apply(self, scope, day));
        if result.is_err() {
            scope.restore_ce_work_checkpoint(ce_checkpoint)?;
        }
        result
    }

    pub(crate) fn retire_completed_partition_inner(
        &mut self,
        scope: &ExecutionScope,
        day: WorldwideDay,
    ) -> Result<RetirementOutcome> {
        let outcome =
            retire_partition(self.storage_handle(), scope, PartitionRef::TributeWwd(day))?;
        if outcome == RetirementOutcome::NotPresent {
            return Ok(outcome);
        }

        let totals = self.get_day_totals(day)?;
        if !totals.initialized
            || !totals.is_sealed
            || totals.tribute_count != 0
            || !totals.tribute_nominal_total_minor.is_zero()
        {
            return Err(outbe_primitives::error::PrecompileError::Revert(
                "Tribute WWD is not completed and empty".into(),
            ));
        }
        self.emit(ITribute::TributePartitionRetired {
            worldwideDay: day.into(),
        })?;
        Ok(outcome)
    }

    pub fn burn(
        &mut self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        tribute_id: WwdEntityId,
    ) -> Result<()> {
        let loaded = self
            .load_tribute(scope, parent, tribute_id)?
            .ok_or(TributeError::TributeNotFound)?;
        self.burn_loaded(scope, loaded)
    }

    /// Loads one Tribute while retaining its verified generic mutation capability.
    pub fn load_tribute(
        &self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        tribute_id: WwdEntityId,
    ) -> Result<Option<LoadedTribute>> {
        read(
            self.storage_handle(),
            scope,
            parent,
            EntityRef::Tribute(tribute_id),
        )?
        .map(LoadedTribute::from_verified)
        .transpose()
    }

    /// Burns a previously loaded Tribute without repeating a parent-body read.
    pub fn burn_loaded(&mut self, scope: &ExecutionScope, loaded: LoadedTribute) -> Result<()> {
        let storage = self.storage_handle();
        storage.with_checkpoint(|| self.burn_loaded_inner(scope, loaded))
    }

    fn burn_loaded_inner(&mut self, scope: &ExecutionScope, loaded: LoadedTribute) -> Result<()> {
        let LoadedTribute { body, current } = loaded;
        let tribute = body;
        self.ensure_day_accepts_tributes(tribute.worldwide_day)?;
        let record = record_from_verified(&current)?;
        self.bump_day_bucket_record(&record, false)?;
        self.update_pre_admission_for_tribute(&record, false)?;

        let supply = self.total_supply.read()?.checked_sub(1).ok_or_else(|| {
            outbe_primitives::error::PrecompileError::BodyReadCorruption(
                "Tribute total supply underflow during burn".into(),
            )
        })?;
        self.total_supply.write(supply)?;

        delete(self.storage_handle(), scope, current)?;
        self.emit(ITribute::TributeBurned {
            tributeId: tribute.tribute_id.to_u256(),
            owner: tribute.owner,
            worldwideDay: tribute.worldwide_day.into(),
        })?;

        Ok(())
    }

    pub fn burn_all_by_wwd(
        &mut self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        day: WorldwideDay,
    ) -> Result<()> {
        let storage = self.storage_handle();
        storage.with_checkpoint(|| {
            let tribute_ids = self.get_tribute_ids_by_day(scope, parent, day)?;
            for tribute_id in tribute_ids {
                let current = read(
                    self.storage_handle(),
                    scope,
                    parent,
                    EntityRef::Tribute(tribute_id),
                )?
                .ok_or(TributeError::TributeNotFound)?;
                self.burn_loaded_inner(scope, LoadedTribute::from_verified(current)?)?;
            }
            Ok(())
        })
    }

    pub fn seal_day(&mut self, day: WorldwideDay) -> Result<()> {
        let mut totals = self.get_day_totals(day)?;
        totals.initialized = true;
        totals.is_sealed = true;
        self.store_day_totals(&totals)?;
        self.emit(ITribute::TributeWorldwideDaySealed {
            worldwideDay: day.into(),
            isSealed: true,
        })?;
        Ok(())
    }

    pub fn unseal_day(&mut self, day: WorldwideDay) -> Result<()> {
        if self.pre_admission_projection(day)?.is_sealed {
            return Err(TributeError::PreAdmissionSealed.into());
        }
        let mut totals = self.get_day_totals(day)?;
        totals.initialized = true;
        totals.is_sealed = false;
        self.store_day_totals(&totals)?;
        self.emit(ITribute::TributeWorldwideDaySealed {
            worldwideDay: day.into(),
            isSealed: false,
        })?;
        Ok(())
    }

    /// Freezes the bounded Tribute projection after the WWD and its CE
    /// collection are both sealed. The root must come from the terminal CE
    /// lifecycle. Callers cannot replace an already sealed value.
    pub fn seal_pre_admission(
        &mut self,
        day: WorldwideDay,
        sealed_collection: outbe_compressed_entities::SealedCollectionRoot,
    ) -> Result<crate::TributePreAdmissionProjection> {
        let storage = self.storage_handle();
        storage.with_checkpoint(|| {
            if !self.ocomp_profile_ready.read()? {
                return Err(TributeError::OcompProfileNotReady.into());
            }
            if sealed_collection.partition()
                != outbe_compressed_entities::PartitionRef::TributeWwd(day)
            {
                return Err(TributeError::InvalidSealedCollectionRoot.into());
            }
            let sealed_collection_root = sealed_collection.root();
            if sealed_collection_root.is_zero() {
                return Err(TributeError::InvalidSealedCollectionRoot.into());
            }
            let totals = self.get_day_totals(day)?;
            if !totals.initialized || !totals.is_sealed {
                return Err(TributeError::WorldwideDaySealed.into());
            }
            let mut admission = self.read_day_pre_admission(day)?;
            if admission.is_sealed {
                return Err(TributeError::PreAdmissionSealed.into());
            }
            admission.initialized = true;
            admission.is_sealed = true;
            admission.sealed_collection_root = sealed_collection_root;
            admission.sealed_tribute_count = totals.tribute_count;
            admission.sealed_tribute_nominal_total_minor = totals.tribute_nominal_total_minor;
            self.store_day_pre_admission(&admission)?;
            self.pre_admission_projection(day)
        })
    }
}
