use outbe_macros::storage_record;
use outbe_primitives::time::WorldwideDay as WorldwideDayKey;

#[storage_record(exists_field = forming_start)]
pub struct WorldwideDay {
    #[key]
    pub wwd: WorldwideDayKey,

    #[attribute(order = 0)]
    pub day: u32,

    #[attribute(order = 1)]
    pub forming_start: u64,

    #[attribute(order = 2)]
    pub forming_end: u64,

    #[attribute(order = 3)]
    pub offerings_start: u64,

    #[attribute(order = 4)]
    pub offering_end: u64,
}
