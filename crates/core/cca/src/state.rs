//! Local record reads and active-index maintenance.
use crate::{
    errors::CcaError,
    precompile::ICca,
    schema::{CcaContract, CcaRecord},
};
use alloy_primitives::Address;
use outbe_primitives::error::Result;

pub(crate) fn decode_state(state: u8) -> Result<ICca::State> {
    match state {
        0 => Ok(ICca::State::Unknown),
        1 => Ok(ICca::State::Active),
        2 => Ok(ICca::State::Suspended),
        3 => Ok(ICca::State::Deregistered),
        other => Err(CcaError::InvalidState(other).into()),
    }
}

impl CcaContract<'_> {
    pub(crate) fn load(&self, cca: Address) -> Result<CcaRecord> {
        self.records
            .get(cca)?
            .ok_or_else(|| CcaError::NotRegistered.into())
    }

    pub(crate) fn save(&mut self, record: &CcaRecord) -> Result<()> {
        if decode_state(record.state)? as u8 == ICca::State::Active as u8 {
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
