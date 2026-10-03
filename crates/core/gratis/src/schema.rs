use alloy_primitives::{Address, U256};
use outbe_macros::contract;
use outbe_primitives::{
    addresses::GRATIS_ADDRESS,
    storage::types::{Mapping, Slot, StorageBytes},
};
/// Fresh-genesis layout. Notes and Credis share the aggregate pledged backing.
#[contract(addr = GRATIS_ADDRESS)]
pub struct Gratis {
    pub total_supply: Slot<U256>,
    pub pledged_total_supply: Slot<U256>,
    pub balance_ct: Mapping<Address, StorageBytes>,
    pub op_nonce: Mapping<Address, u64>,
}
