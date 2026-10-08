use super::*;
use outbe_compressed_entities::{CeAuditLimits, CeAuditWork};
use outbe_offchain_storage::partitioned::PartitionedBatch;
use outbe_offchain_storage::{
    Key, Namespace, PartitionDataSource, StorageError, StorageReader, StorageReaderHandle,
    StorageWriter,
};
use std::sync::atomic::{AtomicBool, Ordering};

struct Repository {
    storage: Arc<PartitionedStorage>,
    reader: outbe_nod::NodRepositoryReader,
    writer: NodRepositoryWriter,
}

impl Repository {
    fn new() -> Self {
        Self::with_source(Arc::new(MemoryPartitionDataSource::new()))
    }

    fn with_source(source: Arc<dyn PartitionDataSource>) -> Self {
        let storage = Arc::new(PartitionedStorage::new(source, routing()));
        Self {
            reader: outbe_nod::nod_reader(storage.clone()),
            writer: outbe_nod::nod_writer(storage.clone(), storage.clone()),
            storage,
        }
    }
}

fn audit_work(root: &std::path::Path) -> CeAuditWork {
    CeAuditWork::create(
        root.join("audit"),
        CeAuditLimits {
            records_per_run: 2,
            merge_fan_in: 2,
        },
    )
    .unwrap()
}

fn owner_key(owner: Address, id: WwdEntityId) -> Key {
    Key::new([owner.as_slice(), id.as_slice()].concat()).unwrap()
}

#[test]
fn one_owner_lists_ids_across_all_256_shards_with_bounded_pages() {
    let repository = Repository::new();
    let owner = Address::repeat_byte(17);
    let mut expected = Vec::new();
    for shard in 0..256u32 {
        let mut body = item(owner, 7);
        body.nod_id = WwdEntityId::from_day_and_digest(
            body.worldwide_day,
            U256::from(shard).to_be_bytes::<32>(),
        );
        outbe_nod::test_support::set_terms(&mut body);
        assert_eq!(outbe_nod::partitioning::item_shard(body.nod_id), shard);
        repository.writer.put_nod(&body).unwrap();
        expected.push(body.nod_id);
    }
    let mut actual = Vec::new();
    let mut after = None;
    loop {
        let page = repository
            .reader
            .list_ids_by_owner(owner, IdPageRequest { after, limit: 17 })
            .unwrap();
        actual.extend(page.ids);
        after = page.next_after;
        if after.is_none() {
            break;
        }
    }
    assert_eq!(actual, expected);
    let scratch = tempfile::tempdir().unwrap();
    repository
        .reader
        .audit_partition_locations(&audit_work(scratch.path()))
        .unwrap();
}

#[test]
fn id_lookup_needs_no_locator_and_owner_change_keeps_the_body_shard() {
    let repository = Repository::new();
    let mut body = item(Address::repeat_byte(17), 255);
    repository.writer.put_nod(&body).unwrap();
    let namespace = Namespace::new("nods").unwrap();
    let key = Key::new(body.nod_id.as_slice().to_vec()).unwrap();
    let expected = Some(StorageScope::numbered("nod", "nod-shards", 255).unwrap());
    assert_eq!(
        repository.storage.storage_scope(&namespace, &key).unwrap(),
        expected
    );
    body.owner = Address::repeat_byte(31);
    outbe_nod::test_support::set_terms(&mut body);
    repository.writer.put_nod(&body).unwrap();
    assert_eq!(
        repository.storage.storage_scope(&namespace, &key).unwrap(),
        expected
    );
    assert_eq!(repository.reader.get(body.nod_id).unwrap(), Some(body));
    let old_locator = Namespace::new("nod_locations")
        .unwrap()
        .with_scope(StorageScope::shared("nod").unwrap());
    assert!(repository.storage.get(old_locator, &key).unwrap().is_none());
}

#[test]
fn audit_rejects_a_body_in_the_wrong_id_shard() {
    let repository = Repository::new();
    let body = item(Address::repeat_byte(17), 7);
    repository.writer.put_nod(&body).unwrap();
    let namespace = Namespace::new("nods").unwrap();
    let key = Key::new(body.nod_id.as_slice().to_vec()).unwrap();
    let record = repository
        .storage
        .get_record(namespace.clone(), &key)
        .unwrap()
        .unwrap();
    repository.storage.delete(namespace.clone(), &key).unwrap();
    let wrong = namespace.with_scope(StorageScope::numbered("nod", "nod-shards", 8).unwrap());
    repository.storage.put(wrong, &key, &record.value).unwrap();
    assert!(repository.reader.get(body.nod_id).unwrap().is_none());
    let scratch = tempfile::tempdir().unwrap();
    assert!(repository
        .reader
        .audit_partition_locations(&audit_work(scratch.path()))
        .is_err());
}

#[test]
fn audit_rejects_a_missing_owner_membership_but_id_reads_still_work() {
    let repository = Repository::new();
    let body = item(Address::repeat_byte(17), 7);
    repository.writer.put_nod(&body).unwrap();
    let namespace = Namespace::new("nods_by_owner").unwrap();
    let key = owner_key(body.owner, body.nod_id);
    assert_eq!(
        repository.storage.storage_scope(&namespace, &key).unwrap(),
        Some(StorageScope::shared("nod").unwrap())
    );
    repository.storage.delete(namespace, &key).unwrap();
    assert_eq!(repository.reader.get(body.nod_id).unwrap(), Some(body));
    let scratch = tempfile::tempdir().unwrap();
    assert!(repository
        .reader
        .audit_partition_locations(&audit_work(scratch.path()))
        .is_err());
}

#[derive(Default)]
struct RejectNextBatch {
    source: MemoryPartitionDataSource,
    reject: AtomicBool,
}

impl PartitionReadSource for RejectNextBatch {
    fn open_reader(
        &self,
        scope: &StorageScope,
    ) -> Result<Option<StorageReaderHandle>, StorageError> {
        self.source.open_reader(scope)
    }

    fn list_scopes(&self, domain: &str) -> Result<Vec<StorageScope>, StorageError> {
        self.source.list_scopes(domain)
    }
}

impl PartitionDataSource for RejectNextBatch {
    fn commit(&self, batch: &PartitionedBatch) -> Result<(), StorageError> {
        if self.reject.swap(false, Ordering::SeqCst) {
            return Err(StorageError::InvalidArgument(
                "injected commit rejection".into(),
            ));
        }
        self.source.commit(batch)
    }

    fn verify_write_capability(&self) -> Result<(), StorageError> {
        self.source.verify_write_capability()
    }
}

#[test]
fn failed_commit_preserves_body_and_shared_owner_memberships() {
    let source = Arc::new(RejectNextBatch::default());
    let repository = Repository::with_source(source.clone());
    let original = item(Address::repeat_byte(17), 7);
    repository.writer.put_nod(&original).unwrap();
    let mut changed = original.clone();
    changed.owner = Address::repeat_byte(31);
    outbe_nod::test_support::set_terms(&mut changed);
    source.reject.store(true, Ordering::SeqCst);
    assert!(repository.writer.put_nod(&changed).is_err());
    assert_eq!(
        repository.reader.get(original.nod_id).unwrap(),
        Some(original.clone())
    );
    let request = IdPageRequest {
        after: None,
        limit: 10,
    };
    assert_eq!(
        repository
            .reader
            .list_ids_by_owner(original.owner, request)
            .unwrap()
            .ids,
        vec![original.nod_id]
    );
    assert!(repository
        .reader
        .list_ids_by_owner(changed.owner, request)
        .unwrap()
        .ids
        .is_empty());
    let scratch = tempfile::tempdir().unwrap();
    repository
        .reader
        .audit_partition_locations(&audit_work(scratch.path()))
        .unwrap();
}
