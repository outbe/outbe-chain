use alloy_primitives::{keccak256, Address};
use outbe_primitives::error::Result;
use outbe_primitives::time::WorldwideDay;
#[cfg(any(test, feature = "test-enclave"))]
use outbe_primitives::wwd_entity_id::derive_poseidon_entity_id;
use outbe_primitives::wwd_entity_id::WwdEntityId;

use crate::errors::TributeError;
use crate::schema::{DayTotals, TributeContract, TributeData};
use crate::storage_records::StoredTributeEntryExt;
use crate::TributeRecord;

impl TributeContract<'_> {
    pub fn owner_of(&self, tribute_id: WwdEntityId) -> Result<Address> {
        if self.retired_days.read(&tribute_id.worldwide_day())? {
            return Err(TributeError::TributeNotFound.into());
        }
        let owner = self.records.entry(tribute_id).owner().read()?;
        if owner.is_zero() {
            return Err(TributeError::TributeNotFound.into());
        }
        Ok(owner)
    }

    pub fn balance_of(&self, owner: Address) -> Result<u64> {
        let count = self.get_tribute_ids_by_owner(owner)?.len();
        let count: u64 = count
            .try_into()
            .map_err(|_| TributeError::OwnerBalanceOverflow)?;
        Ok(count)
    }

    pub fn token_uri(&self, tribute_id: WwdEntityId) -> Result<String> {
        let tribute = self
            .get_record(tribute_id)?
            .ok_or(TributeError::TributeNotFound)?;
        let (issuance, nominal, encryption) = tribute.public_amount_attributes()?;
        let attributes = serde_json::json!([
            {"trait_type":"owner", "value": tribute.owner.to_string()},
            {"trait_type":"worldwide_day", "value": tribute.worldwide_day.to_string()},
            {"trait_type":"issuance_currency", "value": tribute.issuance_currency.to_string()},
            {"trait_type":"issuance_amount_minor", "value": issuance},
            {"trait_type":"nominal_amount_minor", "value": nominal},
            {"trait_type":"reference_currency", "value": tribute.reference_currency.to_string()}
        ]);
        let metadata = serde_json::json!({
            "name": format!("Tribute 0x{}", tribute.tribute_id),
            "description": "Outbe Tribute", "attributes": attributes,
            "encryption": encryption
        });
        Ok(format!("data:application/json;utf8,{metadata}"))
    }

    /// Public authenticated record; no amount decryption occurs on this path.
    pub fn get_record(&self, tribute_id: WwdEntityId) -> Result<Option<TributeRecord>> {
        crate::state_api::get_record(&self.storage_handle(), tribute_id)
    }

    pub fn get_tribute(&self, tribute_id: WwdEntityId) -> Result<Option<TributeData>> {
        self.get_record(tribute_id)?
            .as_ref()
            .map(calculation_view)
            .transpose()
    }

    pub fn get_day_totals(&self, day: WorldwideDay) -> Result<DayTotals> {
        let stored = self
            .day_totals
            .get(day)?
            .unwrap_or_else(|| crate::day_schema::StoredDayTotals::with_key(day));
        let mut view = DayTotals::with_key(day);
        view.initialized = stored.initialized;
        view.tribute_count = stored.tribute_count;
        view.is_sealed = stored.is_sealed;
        view.tribute_nominal_total_minor = self.day_nominal_amount(day, false)?;
        Ok(view)
    }

    pub fn is_day_sealed(&self, day: WorldwideDay) -> Result<bool> {
        Ok(self
            .day_totals
            .get(day)?
            .map(|totals| totals.is_sealed)
            .unwrap_or(false))
    }

    pub fn get_tribute_ids_by_owner(&self, owner: Address) -> Result<Vec<WwdEntityId>> {
        Ok(self
            .state_records_by_owner(owner)?
            .into_iter()
            .map(|record| record.tribute_id)
            .collect())
    }

    pub fn get_tribute_ids_by_day(&self, day: WorldwideDay) -> Result<Vec<WwdEntityId>> {
        Ok(self
            .state_records_by_day(day)?
            .into_iter()
            .map(|record| record.tribute_id)
            .collect())
    }

    pub(crate) fn ensure_day_accepts_tributes(&self, day: WorldwideDay) -> Result<()> {
        if self.retired_days.read(&day)? || self.is_day_sealed(day)? {
            return Err(TributeError::WorldwideDaySealed.into());
        }
        Ok(())
    }

    pub(crate) fn store_day_totals(&mut self, totals: &DayTotals) -> Result<()> {
        if totals.tribute_nominal_total_minor
            != self.day_nominal_amount(totals.worldwide_day, false)?
        {
            return Err(TributeError::DayMetadataDiffersFromEncryptedAmount.into());
        }
        self.store_day_metadata(totals)
    }

    pub(crate) fn bump_day_bucket_record(
        &mut self,
        record: &TributeRecord,
        add: bool,
    ) -> Result<()> {
        let day = record.worldwide_day;
        let mut totals = self
            .day_totals
            .get(day)?
            .unwrap_or_else(|| crate::day_schema::StoredDayTotals::with_key(day));
        totals.initialized = true;
        totals.tribute_count = if add {
            totals.tribute_count.checked_add(1)
        } else {
            totals.tribute_count.checked_sub(1)
        }
        .ok_or(TributeError::DayCountOverflowOrUnderflow)?;
        let operation = match record.encrypted() {
            Some(tribute) => outbe_tee::tribute_day::TributeDayOperationV2::Adjust {
                tribute: Box::new(tribute.clone()),
                add,
            },
            None => outbe_tee::tribute_day::TributeDayOperationV2::AdjustTransient {
                nominal_amount_minor: calculation_view(record)?.nominal_amount_minor,
                add,
            },
        };
        let mut context = b"outbe/tribute/day-count/v2".to_vec();
        context.extend_from_slice(&day.value().to_be_bytes());
        context.extend_from_slice(&totals.tribute_count.to_be_bytes());
        context.push(u8::from(totals.is_sealed));
        self.apply_day_amount(day, operation, keccak256(context))?;
        if self.day_totals.exists(day)? {
            self.day_totals.update(&totals)
        } else {
            self.day_totals.create(&totals)
        }
    }

    pub fn read_all_by_owner(&self, owner: Address) -> Result<Vec<TributeData>> {
        self.state_records_by_owner(owner)?
            .into_iter()
            .map(|record| calculation_view(&crate::record::from_encrypted(record.into())))
            .collect()
    }

    pub fn read_all_by_day(&self, day: WorldwideDay) -> Result<Vec<TributeData>> {
        self.read_records_by_day(day)?
            .iter()
            .map(calculation_view)
            .collect()
    }

    pub fn read_records_by_day(&self, day: WorldwideDay) -> Result<Vec<TributeRecord>> {
        Ok(self
            .state_records_by_day(day)?
            .into_iter()
            .map(|record| crate::record::from_encrypted(record.into()))
            .collect())
    }

    pub fn get_league(&self, tribute_id: WwdEntityId) -> Result<u16> {
        if self.retired_days.read(&tribute_id.worldwide_day())?
            || !self.records.exists(tribute_id)?
        {
            return Err(TributeError::TributeNotFound.into());
        }
        let league = self.tribute_leagues.read(&tribute_id)?;
        Ok(league.max(1))
    }

    pub fn get_day_league_fraction(&self, day: WorldwideDay, league: u16) -> Result<u64> {
        self.day_fractions
            .read(&crate::schema::day_league_key(day, league))
    }

    pub fn get_fi_groups(
        &self,
        day: WorldwideDay,
    ) -> Result<std::collections::BTreeMap<u16, (u32, alloy_primitives::U256)>> {
        let mut groups = std::collections::BTreeMap::new();
        for league in self.day_active_leagues.list(&day).read_all()? {
            let key = crate::schema::day_league_key(day, league);
            let count = (self.fi_group_counts.read(&key)? & 0xFFFF_FFFF) as u32;
            if count > 0 {
                let nominal = self.fi_group_nominals.read(&key)?;
                groups.insert(league, (count, nominal));
            }
        }
        Ok(groups)
    }

    pub(crate) fn adjust_fi_group(
        &mut self,
        day: WorldwideDay,
        league: u16,
        nominal: alloy_primitives::U256,
        add: bool,
    ) -> Result<()> {
        const IN_ACTIVE_LIST_BIT: u64 = 1u64 << 32;
        let key = crate::schema::day_league_key(day, league);
        let packed = self.fi_group_counts.read(&key)?;
        let prev_count = (packed & 0xFFFF_FFFF) as u32;
        if add {
            if (packed & IN_ACTIVE_LIST_BIT) == 0 {
                self.day_active_leagues.list(&day).push(league)?;
            }
            let next_count = prev_count
                .checked_add(1)
                .ok_or(TributeError::DayCountOverflowOrUnderflow)?;
            let prev_nom = if prev_count == 0 {
                alloy_primitives::U256::ZERO
            } else {
                self.fi_group_nominals.read(&key)?
            };
            let next_nom = prev_nom
                .checked_add(nominal)
                .ok_or(TributeError::DayCountOverflowOrUnderflow)?;
            self.fi_group_counts
                .write(&key, IN_ACTIVE_LIST_BIT | u64::from(next_count))?;
            self.fi_group_nominals.write(&key, next_nom)?;
        } else {
            let next_count = prev_count
                .checked_sub(1)
                .ok_or(TributeError::DayCountOverflowOrUnderflow)?;
            let prev_nom = self.fi_group_nominals.read(&key)?;
            let next_nom = prev_nom
                .checked_sub(nominal)
                .ok_or(TributeError::DayCountOverflowOrUnderflow)?;
            self.fi_group_counts
                .write(&key, (packed & IN_ACTIVE_LIST_BIT) | u64::from(next_count))?;
            self.fi_group_nominals.write(&key, next_nom)?;
        }
        Ok(())
    }
}

fn calculation_view(record: &TributeRecord) -> Result<TributeData> {
    record
        .calculation_view()
        .map_err(TributeError::private_amount_read_failed)
}

#[cfg(any(test, feature = "test-enclave"))]
pub(crate) fn validate_tribute_for_issue(tribute: &TributeData) -> Result<()> {
    if tribute.owner.is_zero() {
        return Err(TributeError::InvalidOwner.into());
    }
    if tribute.issuance_amount_minor.is_zero() {
        return Err(TributeError::SettlementAmountMustBePositive.into());
    }
    let expected = derive_poseidon_entity_id(tribute.owner, tribute.worldwide_day);
    if tribute.tribute_id != expected {
        return Err(TributeError::CanonicalIdentityMismatch {
            expected,
            found: tribute.tribute_id,
        }
        .into());
    }
    Ok(())
}
