use alloy_primitives::U256;
use outbe_macros::contract;
use outbe_primitives::addresses::GRATIS_ADDRESS;
use outbe_primitives::storage::types::Slot;

/// Clean-genesis layout: public aggregates only. The global encrypted journal
/// is stored in domain-separated slots by `outbe_tee::pledge_ledger`.
#[contract(addr = GRATIS_ADDRESS)]
pub struct Gratis {
    pub total_supply: Slot<U256>,
    pub pledged_total_supply: Slot<U256>,
}
