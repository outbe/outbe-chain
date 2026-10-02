//! Storage layout and CRUD behavior through the public macro surface.

use alloy_primitives::{Address, U256};
use outbe_macros::{contract, storage_record};
use outbe_primitives::storage::dsl::{Deprecated, Optional, RecordEntry, StorageRecord};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::types::{StorageKey, StorageSet};
use outbe_primitives::storage::{StorageBacked, StorageHandle};

const ADDRESS: Address = Address::repeat_byte(0x42);

#[storage_record(exists_field = active)]
pub struct MixedRecord {
    #[key]
    pub id: u64,
    #[attribute(order = 3, default = 7)]
    pub active: Deprecated<u64>,
    #[attribute(order = 1)]
    pub optional: Optional<u64>,
    #[attribute(order = 0)]
    pub text: String,
    #[attribute(order = 3)]
    pub bytes: Vec<u8>,
}

#[contract(addr = ADDRESS)]
pub struct Ordered {
    #[attribute(order = 4)]
    pub tail: outbe_primitives::storage::dsl::Value<u64>,
    #[attribute(order = 0)]
    pub set: StorageSet<u64>,
    #[attribute(order = 0)]
    pub tied: outbe_primitives::storage::dsl::Value<u64>,
    #[attribute(order = 1)]
    pub records: outbe_primitives::storage::dsl::Map<u64, MixedRecord>,
}

#[contract]
pub struct Explicit {
    #[attribute(order = 9)]
    pub first: outbe_primitives::storage::dsl::Value<u64>,
    #[slot(12)]
    #[attribute(order = 0)]
    pub reset: outbe_primitives::storage::dsl::Value<u64>,
    pub next: outbe_primitives::storage::dsl::Value<u64>,
}

#[storage_record(exists_field = present)]
pub struct OptionalExists {
    #[key]
    pub id: u64,
    pub present: Option<u64>,
}

#[storage_record(exists_field = bytes)]
pub struct DynamicExists {
    #[key]
    pub id: u64,
    pub bytes: Vec<u8>,
}

#[test]
fn contract_order_is_stable_and_reserves_collection_and_record_widths() {
    assert_eq!(MixedRecord::SLOTS, 5);
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let c = Ordered::new(storage.clone());
        assert_eq!(c.tied.slot(), U256::from(2));
        assert_eq!(c.records.base_slot(), U256::from(3));
        assert_eq!(c.tail.slot(), U256::from(8));
        assert_eq!(c.address, ADDRESS);
        assert_eq!(Ordered::DEFAULT_ADDRESS, ADDRESS);
        assert_eq!(Ordered::at(storage, Address::ZERO).address, Address::ZERO);
    });
}

#[test]
fn explicit_slot_disables_order_sorting_and_resets_following_slots() {
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let c = Explicit::new(storage, ADDRESS);
        assert_eq!(c.first.slot(), U256::ZERO);
        assert_eq!(c.reset.slot(), U256::from(12));
        assert_eq!(c.next.slot(), U256::from(13));
    });
}

#[test]
fn record_crud_preserves_defaults_offsets_optional_values_and_dynamic_tails() {
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let c = Ordered::new(storage);
        let entry = c.records.entry(9);
        assert!(!entry.exists().unwrap());
        let mut value = MixedRecord::with_key(9);
        assert_eq!(value.active, 7);
        assert_eq!(value.optional, None);
        value.optional = Some(0);
        value.text = "a".repeat(100);
        value.bytes = vec![1; 100];
        assert!(entry.update(&value).is_err());
        entry.create(&value).unwrap();
        assert!(entry.create(&value).is_err());

        let loaded = entry.load().unwrap().unwrap();
        assert_eq!(loaded.id, 9);
        assert_eq!(loaded.optional, Some(0));
        assert_eq!(loaded.text, value.text);
        assert_eq!(loaded.bytes, value.bytes);
        assert_eq!(entry.active().slot(), 9u64.mapping_slot(U256::from(6)));
        assert_eq!(entry.bytes().read().unwrap(), value.bytes);

        value.optional = None;
        value.text = "short".into();
        value.bytes = vec![2];
        entry.update(&value).unwrap();
        // An unchanged write must preserve the same stored value as a changed one.
        entry.update(&value).unwrap();
        assert_eq!(entry.optional().read().unwrap(), None);
        assert_eq!(entry.text().read().unwrap(), b"short");
        assert_eq!(entry.bytes().read().unwrap(), vec![2]);
        entry.delete().unwrap();
        assert!(entry.load().unwrap().is_none());
        assert!(entry.text().read().unwrap().is_empty());
        assert!(entry.bytes().read().unwrap().is_empty());
        assert_eq!(entry.active().read().unwrap(), 0);
    });
}

#[test]
fn existence_uses_optional_presence_and_dynamic_length() {
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let optional: RecordEntry<'_, u64, OptionalExists> =
            RecordEntry::new(U256::ZERO, ADDRESS, storage.clone(), 1);
        optional
            .create(&OptionalExists {
                id: 1,
                present: Some(0),
            })
            .unwrap();
        assert!(optional.exists().unwrap());
        assert_eq!(optional.load().unwrap().unwrap().present, Some(0));
        optional.delete().unwrap();
        assert!(!optional.exists().unwrap());

        let dynamic: RecordEntry<'_, u64, DynamicExists> =
            RecordEntry::new(U256::from(10), ADDRESS, storage, 1);
        dynamic
            .create(&DynamicExists {
                id: 1,
                bytes: vec![0],
            })
            .unwrap();
        assert!(dynamic.exists().unwrap());
        dynamic.delete().unwrap();
        assert!(!dynamic.exists().unwrap());
    });
}
