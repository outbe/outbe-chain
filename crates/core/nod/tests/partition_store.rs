use alloy_primitives::{Address, U256};
use outbe_compressed_entities::{IdPageRequest, WwdEntityId};
use outbe_nod::{NodItemState, NodPageRequest, NodRepositoryReader, NodRepositoryWriter};
use outbe_offchain_storage::partitioned::{
    adapters::MemoryPartitionDataSource,
    routing::{RoutingRegistry, SharedRouting},
};
use outbe_offchain_storage::{PartitionReadSource, PartitionedStorage, StorageScope};
use outbe_primitives::time::WorldwideDay;
use std::sync::Arc;

fn item(owner: Address, day: u32) -> NodItemState {
    NodItemState {
        nod_id: WwdEntityId::from_day_and_digest(WorldwideDay::new(day), [day as u8; 32]),
        owner,
        gratis_load_minor: U256::from(11),
        worldwide_day: WorldwideDay::new(day),
        league_id: 4,
        floor_price_minor: U256::from(13),
        bucket_key: [1; 32].into(),
        issuance_currency: 840,
        reference_currency: 840,
        issued_at: 1_752_534_000,
        is_settled: false,
    }
}

#[test]
fn repository_moves_between_owner_shards_and_keeps_global_identity_order() {
    let source = Arc::new(MemoryPartitionDataSource::new());
    exercise_repository(source);
}

fn exercise_repository(source: Arc<dyn outbe_offchain_storage::PartitionDataSource>) {
    let mut routing = RoutingRegistry::new(Arc::new(SharedRouting(
        StorageScope::shared("system").unwrap(),
    )));
    outbe_nod::partitioning::register(&mut routing).unwrap();
    let storage = Arc::new(PartitionedStorage::new(source, Arc::new(routing)));
    let writer = NodRepositoryWriter::new(storage.clone(), storage.clone());
    let reader = NodRepositoryReader::new(storage);
    let owner = Address::repeat_byte(17);
    let other = Address::repeat_byte(31);
    let mut first = item(owner, 7);
    let second = item(owner, 9);
    writer.put_nod(&second).unwrap();
    writer.put_nod(&first).unwrap();
    assert_eq!(
        reader
            .list_by_owner(
                owner,
                NodPageRequest {
                    after: None,
                    limit: 10
                }
            )
            .unwrap()
            .records
            .len(),
        2
    );
    first.owner = other;
    writer.put_nod(&first).unwrap();
    assert_eq!(reader.get(first.nod_id).unwrap().unwrap().owner, other);
    assert_eq!(
        reader
            .list_by_owner(
                owner,
                NodPageRequest {
                    after: None,
                    limit: 10
                }
            )
            .unwrap()
            .records
            .len(),
        1
    );
    let page = reader
        .list_ids_all(IdPageRequest {
            after: None,
            limit: 1,
        })
        .unwrap();
    assert_eq!(page.ids, vec![first.nod_id]);
    assert_eq!(page.next_after, Some(first.nod_id));
    writer.delete_nod(first.nod_id).unwrap();
    assert!(reader.get(first.nod_id).unwrap().is_none());
    assert!(reader
        .list_by_owner(
            other,
            NodPageRequest {
                after: None,
                limit: 10
            }
        )
        .unwrap()
        .records
        .is_empty());
}

#[test]
fn audit_rejects_a_missing_or_wrong_location() {
    use outbe_compressed_entities::{CeAuditLimits, CeAuditWork};
    use outbe_offchain_storage::{Key, Namespace, StorageWriter, Value};
    let source = Arc::new(MemoryPartitionDataSource::new());
    let mut routing = RoutingRegistry::new(Arc::new(SharedRouting(
        StorageScope::shared("system").unwrap(),
    )));
    outbe_nod::partitioning::register(&mut routing).unwrap();
    let storage = Arc::new(PartitionedStorage::new(source, Arc::new(routing)));
    let writer = NodRepositoryWriter::new(storage.clone(), storage.clone());
    let reader = NodRepositoryReader::new(storage.clone());
    let nod = item(Address::repeat_byte(17), 7);
    writer.put_nod(&nod).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let work = CeAuditWork::create(
        scratch.path().join("audit"),
        CeAuditLimits {
            records_per_run: 2,
            merge_fan_in: 2,
        },
    )
    .unwrap();
    reader.audit_partition_locations(&work).unwrap();
    let namespace = Namespace::new(outbe_nod::partitioning::NOD_LOCATIONS_NAMESPACE).unwrap();
    let key = Key::new(nod.nod_id.as_slice().to_vec()).unwrap();
    storage
        .put(
            namespace.clone(),
            &key,
            &Value::new(31u32.to_be_bytes().to_vec()).unwrap(),
        )
        .unwrap();
    assert!(reader.audit_partition_locations(&work).is_err());
    storage.delete(namespace, &key).unwrap();
    assert!(reader.audit_partition_locations(&work).is_err());
}

#[test]
fn rocks_repository_moves_and_reopens_all_owner_partitions() {
    use outbe_offchain_storage::partitioned::adapters::{
        RocksPartitionDataSource, RocksPartitionReadView,
    };
    let root = tempfile::tempdir().unwrap();
    exercise_repository(Arc::new(
        RocksPartitionDataSource::open(root.path()).unwrap(),
    ));
    let scratch = tempfile::tempdir().unwrap();
    let source = Arc::new(RocksPartitionReadView::open(root.path(), scratch.path()).unwrap());
    // Scratch must be outside the primary root, and each test owns its own path.
    assert!(!source.list_scopes("nod").unwrap().is_empty());
    assert!(root.path().join("nod/nod-shards/17/CURRENT").is_file());
    assert!(root.path().join("nod/nod-shards/31/CURRENT").is_file());
    assert!(root.path().join("nod/shared/CURRENT").is_file());
    assert!(!root.path().join("nod-days").exists());
}

#[test]
#[ignore = "requires OUTBE_TEST_MONGODB_URI"]
fn mongo_repository_moves_atomically_between_owner_collections() {
    use outbe_offchain_storage::partitioned::adapters::MongoPartitionDataSource;
    use outbe_offchain_storage::{MongoStorage, MongoStorageConfig};
    let uri = std::env::var("OUTBE_TEST_MONGODB_URI").unwrap();
    let database = format!("outbe_nod_shards_{}", std::process::id());
    let client = mongodb::sync::Client::with_uri_str(&uri).unwrap();
    let raw = Arc::new(
        MongoStorage::connect(MongoStorageConfig {
            uri,
            database: database.clone(),
        })
        .unwrap(),
    );
    raw.verify_transaction_support().unwrap();
    exercise_repository(Arc::new(MongoPartitionDataSource::open(raw).unwrap()));
    let names = client
        .database(&database)
        .list_collection_names()
        .run()
        .unwrap();
    assert!(names.contains(&"nod__shared__nod_locations".to_owned()));
    assert!(names.contains(&"nod__nod_shards_17__nods".to_owned()));
    assert!(!names.contains(&"nods".to_owned()));
    client.database(&database).drop().run().unwrap();
}
