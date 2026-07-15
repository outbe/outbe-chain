use alloy_primitives::{Address, Bytes};
use outbe_common::WorldwideDay;
use outbe_compressed_entities::{
    delete, mint, read, BodyInput, EntityId36, EntityRef, ExecutionScope, ParentBodySource,
    VerifiedBody,
};
use outbe_primitives::error::Result;

use crate::errors::TributeError;
use crate::precompile::ITribute;
use crate::schema::{TributeContract, TributeData};
use crate::state::tribute_from_verified;

/// A semantic Tribute paired with the exact generic mutation capability that verified it.
pub struct LoadedTribute {
    body: TributeData,
    current: VerifiedBody,
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
    pub fn get_tributes_by_owner(
        &self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        owner: Address,
    ) -> Result<Vec<TributeData>> {
        self.read_all_by_owner(scope, parent, owner)
    }

    pub fn get_all_day_tributes(
        &self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        day: WorldwideDay,
    ) -> Result<Vec<TributeData>> {
        self.read_all_by_day(scope, parent, day)
    }

    pub fn issue(
        &mut self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        tribute: &TributeData,
    ) -> Result<()> {
        let storage = self.storage_handle();
        storage.with_checkpoint(|| self.issue_inner(scope, parent, tribute))
    }

    fn issue_inner(
        &mut self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        tribute: &TributeData,
    ) -> Result<()> {
        self.validate_tribute_for_issue(tribute)?;
        self.ensure_day_accepts_tributes(tribute.worldwide_day)?;
        if self
            .get_tribute(scope, parent, tribute.tribute_id)?
            .is_some()
        {
            return Err(TributeError::TributeAlreadyExists.into());
        }

        self.bump_day_bucket(tribute.worldwide_day, 1, tribute.nominal_amount_minor)?;

        let supply = self.total_supply.read()?.checked_add(1).ok_or_else(|| {
            outbe_primitives::error::PrecompileError::BodyReadCorruption(
                "Tribute total supply overflow during issuance".into(),
            )
        })?;
        self.total_supply.write(supply)?;

        let canonical = crate::repository::canonical_body(tribute);
        mint(self.storage_handle(), scope, BodyInput::Tribute(&canonical))?;
        self.emit(ITribute::TributeIssued {
            owner: tribute.owner,
            tributeId: Bytes::copy_from_slice(tribute.tribute_id.as_bytes()),
            worldwideDay: tribute.worldwide_day.into(),
            issuanceAmountMinor: tribute.issuance_amount_minor,
            settlementCurrency: tribute.issuance_currency,
            nominalAmountMinor: tribute.nominal_amount_minor,
        })?;

        Ok(())
    }

    pub fn burn(
        &mut self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        tribute_id: EntityId36,
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
        tribute_id: EntityId36,
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
        self.bump_day_bucket(tribute.worldwide_day, -1, tribute.nominal_amount_minor)?;

        let supply = self.total_supply.read()?.checked_sub(1).ok_or_else(|| {
            outbe_primitives::error::PrecompileError::BodyReadCorruption(
                "Tribute total supply underflow during burn".into(),
            )
        })?;
        self.total_supply.write(supply)?;

        delete(self.storage_handle(), scope, current)?;
        self.emit(ITribute::TributeBurned {
            tributeId: Bytes::copy_from_slice(tribute.tribute_id.as_bytes()),
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
}
