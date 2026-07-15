use alloy_primitives::{Address, Bytes, B256};
use outbe_common::WorldwideDay;
use outbe_compressed_entities::{
    body_commitment, encode_tribute_v1, CommitmentState, EntityId36, ACTIVE_COMMITMENT_SCHEME,
    BODY_SCHEMA_V1,
};
use outbe_primitives::error::Result;

use crate::errors::TributeError;
use crate::precompile::ITribute;
use crate::schema::{TributeContract, TributeData};
use crate::TributeRepositoryReader;

impl TributeContract<'_> {
    pub fn get_tributes_by_owner(
        &self,
        bodies: &TributeRepositoryReader,
        owner: Address,
    ) -> Result<Vec<TributeData>> {
        self.read_all_by_owner(bodies, owner)
    }

    pub fn get_all_day_tributes(
        &self,
        bodies: &TributeRepositoryReader,
        day: WorldwideDay,
    ) -> Result<Vec<TributeData>> {
        self.read_all_by_day(bodies, day)
    }

    pub fn issue(&mut self, bodies: &TributeRepositoryReader, tribute: &TributeData) -> Result<()> {
        self.validate_tribute_for_issue(tribute)?;
        self.ensure_day_accepts_tributes(tribute.worldwide_day)?;
        if self.get_tribute(bodies, tribute.tribute_id)?.is_some() {
            return Err(TributeError::TributeAlreadyExists.into());
        }

        let payload = encode_tribute_v1(&crate::repository::canonical_body(tribute))
            .map_err(|error| outbe_primitives::error::PrecompileError::Fatal(error.to_string()))?;
        let new_commitment = body_commitment(
            ACTIVE_COMMITMENT_SCHEME,
            BODY_SCHEMA_V1,
            tribute.tribute_id,
            &payload,
        )
        .map_err(|error| outbe_primitives::error::PrecompileError::Fatal(error.to_string()))?;

        self.bump_day_bucket(tribute.worldwide_day, 1, tribute.nominal_amount_minor)?;

        let supply = self.total_supply.read()?.checked_add(1).ok_or_else(|| {
            outbe_primitives::error::PrecompileError::BodyReadCorruption(
                "Tribute total supply overflow during issuance".into(),
            )
        })?;
        self.total_supply.write(supply)?;

        CommitmentState::new(self.storage_handle())
            .set_tribute(tribute.tribute_id, new_commitment)?;

        self.emit(ITribute::TributeBodyStored {
            tributeId: Bytes::copy_from_slice(tribute.tribute_id.as_bytes()),
            commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
            schemaVersion: BODY_SCHEMA_V1,
            previousCommitment: B256::ZERO,
            newCommitment: B256::from(*new_commitment.as_bytes()),
            canonicalPayload: Bytes::from(payload),
        })?;
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

    pub fn burn(&mut self, bodies: &TributeRepositoryReader, tribute_id: EntityId36) -> Result<()> {
        let tribute = self
            .get_tribute(bodies, tribute_id)?
            .ok_or(TributeError::TributeNotFound)?;

        self.burn_loaded(&tribute)
    }

    fn burn_loaded(&mut self, tribute: &TributeData) -> Result<()> {
        let commitments = CommitmentState::new(self.storage_handle());
        let previous_commitment = commitments.tribute(tribute.tribute_id)?.ok_or_else(|| {
            outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                "Tribute {} became canonically absent during burn",
                tribute.tribute_id
            ))
        })?;
        self.bump_day_bucket(tribute.worldwide_day, -1, tribute.nominal_amount_minor)?;

        let supply = self.total_supply.read()?.checked_sub(1).ok_or_else(|| {
            outbe_primitives::error::PrecompileError::BodyReadCorruption(
                "Tribute total supply underflow during burn".into(),
            )
        })?;
        self.total_supply.write(supply)?;

        commitments.clear_tribute(tribute.tribute_id)?;

        self.emit(ITribute::TributeBodyDeleted {
            tributeId: Bytes::copy_from_slice(tribute.tribute_id.as_bytes()),
            previousCommitment: B256::from(*previous_commitment.as_bytes()),
        })?;
        self.emit(ITribute::TributeBurned {
            tributeId: Bytes::copy_from_slice(tribute.tribute_id.as_bytes()),
            owner: tribute.owner,
            worldwideDay: tribute.worldwide_day.into(),
        })?;

        Ok(())
    }

    pub fn burn_all_by_wwd(
        &mut self,
        bodies: &TributeRepositoryReader,
        day: WorldwideDay,
    ) -> Result<()> {
        for tribute in self.get_all_day_tributes(bodies, day)? {
            self.burn_loaded(&tribute)?;
        }
        Ok(())
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
