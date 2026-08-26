use alloy_primitives::Address;
use outbe_macros::{contract, storage_schema};
use outbe_primitives::addresses::FIDELITY_ADDRESS;

#[storage_schema]
#[contract(addr = FIDELITY_ADDRESS)]
pub struct FidelityContract {
    /// Slot 0: reserved storage schema version.
    #[attribute(order = 0)]
    pub _reserved_schema_version: outbe_primitives::storage::dsl::Value<u32>,

    #[attribute(order = 1)]
    pub fidelity_indices: outbe_primitives::storage::dsl::Map<Address, u64>,
}
