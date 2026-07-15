use std::{
    io,
    panic::AssertUnwindSafe,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use alloy_primitives::{Address, B256, U256};
use mongodb::sync::Client;
use outbe_common::WorldwideDay;
use outbe_nod::{
    NodBucketState, NodItemState, NodPageRequest, NodRepositoryError, NodRepositoryReader,
    NodRepositoryWriter,
};
use outbe_offchain_storage::{
    Key, MemoryStorage, MongoStorage, MongoStorageConfig, Namespace, StorageError, StorageReader,
    StorageReaderHandle, StorageWriter, StorageWriterHandle, Value, MAX_SCAN_ENTRIES,
};

fn nod(nod_id: U256, owner: Address) -> NodItemState {
    NodItemState {
        nod_id,
        owner,
        gratis_load_minor: U256::MAX,
        worldwide_day: WorldwideDay::new(u32::MAX),
        league_id: u16::MAX,
        floor_price_minor: U256::ZERO,
        bucket_key: B256::repeat_byte(0x33),
        cost_amount_minor: U256::MAX,
        issuance_currency: 0,
        reference_currency: u16::MAX,
        issued_at: u64::MAX,
    }
}

fn bucket(key: B256) -> NodBucketState {
    NodBucketState {
        bucket_key: key,
        worldwide_day: WorldwideDay::new(0),
        floor_price_minor: U256::MAX,
        is_qualified: false,
        total_nods: u64::MAX,
        entry_price_minor: U256::ZERO,
    }
}

fn namespace(name: &str) -> Namespace {
    Namespace::new(name).unwrap()
}

fn key(bytes: impl Into<Vec<u8>>) -> Key {
    Key::new(bytes).unwrap()
}

fn id_key(id: U256) -> Key {
    key(id.to_be_bytes::<32>())
}

fn owner_key(owner: Address, id: U256) -> Key {
    key([owner.as_slice(), &id.to_be_bytes::<32>()].concat())
}

#[test]
fn postcard_roundtrips_all_nod_field_boundaries() {
    for body in [
        NodItemState {
            nod_id: U256::ZERO,
            owner: Address::ZERO,
            gratis_load_minor: U256::ZERO,
            worldwide_day: WorldwideDay::new(0),
            league_id: 0,
            floor_price_minor: U256::ZERO,
            bucket_key: B256::ZERO,
            cost_amount_minor: U256::ZERO,
            issuance_currency: 0,
            reference_currency: 0,
            issued_at: 0,
        },
        NodItemState {
            nod_id: U256::MAX,
            owner: Address::repeat_byte(u8::MAX),
            gratis_load_minor: U256::MAX,
            worldwide_day: WorldwideDay::new(u32::MAX),
            league_id: u16::MAX,
            floor_price_minor: U256::MAX,
            bucket_key: B256::repeat_byte(u8::MAX),
            cost_amount_minor: U256::MAX,
            issuance_currency: u16::MAX,
            reference_currency: u16::MAX,
            issued_at: u64::MAX,
        },
    ] {
        let decoded: NodItemState =
            postcard::from_bytes(&postcard::to_stdvec(&body).unwrap()).unwrap();
        assert_eq!(decoded.nod_id, body.nod_id);
        assert_eq!(decoded.owner, body.owner);
        assert_eq!(decoded.gratis_load_minor, body.gratis_load_minor);
        assert_eq!(decoded.worldwide_day, body.worldwide_day);
        assert_eq!(decoded.league_id, body.league_id);
        assert_eq!(decoded.floor_price_minor, body.floor_price_minor);
        assert_eq!(decoded.bucket_key, body.bucket_key);
        assert_eq!(decoded.cost_amount_minor, body.cost_amount_minor);
        assert_eq!(decoded.issuance_currency, body.issuance_currency);
        assert_eq!(decoded.reference_currency, body.reference_currency);
        assert_eq!(decoded.issued_at, body.issued_at);
    }

    for body in [
        NodBucketState {
            bucket_key: B256::ZERO,
            worldwide_day: WorldwideDay::new(0),
            floor_price_minor: U256::ZERO,
            is_qualified: false,
            total_nods: 0,
            entry_price_minor: U256::ZERO,
        },
        NodBucketState {
            bucket_key: B256::repeat_byte(u8::MAX),
            worldwide_day: WorldwideDay::new(u32::MAX),
            floor_price_minor: U256::MAX,
            is_qualified: true,
            total_nods: u64::MAX,
            entry_price_minor: U256::MAX,
        },
    ] {
        let decoded: NodBucketState =
            postcard::from_bytes(&postcard::to_stdvec(&body).unwrap()).unwrap();
        assert_eq!(decoded.bucket_key, body.bucket_key);
        assert_eq!(decoded.worldwide_day, body.worldwide_day);
        assert_eq!(decoded.floor_price_minor, body.floor_price_minor);
        assert_eq!(decoded.is_qualified, body.is_qualified);
        assert_eq!(decoded.total_nods, body.total_nods);
        assert_eq!(decoded.entry_price_minor, body.entry_price_minor);
    }
}

fn run_contract(reader: StorageReaderHandle, writer: StorageWriterHandle) {
    let repository_reader = NodRepositoryReader::new(reader.clone());
    let repository_writer = NodRepositoryWriter::new(reader.clone(), writer.clone());
    let owner_a = Address::repeat_byte(0x22);
    let owner_b = Address::repeat_byte(0x44);

    for id in [U256::from(3), U256::from(1), U256::from(2)] {
        repository_writer.put_nod(&nod(id, owner_a)).unwrap();
    }
    repository_writer
        .put_nod(&nod(U256::from(4), owner_b))
        .unwrap();
    let bucket_key = B256::repeat_byte(0x55);
    repository_writer.put_bucket(&bucket(bucket_key)).unwrap();

    let stored = repository_reader.get(U256::from(1)).unwrap().unwrap();
    assert_eq!(stored.nod_id, U256::from(1));
    assert_eq!(stored.gratis_load_minor, U256::MAX);
    assert_eq!(stored.issued_at, u64::MAX);
    assert_eq!(
        repository_reader
            .get_bucket(bucket_key)
            .unwrap()
            .unwrap()
            .bucket_key,
        bucket_key
    );

    let primary = reader
        .get(namespace("nods"), &id_key(U256::from(1)))
        .unwrap()
        .unwrap();
    assert_eq!(
        postcard::from_bytes::<NodItemState>(primary.as_bytes())
            .unwrap()
            .nod_id,
        U256::from(1)
    );
    assert!(reader
        .get(
            namespace("nods_by_owner"),
            &owner_key(owner_a, U256::from(1)),
        )
        .unwrap()
        .unwrap()
        .as_bytes()
        .is_empty());
    assert_eq!(
        reader
            .get(
                namespace("nod_buckets"),
                &key(bucket_key.as_slice().to_vec())
            )
            .unwrap()
            .unwrap()
            .as_bytes(),
        postcard::to_stdvec(&bucket(bucket_key)).unwrap()
    );

    let first = repository_reader
        .list_all(NodPageRequest {
            after: None,
            limit: 2,
        })
        .unwrap();
    assert_eq!(
        first
            .records
            .iter()
            .map(|body| body.nod_id)
            .collect::<Vec<_>>(),
        [U256::from(1), U256::from(2)]
    );
    assert_eq!(first.next_after, Some(U256::from(2)));
    let second = repository_reader
        .list_all(NodPageRequest {
            after: first.next_after,
            limit: 3,
        })
        .unwrap();
    assert_eq!(
        second
            .records
            .iter()
            .map(|body| body.nod_id)
            .collect::<Vec<_>>(),
        [U256::from(3), U256::from(4)]
    );
    assert_eq!(second.next_after, None);
    assert_eq!(
        repository_reader
            .list_by_owner(
                owner_b,
                NodPageRequest {
                    after: None,
                    limit: 10
                }
            )
            .unwrap()
            .records[0]
            .nod_id,
        U256::from(4)
    );

    repository_writer
        .put_nod(&nod(U256::from(1), owner_b))
        .unwrap();
    assert!(reader
        .get(
            namespace("nods_by_owner"),
            &owner_key(owner_a, U256::from(1)),
        )
        .unwrap()
        .is_none());
    assert_eq!(
        repository_reader.get(U256::from(1)).unwrap().unwrap().owner,
        owner_b
    );

    repository_writer.delete_nod(U256::from(2)).unwrap();
    repository_writer.delete_nod(U256::from(2)).unwrap();
    let remaining = repository_reader
        .list_all(NodPageRequest {
            after: None,
            limit: 10,
        })
        .unwrap();
    assert_eq!(
        remaining
            .records
            .iter()
            .map(|body| body.nod_id)
            .collect::<Vec<_>>(),
        [U256::from(1), U256::from(3), U256::from(4)]
    );
    repository_writer.delete_bucket(bucket_key).unwrap();
    repository_writer.delete_bucket(bucket_key).unwrap();
    assert!(repository_reader.get_bucket(bucket_key).unwrap().is_none());
}

#[test]
fn memory_contract() {
    let storage = Arc::new(MemoryStorage::new());
    run_contract(storage.clone(), storage);
}

#[test]
#[ignore = "requires OUTBE_TEST_MONGODB_URI"]
fn mongo_contract() {
    run_isolated_mongo("nod_repository", run_contract);
}

#[test]
fn rejects_corrupt_items_buckets_indexes_and_limits() {
    let storage = Arc::new(MemoryStorage::new());
    run_corruption_contract(storage.clone(), storage);
}

#[test]
#[ignore = "requires OUTBE_TEST_MONGODB_URI"]
fn mongo_rejects_corrupt_items_buckets_indexes_and_limits() {
    run_isolated_mongo("nod_corruption", run_corruption_contract);
}

fn run_corruption_contract(reader_storage: StorageReaderHandle, storage: StorageWriterHandle) {
    let reader = NodRepositoryReader::new(reader_storage);
    let owner = Address::repeat_byte(0x61);
    let id = U256::from(9);

    storage
        .put(namespace("nods"), &id_key(id), &Value::new([0xff]).unwrap())
        .unwrap();
    assert!(matches!(
        reader.get(id),
        Err(NodRepositoryError::ItemDecode(_))
    ));
    let wrong = nod(U256::from(10), owner);
    storage
        .put(
            namespace("nods"),
            &id_key(id),
            &Value::new(postcard::to_stdvec(&wrong).unwrap()).unwrap(),
        )
        .unwrap();
    assert!(matches!(
        reader.get(id),
        Err(NodRepositoryError::PrimaryKeyBodyMismatch { .. })
    ));

    let mut trailing = postcard::to_stdvec(&nod(id, owner)).unwrap();
    trailing.push(0);
    storage
        .put(
            namespace("nods"),
            &id_key(id),
            &Value::new(trailing).unwrap(),
        )
        .unwrap();
    assert!(matches!(
        reader.get(id),
        Err(NodRepositoryError::ItemDecode(_))
    ));

    storage.delete(namespace("nods"), &id_key(id)).unwrap();
    storage
        .put(
            namespace("nods_by_owner"),
            &owner_key(owner, id),
            &Value::new([1]).unwrap(),
        )
        .unwrap();
    assert!(matches!(
        reader.list_by_owner(
            owner,
            NodPageRequest {
                after: None,
                limit: 1
            }
        ),
        Err(NodRepositoryError::NonEmptyIndexValue)
    ));
    storage
        .put(
            namespace("nods_by_owner"),
            &owner_key(owner, id),
            &Value::new(Vec::new()).unwrap(),
        )
        .unwrap();
    assert!(matches!(
        reader.list_by_owner(
            owner,
            NodPageRequest {
                after: None,
                limit: 1
            }
        ),
        Err(NodRepositoryError::DanglingIndex { .. })
    ));
    storage
        .delete(namespace("nods_by_owner"), &owner_key(owner, id))
        .unwrap();
    let malformed_owner_key = key([owner.as_slice(), &[1, 2]].concat());
    storage
        .put(
            namespace("nods_by_owner"),
            &malformed_owner_key,
            &Value::new(Vec::new()).unwrap(),
        )
        .unwrap();
    assert!(matches!(
        reader.list_by_owner(
            owner,
            NodPageRequest {
                after: None,
                limit: 1
            }
        ),
        Err(NodRepositoryError::MalformedIndexKey)
    ));

    let malformed_primary = key([0xaa]);
    storage
        .put(
            namespace("nods"),
            &malformed_primary,
            &Value::new([0]).unwrap(),
        )
        .unwrap();
    assert!(matches!(
        reader.list_all(NodPageRequest {
            after: None,
            limit: 10
        }),
        Err(NodRepositoryError::MalformedPrimaryKey)
    ));

    let selected_bucket = B256::repeat_byte(0x71);
    let wrong_bucket = bucket(B256::repeat_byte(0x72));
    storage
        .put(
            namespace("nod_buckets"),
            &key(selected_bucket.as_slice().to_vec()),
            &Value::new(postcard::to_stdvec(&wrong_bucket).unwrap()).unwrap(),
        )
        .unwrap();
    assert!(matches!(
        reader.get_bucket(selected_bucket),
        Err(NodRepositoryError::BucketKeyBodyMismatch { .. })
    ));
    storage
        .put(
            namespace("nod_buckets"),
            &key(selected_bucket.as_slice().to_vec()),
            &Value::new([0xff]).unwrap(),
        )
        .unwrap();
    assert!(matches!(
        reader.get_bucket(selected_bucket),
        Err(NodRepositoryError::BucketDecode(_))
    ));
    let mut trailing_bucket = postcard::to_stdvec(&bucket(selected_bucket)).unwrap();
    trailing_bucket.push(0);
    storage
        .put(
            namespace("nod_buckets"),
            &key(selected_bucket.as_slice().to_vec()),
            &Value::new(trailing_bucket).unwrap(),
        )
        .unwrap();
    assert!(matches!(
        reader.get_bucket(selected_bucket),
        Err(NodRepositoryError::BucketDecode(_))
    ));
    assert!(matches!(
        reader.list_all(NodPageRequest {
            after: None,
            limit: 0
        }),
        Err(NodRepositoryError::InvalidPageLimit { .. })
    ));
    assert!(matches!(
        reader.list_all(NodPageRequest {
            after: None,
            limit: MAX_SCAN_ENTRIES + 1,
        }),
        Err(NodRepositoryError::InvalidPageLimit { .. })
    ));

    storage
        .delete(namespace("nods"), &malformed_primary)
        .unwrap();
    storage
        .delete(namespace("nods_by_owner"), &malformed_owner_key)
        .unwrap();
    let mismatched = nod(id, Address::repeat_byte(0x62));
    storage
        .put(
            namespace("nods"),
            &id_key(id),
            &Value::new(postcard::to_stdvec(&mismatched).unwrap()).unwrap(),
        )
        .unwrap();
    storage
        .put(
            namespace("nods_by_owner"),
            &owner_key(owner, id),
            &Value::new(Vec::new()).unwrap(),
        )
        .unwrap();
    assert!(matches!(
        reader.list_by_owner(
            owner,
            NodPageRequest {
                after: None,
                limit: 1
            }
        ),
        Err(NodRepositoryError::IndexedOwnerMismatch { .. })
    ));
}

#[derive(Debug)]
struct FailAfterWriter {
    inner: Arc<MemoryStorage>,
    fail_after: usize,
    calls: AtomicUsize,
}

impl FailAfterWriter {
    fn finish_step(&self) -> Result<(), StorageError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if call == self.fail_after {
            return Err(StorageError::Backend {
                source: Box::new(io::Error::other("injected failure after write step")),
            });
        }
        Ok(())
    }
}

impl StorageWriter for FailAfterWriter {
    fn put(&self, namespace: Namespace, key: &Key, value: &Value) -> Result<(), StorageError> {
        self.inner.put(namespace, key, value)?;
        self.finish_step()
    }

    fn delete(&self, namespace: Namespace, key: &Key) -> Result<(), StorageError> {
        self.inner.delete(namespace, key)?;
        self.finish_step()
    }
}

#[test]
fn failures_after_each_nod_write_step_expose_the_documented_partial_state() {
    let owner = Address::repeat_byte(0x91);
    let new_owner = Address::repeat_byte(0x92);
    let id = U256::from(88);

    for fail_after in 1..=2 {
        let storage = Arc::new(MemoryStorage::new());
        let failing = Arc::new(FailAfterWriter {
            inner: storage.clone(),
            fail_after,
            calls: AtomicUsize::new(0),
        });
        let repository = NodRepositoryWriter::new(storage.clone(), failing);
        assert!(matches!(
            repository.put_nod(&nod(id, owner)),
            Err(NodRepositoryError::Storage(_))
        ));
        assert_eq!(
            storage
                .get(namespace("nods"), &id_key(id))
                .unwrap()
                .is_some(),
            fail_after >= 1
        );
        assert_eq!(
            storage
                .get(namespace("nods_by_owner"), &owner_key(owner, id))
                .unwrap()
                .is_some(),
            fail_after >= 2
        );
    }

    for fail_after in 1..=3 {
        let storage = Arc::new(MemoryStorage::new());
        let seed = NodRepositoryWriter::new(storage.clone(), storage.clone());
        seed.put_nod(&nod(id, owner)).unwrap();
        let failing = Arc::new(FailAfterWriter {
            inner: storage.clone(),
            fail_after,
            calls: AtomicUsize::new(0),
        });
        let repository = NodRepositoryWriter::new(storage.clone(), failing);
        assert!(matches!(
            repository.put_nod(&nod(id, new_owner)),
            Err(NodRepositoryError::Storage(_))
        ));
        assert_eq!(
            storage
                .get(namespace("nods_by_owner"), &owner_key(new_owner, id),)
                .unwrap()
                .is_some(),
            fail_after >= 2
        );
        assert_eq!(
            storage
                .get(namespace("nods_by_owner"), &owner_key(owner, id))
                .unwrap()
                .is_none(),
            fail_after >= 3
        );
    }

    for fail_after in 1..=2 {
        let storage = Arc::new(MemoryStorage::new());
        let seed = NodRepositoryWriter::new(storage.clone(), storage.clone());
        seed.put_nod(&nod(id, owner)).unwrap();
        let failing = Arc::new(FailAfterWriter {
            inner: storage.clone(),
            fail_after,
            calls: AtomicUsize::new(0),
        });
        let repository = NodRepositoryWriter::new(storage.clone(), failing);
        assert!(matches!(
            repository.delete_nod(id),
            Err(NodRepositoryError::Storage(_))
        ));
        assert_eq!(
            storage
                .get(namespace("nods_by_owner"), &owner_key(owner, id))
                .unwrap()
                .is_none(),
            fail_after >= 1
        );
        assert_eq!(
            storage
                .get(namespace("nods"), &id_key(id))
                .unwrap()
                .is_none(),
            fail_after >= 2
        );
    }

    let bucket_key = B256::repeat_byte(0xa1);
    let storage = Arc::new(MemoryStorage::new());
    let failing = Arc::new(FailAfterWriter {
        inner: storage.clone(),
        fail_after: 1,
        calls: AtomicUsize::new(0),
    });
    let repository = NodRepositoryWriter::new(storage.clone(), failing);
    assert!(matches!(
        repository.put_bucket(&bucket(bucket_key)),
        Err(NodRepositoryError::Storage(_))
    ));
    assert!(storage
        .get(
            namespace("nod_buckets"),
            &key(bucket_key.as_slice().to_vec()),
        )
        .unwrap()
        .is_some());

    let failing = Arc::new(FailAfterWriter {
        inner: storage.clone(),
        fail_after: 1,
        calls: AtomicUsize::new(0),
    });
    let repository = NodRepositoryWriter::new(storage.clone(), failing);
    assert!(matches!(
        repository.delete_bucket(bucket_key),
        Err(NodRepositoryError::Storage(_))
    ));
    assert!(storage
        .get(
            namespace("nod_buckets"),
            &key(bucket_key.as_slice().to_vec()),
        )
        .unwrap()
        .is_none());
}

fn run_isolated_mongo(test_name: &str, test: fn(StorageReaderHandle, StorageWriterHandle)) {
    let uri = std::env::var("OUTBE_TEST_MONGODB_URI")
        .expect("set OUTBE_TEST_MONGODB_URI before running ignored MongoDB tests");
    let database = format!("outbe_{}_{}_{}", test_name, std::process::id(), 1);
    let client = Client::with_uri_str(&uri).unwrap();
    client.database(&database).drop().run().unwrap();
    let storage = Arc::new(
        MongoStorage::connect(MongoStorageConfig {
            uri,
            database: database.clone(),
        })
        .unwrap(),
    );
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        test(storage.clone(), storage);
    }));
    client.database(&database).drop().run().unwrap();
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}
