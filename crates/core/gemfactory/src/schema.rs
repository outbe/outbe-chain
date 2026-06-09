use alloy_primitives::U256;
use outbe_macros::{contract, storage_schema};
use outbe_primitives::addresses::GEM_FACTORY_ADDRESS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum GemTypes {
    Genesis = 0,
    Validator = 1,
    Sra = 2,
    Wallet = 3,
    Cca = 4,
    Merchant = 5,
}

#[storage_schema]
#[contract(addr = GEM_FACTORY_ADDRESS)]
pub struct GemFactoryContract {
    #[attribute(order = 0)]
    pub total_gems_issued: outbe_primitives::storage::dsl::Value<U256>,

    #[attribute(order = 1)]
    pub total_intex_parked: outbe_primitives::storage::dsl::Value<U256>,
}
