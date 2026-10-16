use alloy_primitives::{Address, B256, U256};
use outbe_compressed_entities::{IdPageRequest, WwdEntityId};
use outbe_gem::{GemBucketState, GemItemState, GemPageRequest, GemRepositoryWriter};
use outbe_offchain_storage::partitioned::{
    adapters::MemoryPartitionDataSource,
    routing::{RoutingRegistry, SharedRouting},
};
use outbe_offchain_storage::{PartitionReadSource, PartitionedStorage, StorageScope};
use outbe_primitives::time::WorldwideDay;
use std::sync::Arc;

fn gem_id_in_shard(shard: u8) -> WwdEntityId {
    WwdEntityId::from(U256::from(shard))
}

fn item(owner: Address, shard: u8) -> GemItemState {
    GemItemState {
        gem_id: gem_id_in_shard(shard),
        owner,
        gem_type: 1,
        promis_load_minor: U256::from(11_u64),
        issuance_currency: 840,
        reference_currency: 840,
        issued_at: 1_752_534_000,
        bucket_key: [1_u8; 32].into(),
        state: 0,
        called_at: 0,
        settled_at: 0,
    }
}

fn routing() -> Arc<RoutingRegistry> {
    let mut routing = RoutingRegistry::new(Arc::new(SharedRouting(
        StorageScope::shared("system").unwrap(),
    )));
    outbe_gem::partitioning::register(&mut routing).unwrap();
    Arc::new(routing)
}

#[test]
fn repository_updates_owner_index_and_keeps_global_identity_order() {
    let source = Arc::new(MemoryPartitionDataSource::new());
    exercise_repository(source);
}

fn exercise_repository(source: Arc<dyn outbe_offchain_storage::PartitionDataSource>) {
    let storage = Arc::new(PartitionedStorage::new(source, routing()));
    let writer = outbe_gem::gem_writer(storage.clone(), storage.clone());
    let reader = outbe_gem::gem_reader(storage);
    let owner = Address::repeat_byte(17);
    let other = Address::repeat_byte(31);
    let mut first = item(owner, 7);
    let second = item(owner, 9);
    let bucket = GemBucketState {
        bucket_key: B256::repeat_byte(0x55),
        worldwide_day: WorldwideDay::new(7),
        entry_price_minor: U256::from(17_u64),
        reference_currency: 840,
        settled_gems: 0,
    };
    writer.put_bucket(&bucket).unwrap();
    writer.put_item(&second).unwrap();
    writer.put_item(&first).unwrap();
    assert_eq!(
        reader
            .list_by_owner(
                owner,
                GemPageRequest {
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
    writer.put_item(&first).unwrap();
    assert_eq!(reader.get_item(first.gem_id).unwrap().unwrap().owner, other);
    assert_eq!(
        reader
            .list_by_owner(
                owner,
                GemPageRequest {
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
        .scan_stored_items(IdPageRequest {
            after: None,
            limit: 1,
        })
        .unwrap();
    assert_eq!(page.entries[0].0, first.gem_id);
    assert_eq!(page.next_after, Some(first.gem_id));
    writer.delete_item(first.gem_id).unwrap();
    assert!(reader.get_item(first.gem_id).unwrap().is_none());
    assert!(reader
        .list_by_owner(
            other,
            GemPageRequest {
                after: None,
                limit: 10
            }
        )
        .unwrap()
        .records
        .is_empty());
    assert_eq!(
        reader
            .get_bucket(outbe_gem::canonical_bucket_id(&bucket))
            .unwrap()
            .unwrap(),
        bucket
    );
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
    assert!(!source.list_scopes("gem").unwrap().is_empty());
    assert!(root.path().join("gem/gem-shards/7/CURRENT").is_file());
    assert!(root.path().join("gem/gem-shards/9/CURRENT").is_file());
    assert!(root.path().join("gem/shared/CURRENT").is_file());
    assert!(!root.path().join("gem-days").exists());
    let reader = outbe_gem::gem_reader(Arc::new(PartitionedStorage::read_only(source, routing())));
    let expected = item(Address::repeat_byte(17), 9);
    assert_eq!(
        reader.get_item(expected.gem_id).unwrap(),
        Some(expected.clone())
    );
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
        vec![expected.gem_id]
    );
}

#[test]
#[ignore = "requires OUTBE_TEST_MONGODB_URI"]
#[allow(non_snake_case)] // doubled underscores pin the Mongo collection spelling
fn mongo_collections_are_gem__gem_shards_n__gems_and_gem__shared__gems_by_owner() {
    use outbe_offchain_storage::partitioned::adapters::MongoPartitionDataSource;
    use outbe_offchain_storage::{MongoStorage, MongoStorageConfig};
    let uri = std::env::var("OUTBE_TEST_MONGODB_URI").unwrap();
    let database = format!("outbe_gem_shards_{}", std::process::id());
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
    assert!(names.contains(&"gem__shared__gems_by_owner".to_owned()));
    assert!(names.contains(&"gem__shared__gem_buckets".to_owned()));
    assert!(names.contains(&"gem__gem_shards_9__gems".to_owned()));
    assert!(!names.contains(&"gems".to_owned()));
    assert!(!names.contains(&"gem__shared__gem_locations".to_owned()));
    client.database(&database).drop().run().unwrap();
}

#[path = "partition_store/id_shards.rs"]
mod id_shards;
