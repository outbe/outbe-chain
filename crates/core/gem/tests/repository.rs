#[path = "../../../../testing/fixtures/mongo.rs"]
mod mongo_fixture;
use mongo_fixture::run_isolated_mongo;
use std::{
    io,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use alloy_primitives::{Address, B256, U256};
use outbe_compressed_entities::{
    decode_stored_gem_bucket_v1, decode_stored_gem_v1, encode_gem_bucket_v1, encode_gem_v1,
    EntityRef, IdPageRequest, ParentBodySource, ParentBodySourceError, QueryRef, StoredBody,
    WwdEntityId, BODY_SCHEMA_V1,
};
use outbe_gem::{
    canonical_bucket, canonical_bucket_id, canonical_item, from_canonical_bucket,
    from_canonical_item, GemBucketState, GemItemState, GemPageRequest, GemRepositoryError,
};
use outbe_offchain_storage::{
    AtomicWriteBatch, Key, MemoryStorage, Namespace, StorageError, StorageReader,
    StorageReaderHandle, StorageWriter, StorageWriterHandle, Value, MAX_SCAN_ENTRIES,
};
use outbe_primitives::time::WorldwideDay;

fn gem_id(seed: u64) -> WwdEntityId {
    WwdEntityId::from(U256::from(seed))
}

fn item(gem_id: WwdEntityId, owner: Address) -> GemItemState {
    GemItemState {
        gem_id,
        owner,
        gem_type: 1,
        promis_load_minor: U256::MAX,
        floor_price_minor: U256::from(1_u64),
        issuance_currency: 0,
        reference_currency: u16::MAX,
        issued_at: u64::MAX,
        bucket_key: B256::repeat_byte(0x33),
        state: 0,
        called_at: 0,
        settled_at: 0,
    }
}

fn bucket(key: B256, worldwide_day: WorldwideDay) -> GemBucketState {
    GemBucketState {
        settled_gems: 0,
        bucket_key: key,
        worldwide_day,
        entry_price_minor: U256::ZERO,
        reference_currency: 978,
    }
}

fn namespace(name: &str) -> Namespace {
    Namespace::new(name).unwrap()
}

fn key(bytes: impl Into<Vec<u8>>) -> Key {
    Key::new(bytes).unwrap()
}

fn id_key(id: WwdEntityId) -> Key {
    key(id.as_slice().to_vec())
}

fn owner_key(owner: Address, id: WwdEntityId) -> Key {
    key([owner.as_slice(), id.as_slice()].concat())
}

fn stored_item(body: &GemItemState) -> Vec<u8> {
    StoredBody::new(
        BODY_SCHEMA_V1,
        encode_gem_v1(&canonical_item(body)).unwrap(),
    )
    .unwrap()
    .encode()
}

fn stored_bucket(body: &GemBucketState) -> Vec<u8> {
    StoredBody::new(
        BODY_SCHEMA_V1,
        encode_gem_bucket_v1(&canonical_bucket(body)).unwrap(),
    )
    .unwrap()
    .encode()
}

#[test]
fn projection_stores_derive_item_and_bucket_identity_from_canonical_bytes() {
    let storage = Arc::new(MemoryStorage::new());
    let reader: StorageReaderHandle = storage.clone();
    let writer: StorageWriterHandle = storage.clone();
    let item = item(gem_id(41), Address::repeat_byte(0x41));
    let item_value = Value::new(stored_item(&item)).unwrap();
    let repository = outbe_gem::gem_reader(reader);
    let mut session = repository.projection_session(&[item.gem_id], &[]).unwrap();
    let item_batch = session
        .store_item(item.gem_id, item_value.clone(), None)
        .unwrap();
    writer.apply_atomic(&item_batch).unwrap();
    assert_eq!(
        repository
            .list_by_owner(
                item.owner,
                GemPageRequest {
                    after: None,
                    limit: 1,
                },
            )
            .unwrap()
            .records[0]
            .gem_id,
        item.gem_id
    );

    let wrong_item_id = gem_id(42);
    let mut wrong_item_session = repository
        .projection_session(&[wrong_item_id], &[])
        .unwrap();
    assert!(matches!(
        wrong_item_session.store_item(wrong_item_id, item_value, None),
        Err(GemRepositoryError::PrimaryKeyBodyMismatch { expected, actual })
            if expected == wrong_item_id && actual == item.gem_id
    ));
    let mut malformed_item_session = repository.projection_session(&[item.gem_id], &[]).unwrap();
    assert!(matches!(
        malformed_item_session.store_item(item.gem_id, Value::new(vec![0xff]).unwrap(), None),
        Err(GemRepositoryError::CanonicalBody(_))
    ));

    let body = bucket(B256::repeat_byte(0x51), WorldwideDay::new(20_260_716));
    let canonical_id = canonical_bucket_id(&body);
    let wrong_bucket_id =
        outbe_gem::identity::bucket_id(body.worldwide_day, B256::repeat_byte(0x52));
    let mut wrong_bucket_session = repository
        .projection_session(&[], &[wrong_bucket_id])
        .unwrap();
    assert!(matches!(
        wrong_bucket_session.store_bucket(
            wrong_bucket_id,
            Value::new(stored_bucket(&body)).unwrap(),
            None,
        ),
        Err(GemRepositoryError::BucketIdBodyMismatch { expected, actual })
            if expected == wrong_bucket_id && actual == canonical_id
    ));
    let mut malformed_bucket_session = repository.projection_session(&[], &[canonical_id]).unwrap();
    assert!(matches!(
        malformed_bucket_session.store_bucket(canonical_id, Value::new(vec![0xff]).unwrap(), None,),
        Err(GemRepositoryError::CanonicalBody(_))
    ));
}

#[test]
fn projection_session_owns_item_prior_state_and_rejects_untracked_identity() {
    let storage = Arc::new(MemoryStorage::new());
    let reader_handle: StorageReaderHandle = storage.clone();
    let writer_handle: StorageWriterHandle = storage;
    let repository_reader = outbe_gem::gem_reader(reader_handle.clone());
    let repository_writer = outbe_gem::gem_writer(reader_handle, writer_handle.clone());
    let old = item(gem_id(43), Address::repeat_byte(0x43));
    repository_writer.put_item(&old).unwrap();

    let replacement = item(old.gem_id, Address::repeat_byte(0x44));
    let mut session = repository_reader
        .projection_session(&[old.gem_id], &[])
        .unwrap();
    let batch = session
        .store_item(
            old.gem_id,
            Value::new(stored_item(&replacement)).unwrap(),
            None,
        )
        .unwrap();
    writer_handle.apply_atomic(&batch).unwrap();
    assert!(repository_reader
        .list_by_owner(
            old.owner,
            GemPageRequest {
                after: None,
                limit: 1,
            },
        )
        .unwrap()
        .records
        .is_empty());
    assert_eq!(
        repository_reader
            .list_by_owner(
                replacement.owner,
                GemPageRequest {
                    after: None,
                    limit: 1,
                },
            )
            .unwrap()
            .records[0]
            .gem_id,
        old.gem_id
    );

    let mut delete_session = repository_reader
        .projection_session(&[old.gem_id], &[])
        .unwrap();
    writer_handle
        .apply_atomic(&delete_session.delete_item(old.gem_id).unwrap())
        .unwrap();
    assert!(repository_reader
        .list_by_owner(
            replacement.owner,
            GemPageRequest {
                after: None,
                limit: 1,
            },
        )
        .unwrap()
        .records
        .is_empty());

    let mut untracked = repository_reader.projection_session(&[], &[]).unwrap();
    assert!(matches!(
        untracked.delete_item(old.gem_id),
        Err(GemRepositoryError::UntrackedProjectionIdentity { entity: "Gem item", identity })
            if identity == old.gem_id
    ));
    let untracked_bucket_id =
        outbe_gem::identity::bucket_id(WorldwideDay::new(20_260_716), B256::repeat_byte(0x45));
    assert!(matches!(
        untracked.delete_bucket(untracked_bucket_id),
        Err(GemRepositoryError::UntrackedProjectionIdentity { entity: "Gem bucket", identity })
            if identity == untracked_bucket_id
    ));
}

#[test]
fn the_bucket_reference_currency_only_appends_to_the_canonical_payload() {
    let mut body = bucket(B256::repeat_byte(0x21), WorldwideDay::new(20_260_715));
    body.reference_currency = 0;
    let unpriced = encode_gem_bucket_v1(&canonical_bucket(&body)).unwrap();
    body.reference_currency = 840;
    let priced = encode_gem_bucket_v1(&canonical_bucket(&body)).unwrap();
    assert_eq!(priced, [unpriced.as_slice(), &[0x20, 0xC8, 0x06]].concat());
}

#[test]
fn canonical_stored_bodies_roundtrip_field_boundaries() {
    let zero = GemItemState {
        gem_id: WwdEntityId::from(U256::ZERO),
        owner: Address::ZERO,
        gem_type: 0,
        promis_load_minor: U256::ZERO,
        floor_price_minor: U256::ZERO,
        issuance_currency: 0,
        reference_currency: 0,
        issued_at: 0,
        bucket_key: B256::ZERO,
        state: 0,
        called_at: 0,
        settled_at: 0,
    };
    let max = GemItemState {
        gem_id: WwdEntityId::from(U256::MAX),
        owner: Address::repeat_byte(u8::MAX),
        gem_type: u8::MAX,
        promis_load_minor: U256::MAX,
        floor_price_minor: U256::MAX,
        issuance_currency: u16::MAX,
        reference_currency: u16::MAX,
        issued_at: u64::MAX,
        bucket_key: B256::repeat_byte(u8::MAX),
        state: outbe_gem::GemState::Settled as u8,
        called_at: u64::MAX,
        settled_at: u64::MAX,
    };
    for body in [zero, max] {
        let decoded =
            from_canonical_item(decode_stored_gem_v1(&stored_item(&body)).unwrap()).unwrap();
        assert_eq!(decoded, body);
    }
    for body in [
        bucket(B256::ZERO, WorldwideDay::new(0)),
        GemBucketState {
            settled_gems: u64::MAX,
            entry_price_minor: U256::MAX,
            reference_currency: u16::MAX,
            ..bucket(B256::repeat_byte(u8::MAX), WorldwideDay::new(u32::MAX))
        },
    ] {
        let decoded =
            from_canonical_bucket(decode_stored_gem_bucket_v1(&stored_bucket(&body)).unwrap())
                .unwrap();
        assert_eq!(decoded, body);
    }

    let mut wide = canonical_item(&item(gem_id(1), Address::ZERO));
    wide.gem_type = u32::from(u8::MAX) + 1;
    let stored = StoredBody::new(BODY_SCHEMA_V1, encode_gem_v1(&wide).unwrap())
        .unwrap()
        .encode();
    assert!(matches!(
        from_canonical_item(decode_stored_gem_v1(&stored).unwrap()),
        Err(GemRepositoryError::CanonicalFieldOutOfRange { field: "gem_type" })
    ));
    wide.gem_type = 2;
    wide.state = 2;
    assert!(encode_gem_v1(&wide).is_err());
}

fn run_contract(reader: StorageReaderHandle, writer: StorageWriterHandle) {
    let repository_reader = outbe_gem::gem_reader(reader.clone());
    let repository_writer = outbe_gem::gem_writer(reader.clone(), writer.clone());
    let owner_a = Address::repeat_byte(0x22);
    let owner_b = Address::repeat_byte(0x44);
    for id in [gem_id(3), gem_id(1), gem_id(2)] {
        repository_writer.put_item(&item(id, owner_a)).unwrap();
    }
    repository_writer
        .put_item(&item(gem_id(4), owner_b))
        .unwrap();
    let selected = bucket(B256::repeat_byte(0x55), WorldwideDay::new(0));
    let selected_bucket_id = canonical_bucket_id(&selected);
    repository_writer.put_bucket(&selected).unwrap();

    assert_eq!(
        repository_reader
            .get_item(gem_id(1))
            .unwrap()
            .unwrap()
            .gem_id,
        gem_id(1)
    );
    assert_eq!(
        repository_reader
            .get_bucket(selected_bucket_id)
            .unwrap()
            .unwrap()
            .bucket_key,
        selected.bucket_key
    );
    let primary = reader
        .get(namespace("gems"), &id_key(gem_id(1)))
        .unwrap()
        .unwrap();
    assert_eq!(
        decode_stored_gem_v1(primary.as_bytes()).unwrap().gem_id,
        gem_id(1)
    );
    assert!(reader
        .get(namespace("gems_by_owner"), &owner_key(owner_a, gem_id(1)))
        .unwrap()
        .unwrap()
        .as_bytes()
        .is_empty());
    assert_eq!(
        reader
            .get(namespace("gem_buckets"), &id_key(selected_bucket_id))
            .unwrap()
            .unwrap()
            .as_bytes(),
        stored_bucket(&selected)
    );

    let first = repository_reader
        .scan_stored_items(IdPageRequest {
            after: None,
            limit: 2,
        })
        .unwrap();
    assert_eq!(
        first.entries.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        [gem_id(1), gem_id(2)]
    );
    assert_eq!(first.next_after, Some(gem_id(2)));
    let second = repository_reader
        .scan_stored_items(IdPageRequest {
            after: first.next_after,
            limit: 3,
        })
        .unwrap();
    assert_eq!(
        second.entries.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        [gem_id(3), gem_id(4)]
    );
    assert_eq!(second.next_after, None);
    assert_eq!(
        repository_reader
            .list_by_owner(
                owner_b,
                GemPageRequest {
                    after: None,
                    limit: 10
                }
            )
            .unwrap()
            .records[0]
            .gem_id,
        gem_id(4)
    );

    repository_writer
        .put_item(&item(gem_id(1), owner_b))
        .unwrap();
    assert!(reader
        .get(namespace("gems_by_owner"), &owner_key(owner_a, gem_id(1)))
        .unwrap()
        .is_none());
    assert_eq!(
        repository_reader
            .get_item(gem_id(1))
            .unwrap()
            .unwrap()
            .owner,
        owner_b
    );
    repository_writer.delete_item(gem_id(2)).unwrap();
    repository_writer.delete_item(gem_id(2)).unwrap();
    let remaining = repository_reader
        .scan_stored_items(IdPageRequest {
            after: None,
            limit: 10,
        })
        .unwrap();
    assert_eq!(
        remaining
            .entries
            .iter()
            .map(|(id, _)| *id)
            .collect::<Vec<_>>(),
        [gem_id(1), gem_id(3), gem_id(4)]
    );
    repository_writer.delete_bucket(selected_bucket_id).unwrap();
    repository_writer.delete_bucket(selected_bucket_id).unwrap();
    assert!(repository_reader
        .get_bucket(selected_bucket_id)
        .unwrap()
        .is_none());
    assert!(repository_reader
        .scan_stored_buckets(IdPageRequest {
            after: None,
            limit: 10
        })
        .unwrap()
        .entries
        .is_empty());
}

#[test]
fn memory_contract() {
    let storage = Arc::new(MemoryStorage::new());
    run_contract(storage.clone(), storage);
}

#[test]
fn parent_seam_returns_exact_stored_bodies_and_strict_id_pages() {
    let storage = Arc::new(MemoryStorage::new());
    let reader_handle: StorageReaderHandle = storage.clone();
    let writer = outbe_gem::gem_writer(reader_handle.clone(), storage);
    let reader = outbe_gem::gem_reader(reader_handle);
    let owner = Address::repeat_byte(0x29);
    let ids = [gem_id(1), gem_id(2), gem_id(3)];
    for id in [ids[2], ids[0], ids[1]] {
        writer.put_item(&item(id, owner)).unwrap();
    }
    let selected = bucket(B256::repeat_byte(0x59), WorldwideDay::new(0));
    let selected_bucket = canonical_bucket_id(&selected);
    writer.put_bucket(&selected).unwrap();

    assert_eq!(
        reader.get_stored_item(ids[0]).unwrap().unwrap().encode(),
        stored_item(&item(ids[0], owner))
    );
    assert_eq!(
        reader
            .get(EntityRef::Gem(ids[0]))
            .unwrap()
            .unwrap()
            .encode(),
        stored_item(&item(ids[0], owner))
    );
    assert_eq!(
        reader
            .get_stored_bucket(selected_bucket)
            .unwrap()
            .unwrap()
            .encode(),
        stored_bucket(&selected)
    );
    assert!(reader.get_stored_item(gem_id(99)).unwrap().is_none());
    assert!(matches!(
        reader.get(EntityRef::Tribute(ids[0])),
        Err(ParentBodySourceError::Corruption(_))
    ));
    assert!(matches!(
        reader.list(
            QueryRef::NodAll,
            IdPageRequest {
                after: None,
                limit: 1
            }
        ),
        Err(ParentBodySourceError::Corruption(_))
    ));

    let owner_page = reader
        .list(
            QueryRef::GemByOwner(owner),
            IdPageRequest {
                after: None,
                limit: 2,
            },
        )
        .unwrap();
    let scanned = reader
        .scan_stored_items(IdPageRequest {
            after: None,
            limit: 2,
        })
        .unwrap();
    assert_eq!(owner_page.ids, ids[..2]);
    assert_eq!(owner_page.next_after, Some(ids[1]));
    assert_eq!(
        scanned
            .entries
            .iter()
            .map(|(id, _)| *id)
            .collect::<Vec<_>>(),
        ids[..2]
    );
    let tail = reader
        .list_ids_by_owner(
            owner,
            IdPageRequest {
                after: Some(ids[1]),
                limit: 2,
            },
        )
        .unwrap();
    assert_eq!(tail.ids, ids[2..]);
    assert_eq!(tail.next_after, None);
    assert!(matches!(
        reader.list_ids_by_owner(
            owner,
            IdPageRequest {
                after: None,
                limit: 0
            }
        ),
        Err(GemRepositoryError::InvalidPageLimit { .. })
    ));
}

#[test]
#[ignore = "requires OUTBE_TEST_MONGODB_URI"]
fn mongo_contract() {
    run_isolated_mongo("gem_repository", run_contract);
}

#[test]
fn rejects_corrupt_items_buckets_indexes_and_limits() {
    let storage = Arc::new(MemoryStorage::new());
    run_corruption_contract(storage.clone(), storage);
}

#[test]
#[ignore = "requires OUTBE_TEST_MONGODB_URI"]
fn mongo_rejects_corrupt_items_buckets_indexes_and_limits() {
    run_isolated_mongo("gem_corruption", run_corruption_contract);
}

fn run_corruption_contract(reader_storage: StorageReaderHandle, storage: StorageWriterHandle) {
    let reader = outbe_gem::gem_reader(reader_storage);
    let owner = Address::repeat_byte(0x61);
    let id = gem_id(9);
    storage
        .put(namespace("gems"), &id_key(id), &Value::new([0xff]).unwrap())
        .unwrap();
    assert!(matches!(
        reader.get_item(id),
        Err(GemRepositoryError::CanonicalBody(_))
    ));
    let wrong = item(gem_id(10), owner);
    storage
        .put(
            namespace("gems"),
            &id_key(id),
            &Value::new(stored_item(&wrong)).unwrap(),
        )
        .unwrap();
    assert!(matches!(
        reader.get_item(id),
        Err(GemRepositoryError::PrimaryKeyBodyMismatch { .. })
    ));
    let mut trailing = stored_item(&item(id, owner));
    trailing.push(0);
    storage
        .put(
            namespace("gems"),
            &id_key(id),
            &Value::new(trailing).unwrap(),
        )
        .unwrap();
    assert!(matches!(
        reader.get_item(id),
        Err(GemRepositoryError::CanonicalBody(_))
    ));
    storage.delete(namespace("gems"), &id_key(id)).unwrap();
    storage
        .put(
            namespace("gems_by_owner"),
            &owner_key(owner, id),
            &Value::new([1]).unwrap(),
        )
        .unwrap();
    assert!(matches!(
        reader.list_by_owner(
            owner,
            GemPageRequest {
                after: None,
                limit: 1
            }
        ),
        Err(GemRepositoryError::NonEmptyIndexValue)
    ));
    storage
        .put(
            namespace("gems_by_owner"),
            &owner_key(owner, id),
            &Value::new(Vec::new()).unwrap(),
        )
        .unwrap();
    assert!(matches!(
        reader.list_by_owner(
            owner,
            GemPageRequest {
                after: None,
                limit: 1
            }
        ),
        Err(GemRepositoryError::DanglingIndex { .. })
    ));
    storage
        .delete(namespace("gems_by_owner"), &owner_key(owner, id))
        .unwrap();
    let malformed_owner_key = key([owner.as_slice(), &[1, 2]].concat());
    storage
        .put(
            namespace("gems_by_owner"),
            &malformed_owner_key,
            &Value::new(Vec::new()).unwrap(),
        )
        .unwrap();
    assert!(matches!(
        reader.list_by_owner(
            owner,
            GemPageRequest {
                after: None,
                limit: 1
            }
        ),
        Err(GemRepositoryError::MalformedIndexKey)
    ));
    let malformed_primary = key([0xaa]);
    storage
        .put(
            namespace("gems"),
            &malformed_primary,
            &Value::new([0]).unwrap(),
        )
        .unwrap();
    assert!(matches!(
        reader.scan_stored_items(IdPageRequest {
            after: None,
            limit: 10
        }),
        Err(GemRepositoryError::MalformedPrimaryKey)
    ));

    let selected = bucket(B256::repeat_byte(0x71), WorldwideDay::new(0));
    let selected_bucket = canonical_bucket_id(&selected);
    let wrong_bucket = bucket(B256::repeat_byte(0x72), WorldwideDay::new(0));
    storage
        .put(
            namespace("gem_buckets"),
            &id_key(selected_bucket),
            &Value::new(stored_bucket(&wrong_bucket)).unwrap(),
        )
        .unwrap();
    assert!(matches!(
        reader.get_bucket(selected_bucket),
        Err(GemRepositoryError::BucketIdBodyMismatch { .. })
    ));
    storage
        .put(
            namespace("gem_buckets"),
            &id_key(selected_bucket),
            &Value::new([0xff]).unwrap(),
        )
        .unwrap();
    assert!(matches!(
        reader.get_bucket(selected_bucket),
        Err(GemRepositoryError::CanonicalBody(_))
    ));
    let mut trailing_bucket = stored_bucket(&selected);
    trailing_bucket.push(0);
    storage
        .put(
            namespace("gem_buckets"),
            &id_key(selected_bucket),
            &Value::new(trailing_bucket).unwrap(),
        )
        .unwrap();
    assert!(matches!(
        reader.get_bucket(selected_bucket),
        Err(GemRepositoryError::CanonicalBody(_))
    ));
    assert!(matches!(
        reader.scan_stored_items(IdPageRequest {
            after: None,
            limit: 0
        }),
        Err(GemRepositoryError::InvalidPageLimit { .. })
    ));
    assert!(matches!(
        reader.scan_stored_buckets(IdPageRequest {
            after: None,
            limit: u32::try_from(MAX_SCAN_ENTRIES).unwrap() + 1,
        }),
        Err(GemRepositoryError::InvalidPageLimit { .. })
    ));

    storage
        .delete(namespace("gems"), &malformed_primary)
        .unwrap();
    storage
        .delete(namespace("gems_by_owner"), &malformed_owner_key)
        .unwrap();
    let mismatched = item(id, Address::repeat_byte(0x62));
    storage
        .put(
            namespace("gems"),
            &id_key(id),
            &Value::new(stored_item(&mismatched)).unwrap(),
        )
        .unwrap();
    storage
        .put(
            namespace("gems_by_owner"),
            &owner_key(owner, id),
            &Value::new(Vec::new()).unwrap(),
        )
        .unwrap();
    assert!(matches!(
        reader.list_by_owner(
            owner,
            GemPageRequest {
                after: None,
                limit: 1
            }
        ),
        Err(GemRepositoryError::IndexedOwnerMismatch { .. })
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
fn failures_before_each_gem_batch_step_leave_repository_unchanged() {
    let owner = Address::repeat_byte(0x91);
    let new_owner = Address::repeat_byte(0x92);
    let id = gem_id(88);
    for fail_after in 1..=2 {
        let storage = Arc::new(MemoryStorage::new());
        let failing = Arc::new(FailAfterWriter {
            inner: storage.clone(),
            fail_after,
            calls: AtomicUsize::new(0),
        });
        let repository = outbe_gem::gem_writer(storage.clone(), failing);
        assert!(matches!(
            repository.put_item(&item(id, owner)),
            Err(GemRepositoryError::Storage(_))
        ));
        assert!(storage
            .get(namespace("gems"), &id_key(id))
            .unwrap()
            .is_none());
        assert!(storage
            .get(namespace("gems_by_owner"), &owner_key(owner, id))
            .unwrap()
            .is_none());
    }
    for fail_after in 1..=3 {
        let storage = Arc::new(MemoryStorage::new());
        outbe_gem::gem_writer(storage.clone(), storage.clone())
            .put_item(&item(id, owner))
            .unwrap();
        let failing = Arc::new(FailAfterWriter {
            inner: storage.clone(),
            fail_after,
            calls: AtomicUsize::new(0),
        });
        assert!(matches!(
            outbe_gem::gem_writer(storage.clone(), failing).put_item(&item(id, new_owner)),
            Err(GemRepositoryError::Storage(_))
        ));
        assert_eq!(
            outbe_gem::gem_reader(storage.clone())
                .get_item(id)
                .unwrap()
                .unwrap()
                .owner,
            owner
        );
        assert!(storage
            .get(namespace("gems_by_owner"), &owner_key(new_owner, id))
            .unwrap()
            .is_none());
        assert!(storage
            .get(namespace("gems_by_owner"), &owner_key(owner, id))
            .unwrap()
            .is_some());
    }
    for fail_after in 1..=2 {
        let storage = Arc::new(MemoryStorage::new());
        outbe_gem::gem_writer(storage.clone(), storage.clone())
            .put_item(&item(id, owner))
            .unwrap();
        let failing = Arc::new(FailAfterWriter {
            inner: storage.clone(),
            fail_after,
            calls: AtomicUsize::new(0),
        });
        assert!(matches!(
            outbe_gem::gem_writer(storage.clone(), failing).delete_item(id),
            Err(GemRepositoryError::Storage(_))
        ));
        assert!(storage
            .get(namespace("gems_by_owner"), &owner_key(owner, id))
            .unwrap()
            .is_some());
        assert!(storage
            .get(namespace("gems"), &id_key(id))
            .unwrap()
            .is_some());
    }

    let selected = bucket(B256::repeat_byte(0xa1), WorldwideDay::new(0));
    let selected_bucket_id = canonical_bucket_id(&selected);
    let storage = Arc::new(MemoryStorage::new());
    let failing = Arc::new(FailAfterWriter {
        inner: storage.clone(),
        fail_after: 1,
        calls: AtomicUsize::new(0),
    });
    assert!(matches!(
        outbe_gem::gem_writer(storage.clone(), failing).put_bucket(&selected),
        Err(GemRepositoryError::Storage(_))
    ));
    assert!(storage
        .get(namespace("gem_buckets"), &id_key(selected_bucket_id))
        .unwrap()
        .is_none());
    outbe_gem::gem_writer(storage.clone(), storage.clone())
        .put_bucket(&selected)
        .unwrap();
    let failing = Arc::new(FailAfterWriter {
        inner: storage.clone(),
        fail_after: 1,
        calls: AtomicUsize::new(0),
    });
    assert!(matches!(
        outbe_gem::gem_writer(storage.clone(), failing).delete_bucket(selected_bucket_id),
        Err(GemRepositoryError::Storage(_))
    ));
    assert!(storage
        .get(namespace("gem_buckets"), &id_key(selected_bucket_id))
        .unwrap()
        .is_some());
}

#[test]
fn settled_item_and_bucket_reopen_and_keep_owner_membership() {
    let storage = Arc::new(MemoryStorage::new());
    let repository = outbe_gem::gem_reader(storage.clone());
    let mut item = item(gem_id(99), Address::repeat_byte(0x99));
    let mut bucket = bucket(item.bucket_key, WorldwideDay::new(20_260_715));
    let id = canonical_bucket_id(&bucket);
    let mut session = repository
        .projection_session(&[item.gem_id], &[id])
        .unwrap();
    storage
        .apply_atomic(
            &session
                .store_item(item.gem_id, Value::new(stored_item(&item)).unwrap(), None)
                .unwrap(),
        )
        .unwrap();
    storage
        .apply_atomic(
            &session
                .store_bucket(id, Value::new(stored_bucket(&bucket)).unwrap(), None)
                .unwrap(),
        )
        .unwrap();
    item.state = outbe_gem::GemState::Settled as u8;
    item.called_at = 10;
    item.settled_at = 11;
    bucket.settled_gems = 1;
    storage
        .apply_atomic(
            &session
                .store_item(item.gem_id, Value::new(stored_item(&item)).unwrap(), None)
                .unwrap(),
        )
        .unwrap();
    storage
        .apply_atomic(
            &session
                .store_bucket(id, Value::new(stored_bucket(&bucket)).unwrap(), None)
                .unwrap(),
        )
        .unwrap();
    let reopened = outbe_gem::gem_reader(storage);
    assert_eq!(reopened.get_item(item.gem_id).unwrap().unwrap(), item);
    assert_eq!(reopened.get_bucket(id).unwrap().unwrap(), bucket);
    let owned = reopened
        .list_by_owner(
            item.owner,
            GemPageRequest {
                after: None,
                limit: 2,
            },
        )
        .unwrap();
    assert_eq!(owned.records, vec![item]);
}
