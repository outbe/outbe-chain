use alloy_primitives::{B256, U256};
use outbe_macros::contract;
use outbe_primitives::{
    addresses::GRATIS_ADDRESS,
    storage::types::{Mapping, Slot, StorageBytes},
};
/// Fresh-genesis layout. All source-bearing state is in the global encrypted journal.
#[contract(addr = GRATIS_ADDRESS)]
pub struct Gratis {
    pub total_supply: Slot<U256>,
    pub pledged_total_supply: Slot<U256>,
    pub journal_count: Slot<u64>,
    pub journal_head: Slot<B256>,
    pub journal_records: Mapping<u64, StorageBytes>,
    pub journal_roots: Mapping<u64, B256>,
}
