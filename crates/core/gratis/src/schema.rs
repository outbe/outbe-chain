use alloy_primitives::{Address, U256};
use outbe_macros::contract;
use outbe_primitives::{
    addresses::GRATIS_ADDRESS,
    storage::types::{Mapping, Slot, StorageBytes},
};
/// Fresh-genesis layout. `pledged_total_supply` sums every account's pledged blob.
#[contract(addr = GRATIS_ADDRESS)]
pub struct Gratis {
    pub pledged_total_supply: Slot<U256>,
    pub balance_ct: Mapping<Address, StorageBytes>,
    pub op_nonce: Mapping<Address, u64>,
    pub pledged_ct: Mapping<Address, StorageBytes>,
}
