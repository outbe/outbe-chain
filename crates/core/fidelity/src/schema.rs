use alloy_primitives::B256;
use outbe_macros::contract;
use outbe_primitives::{
    addresses::FIDELITY_ADDRESS,
    storage::types::{Mapping, Slot, StorageBytes},
};
/// Independent Fidelity journal. Slot one retains the public league anchor.
#[contract(addr = FIDELITY_ADDRESS)]
pub struct FidelityContract {
    pub reserved: Slot<u64>,
    pub first_qualified_start: Slot<u64>,
    pub journal_count: Slot<u64>,
    pub journal_head: Slot<B256>,
    pub journal_records: Mapping<u64, StorageBytes>,
    pub journal_roots: Mapping<u64, B256>,
}
