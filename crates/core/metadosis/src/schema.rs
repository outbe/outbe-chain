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

#[storage_record(exists_field = metadosis_start)]
pub struct EmissionDay {
    #[key]
    pub day: u32,

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
    pub bootstrap_end_time: outbe_primitives::storage::dsl::Value<u64>,
    #[attribute(order = 1)]
    pub emission_days: outbe_primitives::storage::dsl::Map<u32, EmissionDay>,
}
