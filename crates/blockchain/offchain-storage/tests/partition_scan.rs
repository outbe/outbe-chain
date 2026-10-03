#[path = "partition_scan/support.rs"]
mod support;

use outbe_offchain_storage::{AtomicWriteBatch, AtomicWriteOperation, StorageWriter, Value};
use outbe_offchain_storage::{ScanRequest, StorageErrorKind, StorageReader};
use std::sync::Arc;
use support::{key, namespace, scope, Fixture};

#[test]
fn one_partition_serves_a_full_logical_page_in_one_datasource_scan() {
    let fixture = Fixture::new();
    for number in 1..=1001 {
        fixture.put(0, number, 1);
    }
    let page = fixture
        .storage
        .scan_prefix(
            namespace().with_scope(scope(0)),
            ScanRequest::new(&[], None, 1000).unwrap(),
        )
        .unwrap();
    assert_eq!(page.entries.len(), 1000);
    assert_eq!(page.entries.first().unwrap().key, key(1));
    assert_eq!(page.entries.last().unwrap().key, key(1000));
    assert_eq!(page.next_after, Some(key(1000)));
    // Datasource traffic is an agreed performance guarantee at the injected seam.
    assert_eq!(*fixture.scans.lock().unwrap(), vec![1000]);
}

#[test]
fn owner_prefix_routing_selects_one_shard_without_a_physical_scope_hint() {
    let fixture = Fixture::new();
    let mut owner = [0; 20];
    owner[19] = 7;
    let owner_key = |number: u32| {
        let mut bytes = owner.to_vec();
        bytes.extend(number.to_be_bytes());
        outbe_offchain_storage::Key::new(bytes).unwrap()
    };
    let batch = AtomicWriteBatch::from_operations(
        (1..=1001)
            .map(|number| {
                AtomicWriteOperation::put(
                    namespace().with_scope(scope(7)),
                    owner_key(number),
                    Value::new([42]).unwrap(),
                )
            })
            .collect(),
    );
    fixture.storage.apply_atomic(&batch).unwrap();
    fixture.put(8, 1, 1);
    let page = fixture
        .storage
        .scan_prefix(namespace(), ScanRequest::new(&owner, None, 1000).unwrap())
        .unwrap();
    assert_eq!(page.entries.len(), 1000);
    assert_eq!(page.entries.first().unwrap().key, owner_key(1));
    assert_eq!(page.next_after, Some(owner_key(1000)));
    assert_eq!(*fixture.scans.lock().unwrap(), vec![1000]);
}

#[test]
fn interleaved_partitions_serve_a_page_with_bounded_datasource_traffic() {
    let fixture = Fixture::new();
    for number in 1..=1001 {
        fixture.put(number % 32, number, 1);
    }
    let page = fixture
        .storage
        .scan_prefix(namespace(), ScanRequest::new(&[], None, 1000).unwrap())
        .unwrap();
    assert_eq!(
        page.entries
            .iter()
            .map(|entry| entry.key.clone())
            .collect::<Vec<_>>(),
        (1..=1000).map(key).collect::<Vec<_>>()
    );
    assert_eq!(page.next_after, Some(key(1000)));
    assert!(
        fixture.scans.lock().unwrap().len() <= 96,
        "a small-record page must not require a datasource query for each record"
    );
}

#[test]
fn one_partition_rejects_a_datasource_cursor_that_would_skip_records() {
    let fixture = Fixture::with_fault(|page| page.next_after = Some(key(99)));
    fixture.put(0, 1, 1);
    let error = fixture
        .storage
        .scan_prefix(
            namespace().with_scope(scope(0)),
            ScanRequest::new(&[], None, 1).unwrap(),
        )
        .unwrap_err();
    assert_eq!(
        error.kind(),
        outbe_offchain_storage::StorageErrorKind::Corruption
    );
}

#[test]
fn byte_pressure_preserves_unread_tails_and_exclusive_logical_cursors() {
    let fixture = Fixture::new();
    for number in 1..=8 {
        fixture.put(number % 8, number, 1);
    }
    for number in 9..=88 {
        fixture.put(number % 8, number, 1024 * 1024);
    }
    let mut after = None;
    let mut seen = Vec::new();
    loop {
        let page = fixture
            .storage
            .scan_prefix(
                namespace(),
                ScanRequest::new(&[], after.as_ref(), 1024).unwrap(),
            )
            .unwrap();
        assert!(
            page.entries
                .iter()
                .map(|entry| entry.value.as_bytes().len())
                .sum::<usize>()
                <= 8 * 1024 * 1024
        );
        if let Some(cursor) = &page.next_after {
            assert_eq!(cursor, &page.entries.last().unwrap().key);
        }
        seen.extend(page.entries.iter().map(|entry| entry.key.clone()));
        after = page.next_after;
        if after.is_none() {
            break;
        }
    }
    assert_eq!(seen, (1..=88).map(key).collect::<Vec<_>>());
}

#[test]
fn buffered_prefix_pages_neither_skip_nor_repeat_keys() {
    let fixture = Fixture::new();
    for number in 1..=300 {
        fixture.put(number % 3, number, 1);
    }
    let prefix = [0, 0, 0];
    let mut after = Some(key(90));
    let mut seen = Vec::new();
    loop {
        let page = fixture
            .storage
            .scan_prefix(
                namespace(),
                ScanRequest::new(&prefix, after.as_ref(), 7).unwrap(),
            )
            .unwrap();
        if let Some(cursor) = &page.next_after {
            assert_eq!(cursor, &page.entries.last().unwrap().key);
        }
        seen.extend(page.entries.iter().map(|entry| entry.key.clone()));
        after = page.next_after;
        if after.is_none() {
            break;
        }
    }
    assert_eq!(seen, (91..=255).map(key).collect::<Vec<_>>());
}

#[test]
fn prefetch_is_not_cached_between_scan_calls() {
    let fixture = Fixture::new();
    for number in 1..=4 {
        fixture.put(number % 2, number, 1);
    }
    let page = fixture
        .storage
        .scan_prefix(namespace(), ScanRequest::new(&[], None, 2).unwrap())
        .unwrap();
    assert_eq!(page.next_after, Some(key(2)));
    fixture.put(0, 5, 1);
    let next = fixture
        .storage
        .scan_prefix(
            namespace(),
            ScanRequest::new(&[], page.next_after.as_ref(), 3).unwrap(),
        )
        .unwrap();
    assert_eq!(
        next.entries
            .iter()
            .map(|entry| entry.key.clone())
            .collect::<Vec<_>>(),
        vec![key(3), key(4), key(5)]
    );
    assert_eq!(next.next_after, None);
}

#[test]
fn duplicate_keys_in_different_partitions_are_corruption() {
    let fixture = Fixture::new();
    fixture.put(0, 1, 1);
    fixture.put(1, 1, 1);
    assert_eq!(
        fixture
            .storage
            .scan_prefix(namespace(), ScanRequest::new(&[], None, 10).unwrap())
            .unwrap_err()
            .kind(),
        StorageErrorKind::Corruption
    );
}

#[test]
fn malformed_pages_are_rejected_for_both_single_and_multiple_partitions() {
    let faults: [fn(&mut outbe_offchain_storage::ScanPage); 4] = [
        |page| {
            page.entries.clear();
            page.next_after = Some(key(1));
        },
        |page| {
            page.entries.reverse();
        },
        |page| {
            if let Some(entry) = page.entries.first_mut() {
                entry.key = key(0);
            }
        },
        |page| {
            if let Some(entry) = page.entries.first().cloned() {
                page.entries.push(entry);
            }
        },
    ];
    for fault in faults {
        for single in [false, true] {
            let fixture = Fixture::with_fault(fault);
            fixture.put(0, 1, 1);
            fixture.put(0, 2, 1);
            fixture.put(1, 3, 1);
            fixture.put(0, 4, 1);
            let namespace = if single {
                namespace().with_scope(scope(0))
            } else {
                namespace()
            };
            let result = fixture
                .storage
                .scan_prefix(namespace, ScanRequest::new(&[], Some(&key(0)), 2).unwrap());
            assert_eq!(result.unwrap_err().kind(), StorageErrorKind::Corruption);
        }
    }
}

#[test]
fn byte_bound_includes_metadata_in_both_scan_paths() {
    use outbe_offchain_storage::{StorageMetadata, StoredValue};
    let fixture = Fixture::new();
    let metadata = StorageMetadata::new([("label".into(), "value".into())].into()).unwrap();
    for (shard, number) in [(0, 1), (0, 2), (1, 3)] {
        fixture
            .storage
            .apply_atomic(&AtomicWriteBatch::from_operations(vec![
                AtomicWriteOperation::put_record(
                    namespace().with_scope(scope(shard)),
                    key(number),
                    StoredValue {
                        value: Value::new(vec![7; 4 * 1024 * 1024]).unwrap(),
                        metadata: Some(metadata.clone()),
                    },
                ),
            ]))
            .unwrap();
    }
    for namespace in [namespace(), namespace().with_scope(scope(0))] {
        let page = fixture
            .storage
            .scan_prefix(namespace, ScanRequest::new(&[], None, 10).unwrap())
            .unwrap();
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries[0].metadata, Some(metadata.clone()));
        assert_eq!(page.next_after, Some(key(1)));
    }
}

#[test]
fn corruption_discovered_in_prefetch_fails_even_before_its_logical_page() {
    let fixture = Fixture::with_fault(|page| {
        if page.entries.len() > 1 {
            page.entries[1].key = page.entries[0].key.clone();
        }
    });
    for number in [1, 3, 5] {
        fixture.put(0, number, 1);
    }
    fixture.put(1, 2, 1);
    let error = fixture
        .storage
        .scan_prefix(namespace(), ScanRequest::new(&[], None, 1).unwrap())
        .unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::Corruption);
}

#[test]
fn empty_and_missing_partitions_produce_no_continuation() {
    let fixture = Fixture::new();
    for namespace in [namespace(), namespace().with_scope(scope(0))] {
        let page = fixture
            .storage
            .scan_prefix(namespace, ScanRequest::new(&[], None, 10).unwrap())
            .unwrap();
        assert!(page.entries.is_empty());
        assert_eq!(page.next_after, None);
    }
    fixture
        .storage
        .put(
            namespace().with_scope(scope(0)),
            &key(1),
            &Value::new([7]).unwrap(),
        )
        .unwrap();
    fixture
        .storage
        .put(
            namespace().with_scope(scope(1)),
            &key(2),
            &Value::new([7]).unwrap(),
        )
        .unwrap();
    let page = fixture
        .storage
        .scan_prefix(namespace(), ScanRequest::new(&[0xff], None, 10).unwrap())
        .unwrap();
    assert!(page.entries.is_empty());
    assert_eq!(page.next_after, None);
}

#[test]
fn real_rocks_partitions_preserve_buffered_pagination() {
    use outbe_offchain_storage::partitioned::adapters::RocksPartitionDataSource;
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::with_source(Arc::new(
        RocksPartitionDataSource::open(root.path()).unwrap(),
    ));
    exercise_real_partitions(&fixture);
}

#[test]
#[ignore = "requires OUTBE_TEST_MONGODB_URI"]
fn real_mongo_collections_preserve_buffered_pagination() {
    use outbe_offchain_storage::partitioned::adapters::MongoPartitionDataSource;
    use outbe_offchain_storage::{MongoStorage, MongoStorageConfig};
    let uri = std::env::var("OUTBE_TEST_MONGODB_URI").unwrap();
    let database = format!(
        "outbe_scan_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let client = mongodb::sync::Client::with_uri_str(&uri).unwrap();
    let raw = Arc::new(
        MongoStorage::connect(MongoStorageConfig {
            uri,
            database: database.clone(),
        })
        .unwrap(),
    );
    raw.verify_transaction_support().unwrap();
    let lease = raw.acquire_writer_lease().unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let fixture = Fixture::with_source(Arc::new(
            MongoPartitionDataSource::open(raw.clone()).unwrap(),
        ));
        exercise_real_partitions(&fixture);
    }));
    drop(lease);
    drop(raw);
    client.database(&database).drop().run().unwrap();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

fn exercise_real_partitions(fixture: &Fixture) {
    let batch = AtomicWriteBatch::from_operations(
        (1..=193)
            .map(|number| {
                AtomicWriteOperation::put(
                    namespace().with_scope(scope(number % 3)),
                    key(number),
                    Value::new([42]).unwrap(),
                )
            })
            .collect(),
    );
    fixture.storage.apply_atomic(&batch).unwrap();
    let page = fixture
        .storage
        .scan_prefix(namespace(), ScanRequest::new(&[], None, 100).unwrap())
        .unwrap();
    assert_eq!(
        page.entries
            .iter()
            .map(|entry| entry.key.clone())
            .collect::<Vec<_>>(),
        (1..=100).map(key).collect::<Vec<_>>()
    );
    assert_eq!(page.next_after, Some(key(100)));
    assert!(fixture.scans.lock().unwrap().len() <= 12);
    let next = fixture
        .storage
        .scan_prefix(
            namespace(),
            ScanRequest::new(&[], page.next_after.as_ref(), 100).unwrap(),
        )
        .unwrap();
    assert_eq!(
        next.entries
            .iter()
            .map(|entry| entry.key.clone())
            .collect::<Vec<_>>(),
        (101..=193).map(key).collect::<Vec<_>>()
    );
    assert_eq!(next.next_after, None);
}
