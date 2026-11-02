//! Persistent CCA records and the current active-agent index.
use crate::precompile::ICcaRegistry;
use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::{
    addresses::CCA_REGISTRY_ADDRESS,
    storage::types::{Storable, StorableType},
};

#[derive(Debug, Clone)]
#[storage_record(exists_field = name)]
pub struct CcaRecord {
    #[key]
    pub cca: Address,
    #[attribute(order = 0)]
    pub name: String,
    #[attribute(order = 1, default = ICcaRegistry::State::Bonding)]
    pub state: ICcaRegistry::State,
    /// Native COEN atomic units, retained during deregistration until claimed.
    #[attribute(order = 2)]
    pub bonded_minor: U256,
    /// Unix seconds. The value comes from a checked conversion of the execution timestamp.
    #[attribute(order = 3)]
    pub unbond_unlocks_after: u64,
}

#[storage_schema]
#[contract(addr = CCA_REGISTRY_ADDRESS)]
pub struct CcaContract {
    #[attribute(order = 0)]
    pub records: outbe_primitives::storage::dsl::Map<Address, CcaRecord>,
    #[attribute(order = 1)]
    pub active: outbe_primitives::storage::dsl::Set<Address>,
    /// Six-decimal net GRATIS per UTC day. At most one of weight/deficit is nonzero.
    #[attribute(order = 2)]
    pub gratis_minor_per_utc_day: outbe_primitives::storage::dsl::Map<B256, U256>,
    /// Excess burns offset later openings for the same CCA and UTC day only.
    #[attribute(order = 3)]
    pub gratis_deficit_minor_per_utc_day: outbe_primitives::storage::dsl::Map<B256, U256>,
    /// Native COEN bonded across all records and not yet claimed back.
    #[attribute(order = 4)]
    pub total_bonded_minor: outbe_primitives::storage::dsl::Value<U256>,
    /// Append-only indices include deficit-only addresses and never swap-remove.
    #[attribute(order = 5)]
    pub reward_day_count: outbe_primitives::storage::dsl::Map<u32, u32>,
    #[attribute(order = 6)]
    pub reward_day_total: outbe_primitives::storage::dsl::Map<u32, U256>,
    #[attribute(order = 7)]
    pub reward_day_addresses: outbe_primitives::storage::dsl::Map<B256, Address>,
    #[attribute(order = 8)]
    pub reward_day_members: outbe_primitives::storage::dsl::Map<B256, bool>,
    #[attribute(order = 9)]
    pub sealed_through_day: outbe_primitives::storage::dsl::Value<u32>,
    #[attribute(order = 10)]
    pub has_sealed_day: outbe_primitives::storage::dsl::Value<bool>,
    /// Retained after cleanup so duplicate scheduling cannot recreate work.
    #[attribute(order = 11)]
    pub day_cleanup_scheduled: outbe_primitives::storage::dsl::Map<u32, bool>,
    #[attribute(order = 12)]
    pub day_cleanup_queue: outbe_primitives::storage::dsl::Map<u64, u32>,
    #[attribute(order = 13)]
    pub day_cleanup_head: outbe_primitives::storage::dsl::Value<u64>,
    #[attribute(order = 14)]
    pub day_cleanup_tail: outbe_primitives::storage::dsl::Value<u64>,
    #[attribute(order = 15)]
    pub day_cleanup_cursor: outbe_primitives::storage::dsl::Map<u32, u32>,
    #[attribute(order = 16)]
    pub cleanup_processed_block: outbe_primitives::storage::dsl::Value<u64>,
    #[attribute(order = 17)]
    pub has_cleanup_processed_block: outbe_primitives::storage::dsl::Value<bool>,
}

pub(crate) fn day_index_key(day: u32, index: u32) -> B256 {
    B256::from(U256::from((u64::from(day) << 32) | u64::from(index)))
}

pub(crate) fn address_day_key(cca: Address, day: u32) -> B256 {
    let mut bytes = [0u8; 24];
    bytes[..20].copy_from_slice(cca.as_slice());
    bytes[20..].copy_from_slice(&day.to_be_bytes());
    keccak256(bytes)
}

impl StorableType for ICcaRegistry::State {
    const SLOTS: usize = 1;
}

impl Storable for ICcaRegistry::State {
    fn from_word(word: U256) -> Self {
        // Check the full word fits u8 before decoding the enum. Never truncate storage.
        // Storable is infallible, so validation rejects this sentinel at the record boundary.
        u8::try_from(word)
            .ok()
            .and_then(|value| Self::try_from(value).ok())
            .unwrap_or(Self::__Invalid)
    }

    fn to_word(&self) -> U256 {
        U256::from(u8::from(*self))
    }
}
