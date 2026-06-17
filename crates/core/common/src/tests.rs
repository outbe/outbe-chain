use crate::WorldwideDay;
use alloy_primitives::{address, U256};
use outbe_primitives::storage::{
    hashmap::HashMapStorageProvider,
    types::{Mapping, Slot, Storable, StorageKey},
    StorageHandle,
};
use outbe_primitives::time::date_key_to_utc_timestamp;

#[test]
fn worldwide_day_to_utc_timestamp_roundtrip_known_midnight() {
    let wwd = WorldwideDay::new(20241220);
    // Midnight UTC of 2024-12-20
    assert_eq!(date_key_to_utc_timestamp(wwd.value()), 1_734_652_800);
}

#[test]
fn is_valid_accepts_basic_valid_dates() {
    assert!(WorldwideDay::new(20240101).is_valid());
    assert!(WorldwideDay::new(20000229).is_valid());
}

#[test]
fn is_valid_rejects_basic_invalid_dates() {
    assert!(!WorldwideDay::new(0).is_valid());
    assert!(!WorldwideDay::new(20240001).is_valid());
    assert!(!WorldwideDay::new(20241301).is_valid());
    assert!(!WorldwideDay::new(20240100).is_valid());
    assert!(!WorldwideDay::new(20240431).is_valid());
    assert!(!WorldwideDay::new(20230229).is_valid());
    assert!(!WorldwideDay::new(19000229).is_valid());
}

#[test]
fn serde_is_transparent_u32() {
    let wwd = WorldwideDay::new(20241220);
    let encoded = serde_json::to_string(&wwd).expect("serialize worldwideday");
    assert_eq!(encoded, "20241220");

    let decoded: WorldwideDay = serde_json::from_str(&encoded).expect("deserialize worldwideday");
    assert_eq!(decoded, wwd);
}

#[test]
fn storage_word_roundtrip() {
    let wwd = WorldwideDay::new(20251231);
    let word = wwd.to_word();
    let decoded = WorldwideDay::from_word(word);
    assert_eq!(decoded, wwd);
}

#[test]
fn storage_key_is_big_endian_u32() {
    let wwd = WorldwideDay::new(0x0102_0304);
    assert_eq!(wwd.key_bytes(), vec![0x01, 0x02, 0x03, 0x04]);
}

#[test]
fn storage_slot_roundtrip_is_raw_u32_word() {
    let contract = address!("0x0000000000000000000000000000000000001003");
    let slot_index = U256::from(7u64);
    let value = WorldwideDay::new(20241220);

    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let slot: Slot<WorldwideDay> = Slot::new(slot_index, contract, storage.clone());
        slot.write(value).expect("write worldwideday slot");

        let decoded = slot.read().expect("read worldwideday slot");
        assert_eq!(decoded, value);

        let raw = storage
            .sload(contract, slot_index)
            .expect("read raw storage word");
        assert_eq!(raw, U256::from(value.value()));
    });
}

#[test]
fn storage_mapping_roundtrip_as_key_and_value() {
    let contract = address!("0x0000000000000000000000000000000000001003");
    let key = WorldwideDay::new(20250101);
    let value = WorldwideDay::new(20251231);

    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let mapping: Mapping<WorldwideDay, WorldwideDay> =
            Mapping::new(U256::from(9u64), contract, storage);

        mapping
            .write(&key, value)
            .expect("write worldwideday mapping");
        let decoded = mapping.read(&key).expect("read worldwideday mapping");
        assert_eq!(decoded, value);
    });
}

#[test]
fn display_and_parse_roundtrip() {
    let wwd = WorldwideDay::new(20241220);
    let encoded = wwd.to_string();
    assert_eq!(encoded, "20241220");

    let decoded: WorldwideDay = encoded.parse().expect("parse worldwideday");
    assert_eq!(decoded, wwd);
}

#[test]
fn parse_rejects_invalid_date() {
    let err = "20240230".parse::<WorldwideDay>().unwrap_err();
    assert!(err.contains("valid YYYYMMDD"));
}
