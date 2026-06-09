use alloy_primitives::{Address, U256};
use outbe_macros::{contract, storage_schema};
use outbe_primitives::addresses::PROMIS_ADDRESS;

/// EVM storage layout for the Promis token contract.
///
/// Storage slots:
///   0: total_supply (U256)
///   1: mapping(address => U256) — balance
#[storage_schema]
#[contract(addr = PROMIS_ADDRESS)]
pub struct Promis {
    #[attribute(order = 0)]
    pub total_supply: outbe_primitives::storage::dsl::Value<U256>,

    #[attribute(order = 1)]
    pub balances: outbe_primitives::storage::dsl::Map<Address, U256>,
}
