use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

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
    let writer = Arc::new(CountingWriter(AtomicUsize::new(0)));
    let overlay = PendingOverlayStorage::new(Arc::new(MemoryStorage::new()), writer.clone());
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
    assert!(second.persist().is_err());
    assert_eq!(writer.0.load(Ordering::SeqCst), 0);
    drop(first.persist().unwrap());
    assert!(second.persist().is_err(), "dropped receipt is not an ACK");
    assert_eq!(writer.0.load(Ordering::SeqCst), 1);
    first.persist().unwrap().acknowledge();
    second.persist().unwrap().acknowledge();
    assert_eq!(writer.0.load(Ordering::SeqCst), 3);
    assert!(
        first.persist().is_err(),
        "an acknowledged handle cannot be persisted again"
    );
    assert_eq!(writer.0.load(Ordering::SeqCst), 3);
}

#[test]
fn durable_receipt_is_required_to_release_pending_and_drop_preserves_retry() {
    let base = Arc::new(MemoryStorage::new());
    let overlay = PendingOverlayStorage::new(base.clone(), base.clone());
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
    let receipt = pending.persist().unwrap();
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
    pending.persist().unwrap().acknowledge();
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
    let overlay = PendingOverlayStorage::new(base.clone(), base.clone());
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
    first.persist().unwrap().acknowledge();
    assert!(base.get(namespace.clone(), &key).unwrap().is_some());
    assert!(overlay.get(namespace.clone(), &key).unwrap().is_none());
    newer.persist().unwrap().acknowledge();
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
    assert!(later.persist().is_err());
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
    let overlay = PendingOverlayStorage::new(base.clone(), base.clone());
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
    older.persist().unwrap().acknowledge();
    assert!(base.get(bodies.clone(), &retired).unwrap().is_some());
    assert!(overlay.get(bodies.clone(), &retired).unwrap().is_none());
    first_retirement.persist().unwrap().acknowledge();
    base.put(bodies.clone(), &retired, &value).unwrap();
    assert!(overlay.get(bodies.clone(), &retired).unwrap().is_none());
    assert_eq!(
        overlay.get(bodies.clone(), &sibling).unwrap().unwrap(),
        value
    );
    newer_retirement.persist().unwrap().acknowledge();
    base.put(bodies.clone(), &retired, &value).unwrap();
    assert_eq!(overlay.get(bodies, &retired).unwrap().unwrap(), value);
}

#[test]
fn staging_during_durable_write_remains_visible_after_the_older_receipt_ack() {
    use outbe_offchain_storage::PendingWrite;
    use std::sync::Mutex;
    struct StageDuringWrite {
        base: Arc<MemoryStorage>,
        overlay: Weak<PendingOverlayStorage>,
        staged: AtomicBool,
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
            if self.staged.swap(true, Ordering::SeqCst) {
                return Ok(());
            }
            let overlay = self
                .overlay
                .upgrade()
                .expect("overlay outlives the durable writer");
            *self.newer.lock().unwrap() =
                Some(overlay.stage(AtomicWriteBatch::from_operations(vec![
                    AtomicWriteOperation::delete(self.namespace.clone(), self.key.clone()),
                ]))?);
            Ok(())
        }
    }
    let base = Arc::new(MemoryStorage::new());
    let namespace = Namespace::new("records").unwrap();
    let key = Key::new(b"key".to_vec()).unwrap();
    let writer_slot: Arc<Mutex<Option<Arc<StageDuringWrite>>>> = Arc::new(Mutex::new(None));
    let slot = writer_slot.clone();
    let captured_base = base.clone();
    let captured_namespace = namespace.clone();
    let captured_key = key.clone();
    let overlay = Arc::new_cyclic(move |overlay_weak| {
        let writer = Arc::new(StageDuringWrite {
            base: captured_base.clone(),
            overlay: overlay_weak.clone(),
            staged: AtomicBool::new(false),
            newer: Mutex::default(),
            namespace: captured_namespace.clone(),
            key: captured_key.clone(),
        });
        *slot.lock().unwrap() = Some(writer.clone());
        PendingOverlayStorage::new(captured_base, writer)
    });
    let writer = writer_slot.lock().unwrap().take().unwrap();
    let first = overlay
        .stage(AtomicWriteBatch::from_operations(vec![
            AtomicWriteOperation::put(
                namespace.clone(),
                key.clone(),
                Value::new(b"first".to_vec()).unwrap(),
            ),
        ]))
        .unwrap();
    first.persist().unwrap().acknowledge();
    assert!(base.get(namespace.clone(), &key).unwrap().is_some());
    assert!(overlay.get(namespace.clone(), &key).unwrap().is_none());
    let newer = writer.newer.lock().unwrap().take().unwrap();
    newer.persist().unwrap().acknowledge();
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
