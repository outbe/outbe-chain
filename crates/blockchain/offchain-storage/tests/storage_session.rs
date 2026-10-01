use outbe_offchain_storage::{
    MemoryStorage, OpenedStorage, StorageCloseError, StorageError, StorageLifecycle,
};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

struct Ownership(Arc<AtomicBool>);
impl StorageLifecycle for Ownership {
    fn activate(&self) -> Result<(), StorageError> {
        Ok(())
    }
    fn close(&mut self) -> Result<(), StorageError> {
        self.0.store(true, Ordering::Release);
        Ok(())
    }
}

#[test]
fn capability_clones_keep_session_ownership_until_last_handle_is_released() {
    let released = Arc::new(AtomicBool::new(false));
    let memory = Arc::new(MemoryStorage::new());
    let opened = OpenedStorage::new(
        memory.clone(),
        memory,
        Box::new(Ownership(released.clone())),
    );
    let completion = opened.ownership.completion();
    let writer = opened.writer.clone();
    let reader = opened.reader.clone();
    drop(opened);
    assert!(matches!(
        completion.wait_timeout(Duration::from_millis(5)),
        Err(StorageCloseError::Timeout)
    ));
    assert!(!released.load(Ordering::Acquire));
    drop(writer);
    assert!(matches!(
        completion.wait_timeout(Duration::from_millis(5)),
        Err(StorageCloseError::Timeout)
    ));
    drop(reader);
    completion.wait_timeout(Duration::from_secs(1)).unwrap();
    assert!(released.load(Ordering::Acquire));
}

#[test]
fn ordinary_writes_are_blocked_until_activation() {
    use outbe_offchain_storage::{Key, Namespace, Value};
    let memory = Arc::new(MemoryStorage::new());
    let opened = OpenedStorage::new(memory.clone(), memory, Box::new(Ownership(Arc::default())));
    let namespace = Namespace::new("records").unwrap();
    let key = Key::new(vec![1]).unwrap();
    let value = Value::new(vec![2]).unwrap();
    assert!(opened.writer.put(namespace.clone(), &key, &value).is_err());
    assert!(opened
        .reader
        .get(namespace.clone(), &key)
        .unwrap()
        .is_none());
    opened.ownership.activate().unwrap();
    opened.writer.put(namespace.clone(), &key, &value).unwrap();
    assert_eq!(opened.reader.get(namespace, &key).unwrap(), Some(value));
}

#[test]
fn preflight_writer_expires_without_enabling_ordinary_writes() {
    use outbe_offchain_storage::{Key, Namespace, Value};
    let memory = Arc::new(MemoryStorage::new());
    let opened = OpenedStorage::new(memory.clone(), memory, Box::new(Ownership(Arc::default())));
    let namespace = Namespace::new("metadata").unwrap();
    let key = Key::new(vec![1]).unwrap();
    let value = Value::new(vec![2]).unwrap();
    let escaped = opened
        .ownership
        .preflight(|reader, writer| {
            writer.put(namespace.clone(), &key, &value).unwrap();
            assert_eq!(
                reader.get(namespace.clone(), &key).unwrap(),
                Some(value.clone())
            );
            assert!(opened.writer.put(namespace.clone(), &key, &value).is_err());
            writer
        })
        .unwrap();
    assert!(escaped.put(namespace.clone(), &key, &value).is_err());
    assert!(opened.writer.put(namespace.clone(), &key, &value).is_err());
    opened.ownership.activate().unwrap();
    assert!(escaped.put(namespace.clone(), &key, &value).is_err());
    opened.writer.put(namespace, &key, &value).unwrap();
}
