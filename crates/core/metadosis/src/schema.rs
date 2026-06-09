use alloy_primitives::{B256, U256};
use outbe_common::WorldwideDay as WorldwideDayKey;
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::addresses::METADOSIS_ADDRESS;

/// WorldwideDay status values stored as u8.
pub mod status {
    pub const FORMING: u8 = 0;
    pub const LOOKBACK_DELAY: u8 = 1;
    pub const OFFERING: u8 = 2;
    pub const WAITING: u8 = 3;
    pub const READY: u8 = 4;
    pub const IN_PROGRESS: u8 = 5;
    pub const COMPLETED: u8 = 6;
    pub const FAILED: u8 = 7;
}

/// Day type values.
pub mod day_type {
    pub const UNKNOWN: u8 = 0;
    pub const GREEN: u8 = 1;
    pub const RED: u8 = 2;
}

#[storage_record(exists_field = forming_start)]
pub struct WorldwideDay {
    #[key]
    pub wwd: WorldwideDayKey,

    #[attribute(order = 0, default = status::FORMING)]
    pub status: u8,

    #[attribute(order = 1, default = day_type::UNKNOWN)]
    pub day_type: u8,

    #[attribute(order = 2)]
    pub forming_start: u64,

    #[attribute(order = 3)]
    pub forming_end: u64,

    #[attribute(order = 4)]
    pub lookback_end: u64,

    #[attribute(order = 5)]
    pub offering_end: u64,

    #[attribute(order = 6)]
    pub scheduled_process_time: u64,

    #[attribute(order = 7, default = U256::ZERO)]
    pub previous_vwap: U256,

    #[attribute(order = 8, default = U256::ZERO)]
    pub current_vwap: U256,
}

/// EVM storage layout for the Metadosis orchestrator contract.
///
/// Manages worldwide day lifecycle and daily emission accumulation.
#[storage_schema]
#[contract(addr = METADOSIS_ADDRESS)]
pub struct MetadosisContract {
    #[attribute(order = 0)]
    pub bootstrap_end_time: outbe_primitives::storage::dsl::Value<u64>,

    #[attribute(order = 1)]
    pub worldwide_days: outbe_primitives::storage::dsl::Map<WorldwideDayKey, WorldwideDay>,

    #[attribute(order = 2)]
    pub day_limit_amount: outbe_primitives::storage::dsl::Map<WorldwideDayKey, U256>,

    #[attribute(order = 3)]
    pub day_limit_used: outbe_primitives::storage::dsl::Map<WorldwideDayKey, bool>,

    #[attribute(order = 4)]
    pub active_wwd_count: outbe_primitives::storage::dsl::Value<u32>,

    #[attribute(order = 5)]
    pub active_wwds: outbe_primitives::storage::dsl::Map<u32, u32>,

    #[attribute(order = 6)]
    pub config_oracle_pair_hash: outbe_primitives::storage::dsl::Value<B256>,

    #[attribute(order = 7)]
    pub day_limit_exists: outbe_primitives::storage::dsl::Map<WorldwideDayKey, bool>,

    #[attribute(order = 8)]
    pub day_limit_count: outbe_primitives::storage::dsl::Value<u32>,

    #[attribute(order = 9)]
    pub day_limit_dates: outbe_primitives::storage::dsl::Map<u32, u32>,
}
