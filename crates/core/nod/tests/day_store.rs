use std::sync::Arc;

use alloy_primitives::{Address, U256};
use outbe_compressed_entities::IdPageRequest;
use outbe_nod::{NodContract, NodItemState, NodRepositoryReader, NodRepositoryWriter};
use outbe_offchain_storage::{
    DayDatabases, Key, Namespace, RocksDbStorage, StorageReader, StorageWriter, Value,
};
use outbe_primitives::time::WorldwideDay;

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

fn item(owner: Address, day: u32) -> NodItemState {
    let worldwide_day = WorldwideDay::new(day);
    let floor = U256::from(13u64);
    NodItemState {
        is_settled: false,
        nod_id: NodContract::generate_nod_id(owner, worldwide_day).unwrap(),
        owner,
        gratis_load_minor: U256::from(11u64),
        worldwide_day,
        league_id: 4,
        floor_price_minor: floor,
        bucket_key: NodContract::bucket_key(worldwide_day, floor, 840),
        issuance_currency: 840,
        reference_currency: 840,
        issued_at: 1_752_534_000,
    }
}

fn routed_reader(store: &Store) -> NodRepositoryReader {
    NodRepositoryReader::with_days(
        store.shared.clone(),
        store.shared.clone(),
        store.databases.clone(),
    )
}

fn routed_writer(store: &Store) -> NodRepositoryWriter {
    NodRepositoryWriter::with_days(
        store.shared.clone(),
        store.shared.clone(),
        store.databases.clone(),
    )
}

#[test]
fn nod_list_resumes_from_id_cursor() {
    let store = open();
    let owner = Address::repeat_byte(0x31);
    let day9 = item(owner, 9);
    let day7 = item(owner, 7);
    let writer = routed_writer(&store);
    writer.put_nod(&day9).unwrap();
    writer.put_nod(&day7).unwrap();

    let reader = routed_reader(&store);
    let first = reader
        .list_ids_all(IdPageRequest {
            after: None,
            limit: 1,
        })
        .unwrap();
    assert_eq!(first.ids, vec![day7.nod_id]);
    assert_eq!(first.next_after, Some(day7.nod_id));

    let second = reader
        .list_ids_all(IdPageRequest {
            after: first.next_after,
            limit: 1,
        })
        .unwrap();
    assert_eq!(second.ids, vec![day9.nod_id]);
    assert_eq!(second.next_after, None);
}

#[test]
fn legacy_keys_move_once_and_read_from_the_day() {
    let store = open();
    let nod = item(Address::repeat_byte(0x32), 7);
    NodRepositoryWriter::new(store.shared.clone(), store.shared.clone())
        .put_nod(&nod)
        .unwrap();

    let reader = routed_reader(&store);
    let loaded = reader.get(nod.nod_id).unwrap().unwrap();
    assert_eq!(loaded.gratis_load_minor, nod.gratis_load_minor);

    let key = Key::new(nod.nod_id.as_slice().to_vec()).unwrap();
    let namespace = Namespace::new("nods").unwrap();
    assert!(
        StorageReader::get(store.shared.as_ref(), namespace.clone(), &key)
            .unwrap()
            .is_none()
    );
    let day = store.databases.nod_if_present(7).unwrap().unwrap();
    assert_eq!(
        NodRepositoryReader::new(day)
            .get(nod.nod_id)
            .unwrap()
            .unwrap()
            .gratis_load_minor,
        nod.gratis_load_minor
    );

    StorageWriter::put(
        store.shared.as_ref(),
        namespace.clone(),
        &key,
        &Value::new(b"reinserted".to_vec()).unwrap(),
    )
    .unwrap();
    let again = reader.get(nod.nod_id).unwrap().unwrap();
    assert_eq!(again.gratis_load_minor, nod.gratis_load_minor);
    assert_eq!(
        StorageReader::get(store.shared.as_ref(), namespace, &key)
            .unwrap()
            .unwrap()
            .as_bytes(),
        b"reinserted"
    );
}
