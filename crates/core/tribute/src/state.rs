use alloy_primitives::{Address, U256};
use outbe_common::WorldwideDay;
use outbe_compressed_entities::{
    body_commitment, derive_poseidon_entity_id, encode_tribute_v1, Commitment, CommitmentState,
    EntityId36, ACTIVE_COMMITMENT_SCHEME, BODY_SCHEMA_V1,
};
use outbe_offchain_storage::MAX_SCAN_ENTRIES;
use outbe_primitives::error::Result;

use crate::errors::TributeError;
use crate::schema::{DayTotals, TributeContract, TributeData};
use crate::{TributePageRequest, TributeRepositoryReader};

const MAX_RUNTIME_QUERY_RECORDS: usize = MAX_SCAN_ENTRIES * 4;

impl TributeContract<'_> {
    pub fn total_supply(&self) -> Result<u64> {
        self.total_supply.read()
    }

    pub fn owner_of(
        &self,
        bodies: &TributeRepositoryReader,
        tribute_id: EntityId36,
    ) -> Result<Address> {
        let tribute = self
            .get_tribute(bodies, tribute_id)?
            .ok_or(TributeError::TributeNotFound)?;
        Ok(tribute.owner)
    }

    pub fn balance_of(&self, bodies: &TributeRepositoryReader, owner: Address) -> Result<u64> {
        let count = self.get_tribute_ids_by_owner(bodies, owner)?.len();
        let count: u64 = count
            .try_into()
            .map_err(|_| TributeError::OwnerBalanceOverflow)?;
        Ok(count)
    }

    pub fn token_uri(
        &self,
        bodies: &TributeRepositoryReader,
        tribute_id: EntityId36,
    ) -> Result<String> {
        let tribute = self
            .get_tribute(bodies, tribute_id)?
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
        bodies: &TributeRepositoryReader,
        tribute_id: EntityId36,
    ) -> Result<Option<TributeData>> {
        let commitments = CommitmentState::new(self.storage_handle());
        let Some(expected) = commitments.tribute(tribute_id)? else {
            return Ok(None);
        };
        let body = bodies.get(tribute_id)?.ok_or_else(|| {
            outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                "CommittedBodyMissing: Tribute {tribute_id}"
            ))
        })?;
        self.verify_returned_tribute(&body, expected)?;
        Ok(Some(body))
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
        bodies: &TributeRepositoryReader,
        owner: Address,
    ) -> Result<Vec<EntityId36>> {
        Ok(self
            .read_all_by_owner(bodies, owner)?
            .into_iter()
            .map(|tribute| tribute.tribute_id)
            .collect())
    }

    pub fn get_tribute_ids_by_day(
        &self,
        bodies: &TributeRepositoryReader,
        day: WorldwideDay,
    ) -> Result<Vec<EntityId36>> {
        Ok(self
            .read_all_by_day(bodies, day)?
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
        bodies: &TributeRepositoryReader,
        owner: Address,
    ) -> Result<Vec<TributeData>> {
        let mut records = Vec::new();
        let mut after = None;
        loop {
            let page = bodies.list_by_owner(
                owner,
                TributePageRequest {
                    after,
                    limit: MAX_SCAN_ENTRIES,
                },
            )?;
            if records.len().saturating_add(page.records.len()) > MAX_RUNTIME_QUERY_RECORDS {
                return Err(TributeError::QueryLimitExceeded.into());
            }
            self.extend_verified_page(&mut records, page.records, "owner")?;
            let Some(next) = page.next_after else {
                return Ok(records);
            };
            if records.len() == MAX_RUNTIME_QUERY_RECORDS {
                return Err(TributeError::QueryLimitExceeded.into());
            }
            after = Some(next);
        }
    }

    pub(crate) fn read_all_by_day(
        &self,
        bodies: &TributeRepositoryReader,
        day: WorldwideDay,
    ) -> Result<Vec<TributeData>> {
        let mut records = Vec::new();
        let mut after = None;
        loop {
            let page = bodies.list_by_day(
                day,
                TributePageRequest {
                    after,
                    limit: MAX_SCAN_ENTRIES,
                },
            )?;
            if records.len().saturating_add(page.records.len()) > MAX_RUNTIME_QUERY_RECORDS {
                return Err(TributeError::QueryLimitExceeded.into());
            }
            self.extend_verified_page(&mut records, page.records, "day")?;
            let Some(next) = page.next_after else {
                return Ok(records);
            };
            if records.len() == MAX_RUNTIME_QUERY_RECORDS {
                return Err(TributeError::QueryLimitExceeded.into());
            }
            after = Some(next);
        }
    }

    fn extend_verified_page(
        &self,
        records: &mut Vec<TributeData>,
        page: Vec<TributeData>,
        index: &'static str,
    ) -> Result<()> {
        let commitments = CommitmentState::new(self.storage_handle());
        for body in page {
            let expected = commitments.tribute(body.tribute_id)?.ok_or_else(|| {
                outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                    "{index} index returned canonically absent Tribute {}",
                    body.tribute_id
                ))
            })?;
            self.verify_returned_tribute(&body, expected)?;
            if records
                .last()
                .is_some_and(|last| last.tribute_id >= body.tribute_id)
            {
                return Err(
                    outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                        "Tribute {index} page contains duplicate or unordered identities"
                    )),
                );
            }
            records.push(body);
        }
        Ok(())
    }

    fn verify_returned_tribute(&self, body: &TributeData, expected: Commitment) -> Result<()> {
        let canonical_id =
            derive_poseidon_entity_id(body.owner, body.worldwide_day).map_err(|error| {
                outbe_primitives::error::PrecompileError::BodyReadCorruption(error.to_string())
            })?;
        if body.tribute_id != canonical_id {
            return Err(
                outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                    "Tribute canonical identity mismatch: expected {canonical_id}, found {}",
                    body.tribute_id
                )),
            );
        }
        let payload =
            encode_tribute_v1(&crate::repository::canonical_body(body)).map_err(|error| {
                outbe_primitives::error::PrecompileError::BodyReadCorruption(error.to_string())
            })?;
        let actual = body_commitment(
            ACTIVE_COMMITMENT_SCHEME,
            BODY_SCHEMA_V1,
            body.tribute_id,
            &payload,
        )
        .map_err(|error| {
            outbe_primitives::error::PrecompileError::BodyReadCorruption(error.to_string())
        })?;
        if actual != expected {
            return Err(
                outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                    "Tribute commitment mismatch for {}",
                    body.tribute_id
                )),
            );
        }
        Ok(())
    }
}
