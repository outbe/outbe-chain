//! Slot reservation of `StorageArray` and its sibling collections in
//! `#[contract]` schemas.
//!
//! `Value` needs no import: `#[contract]` replaces it with its full path.

use alloy_primitives::{Address, U256};
use outbe_macros::contract;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::types::{
    Storable, StorableType, StorageArray, StorageBytes, StorageSet, StorageVec,
};
use outbe_primitives::storage::StorageHandle;

const ADDRESS: Address = Address::repeat_byte(0x42);

/// An element type that occupies two slots. No production `Storable` type
/// has `SLOTS = 2`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Wide(pub u64);

impl StorableType for Wide {
    const SLOTS: usize = 2;
}

impl Storable for Wide {
    fn from_word(word: U256) -> Self {
        Self(word.to::<u64>())
    }

    fn to_word(&self) -> U256 {
        U256::from(self.0)
    }
}

#[contract(addr = ADDRESS)]
pub struct WordArray {
    pub head: Value<u64>,
    pub words: StorageArray<U256, 4>,
    pub tail: Value<u64>,
}

#[contract(addr = ADDRESS)]
pub struct WideArray {
    pub head: Value<u64>,
    pub wide: StorageArray<Wide, 4>,
    pub tail: Value<u64>,
}

#[contract(addr = ADDRESS)]
pub struct Siblings {
    pub vec: StorageVec<u64>,
    pub after_vec: Value<u64>,
    pub set: StorageSet<u64>,
    pub after_set: Value<u64>,
    pub bytes: StorageBytes,
    pub after_bytes: Value<u64>,
}

#[test]
fn array_reserves_its_full_width() {
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let contract = WordArray::new(storage.clone());
        assert_eq!(contract.head.slot(), U256::ZERO);
        // The array base is slot 1: element 0 is stored there.
        contract.words.set(0, U256::from(11)).unwrap();
        assert_eq!(
            storage.sload(ADDRESS, U256::from(1)).unwrap(),
            U256::from(11)
        );
        assert_eq!(contract.tail.slot(), U256::from(1 + 4));
    });
}

#[test]
fn array_of_multi_slot_elements_reserves_n_times_width() {
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let contract = WideArray::new(storage.clone());
        contract.wide.set(0, Wide(5)).unwrap();
        contract.wide.set(3, Wide(8)).unwrap();
        assert_eq!(
            storage.sload(ADDRESS, U256::from(1)).unwrap(),
            U256::from(5)
        );
        assert_eq!(
            storage.sload(ADDRESS, U256::from(1 + 3 * 2)).unwrap(),
            U256::from(8)
        );
        assert_eq!(contract.tail.slot(), U256::from(1 + 4 * 2));
    });
}

#[test]
fn array_write_does_not_reach_the_next_field() {
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let contract = WordArray::new(storage.clone());
        contract
            .words
            .write_all(&[U256::from(1), U256::from(2), U256::from(3), U256::from(4)])
            .unwrap();
        assert_eq!(contract.tail.read().unwrap(), 0);

        contract.tail.write(77).unwrap();
        contract.words.write_all(&[U256::MAX; 4]).unwrap();
        assert_eq!(contract.tail.read().unwrap(), 77);
        assert_eq!(contract.head.read().unwrap(), 0);
        assert_eq!(contract.words.read_all().unwrap(), vec![U256::MAX; 4]);

        let wide = WideArray::new(storage);
        wide.wide.write_all(&[Wide(9); 4]).unwrap();
        assert_eq!(wide.tail.read().unwrap(), 0);
    });
}

#[test]
fn sibling_collection_slots_unchanged() {
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let contract = Siblings::new(storage);
        // StorageVec reserves 1 slot, StorageSet 2 and StorageBytes 1.
        assert_eq!(contract.vec.base_slot(), U256::ZERO);
        assert_eq!(contract.after_vec.slot(), U256::from(1));
        assert_eq!(contract.after_set.slot(), U256::from(1 + 1 + 2));
        assert_eq!(contract.after_bytes.slot(), U256::from(4 + 1 + 1));
    });
}
