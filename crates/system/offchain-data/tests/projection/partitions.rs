//! Finalized entity projection through the same ports used by the node.
use super::support::*;
use alloy_primitives::{Address, B256};
use outbe_nod::NodRepositoryReader;
use outbe_offchain_data::{entity_partition_routing, FinalizedBlock, OffchainDataProjection};
use outbe_offchain_storage::partitioned::adapters::{
    MemoryPartitionDataSource, RocksPartitionDataSource, RocksPartitionReadView,
};
use outbe_offchain_storage::{PartitionDataSource, PartitionedStorage};
use outbe_primitives::{
    addresses::{NOD_ADDRESS, TRIBUTE_ADDRESS},
    time::WorldwideDay,
};
use outbe_tribute::{RetainedTributePin, RetainedTributeReader, TributeRepositoryReader};
use std::sync::Arc;

fn exercise(source: Arc<dyn PartitionDataSource>, retained: bool) {
    let storage = Arc::new(PartitionedStorage::new(
        source,
        entity_partition_routing().unwrap(),
    ));
    let day = 20260715;
    let owner = Address::repeat_byte(17);
    let tribute_id = poseidon_entity(owner, day);
    let nod_id = poseidon_entity(owner, day);
    let pin = RetainedTributePin {
        worldwide_day: WorldwideDay::new(day),
        input_lease_id: B256::repeat_byte(0x71),
    };
    let mut projection = if retained {
        OffchainDataProjection::open_with_retention_selector(
            config(10),
            storage.clone(),
            storage.clone(),
            Arc::new(FixedRetentionSelector { pin }),
        )
        .unwrap()
    } else {
        OffchainDataProjection::open(config(10), storage.clone(), storage.clone()).unwrap()
    };
    projection.enable_partition_retirement();
    projection
        .project_block(&FinalizedBlock {
            number: 10,
            hash: B256::repeat_byte(10),
            receipts: vec![receipt(
                0,
                11,
                vec![
                    log(0, TRIBUTE_ADDRESS, tribute_stored(tribute_id, owner, day)),
                    log(
                        1,
                        NOD_ADDRESS,
                        nod_stored(nod_id, owner, B256::repeat_byte(2)),
                    ),
                ],
            )],
        })
        .unwrap();
    let tribute = TributeRepositoryReader::new(storage.clone());
    let before = tribute.get_stored_body(tribute_id).unwrap().unwrap();
    let prepared = projection
        .prepare_block(&FinalizedBlock {
            number: 11,
            hash: B256::repeat_byte(11),
            receipts: vec![receipt(
                0,
                12,
                vec![log(0, TRIBUTE_ADDRESS, tribute_partition_retired(day))],
            )],
        })
        .unwrap();
    let (_, batch) = projection.apply_prepared_with_batch(prepared).unwrap();
    // Without a retention pin, bulk retirement needs no body or index enumeration.
    if !retained {
        assert_eq!(batch.operations().len(), 2);
    }
    assert_eq!(
        batch.retired_scopes(),
        &[outbe_tribute::partitioning::day_scope(day).unwrap()]
    );
    assert!(tribute.get(tribute_id).unwrap().is_none());
    assert!(NodRepositoryReader::new(storage.clone())
        .get(nod_id)
        .unwrap()
        .is_some());
    if retained {
        let body = RetainedTributeReader::new(storage)
            .get_current_or_retained(
                pin,
                tribute_id,
                tribute_commitment(&tribute_body(tribute_id, owner, day)),
            )
            .unwrap()
            .unwrap();
        assert_eq!(body.encode(), before.encode());
    }
}

#[test]
fn finalized_memory_retirement_preserves_nod_and_pinned_bodies() {
    for retained in [false, true] {
        exercise(Arc::new(MemoryPartitionDataSource::new()), retained);
    }
}

#[test]
fn finalized_rocks_retirement_removes_only_tribute_folder_and_snapshot_opens_every_scope() {
    for retained in [false, true] {
        let root = tempfile::tempdir().unwrap();
        exercise(
            Arc::new(RocksPartitionDataSource::open(root.path()).unwrap()),
            retained,
        );
        assert!(!root.path().join("tribute/wwd/20260715").exists());
        assert!(root.path().join("nod/nod-shards/17/CURRENT").is_file());
        assert!(root.path().join("system/shared/CURRENT").is_file());
        let scratch = tempfile::tempdir().unwrap();
        let reader = Arc::new(PartitionedStorage::read_only(
            Arc::new(RocksPartitionReadView::open(root.path(), scratch.path()).unwrap()),
            entity_partition_routing().unwrap(),
        ));
        assert!(NodRepositoryReader::new(reader)
            .get(poseidon_entity(Address::repeat_byte(17), 20260715))
            .unwrap()
            .is_some());
    }
}

#[test]
#[ignore = "requires OUTBE_TEST_MONGODB_URI"]
fn finalized_mongo_retirement_commits_shared_retention_and_preserves_nod_shard() {
    use outbe_offchain_storage::partitioned::adapters::MongoPartitionDataSource;
    use outbe_offchain_storage::{MongoStorage, MongoStorageConfig};
    let uri = std::env::var("OUTBE_TEST_MONGODB_URI").unwrap();
    let client = mongodb::sync::Client::with_uri_str(&uri).unwrap();
    for retained in [false, true] {
        let database = format!("outbe_partition_retire_{}_{}", std::process::id(), retained);
        let raw = Arc::new(
            MongoStorage::connect(MongoStorageConfig {
                uri: uri.clone(),
                database: database.clone(),
            })
            .unwrap(),
        );
        raw.verify_transaction_support().unwrap();
        exercise(
            Arc::new(MongoPartitionDataSource::open(raw).unwrap()),
            retained,
        );
        let collection = client
            .database(&database)
            .collection::<mongodb::bson::Document>("tribute__wwd_20260715__tributes");
        assert_eq!(
            collection
                .count_documents(mongodb::bson::doc! {})
                .run()
                .unwrap(),
            0
        );
        client.database(&database).drop().run().unwrap();
    }
}
