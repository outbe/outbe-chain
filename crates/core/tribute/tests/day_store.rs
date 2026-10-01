use std::sync::Arc;

use alloy_primitives::{Address, B256, U256};
use outbe_compressed_entities::{
    body_commitment, WwdEntityId, ACTIVE_COMMITMENT_SCHEME, BODY_SCHEMA_V1,
};
use outbe_offchain_storage::{
    DayDatabases, Key, Namespace, RocksDbStorage, ScanRequest, StorageReader, StorageReaderHandle,
    StorageWriter, Value,
};
use outbe_primitives::time::WorldwideDay;
use outbe_tribute::{
    write_tribute_day_mark, RetainedTributePin, RetainedTributeReader, TributeData, TributeDayMark,
    TributePageRequest, TributeRepositoryReader, TributeRepositoryWriter,
};

struct Store {
    _dir: tempfile::TempDir,
    databases: Arc<DayDatabases>,
    shared: Arc<RocksDbStorage>,
}

fn open() -> Store {
    let dir = tempfile::tempdir().unwrap();
    let databases = Arc::new(DayDatabases::open(dir.path()).unwrap());
    let shared = Arc::new(databases.directory().open_shared().unwrap());
    Store {
        _dir: dir,
        databases,
        shared,
    }
}

fn body(owner: Address, day: u32) -> TributeData {
    let worldwide_day = WorldwideDay::new(day);
    TributeData {
        tribute_id: WwdEntityId::from_day_and_digest(worldwide_day, [day as u8; 32]),
        owner,
        worldwide_day,
        issuance_amount_minor: U256::from(day),
        issuance_currency: 840,
        nominal_amount_minor: U256::from(1u64),
        reference_currency: 840,
        tribute_price_minor: U256::from(2u64),
        exclude_from_intex_issuance: false,
    }
}

fn routed_reader(store: &Store) -> TributeRepositoryReader {
    TributeRepositoryReader::with_days(
        store.shared.clone(),
        store.shared.clone(),
        store.databases.clone(),
    )
}

fn routed_writer(store: &Store) -> TributeRepositoryWriter {
    TributeRepositoryWriter::with_days(
        store.shared.clone(),
        store.shared.clone(),
        store.databases.clone(),
    )
}

#[test]
fn owner_list_across_two_days_is_id_order() {
    let store = open();
    let owner = Address::repeat_byte(0x11);
    let day8 = body(owner, 8);
    let day7 = body(owner, 7);
    let writer = routed_writer(&store);
    writer.put(&day8).unwrap();
    writer.put(&day7).unwrap();

    let reader = routed_reader(&store);
    let first = reader
        .list_by_owner(
            owner,
            TributePageRequest {
                after: None,
                limit: 1,
            },
        )
        .unwrap();
    assert_eq!(first.records.len(), 1);
    assert_eq!(first.records[0].tribute_id, day7.tribute_id);
    assert_eq!(first.next_after, Some(day7.tribute_id));

    let second = reader
        .list_by_owner(
            owner,
            TributePageRequest {
                after: first.next_after,
                limit: 1,
            },
        )
        .unwrap();
    assert_eq!(second.records.len(), 1);
    assert_eq!(second.records[0].tribute_id, day8.tribute_id);
    assert_eq!(second.next_after, None);

    let shared = StorageReader::scan_prefix(
        store.shared.as_ref(),
        Namespace::new("tributes").unwrap(),
        ScanRequest::new(&[], None, 10).unwrap(),
    )
    .unwrap();
    assert!(shared.entries.is_empty());
}

#[test]
fn legacy_keys_move_once_and_read_from_the_day() {
    let store = open();
    let tribute = body(Address::repeat_byte(0x22), 7);
    TributeRepositoryWriter::new(store.shared.clone(), store.shared.clone())
        .put(&tribute)
        .unwrap();

    let reader = routed_reader(&store);
    let loaded = reader.get(tribute.tribute_id).unwrap().unwrap();
    assert_eq!(loaded.issuance_amount_minor, tribute.issuance_amount_minor);

    let key = Key::new(tribute.tribute_id.as_slice().to_vec()).unwrap();
    let namespace = Namespace::new("tributes").unwrap();
    assert!(
        StorageReader::get(store.shared.as_ref(), namespace.clone(), &key)
            .unwrap()
            .is_none()
    );
    let day = store.databases.tribute_if_present(7).unwrap().unwrap();
    assert_eq!(
        TributeRepositoryReader::new(day)
            .get(tribute.tribute_id)
            .unwrap()
            .unwrap()
            .issuance_amount_minor,
        tribute.issuance_amount_minor
    );

    StorageWriter::put(
        store.shared.as_ref(),
        namespace.clone(),
        &key,
        &Value::new(b"reinserted".to_vec()).unwrap(),
    )
    .unwrap();
    let again = reader.get(tribute.tribute_id).unwrap().unwrap();
    assert_eq!(again.issuance_amount_minor, tribute.issuance_amount_minor);
    assert_eq!(
        StorageReader::get(store.shared.as_ref(), namespace, &key)
            .unwrap()
            .unwrap()
            .as_bytes(),
        b"reinserted"
    );
}

#[test]
fn retained_reader_resolves_a_live_or_own_pinned_day_from_its_database() {
    let store = open();
    let tribute = body(Address::repeat_byte(0x33), 7);
    routed_writer(&store).put(&tribute).unwrap();
    let day: StorageReaderHandle = store.databases.tribute_if_present(7).unwrap().unwrap();
    let stored = TributeRepositoryReader::new(day.clone())
        .get_stored_body(tribute.tribute_id)
        .unwrap()
        .unwrap();
    let commitment = B256::from(
        *body_commitment(
            ACTIVE_COMMITMENT_SCHEME,
            BODY_SCHEMA_V1,
            tribute.tribute_id,
            stored.payload(),
        )
        .unwrap()
        .as_bytes(),
    );
    let pin = RetainedTributePin {
        input_lease_id: B256::repeat_byte(0x51),
        worldwide_day: tribute.worldwide_day,
    };
    let readers = [
        RetainedTributeReader::with_days(store.shared.clone(), store.databases.clone()),
        RetainedTributeReader::with_day_reader(store.shared.clone(), tribute.worldwide_day, day),
    ];
    let resolve = |reader: &RetainedTributeReader| {
        reader
            .get_current_or_retained(pin, tribute.tribute_id, commitment)
            .unwrap()
    };

    for reader in &readers {
        assert_eq!(resolve(reader), Some(stored.clone()));
    }
    write_tribute_day_mark(
        store.shared.as_ref(),
        7,
        TributeDayMark::Retained(pin.input_lease_id),
    )
    .unwrap();
    for reader in &readers {
        assert_eq!(resolve(reader), Some(stored.clone()));
    }
    write_tribute_day_mark(
        store.shared.as_ref(),
        7,
        TributeDayMark::Retained(B256::repeat_byte(0x52)),
    )
    .unwrap();
    for reader in &readers {
        assert_eq!(resolve(reader), None);
    }
}
