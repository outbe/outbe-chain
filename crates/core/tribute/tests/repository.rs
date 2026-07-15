use std::{
    io,
    panic::AssertUnwindSafe,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use alloy_primitives::{Address, U256};
use mongodb::sync::Client;
use outbe_common::WorldwideDay;
use outbe_offchain_storage::{
    AtomicWriteBatch, Key, MemoryStorage, MongoStorage, MongoStorageConfig, Namespace,
    StorageError, StorageReader, StorageReaderHandle, StorageWriter, StorageWriterHandle, Value,
    MAX_SCAN_ENTRIES,
};
use outbe_tribute::{
    TributeData, TributePageRequest, TributeRepositoryError, TributeRepositoryReader,
    TributeRepositoryWriter,
};

fn tribute(token_id: U256, owner: Address, day: u32) -> TributeData {
    TributeData {
        token_id,
        owner,
        worldwide_day: WorldwideDay::new(day),
        issuance_amount_minor: U256::MAX,
        issuance_currency: u16::MAX,
        nominal_amount_minor: U256::ZERO,
        reference_currency: 0,
        tribute_price_minor: U256::MAX,
        exclude_from_intex_issuance: true,
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

fn day_key(day: u32, id: U256) -> Key {
    key([day.to_be_bytes().as_slice(), &id.to_be_bytes::<32>()].concat())
}

#[test]
fn postcard_roundtrips_all_tribute_field_boundaries() {
    for body in [
        TributeData {
            token_id: U256::ZERO,
            owner: Address::ZERO,
            worldwide_day: WorldwideDay::new(0),
            issuance_amount_minor: U256::ZERO,
            issuance_currency: 0,
            nominal_amount_minor: U256::ZERO,
            reference_currency: 0,
            tribute_price_minor: U256::ZERO,
            exclude_from_intex_issuance: false,
        },
        TributeData {
            token_id: U256::MAX,
            owner: Address::repeat_byte(u8::MAX),
            worldwide_day: WorldwideDay::new(u32::MAX),
            issuance_amount_minor: U256::MAX,
            issuance_currency: u16::MAX,
            nominal_amount_minor: U256::MAX,
            reference_currency: u16::MAX,
            tribute_price_minor: U256::MAX,
            exclude_from_intex_issuance: true,
        },
    ] {
        let decoded: TributeData =
            postcard::from_bytes(&postcard::to_stdvec(&body).unwrap()).unwrap();
        assert_eq!(decoded.token_id, body.token_id);
        assert_eq!(decoded.owner, body.owner);
        assert_eq!(decoded.worldwide_day, body.worldwide_day);
        assert_eq!(decoded.issuance_amount_minor, body.issuance_amount_minor);
        assert_eq!(decoded.issuance_currency, body.issuance_currency);
        assert_eq!(decoded.nominal_amount_minor, body.nominal_amount_minor);
        assert_eq!(decoded.reference_currency, body.reference_currency);
        assert_eq!(decoded.tribute_price_minor, body.tribute_price_minor);
        assert_eq!(
            decoded.exclude_from_intex_issuance,
            body.exclude_from_intex_issuance
        );
    }
}

fn run_contract(reader: StorageReaderHandle, writer: StorageWriterHandle) {
    let repository_reader = TributeRepositoryReader::new(reader.clone());
    let repository_writer = TributeRepositoryWriter::new(reader.clone(), writer.clone());
    let owner_a = Address::repeat_byte(0x11);
    let owner_b = Address::repeat_byte(0x22);

    for id in [U256::from(3), U256::from(1), U256::from(2)] {
        repository_writer.put(&tribute(id, owner_a, 7)).unwrap();
    }
    repository_writer
        .put(&tribute(U256::from(4), owner_b, 8))
        .unwrap();

    let stored = repository_reader.get(U256::from(1)).unwrap().unwrap();
    assert_eq!(stored.token_id, U256::from(1));
    assert_eq!(stored.issuance_amount_minor, U256::MAX);
    assert_eq!(stored.nominal_amount_minor, U256::ZERO);
    assert!(stored.exclude_from_intex_issuance);

    let primary = reader
        .get(namespace("tributes"), &id_key(U256::from(1)))
        .unwrap()
        .unwrap();
    assert_eq!(
        postcard::from_bytes::<TributeData>(primary.as_bytes())
            .unwrap()
            .token_id,
        U256::from(1)
    );
    assert!(reader
        .get(
            namespace("tributes_by_owner"),
            &owner_key(owner_a, U256::from(1)),
        )
        .unwrap()
        .unwrap()
        .as_bytes()
        .is_empty());
    assert!(reader
        .get(namespace("tributes_by_day"), &day_key(7, U256::from(1)),)
        .unwrap()
        .unwrap()
        .as_bytes()
        .is_empty());

    let first = repository_reader
        .list_by_owner(
            owner_a,
            TributePageRequest {
                after: None,
                limit: 2,
            },
        )
        .unwrap();
    assert_eq!(
        first
            .records
            .iter()
            .map(|body| body.token_id)
            .collect::<Vec<_>>(),
        [U256::from(1), U256::from(2)]
    );
    assert_eq!(first.next_after, Some(U256::from(2)));
    let second = repository_reader
        .list_by_owner(
            owner_a,
            TributePageRequest {
                after: first.next_after,
                limit: 2,
            },
        )
        .unwrap();
    assert_eq!(
        second
            .records
            .iter()
            .map(|body| body.token_id)
            .collect::<Vec<_>>(),
        [U256::from(3)]
    );
    assert_eq!(second.next_after, None);
    assert_eq!(
        repository_reader
            .list_by_day(
                WorldwideDay::new(8),
                TributePageRequest {
                    after: None,
                    limit: 10,
                },
            )
            .unwrap()
            .records[0]
            .token_id,
        U256::from(4)
    );

    let replacement = tribute(U256::from(1), owner_b, 8);
    repository_writer.put(&replacement).unwrap();
    assert!(reader
        .get(
            namespace("tributes_by_owner"),
            &owner_key(owner_a, U256::from(1)),
        )
        .unwrap()
        .is_none());
    assert!(reader
        .get(namespace("tributes_by_day"), &day_key(7, U256::from(1)),)
        .unwrap()
        .is_none());
    assert_eq!(
        repository_reader.get(U256::from(1)).unwrap().unwrap().owner,
        owner_b
    );

    repository_writer.delete(U256::from(1)).unwrap();
    repository_writer.delete(U256::from(1)).unwrap();
    assert!(repository_reader.get(U256::from(1)).unwrap().is_none());
    assert!(reader
        .get(
            namespace("tributes_by_owner"),
            &owner_key(owner_b, U256::from(1)),
        )
        .unwrap()
        .is_none());
}

#[test]
fn memory_contract() {
    let storage = Arc::new(MemoryStorage::new());
    run_contract(storage.clone(), storage);
}

#[test]
#[ignore = "requires OUTBE_TEST_MONGODB_URI"]
fn mongo_contract() {
    run_isolated_mongo("tribute_repository", run_contract);
}

#[test]
fn rejects_corrupt_bodies_indexes_and_limits() {
    let storage = Arc::new(MemoryStorage::new());
    run_corruption_contract(storage.clone(), storage);
}

#[test]
#[ignore = "requires OUTBE_TEST_MONGODB_URI"]
fn mongo_rejects_corrupt_bodies_indexes_and_limits() {
    run_isolated_mongo("tribute_corruption", run_corruption_contract);
}

fn run_corruption_contract(reader_storage: StorageReaderHandle, storage: StorageWriterHandle) {
    let reader = TributeRepositoryReader::new(reader_storage);
    let owner = Address::repeat_byte(0x31);
    let id = U256::from(9);

    storage
        .put(
            namespace("tributes"),
            &id_key(id),
            &Value::new([0xff]).unwrap(),
        )
        .unwrap();
    assert!(matches!(
        reader.get(id),
        Err(TributeRepositoryError::Decode(_))
    ));

    let wrong = tribute(U256::from(10), owner, 4);
    storage
        .put(
            namespace("tributes"),
            &id_key(id),
            &Value::new(postcard::to_stdvec(&wrong).unwrap()).unwrap(),
        )
        .unwrap();
    assert!(matches!(
        reader.get(id),
        Err(TributeRepositoryError::PrimaryKeyBodyMismatch { .. })
    ));

    let mut trailing = postcard::to_stdvec(&tribute(id, owner, 4)).unwrap();
    trailing.push(0);
    storage
        .put(
            namespace("tributes"),
            &id_key(id),
            &Value::new(trailing).unwrap(),
        )
        .unwrap();
    assert!(matches!(
        reader.get(id),
        Err(TributeRepositoryError::Decode(_))
    ));

    storage.delete(namespace("tributes"), &id_key(id)).unwrap();
    storage
        .put(
            namespace("tributes_by_owner"),
            &owner_key(owner, id),
            &Value::new([1]).unwrap(),
        )
        .unwrap();
    assert!(matches!(
        reader.list_by_owner(
            owner,
            TributePageRequest {
                after: None,
                limit: 1
            }
        ),
        Err(TributeRepositoryError::NonEmptyIndexValue { .. })
    ));
    storage
        .put(
            namespace("tributes_by_owner"),
            &owner_key(owner, id),
            &Value::new(Vec::new()).unwrap(),
        )
        .unwrap();
    assert!(matches!(
        reader.list_by_owner(
            owner,
            TributePageRequest {
                after: None,
                limit: 1
            }
        ),
        Err(TributeRepositoryError::DanglingIndex { .. })
    ));
    storage
        .delete(namespace("tributes_by_owner"), &owner_key(owner, id))
        .unwrap();
    let malformed_owner_key = key([owner.as_slice(), &[1, 2, 3]].concat());
    storage
        .put(
            namespace("tributes_by_owner"),
            &malformed_owner_key,
            &Value::new(Vec::new()).unwrap(),
        )
        .unwrap();
    assert!(matches!(
        reader.list_by_owner(
            owner,
            TributePageRequest {
                after: None,
                limit: 1
            }
        ),
        Err(TributeRepositoryError::MalformedIndexKey { .. })
    ));
    assert!(matches!(
        reader.list_by_owner(
            owner,
            TributePageRequest {
                after: None,
                limit: 0
            }
        ),
        Err(TributeRepositoryError::InvalidPageLimit { .. })
    ));
    assert!(matches!(
        reader.list_by_owner(
            owner,
            TributePageRequest {
                after: None,
                limit: MAX_SCAN_ENTRIES + 1,
            },
        ),
        Err(TributeRepositoryError::InvalidPageLimit { .. })
    ));

    storage
        .delete(namespace("tributes_by_owner"), &malformed_owner_key)
        .unwrap();
    let actual_owner = Address::repeat_byte(0x32);
    let mismatched = tribute(id, actual_owner, 6);
    storage
        .put(
            namespace("tributes"),
            &id_key(id),
            &Value::new(postcard::to_stdvec(&mismatched).unwrap()).unwrap(),
        )
        .unwrap();
    storage
        .put(
            namespace("tributes_by_owner"),
            &owner_key(owner, id),
            &Value::new(Vec::new()).unwrap(),
        )
        .unwrap();
    assert!(matches!(
        reader.list_by_owner(
            owner,
            TributePageRequest {
                after: None,
                limit: 1
            }
        ),
        Err(TributeRepositoryError::IndexedOwnerMismatch { .. })
    ));
    storage
        .put(
            namespace("tributes_by_day"),
            &day_key(5, id),
            &Value::new(Vec::new()).unwrap(),
        )
        .unwrap();
    assert!(matches!(
        reader.list_by_day(
            WorldwideDay::new(5),
            TributePageRequest {
                after: None,
                limit: 1
            },
        ),
        Err(TributeRepositoryError::IndexedDayMismatch { .. })
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
    fn apply_atomic(&self, batch: &AtomicWriteBatch) -> Result<(), StorageError> {
        for _ in batch.operations() {
            self.finish_step()?;
        }
        self.inner.apply_atomic(batch)
    }
}

#[test]
fn failures_before_each_tribute_batch_step_leave_repository_unchanged() {
    let owner = Address::repeat_byte(0x81);
    let new_owner = Address::repeat_byte(0x82);
    let id = U256::from(77);

    for fail_after in 1..=3 {
        let storage = Arc::new(MemoryStorage::new());
        let failing = Arc::new(FailAfterWriter {
            inner: storage.clone(),
            fail_after,
            calls: AtomicUsize::new(0),
        });
        let repository = TributeRepositoryWriter::new(storage.clone(), failing);
        assert!(matches!(
            repository.put(&tribute(id, owner, 7)),
            Err(TributeRepositoryError::Storage(_))
        ));
        assert!(storage
            .get(namespace("tributes"), &id_key(id))
            .unwrap()
            .is_none());
        assert!(storage
            .get(namespace("tributes_by_owner"), &owner_key(owner, id))
            .unwrap()
            .is_none());
        assert!(storage
            .get(namespace("tributes_by_day"), &day_key(7, id))
            .unwrap()
            .is_none());
    }

    for fail_after in 1..=5 {
        let storage = Arc::new(MemoryStorage::new());
        let seed = TributeRepositoryWriter::new(storage.clone(), storage.clone());
        seed.put(&tribute(id, owner, 7)).unwrap();
        let failing = Arc::new(FailAfterWriter {
            inner: storage.clone(),
            fail_after,
            calls: AtomicUsize::new(0),
        });
        let repository = TributeRepositoryWriter::new(storage.clone(), failing);
        assert!(matches!(
            repository.put(&tribute(id, new_owner, 8)),
            Err(TributeRepositoryError::Storage(_))
        ));
        assert_eq!(
            TributeRepositoryReader::new(storage.clone())
                .get(id)
                .unwrap()
                .unwrap()
                .owner,
            owner
        );
        assert!(storage
            .get(namespace("tributes_by_owner"), &owner_key(new_owner, id))
            .unwrap()
            .is_none());
        assert!(storage
            .get(namespace("tributes_by_day"), &day_key(8, id))
            .unwrap()
            .is_none());
        assert!(storage
            .get(namespace("tributes_by_owner"), &owner_key(owner, id))
            .unwrap()
            .is_some());
        assert!(storage
            .get(namespace("tributes_by_day"), &day_key(7, id))
            .unwrap()
            .is_some());
    }

    for fail_after in 1..=3 {
        let storage = Arc::new(MemoryStorage::new());
        let seed = TributeRepositoryWriter::new(storage.clone(), storage.clone());
        seed.put(&tribute(id, owner, 7)).unwrap();
        let failing = Arc::new(FailAfterWriter {
            inner: storage.clone(),
            fail_after,
            calls: AtomicUsize::new(0),
        });
        let repository = TributeRepositoryWriter::new(storage.clone(), failing);
        assert!(matches!(
            repository.delete(id),
            Err(TributeRepositoryError::Storage(_))
        ));
        assert!(storage
            .get(namespace("tributes_by_owner"), &owner_key(owner, id))
            .unwrap()
            .is_some());
        assert!(storage
            .get(namespace("tributes_by_day"), &day_key(7, id))
            .unwrap()
            .is_some());
        assert!(storage
            .get(namespace("tributes"), &id_key(id))
            .unwrap()
            .is_some());
    }
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
