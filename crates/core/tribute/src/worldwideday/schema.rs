use alloy_primitives::U256;
use outbe_macros::storage_record;
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
    pub previous_vwap_minor: U256,

    #[attribute(order = 9, default = U256::ZERO)]
    pub current_vwap_minor: U256,
}
