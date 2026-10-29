use outbe_macros::{contract, storage_schema};
use outbe_primitives::addresses::METADOSIS_ADDRESS;

pub use outbe_emission::schema::{EmissionContract, EmissionDay};

/// Metadosis execution state aliases mapped to [`outbe_emission::schema::metadosis_state`].
pub mod emission_day_state {
    pub const NONE: u8 = outbe_emission::schema::metadosis_state::PENDING;
    pub const SUCCEEDED: u8 = outbe_emission::schema::metadosis_state::COMPLETED;
    pub const CANCELLED: u8 = outbe_emission::schema::metadosis_state::CANCELLED;
    pub const FAILED: u8 = outbe_emission::schema::metadosis_state::FAILED;
}

/// Native Metadosis event-emitting contract handle at `METADOSIS_ADDRESS`.
///
/// `EmissionDay` storage lives in [`outbe_emission::EmissionContract`] at `EMISSION_ADDRESS`.
#[storage_schema]
#[contract(addr = METADOSIS_ADDRESS)]
pub struct MetadosisContract {}

impl<'a> MetadosisContract<'a> {
    /// Returns the `emission_days` map stored at `EMISSION_ADDRESS`.
    #[inline]
    pub fn emission_days(
        &self,
    ) -> outbe_primitives::storage::dsl::Map<'a, u32, outbe_emission::schema::EmissionDay> {
        EmissionContract::new(self.storage.clone()).emission_days
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::U256;
    use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};

    #[test]
    fn emission_days_map_base_is_slot_zero_on_emission_contract() {
        let mut provider = HashMapStorageProvider::new(1);
        let contract = MetadosisContract::new(StorageHandle::new(&mut provider));
        assert_eq!(contract.emission_days().base_slot(), U256::from(0));
    }
}
