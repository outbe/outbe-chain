use alloy_primitives::U256;
use outbe_macros::{contract, storage_schema};
use outbe_primitives::addresses::PROMIS_LIMIT_ADDRESS;

#[storage_schema]
#[contract(addr = PROMIS_LIMIT_ADDRESS)]
pub struct PromisLimitContract {
    /// Slot 0: reserved storage schema version.
    #[attribute(order = 0)]
    pub _reserved_schema_version: outbe_primitives::storage::dsl::Value<u32>,

    #[attribute(order = 1)]
    pub total_unallocated: outbe_primitives::storage::dsl::Value<U256>,
}
