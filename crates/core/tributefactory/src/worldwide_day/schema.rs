use outbe_macros::storage_record;
use outbe_system_common::time::WorldwideDay as WorldwideDayKey;

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
}

impl WorldwideDay {
    pub fn forming_start(&self) -> Result<u64, outbe_system_common::time::TimeError> {
        self.wwd.start_timestamp()
    }

    pub fn forming_end(&self) -> Result<u64, outbe_system_common::time::TimeError> {
        Ok(self
            .forming_start()?
            .saturating_add(outbe_chain_constants::get_metadosis_forming_period_seconds()))
    }
}
