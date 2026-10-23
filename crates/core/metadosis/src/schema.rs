use alloy_primitives::U256;
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::addresses::METADOSIS_ADDRESS;
use outbe_primitives::time::WorldwideDay as WorldwideDayKey;

/// Execution state of an `EmissionDay`.
pub mod emission_day_state {
    pub const NONE: u8 = 0;
    pub const SUCCEEDED: u8 = 1;
    pub const FAILED: u8 = 2;
}

#[storage_record(packed, exists_field = metadosis_start)]
pub struct EmissionDay {
    #[key]
    pub day: u32,

    /// Six-decimal protocol COEN minor units. 1_000_000 minor units equal 1 COEN.
    /// The scale matches `metadosis_day_emission_limit` and `tribute_nominal_total_minor`.
    /// This value is not 18-decimal native wei.
    #[attribute(order = 1, default = U256::ZERO)]
    pub emission_limit_minor: U256,

    #[attribute(order = 2)]
    pub wwd: WorldwideDayKey,

    #[attribute(order = 3)]
    pub metadosis_start: u64,

    #[attribute(order = 4, default = emission_day_state::NONE)]
    pub state: u8,
}

/// Native EmissionDay and WorldwideDay orchestration.
#[storage_schema]
#[contract(addr = METADOSIS_ADDRESS)]
pub struct MetadosisContract {
    #[attribute(order = 0)]
    pub emission_days: outbe_primitives::storage::dsl::Map<u32, EmissionDay>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};

    #[test]
    fn emission_days_map_base_is_slot_zero() {
        let mut provider = HashMapStorageProvider::new(1);
        let contract = MetadosisContract::new(StorageHandle::new(&mut provider));
        assert_eq!(contract.emission_days.base_slot(), U256::from(0));
    }
}
