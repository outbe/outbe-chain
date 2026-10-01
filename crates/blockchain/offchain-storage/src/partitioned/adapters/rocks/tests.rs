//! Crash boundaries and immutable recovery inspection against actual RocksDB.
use super::*;
use crate::partitioned::PartitionedOperation;
use crate::{AtomicWriteOperation, Key, StorageReader, Value};
fn operation(scope: StorageScope, byte: u8) -> PartitionedOperation {
    PartitionedOperation {
        scope,
        operation: AtomicWriteOperation::put(
            Namespace::new("records").unwrap(),
            Key::new([1]).unwrap(),
            Value::new([byte]).unwrap(),
        ),
    }
}
#[test]
fn prepared_commit_is_inspectable_but_not_replayed_before_owner_validation() {
    let root = tempfile::tempdir().unwrap();
    let system = StorageScope::shared("system").unwrap();
    let a = StorageScope::numbered("account", "owners", 1).unwrap();
    let b = StorageScope::numbered("account", "owners", 2).unwrap();
    let batch = PartitionedBatch {
        operations: vec![
            operation(a.clone(), 11),
            operation(b.clone(), 22),
            operation(system.clone(), 33),
        ],
        ..Default::default()
    };
    {
        let source = RocksPartitionDataSource::open(root.path()).unwrap();
        source.save_journal(&batch).unwrap();
        // Simulate a process dying after the first physical partition.
        source
            .writer(&a)
            .unwrap()
            .apply_atomic(&AtomicWriteBatch::from_operations(vec![batch.operations
                [0]
            .operation
            .clone()]))
            .unwrap();
    }
    let source = RocksPartitionDataSource::open(root.path()).unwrap();
    let ns = Namespace::new("records").unwrap();
    let key = Key::new([1]).unwrap();
    assert_eq!(
        source
            .open_reader(&b)
            .unwrap()
            .unwrap()
            .get(ns.clone(), &key)
            .unwrap()
            .unwrap()
            .as_bytes(),
        &[22]
    );
    assert_eq!(
        source
            .open_reader(&system)
            .unwrap()
            .unwrap()
            .get(ns.clone(), &key)
            .unwrap()
            .unwrap()
            .as_bytes(),
        &[33]
    );
    assert!(source.system.get(ns.clone(), &key).unwrap().is_none());
    assert!(!root.path().join("account/owners/2").exists());
    assert!(source.commit(&batch).is_err());
    let scratch = tempfile::tempdir().unwrap();
    assert!(RocksPartitionReadView::open(root.path(), scratch.path()).is_err());
    source.complete_recovery().unwrap();
    source.complete_recovery().unwrap();
    assert_eq!(
        source.system.get(ns, &key).unwrap().unwrap().as_bytes(),
        &[33]
    );
    assert!(root.path().join("account/owners/2/CURRENT").is_file());
    assert!(!root
        .path()
        .join("system/shared/partition-journal.v3")
        .exists());
}
#[test]
fn modified_prepared_journal_is_rejected_without_replay() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("system/shared/partition-journal.v3");
    {
        let source = RocksPartitionDataSource::open(root.path()).unwrap();
        source
            .save_journal(&PartitionedBatch {
                operations: vec![operation(StorageScope::shared("system").unwrap(), 1)],
                ..Default::default()
            })
            .unwrap();
    }
    let mut bytes = std::fs::read(&path).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    std::fs::write(&path, bytes).unwrap();
    assert!(RocksPartitionDataSource::open(root.path()).is_err());
}
#[test]
fn missing_partition_reads_do_not_create_databases_and_legacy_root_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    let source = RocksPartitionDataSource::open(root.path()).unwrap();
    let missing = StorageScope::numbered("account", "owners", 31).unwrap();
    assert!(source.open_reader(&missing).unwrap().is_none());
    assert!(!root.path().join("account").exists());
    let old = tempfile::tempdir().unwrap();
    std::fs::create_dir(old.path().join("shared")).unwrap();
    assert!(RocksPartitionDataSource::open(old.path()).is_err());
    assert!(!old.path().join("system").exists());
}

#[test]
fn retry_of_an_authorized_commit_finishes_the_journal_before_acknowledging() {
    let root = tempfile::tempdir().unwrap();
    let source = RocksPartitionDataSource::open(root.path()).unwrap();
    let scope = StorageScope::numbered("account", "owners", 1).unwrap();
    let batch = PartitionedBatch {
        operations: vec![
            operation(scope.clone(), 7),
            operation(StorageScope::shared("system").unwrap(), 8),
        ],
        ..Default::default()
    };
    std::fs::write(
        root.path().join("account"),
        b"temporarily unavailable directory",
    )
    .unwrap();
    assert!(source.commit(&batch).is_err());
    std::fs::remove_file(root.path().join("account")).unwrap();
    source.commit(&batch).unwrap();
    assert!(!root
        .path()
        .join("system/shared/partition-journal.v3")
        .exists());
    assert_eq!(
        source
            .open_reader(&scope)
            .unwrap()
            .unwrap()
            .get(Namespace::new("records").unwrap(), &Key::new([1]).unwrap())
            .unwrap()
            .unwrap()
            .as_bytes(),
        &[7]
    );
}

#[test]
fn read_session_rejects_scratch_anywhere_inside_primary_root_before_creating_it() {
    let root = tempfile::tempdir().unwrap();
    let source = RocksPartitionDataSource::open(root.path()).unwrap();
    let scratch = root.path().join("reader-cache");
    assert!(RocksPartitionReadView::open(root.path(), &scratch).is_err());
    assert!(!scratch.exists());
    drop(source);
}
