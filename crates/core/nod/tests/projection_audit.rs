use alloy_primitives::{Address, B256, U256};
use outbe_compressed_entities::{
    CeAuditLimits, CeAuditWork, IdPageRequest, StoredBody, StoredBodyPage, WwdEntityId,
};
use outbe_nod::{NodBucketState, NodItemState, NodRepositoryReader, NodRepositoryWriter};
use outbe_offchain_storage::{
    AtomicWriteBatch, AtomicWriteOperation, Key, MemoryStorage, Namespace, ScanEntry, ScanPage,
    ScanRequest, StorageError, StorageMetadata, StorageReader, StorageWriter, StoredValue, Value,
    MAX_SCAN_ENTRIES,
};
use outbe_primitives::time::WorldwideDay;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

fn id(seed: u8) -> WwdEntityId {
    WwdEntityId::from_day_and_digest(WorldwideDay::new(20_260_717), B256::with_last_byte(seed).0)
}

fn ns(name: &str) -> Namespace {
    Namespace::new(name).unwrap()
}
fn key(id: WwdEntityId) -> Key {
    Key::new(id.as_slice().to_vec()).unwrap()
}
fn owner_key(owner: Address, id: WwdEntityId) -> Key {
    Key::new([owner.as_slice(), id.as_slice()].concat()).unwrap()
}

fn work() -> CeAuditWork {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "outbe-nod-projection-audit-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    CeAuditWork::create(
        path,
        CeAuditLimits {
            records_per_run: 2,
            merge_fan_in: 2,
        },
    )
    .unwrap()
}

fn fixture() -> Arc<MemoryStorage> {
    let storage = Arc::new(MemoryStorage::new());
    let writer = NodRepositoryWriter::new(storage.clone(), storage.clone());
    for seed in [3, 1, 2] {
        writer
            .put_nod(&NodItemState {
                is_settled: false,
                nod_id: id(seed),
                owner: Address::repeat_byte(seed),
                gratis_load_minor: U256::from(1),
                worldwide_day: id(seed).worldwide_day(),
                league_id: 7,
                floor_price_minor: U256::from(2),
                bucket_key: B256::repeat_byte(0x33),
                issuance_currency: 840,
                reference_currency: 978,
                issued_at: 123,
            })
            .unwrap();
        writer
            .put_bucket(&NodBucketState {
                settled_nods: 0,
                bucket_key: B256::with_last_byte(seed + 10),
                worldwide_day: id(seed).worldwide_day(),
                floor_price_minor: U256::from(1),
                is_qualified: false,
                entry_price_minor: U256::from(2),
                reference_currency: 978,
            })
            .unwrap();
    }
    storage
        .put(
            ns("protected"),
            &Key::new(vec![1]).unwrap(),
            &Value::new(b"sentinel").unwrap(),
        )
        .unwrap();
    storage
}

fn snapshot(storage: &MemoryStorage) -> Vec<ScanPage> {
    ["nods", "nod_buckets", "nods_by_owner", "protected"]
        .into_iter()
        .map(|name| {
            storage
                .scan_prefix(
                    ns(name),
                    ScanRequest::new(&[], None, MAX_SCAN_ENTRIES).unwrap(),
                )
                .unwrap()
        })
        .collect()
}

fn scan(
    reader: &NodRepositoryReader,
    bucket: bool,
    request: IdPageRequest,
) -> Result<StoredBodyPage, outbe_nod::NodRepositoryError> {
    if bucket {
        reader.scan_stored_buckets(request)
    } else {
        reader.scan_stored_items(request)
    }
}

#[test]
fn primary_scans_return_exact_bodies_and_strict_exclusive_boundaries() {
    let storage = fixture();
    let before = snapshot(&storage);
    let reader = NodRepositoryReader::new(storage.clone());
    for bucket in [false, true] {
        let offset = if bucket { 10 } else { 0 };
        let page = scan(
            &reader,
            bucket,
            IdPageRequest {
                after: None,
                limit: 2,
            },
        )
        .unwrap();
        assert_eq!(
            page.entries.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![id(1 + offset), id(2 + offset)]
        );
        assert_eq!(page.next_after, Some(id(2 + offset)));
        for (id, body) in page.entries {
            let bytes = storage
                .get(ns(if bucket { "nod_buckets" } else { "nods" }), &key(id))
                .unwrap()
                .unwrap();
            assert_eq!(body.encode(), bytes.as_bytes());
        }
        let tail = scan(
            &reader,
            bucket,
            IdPageRequest {
                after: page.next_after,
                limit: 2,
            },
        )
        .unwrap();
        assert_eq!(tail.entries[0].0, id(3 + offset));
        assert_eq!(tail.next_after, None);
        let empty = scan(
            &reader,
            bucket,
            IdPageRequest {
                after: Some(id(3 + offset)),
                limit: 1,
            },
        )
        .unwrap();
        assert!(empty.entries.is_empty());
        assert_eq!(empty.next_after, None);
        assert!(scan(
            &reader,
            bucket,
            IdPageRequest {
                after: None,
                limit: 0
            }
        )
        .is_err());
        assert!(scan(
            &reader,
            bucket,
            IdPageRequest {
                after: None,
                limit: u32::MAX
            }
        )
        .is_err());
    }
    reader.audit_indexes(&work()).unwrap();
    assert_eq!(snapshot(&storage), before);
}

#[test]
fn owner_audit_rejects_missing_dangling_wrong_owner_duplicates_and_bad_records() {
    for corruption in 0..7 {
        let storage = fixture();
        let correct = owner_key(Address::repeat_byte(1), id(1));
        let wrong = owner_key(Address::repeat_byte(9), id(1));
        let empty = Value::new([]).unwrap();
        match corruption {
            0 => storage.delete(ns("nods_by_owner"), &correct).unwrap(),
            1 => storage
                .put(
                    ns("nods_by_owner"),
                    &owner_key(Address::repeat_byte(9), id(99)),
                    &empty,
                )
                .unwrap(),
            2 => {
                storage.delete(ns("nods_by_owner"), &correct).unwrap();
                storage.put(ns("nods_by_owner"), &wrong, &empty).unwrap();
            }
            3 => storage.put(ns("nods_by_owner"), &wrong, &empty).unwrap(),
            4 => storage
                .put(ns("nods_by_owner"), &correct, &Value::new([1]).unwrap())
                .unwrap(),
            5 => {
                let metadata =
                    StorageMetadata::new([("test".to_owned(), "index-metadata".to_owned())].into())
                        .unwrap();
                storage
                    .apply_atomic(&AtomicWriteBatch::from_operations(vec![
                        AtomicWriteOperation::put_record(
                            ns("nods_by_owner"),
                            correct,
                            StoredValue::with_metadata(empty, metadata),
                        ),
                    ]))
                    .unwrap();
            }
            6 => storage
                .put(ns("nods_by_owner"), &Key::new(vec![1]).unwrap(), &empty)
                .unwrap(),
            _ => unreachable!(),
        }
        let before = snapshot(&storage);
        let reader = NodRepositoryReader::new(storage.clone());
        // Primary enumeration must still expose all bodies despite damaged indexes.
        assert_eq!(
            reader
                .scan_stored_items(IdPageRequest {
                    after: None,
                    limit: 10
                })
                .unwrap()
                .entries
                .len(),
            3
        );
        assert!(
            reader.audit_indexes(&work()).is_err(),
            "accepted corruption {corruption}"
        );
        assert_eq!(snapshot(&storage), before);
    }
}

#[test]
fn primary_scans_reject_malformed_keys_bodies_id_mismatches_and_schema() {
    for bucket in [false, true] {
        for corruption in 0..4 {
            let storage = fixture();
            let namespace = ns(if bucket { "nod_buckets" } else { "nods" });
            let selected = id(if bucket { 11 } else { 1 });
            let original = storage
                .get(namespace.clone(), &key(selected))
                .unwrap()
                .unwrap();
            match corruption {
                0 => storage
                    .put(namespace.clone(), &Key::new(vec![1]).unwrap(), &original)
                    .unwrap(),
                1 => storage
                    .put(
                        namespace.clone(),
                        &key(selected),
                        &Value::new([0xff]).unwrap(),
                    )
                    .unwrap(),
                2 => {
                    let other = storage
                        .get(namespace.clone(), &key(id(if bucket { 12 } else { 2 })))
                        .unwrap()
                        .unwrap();
                    storage
                        .put(namespace.clone(), &key(selected), &other)
                        .unwrap();
                }
                3 => {
                    let stored = StoredBody::decode(original.as_bytes()).unwrap();
                    let value = Value::new(
                        StoredBody::new(2, stored.payload().to_vec())
                            .unwrap()
                            .encode(),
                    )
                    .unwrap();
                    storage
                        .put(namespace.clone(), &key(selected), &value)
                        .unwrap();
                }
                _ => unreachable!(),
            }
            let before = snapshot(&storage);
            let reader = NodRepositoryReader::new(storage.clone());
            assert!(scan(
                &reader,
                bucket,
                IdPageRequest {
                    after: None,
                    limit: 10
                }
            )
            .is_err());
            assert_eq!(snapshot(&storage), before);
        }
    }
}

#[derive(Clone, Copy)]
enum PageMode {
    Short,
    Descending,
    Duplicate,
    WrongContinuation,
    EmptyContinuation,
    Oversized,
    RepeatCursor,
    LateError,
}

struct PageReader {
    storage: Arc<MemoryStorage>,
    mode: PageMode,
}

impl StorageReader for PageReader {
    fn get_record(
        &self,
        namespace: Namespace,
        key: &Key,
    ) -> Result<Option<StoredValue>, StorageError> {
        self.storage.get_record(namespace, key)
    }

    fn scan_prefix(
        &self,
        namespace: Namespace,
        request: ScanRequest<'_>,
    ) -> Result<ScanPage, StorageError> {
        if matches!(self.mode, PageMode::LateError) && request.after().is_some() {
            return Err(StorageError::Corruption("interrupted later page".into()));
        }
        let limit = if matches!(self.mode, PageMode::Short | PageMode::LateError) {
            1
        } else {
            request.limit()
        };
        let mut page = self.storage.scan_prefix(
            namespace.clone(),
            ScanRequest::new(request.prefix(), request.after(), limit)?,
        )?;
        match self.mode {
            PageMode::Short | PageMode::LateError => {}
            PageMode::Descending => page.entries.reverse(),
            PageMode::Duplicate => {
                if let Some(first) = page.entries.first().cloned() {
                    page.entries.insert(0, first);
                }
            }
            PageMode::WrongContinuation => page.next_after = Some(Key::new(vec![0xff])?),
            PageMode::EmptyContinuation => {
                page.entries.clear();
                page.next_after = Some(Key::new(vec![0xff])?);
            }
            PageMode::Oversized => {
                if let Some(first) = page.entries.first().cloned() {
                    page.entries.resize(request.limit() + 1, first);
                }
                page.next_after = None;
            }
            PageMode::RepeatCursor => {
                if let Some(after) = request.after() {
                    if let Some(record) = self.storage.get_record(namespace, after)? {
                        page.entries.insert(
                            0,
                            ScanEntry {
                                key: after.clone(),
                                value: record.value,
                                metadata: record.metadata,
                            },
                        );
                    }
                }
            }
        }
        Ok(page)
    }
}

#[test]
fn primary_scans_reject_invalid_backend_page_order_size_and_continuations() {
    for mode in [
        PageMode::Descending,
        PageMode::Duplicate,
        PageMode::WrongContinuation,
        PageMode::EmptyContinuation,
        PageMode::Oversized,
        PageMode::RepeatCursor,
    ] {
        let storage = fixture();
        let before = snapshot(&storage);
        let reader = NodRepositoryReader::new(Arc::new(PageReader {
            storage: storage.clone(),
            mode,
        }));
        for bucket in [false, true] {
            assert!(scan(
                &reader,
                bucket,
                IdPageRequest {
                    after: Some(id(if bucket { 11 } else { 1 })),
                    limit: 2
                }
            )
            .is_err());
        }
        assert_eq!(snapshot(&storage), before);
    }
}

#[test]
fn audits_follow_short_pages_and_propagate_later_page_failures() {
    let storage = fixture();
    let before = snapshot(&storage);
    let reader = NodRepositoryReader::new(Arc::new(PageReader {
        storage: storage.clone(),
        mode: PageMode::Short,
    }));
    for bucket in [false, true] {
        let mut after = None;
        let mut ids = Vec::new();
        loop {
            let page = scan(&reader, bucket, IdPageRequest { after, limit: 10 }).unwrap();
            assert_eq!(page.entries.len(), 1);
            ids.push(page.entries[0].0);
            after = page.next_after;
            if after.is_none() {
                break;
            }
        }
        assert_eq!(
            ids,
            if bucket {
                vec![id(11), id(12), id(13)]
            } else {
                vec![id(1), id(2), id(3)]
            }
        );
    }
    reader.audit_indexes(&work()).unwrap();
    let interrupted = NodRepositoryReader::new(Arc::new(PageReader {
        storage: storage.clone(),
        mode: PageMode::LateError,
    }));
    assert!(interrupted
        .audit_indexes(&work())
        .unwrap_err()
        .to_string()
        .contains("interrupted later page"));
    assert_eq!(snapshot(&storage), before);
}
