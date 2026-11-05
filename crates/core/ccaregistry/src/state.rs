//! Local record reads and active-index maintenance.
use crate::{
    errors::CcaError,
    precompile::ICcaRegistry,
    schema::{address_day_key, day_index_key, CcaContract, CcaRecord},
};
use alloy_primitives::{Address, U256};
use outbe_primitives::error::Result;

pub(crate) fn validate_state(state: ICcaRegistry::State) -> Result<ICcaRegistry::State> {
    if state == ICcaRegistry::State::__Invalid {
        return Err(CcaError::InvalidState(state.into()).into());
    }
    Ok(state)
}

impl CcaContract<'_> {
    pub(crate) fn load(&self, cca: Address) -> Result<CcaRecord> {
        let record = self.records.get(cca)?.ok_or(CcaError::NotRegistered)?;
        validate_state(record.state)?;
        Ok(record)
    }

    pub(crate) fn save(&mut self, record: &CcaRecord) -> Result<()> {
        if validate_state(record.state)? == ICcaRegistry::State::Active {
            // A top-up of an address already in the set does not take another slot.
            if !self.active.contains(&record.cca)? && self.active.len()? == u32::MAX {
                return Err(CcaError::Arithmetic.into());
            }
            self.active.insert(record.cca)?;
        } else {
            self.active.remove(&record.cca)?;
        }
        if self.records.exists(record.cca)? {
            self.records.update(record)
        } else {
            self.records.create(record)
        }
    }

    pub(crate) fn require_unsealed_day(&self, day: u32) -> Result<()> {
        if day <= outbe_rewards::api::get_last_sealed_day(&self.storage)? {
            return Err(CcaError::RewardDaySealed.into());
        }
        Ok(())
    }

    /// Called inside the issuance/forfeiture checkpoint, including first deficits.
    pub(crate) fn update_day_weight(
        &self,
        cca: Address,
        day: u32,
        old: U256,
        new: U256,
        deficit: U256,
    ) -> Result<()> {
        let key = address_day_key(cca, day);
        let total = self
            .reward_day_total
            .read(&day)?
            .checked_sub(old)
            .and_then(|value| value.checked_add(new))
            .ok_or(CcaError::Arithmetic)?;
        if (!new.is_zero() || !deficit.is_zero()) && !self.reward_day_members.read(&key)? {
            let count = self.reward_day_count.read(&day)?;
            let next = count.checked_add(1).ok_or(CcaError::Arithmetic)?;
            self.reward_day_addresses
                .write(&day_index_key(day, count), cca)?;
            self.reward_day_members.write(&key, true)?;
            self.reward_day_count.write(&day, next)?;
        }
        self.gratis_minor_per_utc_day.write(&key, new)?;
        self.gratis_deficit_minor_per_utc_day.write(&key, deficit)?;
        self.reward_day_total.write(&day, total)
    }
}
