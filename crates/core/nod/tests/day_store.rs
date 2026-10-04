use std::sync::Arc;

use alloy_primitives::{Address, U256};
use outbe_compressed_entities::{IdPageRequest, WwdEntityId};
use outbe_nod::{
    NodContract, NodItemState, NodPageRequest, NodRepositoryReader, NodRepositoryWriter,
};
use outbe_offchain_storage::{
    DayDatabases, Key, Namespace, RocksDbStorage, ScanRequest, StorageReader, StorageWriter, Value,
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
    let entry = U256::from(13u64);
    NodItemState {
        is_settled: false,
        nod_id: NodContract::generate_nod_id(owner, worldwide_day).unwrap(),
        owner,
        gratis_load_minor: U256::from(11u64),
        worldwide_day,
        league_id: 4,
        bucket_key: NodContract::bucket_key(worldwide_day, entry, 840),
        issuance_currency: 840,
        reference_currency: 840,
        issued_at: 1_752_534_000,
    }
}

fn item_with_digest(owner: Address, day: u32, digest_byte: u8) -> NodItemState {
    let mut nod = item(owner, day);
    nod.nod_id = WwdEntityId::from_day_and_digest(WorldwideDay::new(day), [digest_byte; 32]);
    nod
}

fn owner_day_count(store: &Store) -> usize {
    StorageReader::scan_prefix(
        store.shared.as_ref(),
        Namespace::new("nod_owner_days").unwrap(),
        ScanRequest::new(&[], None, 10).unwrap(),
    )
    .unwrap()
    .entries
    .len()
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

#[test]
fn owner_list_opens_indexed_days_only() {
    let store = open();
    let owner = Address::repeat_byte(0x41);
    let other = Address::repeat_byte(0x42);
    let day7 = item(owner, 7);
    let day9 = item(owner, 9);
    let writer = routed_writer(&store);
    writer.put_nod(&day9).unwrap();
    writer.put_nod(&day7).unwrap();
    writer.put_nod(&item(other, 8)).unwrap();

    let reader = routed_reader(&store);
    let first = reader
        .list_by_owner(
            owner,
            NodPageRequest {
                after: None,
                limit: 1,
            },
        )
        .unwrap();
    assert_eq!(first.records[0].nod_id, day7.nod_id);
    assert_eq!(first.next_after, Some(day7.nod_id));
    let second = reader
        .list_by_owner(
            owner,
            NodPageRequest {
                after: first.next_after,
                limit: 10,
            },
        )
        .unwrap();
    assert_eq!(second.records.len(), 1);
    assert_eq!(second.records[0].nod_id, day9.nod_id);
    assert!(second.next_after.is_none());

    let mut marker = owner.as_slice().to_vec();
    marker.extend_from_slice(&9u32.to_be_bytes());
    StorageWriter::delete(
        store.shared.as_ref(),
        Namespace::new("nod_owner_days").unwrap(),
        &Key::new(marker).unwrap(),
    )
    .unwrap();
    let listed = reader
        .list_by_owner(
            owner,
            NodPageRequest {
                after: None,
                limit: 10,
            },
        )
        .unwrap();
    assert_eq!(listed.records.len(), 1);
    assert_eq!(listed.records[0].nod_id, day7.nod_id);
    assert_eq!(
        reader
            .list_ids_by_owner(
                owner,
                IdPageRequest {
                    after: None,
                    limit: 10,
                },
            )
            .unwrap()
            .ids,
        vec![day7.nod_id]
    );
    assert!(reader.get(day9.nod_id).unwrap().is_some());
}

#[test]
fn deleting_the_last_nod_drops_the_owner_day() {
    let store = open();
    let owner = Address::repeat_byte(0x43);
    let first = item_with_digest(owner, 7, 1);
    let second = item_with_digest(owner, 7, 2);
    let writer = routed_writer(&store);
    writer.put_nod(&first).unwrap();
    writer.put_nod(&second).unwrap();
    assert_eq!(owner_day_count(&store), 1);

    writer.delete_nod(first.nod_id).unwrap();
    assert_eq!(owner_day_count(&store), 1);
    let listed = routed_reader(&store)
        .list_by_owner(
            owner,
            NodPageRequest {
                after: None,
                limit: 10,
            },
        )
        .unwrap();
    assert_eq!(listed.records.len(), 1);
    assert_eq!(listed.records[0].nod_id, second.nod_id);

    writer.delete_nod(second.nod_id).unwrap();
    assert_eq!(owner_day_count(&store), 0);
    assert!(routed_reader(&store)
        .list_by_owner(
            owner,
            NodPageRequest {
                after: None,
                limit: 10,
            },
        )
        .unwrap()
        .records
        .is_empty());
}

#[test]
fn owner_change_clears_the_previous_owner_day() {
    let store = open();
    let owner = Address::repeat_byte(0x45);
    let next_owner = Address::repeat_byte(0x46);
    let mut nod = item(owner, 7);
    let writer = routed_writer(&store);
    writer.put_nod(&nod).unwrap();
    nod.owner = next_owner;
    writer.put_nod(&nod).unwrap();

    let reader = routed_reader(&store);
    assert!(reader
        .list_by_owner(
            owner,
            NodPageRequest {
                after: None,
                limit: 10,
            },
        )
        .unwrap()
        .records
        .is_empty());
    let listed = reader
        .list_by_owner(
            next_owner,
            NodPageRequest {
                after: None,
                limit: 10,
            },
        )
        .unwrap();
    assert_eq!(listed.records.len(), 1);
    assert_eq!(listed.records[0].nod_id, nod.nod_id);
    assert_eq!(owner_day_count(&store), 1);
}

#[test]
fn legacy_owner_list_reads_the_moved_nod() {
    let store = open();
    let nod = item(Address::repeat_byte(0x44), 7);
    NodRepositoryWriter::new(store.shared.clone(), store.shared.clone())
        .put_nod(&nod)
        .unwrap();

    let listed = routed_reader(&store)
        .list_by_owner(
            nod.owner,
            NodPageRequest {
                after: None,
                limit: 10,
            },
        )
        .unwrap();
    assert_eq!(listed.records.len(), 1);
    assert_eq!(listed.records[0].nod_id, nod.nod_id);
    assert_eq!(owner_day_count(&store), 1);
    let key = Key::new(nod.nod_id.as_slice().to_vec()).unwrap();
    assert!(
        StorageReader::get(store.shared.as_ref(), Namespace::new("nods").unwrap(), &key,)
            .unwrap()
            .is_none()
    );
}
