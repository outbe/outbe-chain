use alloy_primitives::{Address, B256, U256};
use outbe_compressed_entities::WwdEntityId;
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::addresses::TRIBUTE_ADDRESS;
use outbe_primitives::storage::types::{Mapping, StorageBytes};
use outbe_primitives::time::WorldwideDay;
use serde::{Deserialize, Serialize};

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

    #[attribute(order = 7, default = false)]
    pub exclude_from_intex_issuance: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DayTotals {
    pub worldwide_day: WorldwideDay,

    pub initialized: bool,

    pub tribute_count: u32,

    pub tribute_nominal_total_minor: U256,

    pub is_sealed: bool,
}

/// Bounded, incrementally maintained Tribute inputs used by OCOMP
/// pre-admission. The live accumulator is frozen exactly once after the
/// Tribute WWD and its CE collection have both been sealed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DayPreAdmission {
    pub worldwide_day: WorldwideDay,

    pub initialized: bool,

    pub is_sealed: bool,

    pub sealed_collection_root: B256,

    pub sealed_tribute_count: u32,

    pub sealed_tribute_nominal_total_minor: U256,

    pub canonical_body_bytes: u64,

    pub distinct_owner_count: u32,

    pub distinct_reference_currency_count: u16,

    /// Certified source generation. A sealed, unconsumed Tribute partition is
    /// generation 0; OCM-20 advances it exactly once on logical retirement.
    /// This is owner state, not an OCOMP reservation.
    pub source_generation: u64,
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

impl DayPreAdmission {
    pub fn with_key(worldwide_day: WorldwideDay) -> Self {
        Self {
            worldwide_day,
            initialized: false,
            is_sealed: false,
            sealed_collection_root: B256::ZERO,
            sealed_tribute_count: 0,
            sealed_tribute_nominal_total_minor: U256::ZERO,
            canonical_body_bytes: 0,
            distinct_owner_count: 0,
            distinct_reference_currency_count: 0,
            source_generation: 0,
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

    #[attribute(order = 3)]
    pub day_pre_admission:
        outbe_primitives::storage::dsl::Map<WorldwideDay, crate::day_schema::StoredDayPreAdmission>,

    #[attribute(order = 5)]
    pub day_reference_currency_refcount: outbe_primitives::storage::dsl::Map<B256, u32>,

    /// Set only by the genesis-bound OCOMP lifecycle. When false, all
    /// historical Tribute mutations keep their pre-activation storage footprint.
    #[attribute(order = 6)]
    pub ocomp_profile_ready: outbe_primitives::storage::dsl::Value<bool>,
    #[attribute(order = 7)]
    pub day_nominal_ct: outbe_primitives::storage::types::Mapping<
        WorldwideDay,
        outbe_primitives::storage::types::StorageBytes,
    >,
    #[attribute(order = 8)]
    pub frozen_day_nominal_ct: outbe_primitives::storage::types::Mapping<
        WorldwideDay,
        outbe_primitives::storage::types::StorageBytes,
    >,
}

impl<'storage> TributeContract<'storage> {
    pub(crate) fn storage_handle(&self) -> outbe_primitives::storage::StorageHandle<'storage> {
        self.storage.clone()
    }
}
