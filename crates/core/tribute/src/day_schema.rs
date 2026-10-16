//! Persisted day metadata. Amounts live only in the encrypted byte mappings.

use outbe_macros::storage_record;
use outbe_primitives::time::WorldwideDay;

#[storage_record(exists_field = initialized)]
pub struct StoredDayTotals {
    #[key]
    pub worldwide_day: WorldwideDay,
    #[attribute(order = 0, default = false)]
    pub initialized: bool,
    #[attribute(order = 1, default = 0)]
    pub tribute_count: u32,
    #[attribute(order = 4, default = false)]
    pub is_sealed: bool,
}
