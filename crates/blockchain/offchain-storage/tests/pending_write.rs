use std::sync::Arc;

use outbe_offchain_storage::{
    AtomicWriteBatch, AtomicWriteOperation, Key, MemoryStorage, Namespace, PendingOverlayStorage,
    StorageReader, StorageWriter, Value,
};

#[test]
fn later_pending_batch_cannot_reach_the_backend_before_the_earlier_ack() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct CountingWriter(AtomicUsize);
    impl StorageWriter for CountingWriter {
        fn apply_atomic(
            &self,
            _: &AtomicWriteBatch,
        ) -> Result<(), outbe_offchain_storage::StorageError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }
    let overlay = PendingOverlayStorage::new(Arc::new(MemoryStorage::new()));
    let namespace = Namespace::new("records").unwrap();
    let batch = |key: &[u8]| {
        AtomicWriteBatch::from_operations(vec![AtomicWriteOperation::put(
            namespace.clone(),
            Key::new(key.to_vec()).unwrap(),
            Value::new(b"value".to_vec()).unwrap(),
        )])
    };
    let first = overlay.stage(batch(b"first")).unwrap();
    let second = overlay.stage(batch(b"second")).unwrap();
    let writer = CountingWriter(AtomicUsize::new(0));
    assert!(second.persist(&writer).is_err());
    assert_eq!(writer.0.load(Ordering::SeqCst), 0);
    drop(first.persist(&writer).unwrap());
    assert!(
        second.persist(&writer).is_err(),
        "dropped receipt is not an ACK"
    );
    assert_eq!(writer.0.load(Ordering::SeqCst), 1);
    first.persist(&writer).unwrap().acknowledge();
    second.persist(&writer).unwrap().acknowledge();
    assert_eq!(writer.0.load(Ordering::SeqCst), 3);
    assert!(
        first.persist(&writer).is_err(),
        "an acknowledged handle cannot be persisted again"
    );
    assert_eq!(writer.0.load(Ordering::SeqCst), 3);
}

#[test]
fn durable_receipt_is_required_to_release_pending_and_drop_preserves_retry() {
    let base = Arc::new(MemoryStorage::new());
    let overlay = PendingOverlayStorage::new(base.clone());
    let namespace = Namespace::new("records").unwrap();
    let key = Key::new(b"key".to_vec()).unwrap();
    let pending = overlay
        .stage(AtomicWriteBatch::from_operations(vec![
            AtomicWriteOperation::put(
                namespace.clone(),
                key.clone(),
                Value::new(b"pending".to_vec()).unwrap(),
            ),
        ]))
        .unwrap();
    assert!(base.get(namespace.clone(), &key).unwrap().is_none());
    let receipt = pending.persist(base.as_ref()).unwrap();
    base.put(
        namespace.clone(),
        &key,
        &Value::new(b"changed-base".to_vec()).unwrap(),
    )
    .unwrap();
    drop(receipt);
    assert_eq!(
        overlay
            .get(namespace.clone(), &key)
            .unwrap()
            .unwrap()
            .as_bytes(),
        b"pending"
    );
    pending.persist(base.as_ref()).unwrap().acknowledge();
    base.put(
        namespace.clone(),
        &key,
        &Value::new(b"acknowledged-base".to_vec()).unwrap(),
    )
    .unwrap();
    assert_eq!(
        overlay.get(namespace, &key).unwrap().unwrap().as_bytes(),
        b"acknowledged-base"
    );
}

#[test]
fn old_put_ack_preserves_a_newer_delete_and_abandoned_write_blocks_later_persistence() {
    let base = Arc::new(MemoryStorage::new());
    let overlay = PendingOverlayStorage::new(base.clone());
    let namespace = Namespace::new("records").unwrap();
    let key = Key::new(b"deleted".to_vec()).unwrap();
    let put = AtomicWriteBatch::from_operations(vec![AtomicWriteOperation::put(
        namespace.clone(),
        key.clone(),
        Value::new(b"first".to_vec()).unwrap(),
    )]);
    let first = overlay.stage(put.clone()).unwrap();
    let newer = overlay
        .stage(AtomicWriteBatch::from_operations(vec![
            AtomicWriteOperation::delete(namespace.clone(), key.clone()),
        ]))
        .unwrap();
    first.persist(base.as_ref()).unwrap().acknowledge();
    assert!(base.get(namespace.clone(), &key).unwrap().is_some());
    assert!(overlay.get(namespace.clone(), &key).unwrap().is_none());
    newer.persist(base.as_ref()).unwrap().acknowledge();
    base.put(
        namespace.clone(),
        &key,
        &Value::new(b"base".to_vec()).unwrap(),
    )
    .unwrap();
    assert_eq!(
        overlay
            .get(namespace.clone(), &key)
            .unwrap()
            .unwrap()
            .as_bytes(),
        b"base"
    );
    drop(overlay.stage(put.clone()).unwrap());
    let later = overlay.stage(put).unwrap();
    assert!(later.persist(base.as_ref()).is_err());
    assert_eq!(
        base.get(namespace.clone(), &key)
            .unwrap()
            .unwrap()
            .as_bytes(),
        b"base"
    );
    assert_eq!(
        overlay.get(namespace, &key).unwrap().unwrap().as_bytes(),
        b"first"
    );
}

#[test]
fn older_ack_preserves_newer_retirements_until_their_own_ack() {
    use outbe_offchain_storage::partitioned::{
        adapters::MemoryPartitionDataSource,
        routing::{DayPrefixRouting, RoutingRegistry, SharedRouting},
    };
    use outbe_offchain_storage::{PartitionedStorage, StorageScope};
    let source = Arc::new(MemoryPartitionDataSource::new());
    let mut routing = RoutingRegistry::new(Arc::new(SharedRouting(
        StorageScope::shared("system").unwrap(),
    )));
    routing
        .register(
            "bodies",
            Arc::new(DayPrefixRouting::new("tribute", 0).unwrap()),
        )
        .unwrap();
    let base = Arc::new(PartitionedStorage::new(source, Arc::new(routing)));
    let bodies = Namespace::new("bodies").unwrap();
    let retired = Key::new(7u32.to_be_bytes()).unwrap();
    let sibling = Key::new(8u32.to_be_bytes()).unwrap();
    let value = Value::new(b"body".to_vec()).unwrap();
    base.put(bodies.clone(), &retired, &value).unwrap();
    base.put(bodies.clone(), &sibling, &value).unwrap();
    let overlay = PendingOverlayStorage::new(base.clone());
    let older = overlay
        .stage(AtomicWriteBatch::from_operations(vec![
            AtomicWriteOperation::put(
                Namespace::new("checkpoint").unwrap(),
                Key::new(b"key".to_vec()).unwrap(),
                value.clone(),
            ),
        ]))
        .unwrap();
    let retirement = || {
        let mut batch = AtomicWriteBatch::new();
        batch.retire_scope(StorageScope::numbered("tribute", "wwd", 7).unwrap());
        batch
    };
    let first_retirement = overlay.stage(retirement()).unwrap();
    let newer_retirement = overlay.stage(retirement()).unwrap();
    older.persist(base.as_ref()).unwrap().acknowledge();
    assert!(base.get(bodies.clone(), &retired).unwrap().is_some());
    assert!(overlay.get(bodies.clone(), &retired).unwrap().is_none());
    first_retirement
        .persist(base.as_ref())
        .unwrap()
        .acknowledge();
    base.put(bodies.clone(), &retired, &value).unwrap();
    assert!(overlay.get(bodies.clone(), &retired).unwrap().is_none());
    assert_eq!(
        overlay.get(bodies.clone(), &sibling).unwrap().unwrap(),
        value
    );
    newer_retirement
        .persist(base.as_ref())
        .unwrap()
        .acknowledge();
    base.put(bodies.clone(), &retired, &value).unwrap();
    assert_eq!(overlay.get(bodies, &retired).unwrap().unwrap(), value);
}

#[test]
fn staging_during_durable_write_remains_visible_after_the_older_receipt_ack() {
    use outbe_offchain_storage::PendingWrite;
    use std::sync::Mutex;
    struct StageDuringWrite {
        base: Arc<MemoryStorage>,
        overlay: Arc<PendingOverlayStorage>,
        newer: Mutex<Option<PendingWrite>>,
        namespace: Namespace,
        key: Key,
    }
    impl StorageWriter for StageDuringWrite {
        fn apply_atomic(
            &self,
            batch: &AtomicWriteBatch,
        ) -> Result<(), outbe_offchain_storage::StorageError> {
            self.base.apply_atomic(batch)?;
            *self.newer.lock().unwrap() =
                Some(self.overlay.stage(AtomicWriteBatch::from_operations(vec![
                    AtomicWriteOperation::delete(self.namespace.clone(), self.key.clone()),
                ]))?);
            Ok(())
        }
    }
    let base = Arc::new(MemoryStorage::new());
    let overlay = Arc::new(PendingOverlayStorage::new(base.clone()));
    let namespace = Namespace::new("records").unwrap();
    let key = Key::new(b"key".to_vec()).unwrap();
    let first = overlay
        .stage(AtomicWriteBatch::from_operations(vec![
            AtomicWriteOperation::put(
                namespace.clone(),
                key.clone(),
                Value::new(b"first".to_vec()).unwrap(),
            ),
        ]))
        .unwrap();
    let writer = StageDuringWrite {
        base: base.clone(),
        overlay: overlay.clone(),
        newer: Mutex::default(),
        namespace: namespace.clone(),
        key: key.clone(),
    };
    first.persist(&writer).unwrap().acknowledge();
    assert!(base.get(namespace.clone(), &key).unwrap().is_some());
    assert!(overlay.get(namespace.clone(), &key).unwrap().is_none());
    let newer = writer.newer.lock().unwrap().take().unwrap();
    newer.persist(base.as_ref()).unwrap().acknowledge();
    base.put(
        namespace.clone(),
        &key,
        &Value::new(b"after".to_vec()).unwrap(),
    )
    .unwrap();
    assert_eq!(
        overlay.get(namespace, &key).unwrap().unwrap().as_bytes(),
        b"after"
    );
}
