//! Isolate canonical NOD serialization and durable RocksDB byte preservation.

use alloy_primitives::{Address, B256, U256};
use outbe_compressed_entities::{
    body_commitment, decode_nod_item_v2, encode_nod_item_v2, NodItemBodyV2, StoredBody,
    ACTIVE_COMMITMENT_SCHEME, NOD_BODY_SCHEMA_V2,
};
use outbe_offchain_storage::{Key, Namespace, RocksDbStorage, StorageReader, StorageWriter, Value};
use outbe_primitives::time::WorldwideDay;

#[test]
fn nod_bytes_and_commitment_survive_rocksdb_write_read_and_reopen() {
    let day = WorldwideDay::new(20260906);
    let encrypted = outbe_nod::test_support::encrypted_fixture(
        &outbe_nod::NodIssueParams {
            owner: Address::repeat_byte(0x73),
            worldwide_day: day,
            league_id: 257,
            gratis_load_minor: U256::from_be_bytes([0xa7; 32]),
            entry_price_minor: U256::from(5),
            issuance_currency: 840,
            reference_currency: 978,
        },
        7,
    );
    let nod_id = encrypted.terms.nod_id;
    let nod = NodItemBodyV2 {
        encrypted,
        bucket_key: B256::repeat_byte(0xff),
        issued_at: 1_788_652_800,
        is_settled: false,
    };
    let payload = encode_nod_item_v2(&nod).unwrap();
    let expected_commitment = body_commitment(
        ACTIVE_COMMITMENT_SCHEME,
        NOD_BODY_SCHEMA_V2,
        nod_id,
        &payload,
    )
    .unwrap();
    let stored_bytes = StoredBody::new(NOD_BODY_SCHEMA_V2, payload.clone())
        .unwrap()
        .encode();
    let value = Value::new(stored_bytes.clone()).unwrap();
    let namespace = Namespace::new("nods").unwrap();
    let key = Key::new(nod_id.as_slice().to_vec()).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("rocksdb");

    let assert_roundtrip = |storage: &RocksDbStorage, stage: &str| {
        let read_value = storage.get(namespace.clone(), &key).unwrap().unwrap();
        assert_eq!(read_value.as_bytes(), stored_bytes, "stored bytes: {stage}");
        let read_body = StoredBody::decode(read_value.as_bytes()).unwrap();
        assert_eq!(read_body.payload(), payload, "payload bytes: {stage}");
        let actual_commitment = body_commitment(
            ACTIVE_COMMITMENT_SCHEME,
            read_body.schema_version(),
            nod_id,
            read_body.payload(),
        )
        .unwrap();
        assert_eq!(
            actual_commitment, expected_commitment,
            "commitment: {stage}"
        );
        assert_eq!(
            decode_nod_item_v2(read_body.payload()).unwrap(),
            nod,
            "NOD fields: {stage}"
        );
    };

    let storage = RocksDbStorage::open(&path).unwrap();
    storage.put(namespace.clone(), &key, &value).unwrap();
    assert_roundtrip(&storage, "after write");
    drop(storage);

    let reopened = RocksDbStorage::open(&path).unwrap();
    assert_roundtrip(&reopened, "after close and reopen");
}
