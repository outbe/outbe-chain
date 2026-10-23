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

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};

    #[test]
    fn storage_slot_layout_is_pinned() {
        let mut provider = HashMapStorageProvider::new(1);
        let c = Gratis::new(StorageHandle::new(&mut provider));

        assert_eq!(c.pledged_total_supply.slot(), U256::from(0));
        assert_eq!(c.balance_ct.base_slot(), U256::from(1));
        assert_eq!(c.op_nonce.base_slot(), U256::from(2));
        assert_eq!(c.pledged_ct.base_slot(), U256::from(3));
    }
}
