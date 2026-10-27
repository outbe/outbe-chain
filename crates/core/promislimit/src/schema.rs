use alloy_primitives::U256;
use outbe_macros::{contract, storage_schema};
use outbe_primitives::addresses::PROMIS_LIMIT_ADDRESS;

#[storage_schema]
#[contract(addr = PROMIS_LIMIT_ADDRESS)]
pub struct PromisLimitContract {
    #[attribute(order = 0)]
    pub(crate) promis_limit: outbe_primitives::storage::dsl::Value<U256>,
    #[attribute(order = 1)]
    pub(crate) native_rest: outbe_primitives::storage::dsl::Value<U256>,
}
