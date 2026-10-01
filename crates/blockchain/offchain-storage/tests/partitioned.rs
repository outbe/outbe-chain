mod support;

use outbe_offchain_storage::{
    Namespace, OwnerModuloPartition, PartitionContext, PartitionId, PartitionStrategy, ScanRequest,
    StorageReader, Value,
};
use support::{id, storage, store_nod};

#[test]
fn strategies_use_the_full_unsigned_address_modulo_32() {
    let strategy = OwnerModuloPartition::new("nod-shards", 32).unwrap();
    for (last, expected) in [(32, 0), (17, 17), (255, 31)] {
        let mut owner = [0xff; 20];
        owner[19] = last;
        let actual = strategy
            .partition(&PartitionContext {
                entity_id: &[],
                worldwide_day: None,
                owner: Some(owner),
            })
            .unwrap();
        assert_eq!(
            actual,
            PartitionId::Numbered {
                family: "nod-shards".into(),
                index: expected
            }
        );
    }
}

#[test]
fn owner_modulo_is_a_strategy_not_a_fixed_storage_rule() {
    let strategy = OwnerModuloPartition::new("customer-buckets", 3).unwrap();
    // 256 % 3 = 1. Testing a non-power-of-two count detects low-byte shortcuts.
    let mut owner = [0; 20];
    owner[18] = 1;
    assert_eq!(
        strategy
            .partition(&PartitionContext {
                entity_id: &[],
                worldwide_day: None,
                owner: Some(owner)
            })
            .unwrap(),
        PartitionId::Numbered {
            family: "customer-buckets".into(),
            index: 1
        }
    );
}

#[test]
fn owner_partition_contains_nods_from_different_days_and_point_lookup_uses_locator() {
    let storage = storage();
    let owner = [17; 20];
    store_nod(&storage, &id(9, 1), owner);
    store_nod(&storage, &id(7, 2), owner);
    assert_eq!(
        storage
            .get(Namespace::new("nods").unwrap(), &id(9, 1))
            .unwrap(),
        Some(Value::new(vec![7]).unwrap())
    );
    let page = storage
        .scan_prefix(
            Namespace::new("nods_by_owner").unwrap(),
            ScanRequest::new(&owner, None, 10).unwrap(),
        )
        .unwrap();
    assert_eq!(page.entries.len(), 2);
    assert_eq!(
        storage.list_scopes("nod").unwrap(),
        vec![
            outbe_offchain_storage::StorageScope::shared("nod").unwrap(),
            outbe_offchain_storage::StorageScope::numbered("nod", "nod-shards", 17).unwrap()
        ]
    );
}

#[test]
fn global_pages_merge_shards_by_identity_instead_of_shard_number() {
    let storage = storage();
    store_nod(&storage, &id(9, 1), [0; 20]);
    store_nod(&storage, &id(7, 2), [31; 20]);
    let namespace = Namespace::new("nods").unwrap();
    let first = storage
        .scan_prefix(namespace.clone(), ScanRequest::new(&[], None, 1).unwrap())
        .unwrap();
    assert_eq!(first.entries[0].key, id(7, 2));
    assert_eq!(first.next_after, Some(id(7, 2)));
    let last = storage
        .scan_prefix(
            namespace,
            ScanRequest::new(&[], first.next_after.as_ref(), 1).unwrap(),
        )
        .unwrap();
    assert_eq!(last.entries[0].key, id(9, 1));
    assert!(last.next_after.is_none());
}

#[test]
fn retirement_clears_only_one_scope_and_overlay_hides_it_before_acknowledgement() {
    use outbe_offchain_storage::partitioned::{
        adapters::MemoryPartitionDataSource,
        routing::{DayPrefixRouting, RoutingRegistry, SharedRouting},
    };
    use outbe_offchain_storage::{
        AtomicWriteBatch, AtomicWriteOperation, Key, Namespace, PendingOverlayStorage,
        StorageReader, StorageScope, StorageWriter, Value,
    };
    use std::sync::Arc;
    let source = Arc::new(MemoryPartitionDataSource::new());
    let mut rules = RoutingRegistry::new(Arc::new(SharedRouting(
        StorageScope::shared("system").unwrap(),
    )));
    rules
        .register(
            "bodies",
            Arc::new(DayPrefixRouting::new("entity", 0).unwrap()),
        )
        .unwrap();
    let storage = Arc::new(outbe_offchain_storage::PartitionedStorage::new(
        source,
        Arc::new(rules),
    ));
    let ns = Namespace::new("bodies").unwrap();
    let a = Key::new(7u32.to_be_bytes()).unwrap();
    let b = Key::new(8u32.to_be_bytes()).unwrap();
    storage
        .put(ns.clone(), &a, &Value::new(vec![7]).unwrap())
        .unwrap();
    storage
        .put(ns.clone(), &b, &Value::new(vec![8]).unwrap())
        .unwrap();
    let overlay = PendingOverlayStorage::new(storage.clone());
    let mut batch = AtomicWriteBatch::from_operations(vec![AtomicWriteOperation::put(
        Namespace::new("checkpoint").unwrap(),
        Key::new(vec![1]).unwrap(),
        Value::new(vec![3]).unwrap(),
    )]);
    batch.retire_scope(StorageScope::numbered("entity", "wwd", 7).unwrap());
    overlay.apply_atomic(&batch).unwrap();
    assert!(overlay.get(ns.clone(), &a).unwrap().is_none());
    assert!(storage.get(ns.clone(), &a).unwrap().is_some());
    storage.apply_atomic(&batch).unwrap();
    overlay.acknowledge(overlay.current_generation());
    assert!(storage.get(ns.clone(), &a).unwrap().is_none());
    assert_eq!(storage.get(ns, &b).unwrap().unwrap().as_bytes(), &[8]);
}
