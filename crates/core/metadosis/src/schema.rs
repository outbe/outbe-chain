use alloy_primitives::U256;
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::addresses::METADOSIS_ADDRESS;
use outbe_primitives::storage::types::{Mapping, StorageBytes};
use outbe_primitives::time::WorldwideDay as WorldwideDayKey;

#[allow(unused_imports)]
pub use outbe_tribute::worldwideday::schema::{day_type, status};

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

/// Native WorldwideDay orchestration and durable receipts.
#[storage_schema]
#[contract(addr = METADOSIS_ADDRESS)]
pub struct MetadosisContract {
    #[attribute(order = 0)]
    pub bootstrap_end_time: outbe_primitives::storage::dsl::Value<u64>,
    #[attribute(order = 4)]
    pub worldwide_day_terminal_receipts: Mapping<WorldwideDayKey, StorageBytes>,
    #[attribute(order = 5)]
    pub day_limit_formation_receipts:
        outbe_primitives::storage::dsl::Map<WorldwideDayKey, DayLimitFormationReceiptState>,
}
