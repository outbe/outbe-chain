//! Local record reads and active-index maintenance.
use crate::{
    constants::MAX_ACTIVE_CCAS,
    errors::CcaError,
    precompile::ICcaRegistry,
    schema::{CcaContract, CcaRecord},
};
use alloy_primitives::Address;
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
            if !self.active.contains(&record.cca)? && self.active.len()? >= MAX_ACTIVE_CCAS {
                return Err(CcaError::ActiveSetFull.into());
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
}
