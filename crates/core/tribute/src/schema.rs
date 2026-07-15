use alloy_primitives::{Address, B256, U256};
use outbe_common::WorldwideDay;
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::addresses::TRIBUTE_ADDRESS;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[storage_record(exists_field = owner)]
pub struct TributeData {
    #[key]
    pub token_id: U256,

    #[attribute(order = 0)]
    pub owner: Address,

    #[attribute(order = 1)]
    pub worldwide_day: WorldwideDay,

    #[attribute(order = 2)]
    pub issuance_amount_minor: U256,

    #[attribute(order = 3)]
    pub issuance_currency: u16,

    #[attribute(order = 4)]
    pub nominal_amount_minor: U256,

    #[attribute(order = 5)]
    pub reference_currency: u16,

    #[attribute(order = 6)]
    pub tribute_price_minor: U256,

    #[attribute(order = 7, default = false)]
    pub exclude_from_intex_issuance: bool,
}

#[storage_record(exists_field = initialized)]
pub struct DayTotals {
    #[key]
    pub worldwide_day: WorldwideDay,

    #[attribute(order = 0, default = false)]
    pub initialized: bool,

    #[attribute(order = 1, default = 0)]
    pub tribute_count: u32,

    #[attribute(order = 2, default = U256::ZERO)]
    pub tribute_nominal_amount: U256,

    #[attribute(order = 4, default = false)]
    pub is_sealed: bool,
}

#[storage_schema]
#[contract(addr = TRIBUTE_ADDRESS)]
pub struct TributeContract {
    #[attribute(order = 0)]
    pub total_supply: outbe_primitives::storage::dsl::Value<u64>,

    #[attribute(order = 1)]
    pub tributes: outbe_primitives::storage::dsl::Map<U256, TributeData>,

    #[attribute(order = 2)]
    pub day_totals: outbe_primitives::storage::dsl::Map<WorldwideDay, DayTotals>,

    #[attribute(order = 3)]
    pub day_index_counts: outbe_primitives::storage::dsl::Map<WorldwideDay, u32>,

    #[attribute(order = 4)]
    pub day_token_ids: outbe_primitives::storage::dsl::Map<B256, U256>,

    #[attribute(order = 5)]
    pub owner_index_counts: outbe_primitives::storage::dsl::Map<Address, u32>,

    #[attribute(order = 6)]
    pub owner_tribute_ids: outbe_primitives::storage::dsl::Map<B256, U256>,
}
