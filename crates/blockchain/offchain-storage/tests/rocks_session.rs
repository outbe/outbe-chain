use outbe_offchain_storage::{
    partitioned::{adapters::RocksPartitionDataSource, routing::OwnerPrefixRouting},
    Key, Namespace, OpenedStorage, PartitionReadSource, PartitionedStorage, StorageCloseError,
    StorageScope, Value,
};
use std::{sync::Arc, time::Duration};

#[test]
fn completion_waits_for_an_independent_partition_reader() {
    let root = tempfile::tempdir().unwrap();
    let source = Arc::new(RocksPartitionDataSource::open(root.path()).unwrap());
    let logical = Arc::new(PartitionedStorage::new(
        source.clone(),
        Arc::new(OwnerPrefixRouting::new("entity", "owners", 32).unwrap()),
    ));
    let opened = OpenedStorage::new(logical.clone(), logical, source.lifecycle());
    opened.ownership.activate().unwrap();
    let scope = StorageScope::numbered("entity", "owners", 7).unwrap();
    let namespace = Namespace::new("records").unwrap();
    let key = Key::new([1]).unwrap();
    opened
        .writer
        .put(
            namespace.clone().with_scope(scope.clone()),
            &key,
            &Value::new([2]).unwrap(),
        )
        .unwrap();
    let partition_reader = source.open_reader(&scope).unwrap().unwrap();
    let completion = opened.ownership.completion();
    drop(source);
    drop(opened);
    assert!(matches!(
        completion.wait_timeout(Duration::from_millis(100)),
        Err(StorageCloseError::Timeout)
    ));
    assert_eq!(
        partition_reader
            .get(namespace, &key)
            .unwrap()
            .unwrap()
            .as_bytes(),
        &[2]
    );
    drop(partition_reader);
    completion.wait_timeout(Duration::from_secs(5)).unwrap();
    // Successful completion also guarantees every primary lock is available.
    let reopened = RocksPartitionDataSource::open(root.path()).unwrap();
    reopened.open_reader(&scope).unwrap().unwrap();
}

#[test]
fn failed_activation_remains_blocked_and_retries_with_an_already_removed_journal() {
    use outbe_offchain_storage::{
        partitioned::PartitionedOperation, AtomicWriteOperation, PartitionDataSource,
        PartitionedBatch,
    };
    let root = tempfile::tempdir().unwrap();
    let scope = StorageScope::numbered("entity", "owners", 3).unwrap();
    let namespace = Namespace::new("records").unwrap();
    let key = Key::new([1]).unwrap();
    let value = Value::new([7]).unwrap();
    std::fs::write(root.path().join("entity"), b"temporarily blocked directory").unwrap();
    {
        let source = RocksPartitionDataSource::open(root.path()).unwrap();
        assert!(source
            .commit(&PartitionedBatch {
                operations: vec![PartitionedOperation {
                    scope: scope.clone(),
                    operation: AtomicWriteOperation::put(
                        namespace.clone(),
                        key.clone(),
                        value.clone()
                    )
                }],
                ..Default::default()
            })
            .is_err());
    }
    let source = Arc::new(RocksPartitionDataSource::open(root.path()).unwrap());
    let logical = Arc::new(PartitionedStorage::new(
        source.clone(),
        Arc::new(OwnerPrefixRouting::new("entity", "owners", 32).unwrap()),
    ));
    let opened = OpenedStorage::new(logical.clone(), logical, source.lifecycle());
    let namespace = namespace.with_scope(scope);
    assert_eq!(
        opened.reader.get(namespace.clone(), &key).unwrap(),
        Some(value.clone())
    );
    assert!(opened.ownership.activate().is_err());
    assert!(opened.writer.put(namespace.clone(), &key, &value).is_err());
    // Retry must also tolerate a cleanup that unlinked the journal before failing.
    std::fs::remove_file(root.path().join("system/shared/partition-journal.v3")).unwrap();
    std::fs::remove_file(root.path().join("entity")).unwrap();
    opened.ownership.activate().unwrap();
    opened.ownership.activate().unwrap();
    assert_eq!(
        opened.reader.get(namespace.clone(), &key).unwrap(),
        Some(value.clone())
    );
    opened
        .writer
        .put(namespace, &Key::new([2]).unwrap(), &value)
        .unwrap();
    let completion = opened.ownership.completion();
    drop(source);
    drop(opened);
    completion.wait_timeout(Duration::from_secs(5)).unwrap();
}
