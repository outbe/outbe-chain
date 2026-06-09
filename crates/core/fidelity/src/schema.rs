use alloy_primitives::Address;
use outbe_macros::{contract, storage_schema};
use outbe_primitives::addresses::FIDELITY_ADDRESS;

#[storage_schema]
#[contract(addr = FIDELITY_ADDRESS)]
pub struct FidelityContract {
    #[attribute(order = 0)]
    pub fidelity_indices: outbe_primitives::storage::dsl::Map<Address, u64>,
}
