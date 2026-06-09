use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_common::WorldwideDay;
use outbe_primitives::error::Result;
use outbe_primitives::storage::types::StorageKey;

use crate::errors::TributeError;
use crate::schema::{DayTotals, TributeContract, TributeData};

impl TributeContract<'_> {
    pub fn total_supply(&self) -> Result<u64> {
        self.total_supply.read()
    }

    pub fn owner_of(&self, token_id: U256) -> Result<Address> {
        let tribute = self
            .get_tribute(token_id)?
            .ok_or(TributeError::TributeNotFound)?;
        Ok(tribute.owner)
    }

    pub fn balance_of(&self, owner: Address) -> Result<u64> {
        let count = self.get_tribute_ids_by_owner(owner)?.len();
        let count: u64 = count
            .try_into()
            .map_err(|_| TributeError::OwnerBalanceOverflow)?;
        Ok(count)
    }

    pub fn token_uri(&self, token_id: U256) -> Result<String> {
        let tribute = self
            .get_tribute(token_id)?
            .ok_or(TributeError::TributeNotFound)?;
        Ok(format!(
            "data:application/json;utf8,{{\"name\":\"Tribute 0x{:064x}\",\"description\":\"Outbe Tribute\",\"attributes\":[{{\"trait_type\":\"owner\",\"value\":\"{}\"}},{{\"trait_type\":\"worldwide_day\",\"value\":\"{}\"}},{{\"trait_type\":\"issuance_currency\",\"value\":\"{}\"}},{{\"trait_type\":\"issuance_amount_minor\",\"value\":\"{}\"}},{{\"trait_type\":\"nominal_amount_minor\",\"value\":\"{}\"}}]}}",
            tribute.token_id,
            tribute.owner,
            tribute.worldwide_day,
            tribute.issuance_currency,
            tribute.issuance_amount_minor,
            tribute.nominal_amount_minor,
        ))
    }

    pub fn get_tribute(&self, token_id: U256) -> Result<Option<TributeData>> {
        self.tributes.get(token_id)
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

    pub fn get_tribute_ids_by_owner(&self, owner: Address) -> Result<Vec<U256>> {
        let count = self.owner_index_counts.read(&owner)?;
        let mut token_ids = Vec::new();
        for i in 0..count {
            let key = Self::owner_index_key(owner, i);
            let token_id = self.owner_tribute_ids.read(&key)?;
            if token_id.is_zero() {
                continue;
            }
            if self.tributes.exists(token_id)? {
                token_ids.push(token_id);
            }
        }
        Ok(token_ids)
    }

    pub fn get_tribute_ids_by_day(&self, day: WorldwideDay) -> Result<Vec<U256>> {
        let count = self.day_index_counts.read(&day)?;
        let mut token_ids = Vec::new();
        for i in 0..count {
            let token_id = self.get_day_token_id(day, i)?;
            if token_id.is_zero() {
                continue;
            }
            if self.tributes.exists(token_id)? {
                token_ids.push(token_id);
            }
        }
        Ok(token_ids)
    }

    pub fn clear_day_index(&mut self, day: WorldwideDay) -> Result<()> {
        let count = self.day_index_counts.read(&day)?;
        for i in 0..count {
            let key = Self::day_index_key(day, i);
            self.day_token_ids.write(&key, U256::ZERO)?;
        }
        self.day_index_counts.write(&day, 0)
    }

    pub(crate) fn get_day_token_id(&self, day: WorldwideDay, index: u32) -> Result<U256> {
        let key = Self::day_index_key(day, index);
        self.day_token_ids.read(&key)
    }

    pub(crate) fn validate_tribute_for_issue(&self, tribute: &TributeData) -> Result<()> {
        if tribute.owner.is_zero() {
            return Err(TributeError::InvalidOwner.into());
        }
        if tribute.issuance_amount_minor.is_zero() {
            return Err(TributeError::SettlementAmountMustBePositive.into());
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
            totals.tribute_count = totals.tribute_count.saturating_add(delta_count as u32);
            totals.tribute_nominal_amount =
                totals.tribute_nominal_amount.saturating_add(nominal_amount);
        } else {
            let delta = (-delta_count) as u32;
            totals.tribute_count = totals.tribute_count.saturating_sub(delta);
            totals.tribute_nominal_amount =
                totals.tribute_nominal_amount.saturating_sub(nominal_amount);
        }

        self.store_day_totals(&totals)
    }

    pub(crate) fn add_to_day_index(&mut self, day: WorldwideDay, token_id: U256) -> Result<()> {
        let count = self.day_index_counts.read(&day)?;
        let key = Self::day_index_key(day, count);
        self.day_token_ids.write(&key, token_id)?;
        self.day_index_counts.write(&day, count + 1)
    }

    pub(crate) fn add_to_owner_index(&mut self, owner: Address, token_id: U256) -> Result<()> {
        let count = self.owner_index_counts.read(&owner)?;
        let key = Self::owner_index_key(owner, count);
        self.owner_tribute_ids.write(&key, token_id)?;
        self.owner_index_counts.write(&owner, count + 1)
    }

    pub(crate) fn day_index_key(day: WorldwideDay, index: u32) -> B256 {
        let mut buf = [0u8; 8];
        buf[0..4].copy_from_slice(day.key_bytes().as_slice());
        buf[4..8].copy_from_slice(&index.to_be_bytes());
        keccak256(buf)
    }

    pub(crate) fn owner_index_key(owner: Address, index: u32) -> B256 {
        let mut buf = [0u8; 24];
        buf[0..20].copy_from_slice(owner.as_slice());
        buf[20..24].copy_from_slice(&index.to_be_bytes());
        keccak256(buf)
    }
}
