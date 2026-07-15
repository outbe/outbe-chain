use alloy_primitives::{Address, U256};
use outbe_common::WorldwideDay;
use outbe_primitives::error::Result;

use crate::errors::TributeError;
use crate::precompile::ITribute;
use crate::schema::{TributeContract, TributeData};

impl TributeContract<'_> {
    pub fn get_tributes_by_owner(&self, owner: Address) -> Result<Vec<TributeData>> {
        let mut tributes = Vec::new();
        for token_id in self.get_tribute_ids_by_owner(owner)? {
            if let Some(tribute) = self.get_tribute(token_id)? {
                tributes.push(tribute);
            }
        }
        Ok(tributes)
    }

    pub fn get_all_day_tributes(&self, day: WorldwideDay) -> Result<Vec<TributeData>> {
        let mut tributes = Vec::new();
        for token_id in self.get_tribute_ids_by_day(day)? {
            if let Some(tribute) = self.get_tribute(token_id)? {
                tributes.push(tribute);
            }
        }
        Ok(tributes)
    }

    pub fn issue(&mut self, tribute: &TributeData) -> Result<()> {
        self.validate_tribute_for_issue(tribute)?;
        self.ensure_day_accepts_tributes(tribute.worldwide_day)?;

        self.tributes.create(tribute)?;
        self.add_to_day_index(tribute.worldwide_day, tribute.token_id)?;
        self.add_to_owner_index(tribute.owner, tribute.token_id)?;
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

    pub fn burn(&mut self, token_id: U256) -> Result<()> {
        let tribute = self
            .get_tribute(token_id)?
            .ok_or(TributeError::TributeNotFound)?;

        self.tributes.delete(token_id)?;
        self.bump_day_bucket(tribute.worldwide_day, -1, tribute.nominal_amount_minor)?;

        let supply = self.total_supply.read()?;
        if supply > 0 {
            self.total_supply.write(supply - 1)?;
        }

        self.emit(ITribute::TributeBodyDeleted { tokenId: token_id })?;
        self.emit(ITribute::TributeBurned {
            tokenId: token_id,
            owner: tribute.owner,
            worldwideDay: tribute.worldwide_day.into(),
        })?;

        Ok(())
    }

    pub fn burn_all_by_wwd(&mut self, day: WorldwideDay) -> Result<()> {
        let token_ids = self.get_tribute_ids_by_day(day)?;
        for token_id in token_ids {
            self.burn(token_id)?;
        }
        self.clear_day_index(day)
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
