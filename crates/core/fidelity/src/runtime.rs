use crate::errors::FidelityError;
use crate::schema::FidelityContract;
use alloy_primitives::Address;
use outbe_primitives::error::Result;

const DEFAULT_FIDELITY_INDEX: u64 = 1;

impl FidelityContract<'_> {
    pub fn get_fidelity_index(&self, address: Address) -> Result<u64> {
        let val = self.fidelity_indices.read(&address)?;
        if val == 0 {
            Ok(DEFAULT_FIDELITY_INDEX)
        } else {
            Ok(val)
        }
    }

    // reject values exceeding u32::MAX — downstream lysis casts the
    // index to u32 for `league_id`; a larger value would truncate silently.
    pub fn set_fidelity_index(&mut self, address: Address, index: u64) -> Result<()> {
        if index > u32::MAX as u64 {
            return Err(FidelityError::IndexOutOfRange { address, index }.into());
        }
        self.fidelity_indices.write(&address, index)
    }
}
