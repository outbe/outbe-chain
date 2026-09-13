//! Persistent CCA records and the current active-agent index.
use alloy_primitives::{Address, U256};
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::addresses::CCA_ADDRESS;

#[derive(Debug, Clone)]
#[storage_record(exists_field = state)]
pub struct CcaRecord {
    #[key]
    pub cca: Address,
    #[attribute(order = 0)]
    pub state: u8,
    /// Native COEN atomic units, 18 decimals.
    #[attribute(order = 1)]
    pub self_bond: U256,
    #[attribute(order = 2)]
    pub unbond_amount: U256,
    /// Unix seconds; checked conversion from the execution timestamp.
    #[attribute(order = 3)]
    pub unbond_complete_time: u64,
    /// Six-decimal opening GRATIS less the remaining GRATIS burned on void.
    #[attribute(order = 4)]
    pub reward_weight: U256,
    /// Native COEN atomic units, independent of the bond.
    #[attribute(order = 5)]
    pub claimable_rewards: U256,
}

#[storage_schema]
#[contract(addr = CCA_ADDRESS)]
pub struct CcaContract {
    #[attribute(order = 0)]
    pub records: outbe_primitives::storage::dsl::Map<Address, CcaRecord>,
    #[attribute(order = 1)]
    pub active: outbe_primitives::storage::dsl::Set<Address>,
}
