use outbe_macros::storage_record;
use outbe_primitives::time::WorldwideDay as WorldwideDayKey;

pub mod wwd_state {
    pub const PENDING: u8 = 0;
    pub const ACTIVE: u8 = 1;
    pub const CLOSED: u8 = 2;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[storage_record(packed, exists_field = offering_start)]
pub struct WorldwideDay {
    #[key]
    pub wwd: WorldwideDayKey,

    #[attribute(order = 0)]
    pub emission_day: u32,

    #[attribute(order = 1)]
    pub offering_start: u64,

    #[attribute(order = 2)]
    pub offering_end: u64,

    #[attribute(order = 3)]
    pub state: u8,
}

impl WorldwideDay {
    #[must_use]
    pub fn forming_start(&self) -> u64 {
        self.wwd.start_timestamp()
    }

    #[must_use]
    pub fn forming_end(&self) -> u64 {
        self.forming_start()
            .saturating_add(outbe_chain_constants::get_metadosis_forming_period_seconds())
    }
}
