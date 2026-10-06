use std::{collections::BTreeMap, sync::Arc};

use outbe_offchain_storage::{
    AtomicWriteBatch, AtomicWriteOperation, Key, MemoryStorage, Namespace, PendingOverlayStorage,
    ScanRequest, StorageError, StorageMetadata, StorageReader, StorageScope, StorageWriter,
    StoredValue, Value, MAX_SCAN_PAGE_VALUE_BYTES,
};

#[test]
fn pending_batch_overrides_base_puts_and_deletes_without_copying_untouched_records() {
    let base = Arc::new(MemoryStorage::new());
    let namespace = Namespace::new("records").unwrap();
    let replaced = Key::new(b"replaced".to_vec()).unwrap();
    let deleted = Key::new(b"deleted".to_vec()).unwrap();
    let untouched = Key::new(b"untouched".to_vec()).unwrap();
    base.put(
        namespace.clone(),
        &replaced,
        &Value::new(b"base-replaced".to_vec()).unwrap(),
    )
    .unwrap();
    base.put(
        namespace.clone(),
        &deleted,
        &Value::new(b"base-deleted".to_vec()).unwrap(),
    )
    .unwrap();
    base.put(
        namespace.clone(),
        &untouched,
        &Value::new(b"base-untouched".to_vec()).unwrap(),
    )
    .unwrap();

    let overlay = PendingOverlayStorage::new(base.clone(), base.clone());
    drop(
        overlay
            .stage(AtomicWriteBatch::from_operations(vec![
                AtomicWriteOperation::put(
                    namespace.clone(),
                    replaced.clone(),
                    Value::new(b"pending-replaced".to_vec()).unwrap(),
                ),
                AtomicWriteOperation::delete(namespace.clone(), deleted.clone()),
            ]))
            .unwrap(),
    );

    assert_eq!(
        overlay
            .get(namespace.clone(), &replaced)
            .unwrap()
            .unwrap()
            .as_bytes(),
        b"pending-replaced"
    );
    assert_eq!(overlay.get(namespace.clone(), &deleted).unwrap(), None);
    assert_eq!(
        overlay
            .get(namespace.clone(), &untouched)
            .unwrap()
            .unwrap()
            .as_bytes(),
        b"base-untouched"
    );

    assert_eq!(
        base.get(namespace.clone(), &replaced)
            .unwrap()
            .unwrap()
            .as_bytes(),
        b"base-replaced"
    );
    assert!(base.get(namespace, &deleted).unwrap().is_some());
}

#[test]
fn pending_index_mutations_merge_with_base_order_and_pagination() {
    let base = Arc::new(MemoryStorage::new());
    let namespace = Namespace::new("owner_index").unwrap();
    for raw_key in [10_u8, 20, 30, 40] {
        let key = Key::new(vec![raw_key]).unwrap();
        base.put(namespace.clone(), &key, &Value::new(vec![raw_key]).unwrap())
            .unwrap();
    }

    let overlay = PendingOverlayStorage::new(base.clone(), base);
    drop(
        overlay
            .stage(AtomicWriteBatch::from_operations(vec![
                AtomicWriteOperation::delete(namespace.clone(), Key::new(vec![20]).unwrap()),
                AtomicWriteOperation::put(
                    namespace.clone(),
                    Key::new(vec![15]).unwrap(),
                    Value::new(vec![15]).unwrap(),
                ),
                AtomicWriteOperation::put(
                    namespace.clone(),
                    Key::new(vec![30]).unwrap(),
                    Value::new(vec![99]).unwrap(),
                ),
            ]))
            .unwrap(),
    );

    let first = overlay
        .scan_prefix(namespace.clone(), ScanRequest::new(&[], None, 2).unwrap())
        .unwrap();
    assert_eq!(
        first
            .entries
            .iter()
            .map(|entry| entry.key.as_bytes()[0])
            .collect::<Vec<_>>(),
        vec![10, 15]
    );
    let cursor = first.next_after.expect("two merged records remain");

    let second = overlay
        .scan_prefix(namespace, ScanRequest::new(&[], Some(&cursor), 2).unwrap())
        .unwrap();
    assert_eq!(
        second
            .entries
            .iter()
            .map(|entry| (entry.key.as_bytes()[0], entry.value.as_bytes()[0]))
            .collect::<Vec<_>>(),
        vec![(30, 99), (40, 40)]
    );
    assert_eq!(second.next_after, None);
}

#[test]
fn durable_ack_only_retires_overlay_mutations_through_its_generation() {
    let base = Arc::new(MemoryStorage::new());
    let namespace = Namespace::new("records").unwrap();
    let key = Key::new(b"same-key".to_vec()).unwrap();
    let overlay = PendingOverlayStorage::new(base.clone(), base.clone());
    let first_batch = AtomicWriteBatch::from_operations(vec![AtomicWriteOperation::put(
        namespace.clone(),
        key.clone(),
        Value::new(b"first".to_vec()).unwrap(),
    )]);
    let second_batch = AtomicWriteBatch::from_operations(vec![AtomicWriteOperation::put(
        namespace.clone(),
        key.clone(),
        Value::new(b"second".to_vec()).unwrap(),
    )]);

    let first = overlay.stage(first_batch).unwrap();
    let second = overlay.stage(second_batch).unwrap();
    first.persist().unwrap().acknowledge();
    assert_eq!(
        overlay
            .get(namespace.clone(), &key)
            .unwrap()
            .unwrap()
            .as_bytes(),
        b"second",
        "ACK of N must not remove the newer N+1 value"
    );

    second.persist().unwrap().acknowledge();
    base.put(
        namespace.clone(),
        &key,
        &Value::new(b"base-after-ack".to_vec()).unwrap(),
    )
    .unwrap();
    assert_eq!(
        overlay.get(namespace, &key).unwrap().unwrap().as_bytes(),
        b"base-after-ack",
        "after the latest ACK the overlay must fall back to the durable base"
    );
}

fn ordered_overlay_case(
    seed: u8,
) -> Result<(PendingOverlayStorage, BTreeMap<Key, Value>), StorageError> {
    let base = Arc::new(MemoryStorage::new());
    let namespace = Namespace::new("records")?;
    let mut expected = BTreeMap::new();
    for n in (0_u8..64).step_by(2) {
        let key = Key::new(vec![b'p', n])?;
        let value = Value::new(vec![n])?;
        base.put(namespace.clone(), &key, &value)?;
        expected.insert(key, value);
    }
    base.put(
        namespace.clone(),
        &Key::new(vec![b'q', 0])?,
        &Value::new(vec![0])?,
    )?;
    let overlay = PendingOverlayStorage::new(base.clone(), base);
    let mut batch = AtomicWriteBatch::new();
    for n in 0_u8..64 {
        let key = Key::new(vec![b'p', n])?;
        if (n + seed).is_multiple_of(3) {
            batch.push(AtomicWriteOperation::delete(namespace.clone(), key.clone()));
            expected.remove(&key);
        } else if (n + seed) % 3 == 1 {
            let value = Value::new(vec![n, seed])?;
            batch.push(AtomicWriteOperation::put(
                namespace.clone(),
                key.clone(),
                value.clone(),
            ));
            expected.insert(key, value);
        }
    }
    drop(overlay.stage(batch)?);
    Ok((overlay, expected))
}

fn collect_pages(
    overlay: &PendingOverlayStorage,
    limit: usize,
    after: Option<Key>,
) -> Result<Vec<(Key, Value)>, StorageError> {
    let namespace = Namespace::new("records")?;
    let mut cursor = after;
    let mut collected = Vec::new();
    loop {
        let page = overlay.scan_prefix(
            namespace.clone(),
            ScanRequest::new(b"p", cursor.as_ref(), limit)?,
        )?;
        assert!(page.entries.len() <= limit);
        if let Some(next) = &page.next_after {
            assert_eq!(Some(next), page.entries.last().map(|entry| &entry.key));
            assert!(cursor.as_ref().is_none_or(|after| next > after));
        }
        collected.extend(
            page.entries
                .into_iter()
                .map(|entry| (entry.key, entry.value)),
        );
        cursor = page.next_after;
        if cursor.is_none() {
            return Ok(collected);
        }
    }
}

fn check_reference_pagination(seed: u8) -> Result<(), StorageError> {
    let (overlay, expected) = ordered_overlay_case(seed)?;
    for limit in [1, 2, 7, 64] {
        for after in [None, Some(Key::new(vec![b'p', 17])?)] {
            let reference = expected
                .iter()
                .filter(|(key, _)| after.as_ref().is_none_or(|after| *key > after))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect::<Vec<_>>();
            assert_eq!(collect_pages(&overlay, limit, after)?, reference);
        }
    }
    Ok(())
}

#[test]
fn merged_pages_match_an_independent_ordered_reference_map() -> Result<(), StorageError> {
    for seed in 0..8 {
        check_reference_pagination(seed)?;
    }
    Ok(())
}

#[test]
fn mixed_base_and_pending_pages_count_metadata_in_the_byte_budget() -> Result<(), StorageError> {
    let base = Arc::new(MemoryStorage::new());
    let namespace = Namespace::new("records")?;
    let metadata = StorageMetadata::new(BTreeMap::from([("kind".into(), "large".into())]))?;
    // Two values alone fit exactly. Their metadata must force a second page.
    let value_len = MAX_SCAN_PAGE_VALUE_BYTES / 2;
    let large = StoredValue::with_metadata(Value::new(vec![1; value_len])?, metadata);
    let first = Key::new(b"a".to_vec())?;
    let second = Key::new(b"b".to_vec())?;
    let last = Key::new(b"c".to_vec())?;
    base.apply_atomic(&AtomicWriteBatch::from_operations(vec![
        AtomicWriteOperation::put_record(namespace.clone(), first.clone(), large.clone()),
    ]))?;
    let overlay = PendingOverlayStorage::new(base.clone(), base);
    drop(overlay.stage(AtomicWriteBatch::from_operations(vec![
        AtomicWriteOperation::put_record(namespace.clone(), second.clone(), large),
        AtomicWriteOperation::put(namespace.clone(), last.clone(), Value::new(vec![3])?),
    ]))?);
    let page = overlay.scan_prefix(namespace.clone(), ScanRequest::new(&[], None, 10)?)?;
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.next_after.as_ref(), Some(&first));
    let tail = overlay.scan_prefix(namespace, ScanRequest::new(&[], Some(&first), 10)?)?;
    assert_eq!(tail.entries.len(), 2);
    assert_eq!(tail.entries[0].key, second);
    assert_eq!(tail.entries[1].key, last);
    assert_eq!(tail.next_after, None);
    Ok(())
}

#[test]
fn retired_scope_filters_base_and_pending_before_deciding_continuation() -> Result<(), StorageError>
{
    let base = Arc::new(MemoryStorage::new());
    let scope = StorageScope::shared("retired")?;
    let namespace = Namespace::new("records")?.with_scope(scope.clone());
    base.put(
        namespace.clone(),
        &Key::new(b"a".to_vec())?,
        &Value::new(vec![1])?,
    )?;
    let overlay = PendingOverlayStorage::new(base.clone(), base);
    let mut batch = AtomicWriteBatch::from_operations(vec![AtomicWriteOperation::put(
        namespace.clone(),
        Key::new(b"b".to_vec())?,
        Value::new(vec![2])?,
    )]);
    batch.retire_scope(scope);
    drop(overlay.stage(batch)?);
    let page = overlay.scan_prefix(namespace, ScanRequest::new(&[], None, 1)?)?;
    assert!(page.entries.is_empty());
    assert_eq!(page.next_after, None);
    Ok(())
}
