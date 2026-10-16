use alloy_primitives::U256;
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::addresses::METADOSIS_ADDRESS;
use outbe_primitives::storage::types::{Mapping, StorageBytes};
use outbe_primitives::time::WorldwideDay as WorldwideDayKey;

/// WorldwideDay status values stored as u8.
pub mod status {
    pub const FORMING: u8 = 0;
    pub const LOOKBACK_DELAY: u8 = 1;
    pub const OFFERING: u8 = 2;
    pub const WAITING: u8 = 3;
    pub const READY: u8 = 4;
    pub const COMPLETED: u8 = 6;
    pub const FAILED: u8 = 7;
}

/// Day type values.
pub mod day_type {
    pub const UNKNOWN: u8 = 0;
    pub const GREEN: u8 = 1;
    pub const RED: u8 = 2;
}

/// Durable outer-WWD terminal receipt discriminants.
pub mod terminal_outcome {
    pub const NONE: u8 = 0;
    pub const MISSED_OFFERING: u8 = 1;
    pub const CAPACITY_FORFEITURE: u8 = 2;
    pub const METADOSIS_FAILURE: u8 = 3;
    pub const LYSIS_COMPLETED: u8 = 4;
}

/// Canonical Tribute partition-retirement result recorded by a terminal receipt.
pub mod terminal_retirement {
    pub const NONE: u8 = 0;
    pub const NOT_PRESENT: u8 = 1;
    pub const REQUESTED: u8 = 2;
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
    pub metadosis_limit_minor: U256,

    #[attribute(order = 8, default = U256::ZERO)]
    pub previous_vwap: U256,

    #[attribute(order = 9, default = U256::ZERO)]
    pub current_vwap: U256,
}

/// Semantic replay receipt for the Cycle-owned day-limit slot.
#[storage_record(exists_field = formed)]
pub struct DayLimitFormationReceiptState {
    #[key]
    pub wwd: WorldwideDayKey,

    #[attribute(order = 0)]
    pub formed: bool,

    #[attribute(order = 1, default = U256::ZERO)]
    pub base_limit_minor: U256,

    #[attribute(order = 2, default = U256::ZERO)]
    pub promis_limit_before_minor: U256,

    #[attribute(order = 3, default = U256::ZERO)]
    pub promis_limit_taken_minor: U256,

    #[attribute(order = 4, default = U256::ZERO)]
    pub promis_limit_after_minor: U256,

    #[attribute(order = 5, default = U256::ZERO)]
    pub metadosis_limit_minor: U256,

    #[attribute(order = 6)]
    pub block_number: u64,
}

/// Native WorldwideDay lifecycle and durable receipts.
#[storage_schema]
#[contract(addr = METADOSIS_ADDRESS)]
pub struct MetadosisContract {
    #[attribute(order = 0)]
    pub bootstrap_end_time: outbe_primitives::storage::dsl::Value<u64>,
    #[attribute(order = 1)]
    pub worldwide_days: outbe_primitives::storage::dsl::Map<WorldwideDayKey, WorldwideDay>,
    #[attribute(order = 2)]
    pub active_wwd: outbe_primitives::storage::dsl::Set<WorldwideDayKey>,
    /// Retains terminal days up to MAX_RECORDS_KEPT.
    #[attribute(order = 3)]
    pub closed_wwd: outbe_primitives::storage::dsl::Deque<WorldwideDayKey>,
    #[attribute(order = 4)]
    pub worldwide_day_terminal_receipts: Mapping<WorldwideDayKey, StorageBytes>,
    #[attribute(order = 5)]
    pub day_limit_formation_receipts:
        outbe_primitives::storage::dsl::Map<WorldwideDayKey, DayLimitFormationReceiptState>,
}
