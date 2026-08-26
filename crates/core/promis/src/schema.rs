use alloy_primitives::{Address, U256};
use outbe_macros::{contract, storage_schema};
use outbe_primitives::addresses::PROMIS_ADDRESS;

/// EVM storage layout for the Promis token contract.
///
/// Storage slots:
///   0: reserved storage schema version (u32)
///   1: total_supply (U256)
///   2: mapping(address => U256) — balance
#[storage_schema]
#[contract(addr = PROMIS_ADDRESS)]
pub struct Promis {
    /// Slot 0: reserved storage schema version.
    #[attribute(order = 0)]
    pub _reserved_schema_version: outbe_primitives::storage::dsl::Value<u32>,

    #[attribute(order = 1)]
    pub total_supply: outbe_primitives::storage::dsl::Value<U256>,

    #[attribute(order = 2)]
    pub balances: outbe_primitives::storage::dsl::Map<Address, U256>,
}
