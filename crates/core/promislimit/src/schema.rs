use alloy_primitives::U256;
use outbe_macros::{contract, storage_schema};
use outbe_primitives::storage::types::Mapping;
use outbe_system_common::addresses::PROMIS_LIMIT_ADDRESS;

#[storage_schema]
#[contract(addr = PROMIS_LIMIT_ADDRESS)]
pub struct PromisLimitContract {
    #[attribute(order = 0)]
    pub(crate) promis_limit: outbe_primitives::storage::dsl::Value<U256>,
    #[attribute(order = 1)]
    pub(crate) burn_rests: Mapping<u8, U256>,
    #[attribute(order = 2)]
    pub(crate) base_fee_count: outbe_primitives::storage::dsl::Value<U256>,
}
