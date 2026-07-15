use alloy_primitives::{Address, U256};
use outbe_common::WorldwideDay;
use outbe_compressed_entities::EntityId36;
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::addresses::TRIBUTE_ADDRESS;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[storage_record(exists_field = owner)]
pub struct TributeData {
    #[key]
    pub tribute_id: EntityId36,

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

    #[attribute(order = 2)]
    pub day_totals: outbe_primitives::storage::dsl::Map<WorldwideDay, DayTotals>,
}

impl<'storage> TributeContract<'storage> {
    pub(crate) fn storage_handle(&self) -> outbe_primitives::storage::StorageHandle<'storage> {
        self.storage.clone()
    }
}
