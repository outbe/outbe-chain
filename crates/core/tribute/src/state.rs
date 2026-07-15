use alloy_primitives::{Address, U256};
use outbe_common::WorldwideDay;
use outbe_compressed_entities::{
    derive_poseidon_entity_id, list, read, EntityId36, EntityRef, ExecutionScope, IdPageRequest,
    ParentBodySource, QueryRef, VerifiedBody, MAX_ID_PAGE_LIMIT,
};
use outbe_primitives::error::Result;

use crate::errors::TributeError;
use crate::schema::{DayTotals, TributeContract, TributeData};

impl TributeContract<'_> {
    pub fn total_supply(&self) -> Result<u64> {
        self.total_supply.read()
    }

    pub fn owner_of(
        &self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        tribute_id: EntityId36,
    ) -> Result<Address> {
        let tribute = self
            .get_tribute(scope, parent, tribute_id)?
            .ok_or(TributeError::TributeNotFound)?;
        Ok(tribute.owner)
    }

    pub fn balance_of(
        &self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        owner: Address,
    ) -> Result<u64> {
        let count = self.get_tribute_ids_by_owner(scope, parent, owner)?.len();
        let count: u64 = count
            .try_into()
            .map_err(|_| TributeError::OwnerBalanceOverflow)?;
        Ok(count)
    }

    pub fn token_uri(
        &self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        tribute_id: EntityId36,
    ) -> Result<String> {
        let tribute = self
            .get_tribute(scope, parent, tribute_id)?
            .ok_or(TributeError::TributeNotFound)?;
        Ok(format!(
            "data:application/json;utf8,{{\"name\":\"Tribute 0x{}\",\"description\":\"Outbe Tribute\",\"attributes\":[{{\"trait_type\":\"owner\",\"value\":\"{}\"}},{{\"trait_type\":\"worldwide_day\",\"value\":\"{}\"}},{{\"trait_type\":\"issuance_currency\",\"value\":\"{}\"}},{{\"trait_type\":\"issuance_amount_minor\",\"value\":\"{}\"}},{{\"trait_type\":\"nominal_amount_minor\",\"value\":\"{}\"}},{{\"trait_type\":\"reference_currency\",\"value\":\"{}\"}},{{\"trait_type\":\"exclude_from_intex_issuance\",\"value\":{}}}]}}",
            tribute.tribute_id,
            tribute.owner,
            tribute.worldwide_day,
            tribute.issuance_currency,
            tribute.issuance_amount_minor,
            tribute.nominal_amount_minor,
            tribute.reference_currency,
            tribute.exclude_from_intex_issuance,
        ))
    }

    pub fn get_tribute(
        &self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        tribute_id: EntityId36,
    ) -> Result<Option<TributeData>> {
        read(
            self.storage_handle(),
            scope,
            parent,
            EntityRef::Tribute(tribute_id),
        )?
        .map(|current| tribute_from_verified(&current))
        .transpose()
    }

    pub fn get_day_totals(&self, day: WorldwideDay) -> Result<DayTotals> {
        Ok(self
            .day_totals
            .get(day)?
            .unwrap_or_else(|| DayTotals::with_key(day)))
    }

    pub fn is_day_sealed(&self, day: WorldwideDay) -> Result<bool> {
        Ok(self
            .day_totals
            .get(day)?
            .map(|totals| totals.is_sealed)
            .unwrap_or(false))
    }

    pub fn get_tribute_ids_by_owner(
        &self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        owner: Address,
    ) -> Result<Vec<EntityId36>> {
        Ok(self
            .read_all_by_owner(scope, parent, owner)?
            .into_iter()
            .map(|tribute| tribute.tribute_id)
            .collect())
    }

    pub fn get_tribute_ids_by_day(
        &self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        day: WorldwideDay,
    ) -> Result<Vec<EntityId36>> {
        Ok(self
            .read_all_by_day(scope, parent, day)?
            .into_iter()
            .map(|tribute| tribute.tribute_id)
            .collect())
    }

    pub(crate) fn validate_tribute_for_issue(&self, tribute: &TributeData) -> Result<()> {
        if tribute.owner.is_zero() {
            return Err(TributeError::InvalidOwner.into());
        }
        if tribute.issuance_amount_minor.is_zero() {
            return Err(TributeError::SettlementAmountMustBePositive.into());
        }
        let expected = derive_poseidon_entity_id(tribute.owner, tribute.worldwide_day)
            .map_err(|error| outbe_primitives::error::PrecompileError::Fatal(error.to_string()))?;
        if tribute.tribute_id != expected {
            return Err(outbe_primitives::error::PrecompileError::Fatal(format!(
                "Tribute canonical identity mismatch: expected {expected}, found {}",
                tribute.tribute_id
            )));
        }
        Ok(())
    }

    pub(crate) fn ensure_day_accepts_tributes(&self, day: WorldwideDay) -> Result<()> {
        let totals = self.get_day_totals(day)?;
        if !totals.initialized || totals.is_sealed {
            return Err(TributeError::WorldwideDaySealed.into());
        }
        Ok(())
    }

    pub(crate) fn store_day_totals(&mut self, totals: &DayTotals) -> Result<()> {
        if self.day_totals.exists(totals.worldwide_day)? {
            self.day_totals.update(totals)
        } else {
            self.day_totals.create(totals)
        }
    }

    pub(crate) fn bump_day_bucket(
        &mut self,
        day: WorldwideDay,
        delta_count: i32,
        nominal_amount: U256,
    ) -> Result<()> {
        let mut totals = self.get_day_totals(day)?;
        totals.initialized = true;

        if delta_count >= 0 {
            totals.tribute_count = totals
                .tribute_count
                .checked_add(delta_count as u32)
                .ok_or_else(|| {
                    outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                        "Tribute day {day} count overflow"
                    ))
                })?;
            totals.tribute_nominal_amount = totals
                .tribute_nominal_amount
                .checked_add(nominal_amount)
                .ok_or_else(|| {
                    outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                        "Tribute day {day} nominal amount overflow"
                    ))
                })?;
        } else {
            let delta = (-delta_count) as u32;
            totals.tribute_count = totals.tribute_count.checked_sub(delta).ok_or_else(|| {
                outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                    "Tribute day {day} count underflow"
                ))
            })?;
            totals.tribute_nominal_amount = totals
                .tribute_nominal_amount
                .checked_sub(nominal_amount)
                .ok_or_else(|| {
                    outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                        "Tribute day {day} nominal amount underflow"
                    ))
                })?;
        }

        self.store_day_totals(&totals)
    }

    pub(crate) fn read_all_by_owner(
        &self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        owner: Address,
    ) -> Result<Vec<TributeData>> {
        self.read_all(scope, parent, QueryRef::TributeByOwner(owner))
    }

    pub(crate) fn read_all_by_day(
        &self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        day: WorldwideDay,
    ) -> Result<Vec<TributeData>> {
        self.read_all(scope, parent, QueryRef::TributeByDay(day))
    }

    fn read_all(
        &self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        query: QueryRef,
    ) -> Result<Vec<TributeData>> {
        let mut records = Vec::new();
        let mut after = None;
        loop {
            let page = list(
                self.storage_handle(),
                scope,
                parent,
                query,
                IdPageRequest {
                    after,
                    limit: MAX_ID_PAGE_LIMIT,
                },
            )?;
            let next_after = page.next_after();
            let bodies = page.into_bodies();
            records.extend(
                bodies
                    .iter()
                    .map(tribute_from_verified)
                    .collect::<Result<Vec<_>>>()?,
            );
            let Some(next) = next_after else {
                return Ok(records);
            };
            after = Some(next);
        }
    }
}

pub(crate) fn tribute_from_verified(body: &VerifiedBody) -> Result<TributeData> {
    let payload = body.payload().as_tribute().ok_or_else(|| {
        outbe_primitives::error::PrecompileError::BodyReadCorruption(
            "compressed-entity read returned a non-Tribute payload".into(),
        )
    })?;
    Ok(crate::repository::from_canonical_body(payload.clone()))
}
