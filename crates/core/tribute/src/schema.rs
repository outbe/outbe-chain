use alloy_primitives::{Address, U256};
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::addresses::TRIBUTE_ADDRESS;
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::wwd_entity_id::WwdEntityId;
use serde::{Deserialize, Serialize};

/// Legacy plaintext tribute. Current issuance stores ciphertext instead.
///
/// `issuance_amount_minor` and `nominal_amount_minor` use six-decimal minor
/// units. `tribute_price_minor` is the public effective reference price on
/// that same scale.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[storage_record(exists_field = owner)]
pub struct TributeData {
    #[key]
    pub tribute_id: WwdEntityId,

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
}

/// Live day view. `tribute_nominal_total_minor` is the enclave-read nominal
/// sum in six-decimal minor units. The stored day nominal remains ciphertext.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DayTotals {
    pub worldwide_day: WorldwideDay,

    pub initialized: bool,

    pub tribute_count: u32,

    pub tribute_nominal_total_minor: U256,

    pub is_sealed: bool,
}

impl DayTotals {
    pub fn with_key(worldwide_day: WorldwideDay) -> Self {
        Self {
            worldwide_day,
            initialized: false,
            tribute_count: 0,
            tribute_nominal_total_minor: U256::ZERO,
            is_sealed: false,
        }
    }
}

#[storage_schema]
#[contract(addr = TRIBUTE_ADDRESS)]
pub struct TributeContract {
    #[attribute(order = 0)]
    pub total_supply: outbe_primitives::storage::dsl::Value<u64>,

    #[attribute(order = 2)]
    pub day_totals:
        outbe_primitives::storage::dsl::Map<WorldwideDay, crate::day_schema::StoredDayTotals>,

    /// Ciphertext of the live day nominal. Not a plaintext minor integer.
    #[attribute(order = 7)]
    pub day_nominal_ct:
        outbe_primitives::storage::dsl::Map<WorldwideDay, outbe_primitives::storage::dsl::Bytes>,
    #[attribute(order = 8)]
    pub frozen_day_nominal_ct:
        outbe_primitives::storage::dsl::Map<WorldwideDay, outbe_primitives::storage::dsl::Bytes>,
    #[attribute(order = 9)]
    pub records:
        outbe_primitives::storage::dsl::Map<WwdEntityId, crate::storage_records::StoredTribute>,
    #[attribute(order = 10)]
    pub owner_ids: outbe_primitives::storage::dsl::Map<
        Address,
        outbe_primitives::storage::dsl::List<WwdEntityId>,
    >,
    #[attribute(order = 11)]
    pub day_ids: outbe_primitives::storage::dsl::Map<
        WorldwideDay,
        outbe_primitives::storage::dsl::List<WwdEntityId>,
    >,
    #[attribute(order = 12)]
    pub owner_id_position: outbe_primitives::storage::dsl::Map<WwdEntityId, u32>,
    #[attribute(order = 13)]
    pub day_id_position: outbe_primitives::storage::dsl::Map<WwdEntityId, u32>,
    #[attribute(order = 14)]
    pub retired_days: outbe_primitives::storage::dsl::Map<WorldwideDay, bool>,
    #[attribute(order = 15)]
    pub tribute_leagues: outbe_primitives::storage::dsl::Map<WwdEntityId, u16>,
    #[attribute(order = 16)]
    pub day_active_leagues: outbe_primitives::storage::dsl::Map<
        WorldwideDay,
        outbe_primitives::storage::dsl::List<u16>,
    >,
    #[attribute(order = 17)]
    pub fi_group_counts: outbe_primitives::storage::dsl::Map<u64, u64>,
    #[attribute(order = 18)]
    pub fi_group_nominals: outbe_primitives::storage::dsl::Map<u64, U256>,
    #[attribute(order = 19)]
    pub day_fractions: outbe_primitives::storage::dsl::Map<u64, u64>,
    #[attribute(order = 20)]
    pub day_lysis_limit_minor: outbe_primitives::storage::dsl::Map<WorldwideDay, U256>,
    #[attribute(order = 21)]
    pub day_metadosis_completed_at: outbe_primitives::storage::dsl::Map<WorldwideDay, u64>,
    #[attribute(order = 22)]
    pub worldwide_days: outbe_primitives::storage::dsl::Map<
        WorldwideDay,
        crate::worldwideday::schema::WorldwideDay,
    >,
    #[attribute(order = 23)]
    pub active_wwd: outbe_primitives::storage::dsl::Set<WorldwideDay>,
}

pub const fn day_league_key(day: WorldwideDay, league: u16) -> u64 {
    ((day.value() as u64) << 16) | (league as u64)
}

impl<'storage> TributeContract<'storage> {
    pub(crate) fn storage_handle(&self) -> outbe_primitives::storage::StorageHandle<'storage> {
        self.storage.clone()
    }
}
