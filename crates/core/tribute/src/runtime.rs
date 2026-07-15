use alloy_primitives::{Address, U256};
use outbe_common::WorldwideDay;
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
        Self::read_all_by_owner(bodies, owner)
    }

    pub fn get_all_day_tributes(
        &self,
        bodies: &TributeRepositoryReader,
        day: WorldwideDay,
    ) -> Result<Vec<TributeData>> {
        Self::read_all_by_day(bodies, day)
    }

    pub fn issue(&mut self, bodies: &TributeRepositoryReader, tribute: &TributeData) -> Result<()> {
        self.validate_tribute_for_issue(tribute)?;
        self.ensure_day_accepts_tributes(tribute.worldwide_day)?;
        if self.get_tribute(bodies, tribute.token_id)?.is_some() {
            return Err(TributeError::TributeAlreadyExists.into());
        }

        self.bump_day_bucket(tribute.worldwide_day, 1, tribute.nominal_amount_minor)?;

        let supply = self.total_supply.read()?;
        self.total_supply.write(supply + 1)?;

        self.emit(ITribute::TributeBodyStored {
            tokenId: tribute.token_id,
            owner: tribute.owner,
            worldwideDay: tribute.worldwide_day.into(),
            issuanceAmountMinor: tribute.issuance_amount_minor,
            issuanceCurrency: tribute.issuance_currency,
            nominalAmountMinor: tribute.nominal_amount_minor,
            referenceCurrency: tribute.reference_currency,
            tributePriceMinor: tribute.tribute_price_minor,
            excludeFromIntexIssuance: tribute.exclude_from_intex_issuance,
        })?;
        self.emit(ITribute::TributeIssued {
            owner: tribute.owner,
            tokenId: tribute.token_id,
            worldwideDay: tribute.worldwide_day.into(),
            issuanceAmountMinor: tribute.issuance_amount_minor,
            settlementCurrency: tribute.issuance_currency,
            nominalAmountMinor: tribute.nominal_amount_minor,
        })?;

        Ok(())
    }

    pub fn burn(&mut self, bodies: &TributeRepositoryReader, token_id: U256) -> Result<()> {
        let tribute = self
            .get_tribute(bodies, token_id)?
            .ok_or(TributeError::TributeNotFound)?;

        self.burn_loaded(&tribute)
    }

    fn burn_loaded(&mut self, tribute: &TributeData) -> Result<()> {
        self.bump_day_bucket(tribute.worldwide_day, -1, tribute.nominal_amount_minor)?;

        let supply = self.total_supply.read()?;
        if supply > 0 {
            self.total_supply.write(supply - 1)?;
        }

        self.emit(ITribute::TributeBodyDeleted {
            tokenId: tribute.token_id,
        })?;
        self.emit(ITribute::TributeBurned {
            tokenId: tribute.token_id,
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
