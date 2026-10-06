//! Persisted day metadata. Amounts live only in the encrypted byte mappings.

use alloy_primitives::B256;
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

#[storage_record(exists_field = initialized)]
pub struct StoredDayPreAdmission {
    #[key]
    pub worldwide_day: WorldwideDay,
    #[attribute(order = 0, default = false)]
    pub initialized: bool,
    #[attribute(order = 1, default = false)]
    pub is_sealed: bool,
    #[attribute(order = 2, default = B256::ZERO)]
    pub sealed_collection_root: B256,
    #[attribute(order = 3, default = 0)]
    pub sealed_tribute_count: u32,
    #[attribute(order = 5, default = 0)]
    pub canonical_body_bytes: u64,
    #[attribute(order = 6, default = 0)]
    pub distinct_owner_count: u32,
    #[attribute(order = 7, default = 0)]
    pub distinct_reference_currency_count: u16,
    #[attribute(order = 9, default = 0)]
    pub source_generation: u64,
}
