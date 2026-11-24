//! Persistent EVM storage schema for the SpendingUnitV2 precompile.

use alloy_primitives::{Address, B256, U256};
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_system_common::addresses::SPENDING_UNIT_V2_ADDRESS;

/// Stored entity record for one settled SpendingUnitV2.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[storage_record(exists_field = stored_su_id)]
pub struct SpendingUnitV2Record {
    #[key]
    pub su_id: U256,

    #[attribute(order = 0)]
    pub stored_su_id: U256,

    #[attribute(order = 1)]
    pub derived_owner: B256,

    #[attribute(order = 2)]
    pub attester_address: Address,

    #[attribute(order = 3)]
    pub referrer_address: Address,

    #[attribute(order = 4)]
    pub currency: u16,

    #[attribute(order = 5)]
    pub worldwide_day: u64,

    #[attribute(order = 6)]
    pub amount_base: u64,

    #[attribute(order = 7)]
    pub amount_atto: u64,

    #[attribute(order = 8)]
    pub ctx: B256,

    #[attribute(order = 9)]
    pub seal_id: B256,
}

/// EVM storage layout for `SpendingUnitV2` (`0x5200000000000000000000000000000000000008`).
///
/// Storage slots:
///   0: `owner` (`Address`) — genesis writes slot 0
///   1: `paused` (`bool`)
///   2: `attester_registry` (`Address`) — genesis writes slot 2
///   3: `enclave_registry` (`Address`) — genesis writes slot 3
///   4: `fs_epoch_registry` (`Address`) — genesis writes slot 4
///   5: `total_committed` (`U256`)
///   6: `committed` (`Map<U256, bool>`)
///   7: `settlements` (`Map<B256, B256>`)
///   8..18: `entities` (`Map<U256, SpendingUnitV2Record>`, 10 slots)
#[storage_schema]
#[contract(addr = SPENDING_UNIT_V2_ADDRESS)]
pub struct SpendingUnitV2Contract {
    #[attribute(order = 0)]
    pub owner: Value<Address>,

    #[attribute(order = 1)]
    pub paused: Value<bool>,

    #[attribute(order = 2)]
    pub attester_registry: Value<Address>,

    #[attribute(order = 3)]
    pub enclave_registry: Value<Address>,

    #[attribute(order = 4)]
    pub fs_epoch_registry: Value<Address>,

    #[attribute(order = 5)]
    pub total_committed: Value<U256>,

    #[attribute(order = 6)]
    pub committed: Map<U256, bool>,

    #[attribute(order = 7)]
    pub settlements: Map<B256, B256>,

    #[attribute(order = 8)]
    pub entities: Map<U256, SpendingUnitV2Record>,
}
