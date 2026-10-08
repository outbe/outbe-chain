use alloy_primitives::{Address, U256};
use outbe_compressed_entities::{IdPageRequest, WwdEntityId};
use outbe_nod::{NodItemState, NodPageRequest, NodRepositoryWriter};
use outbe_offchain_storage::partitioned::{
    adapters::MemoryPartitionDataSource,
    routing::{RoutingRegistry, SharedRouting},
};
use outbe_offchain_storage::{PartitionReadSource, PartitionedStorage, StorageScope};
use outbe_primitives::time::WorldwideDay;
use std::sync::Arc;

fn item(owner: Address, day: u32) -> NodItemState {
    outbe_nod::test_support::item(
        outbe_nod::test_support::NodItemFixture {
            nod_id: WwdEntityId::from_day_and_digest(WorldwideDay::new(day), [day as u8; 32]),
            owner,
            gratis_load_minor: U256::from(11),
            worldwide_day: WorldwideDay::new(day),
            league_id: 4,
            bucket_key: [1; 32].into(),
            issuance_currency: 840,
            reference_currency: 840,
            issued_at: 1_752_534_000,
            is_settled: false,
        },
        U256::ZERO,
    )
}

fn routing() -> Arc<RoutingRegistry> {
    let mut routing = RoutingRegistry::new(Arc::new(SharedRouting(
        StorageScope::shared("system").unwrap(),
    )));
    outbe_nod::partitioning::register(&mut routing).unwrap();
    Arc::new(routing)
}

#[test]
fn repository_updates_owner_index_and_keeps_global_identity_order() {
    let source = Arc::new(MemoryPartitionDataSource::new());
    exercise_repository(source);
}

fn exercise_repository(source: Arc<dyn outbe_offchain_storage::PartitionDataSource>) {
    let storage = Arc::new(PartitionedStorage::new(source, routing()));
    let writer = outbe_nod::nod_writer(storage.clone(), storage.clone());
    let reader = outbe_nod::nod_reader(storage);
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
    outbe_nod::test_support::set_terms(&mut first);
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
fn rocks_repository_reopens_id_shards_and_shared_owner_index() {
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
    assert!(root.path().join("nod/nod-shards/7/CURRENT").is_file());
    assert!(root.path().join("nod/nod-shards/9/CURRENT").is_file());
    assert!(root.path().join("nod/shared/CURRENT").is_file());
    assert!(!root.path().join("nod-days").exists());
    let reader = outbe_nod::nod_reader(Arc::new(PartitionedStorage::read_only(source, routing())));
    let expected = item(Address::repeat_byte(17), 9);
    assert_eq!(reader.get(expected.nod_id).unwrap(), Some(expected.clone()));
    assert_eq!(
        reader
            .list_ids_by_owner(
                expected.owner,
                IdPageRequest {
                    after: None,
                    limit: 10
                }
            )
            .unwrap()
            .ids,
        vec![expected.nod_id]
    );
}

#[test]
#[ignore = "requires OUTBE_TEST_MONGODB_URI"]
fn mongo_repository_updates_owner_index_across_id_collections() {
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
    assert!(names.contains(&"nod__shared__nods_by_owner".to_owned()));
    assert!(!names.contains(&"nod__shared__nod_locations".to_owned()));
    assert!(names.contains(&"nod__nod_shards_9__nods".to_owned()));
    assert!(!names.contains(&"nods".to_owned()));
    client.database(&database).drop().run().unwrap();
}

#[path = "partition_store/id_shards.rs"]
mod id_shards;
