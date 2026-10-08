use alloy_primitives::{Address, U256};
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::addresses::GRATIS_FACTORY_ADDRESS;

/// Gratis pledged by `source` for one Credis reservation and not yet used.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[storage_record(exists_field = source)]
pub struct PledgeRecord {
    #[key]
    pub reservation_id: U256,
    #[attribute(order = 0)]
    pub source: Address,
    #[attribute(order = 1)]
    pub gratis_minor: U256,
}

/// Gratis backing one issued Credis position. Returns and burns draw it down,
/// and it closes at zero.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[storage_record(exists_field = source)]
pub struct CollateralAllocation {
    #[key]
    pub position_id: U256,
    #[attribute(order = 0)]
    pub source: Address,
    #[attribute(order = 1)]
    pub remaining_minor: U256,
}

#[storage_schema]
#[contract(addr = GRATIS_FACTORY_ADDRESS)]
pub struct GratisFactoryContract {
    #[attribute(order = 0)]
    pub pledges: outbe_primitives::storage::dsl::Map<U256, PledgeRecord>,
    #[attribute(order = 1)]
    pub collateral: outbe_primitives::storage::dsl::Map<U256, CollateralAllocation>,
}
