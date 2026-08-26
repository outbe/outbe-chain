use alloy_primitives::{Address, U256};
use outbe_macros::{contract, storage_schema};
use outbe_primitives::addresses::GRATIS_ADDRESS;

/// EVM storage layout for the Gratis token contract.
///
/// Storage slots:
///   0: reserved storage schema version (u32)
///   1: total_supply (U256)
///   2: mapping(address => U256) — available balance
///   3: mapping(address => U256) — per-account pledged amount
///
/// The aggregate pledged supply (`pledged_total_supply()`) is derived as
/// `balances[CREDIS_ADDRESS]` and is no longer stored separately.
///
/// Per-account pledged balances live here; ticket-level metadata
/// (expiry, ID, …) remains the responsibility of higher-level modules.
#[storage_schema]
#[contract(addr = GRATIS_ADDRESS)]
pub struct Gratis {
    /// Slot 0: reserved storage schema version.
    #[attribute(order = 0)]
    pub _reserved_schema_version: outbe_primitives::storage::dsl::Value<u32>,

    // Slot 1: Total supply of gratis tokens
    #[attribute(order = 1)]
    pub total_supply: outbe_primitives::storage::dsl::Value<U256>,

    // Slot 2: mapping(address => balance)
    #[attribute(order = 2)]
    pub balances: outbe_primitives::storage::dsl::Map<Address, U256>,

    // Slot 3: mapping(address => amount currently pledged by that address)
    #[attribute(order = 3)]
    pub pledged_balances: outbe_primitives::storage::dsl::Map<Address, U256>,
}
