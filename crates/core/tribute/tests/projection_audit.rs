use std::sync::Arc;

use alloy_primitives::{Address, B256, U256};
use outbe_compressed_entities::{
    decode_stored_tribute_v1, CeAuditError, CeAuditLimits, CeAuditWork, IdPageRequest, WwdEntityId,
};
use outbe_offchain_storage::{
    AtomicWriteBatch, AtomicWriteOperation, Key, MemoryStorage, Namespace, ScanEntry, ScanPage,
    ScanRequest, StorageError, StorageMetadata, StorageReader, StorageWriter, StoredValue, Value,
    MAX_SCAN_ENTRIES,
};
use outbe_primitives::time::WorldwideDay;
use outbe_tribute::{
    RetainedTributeAuditEntry, RetainedTributeAuditVisitor, RetainedTributePin,
    RetainedTributeReader, TributeData, TributeRepositoryReader, TributeRepositoryWriter,
    OCOMP_RETAINED_TRIBUTES_BY_DAY_NAMESPACE as RETAINED_INDEX,
    OCOMP_RETAINED_TRIBUTES_NAMESPACE as RETAINED,
};

fn ns(name: &str) -> Namespace {
    Namespace::new(name).unwrap()
}

fn body(seed: u64) -> TributeData {
    let day = WorldwideDay::new(20260901 + (seed % 2) as u32);
    TributeData {
        tribute_id: WwdEntityId::from_day_and_digest(day, U256::from(seed).to_be_bytes::<32>()),
        owner: Address::repeat_byte(seed as u8),
        worldwide_day: day,
        issuance_amount_minor: U256::from(seed),
        issuance_currency: 1,
        nominal_amount_minor: U256::from(2),
        reference_currency: 1,
        tribute_price_minor: U256::from(3),
        exclude_from_intex_issuance: false,
    }
}

fn fixture() -> (
    Arc<MemoryStorage>,
    TributeRepositoryReader,
    Vec<TributeData>,
) {
    let storage = Arc::new(MemoryStorage::new());
    let writer = TributeRepositoryWriter::new(storage.clone(), storage.clone());
    let mut bodies: Vec<_> = (1..=4).map(body).collect();
    bodies.sort_by_key(|body| body.tribute_id);
    for body in &bodies {
        writer.put(body).unwrap();
    }
    let reader = TributeRepositoryReader::new(Arc::new(SmallPages(storage.clone())));
    (storage, reader, bodies)
}

/// Exercises short storage pages even when the caller asks for a larger page.
struct SmallPages(Arc<MemoryStorage>);
impl StorageReader for SmallPages {
    fn get_record(
        &self,
        namespace: Namespace,
        key: &Key,
    ) -> Result<Option<StoredValue>, StorageError> {
        self.0.get_record(namespace, key)
    }
    fn scan_prefix(
        &self,
        namespace: Namespace,
        request: ScanRequest<'_>,
    ) -> Result<ScanPage, StorageError> {
        self.0.scan_prefix(
            namespace,
            ScanRequest::new(request.prefix(), request.after(), request.limit().min(2))?,
        )
    }
}

fn work() -> (tempfile::TempDir, CeAuditWork) {
    let dir = tempfile::tempdir().unwrap();
    let work = CeAuditWork::create(
        dir.path().join("audit"),
        CeAuditLimits {
            records_per_run: 2,
            merge_fan_in: 2,
        },
    )
    .unwrap();
    (dir, work)
}

fn rows(storage: &MemoryStorage, name: &str) -> Vec<ScanEntry> {
    storage
        .scan_prefix(ns(name), ScanRequest::new(&[], None, 100).unwrap())
        .unwrap()
        .entries
}

fn snapshot(storage: &MemoryStorage) -> Vec<Vec<ScanEntry>> {
    [
        "tributes",
        "tributes_by_owner",
        "tributes_by_day",
        RETAINED,
        RETAINED_INDEX,
    ]
    .map(|name| rows(storage, name))
    .into()
}

#[test]
fn primary_pages_are_exclusive_canonical_and_independent_of_indexes() {
    let (storage, reader, bodies) = fixture();
    for entry in rows(&storage, "tributes_by_owner") {
        storage.delete(ns("tributes_by_owner"), &entry.key).unwrap();
    }
    let first = reader
        .scan_stored_bodies(IdPageRequest {
            after: None,
            limit: 2,
        })
        .unwrap();
    assert_eq!(first.entries.len(), 2);
    assert_eq!(first.next_after, Some(bodies[1].tribute_id));
    let second = reader
        .scan_stored_bodies(IdPageRequest {
            after: first.next_after,
            limit: 2,
        })
        .unwrap();
    assert_eq!(second.entries.len(), 2);
    assert_eq!(second.next_after, None);
    let all: Vec<_> = first.entries.into_iter().chain(second.entries).collect();
    assert_eq!(
        all.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        bodies
            .iter()
            .map(|body| body.tribute_id)
            .collect::<Vec<_>>()
    );
    for (id, stored) in all {
        assert_eq!(
            decode_stored_tribute_v1(&stored.encode())
                .unwrap()
                .tribute_id,
            id
        );
        assert_eq!(reader.get_stored_body(id).unwrap().unwrap(), stored);
    }
    assert!(reader
        .scan_stored_bodies(IdPageRequest {
            after: Some(bodies[3].tribute_id),
            limit: 2
        })
        .unwrap()
        .entries
        .is_empty());
}

#[test]
fn complete_indexes_pass_across_short_pages_and_spill_runs_without_writes() {
    let (storage, reader, _) = fixture();
    let before = snapshot(&storage);
    let (_dir, work) = work();
    reader.audit_indexes(&work).unwrap();
    assert_eq!(snapshot(&storage), before);
}

#[test]
fn missing_dangling_and_wrong_selector_indexes_fail() {
    for name in ["tributes_by_owner", "tributes_by_day"] {
        for mutation in ["missing", "dangling", "wrong", "nonempty", "metadata"] {
            let (storage, reader, _) = fixture();
            let row = rows(&storage, name).remove(0);
            match mutation {
                "missing" => storage.delete(ns(name), &row.key).unwrap(),
                "nonempty" => storage
                    .put(ns(name), &row.key, &Value::new(vec![1]).unwrap())
                    .unwrap(),
                "metadata" => add_metadata(&storage, name, row),
                _ => {
                    let mut key = row.key.as_bytes().to_vec();
                    if mutation == "dangling" {
                        *key.last_mut().unwrap() ^= 0x80;
                    } else {
                        key[0] ^= 0x80;
                        storage.delete(ns(name), &row.key).unwrap();
                    }
                    storage
                        .put(ns(name), &Key::new(key).unwrap(), &row.value)
                        .unwrap();
                }
            }
            let before = snapshot(&storage);
            let (_dir, work) = work();
            assert!(reader.audit_indexes(&work).is_err(), "{name}/{mutation}");
            assert_eq!(snapshot(&storage), before);
        }
    }
}

#[test]
fn primary_scan_rejects_invalid_limits_keys_and_bodies() {
    let (_, reader, _) = fixture();
    for limit in [0, u32::MAX] {
        assert!(reader
            .scan_stored_bodies(IdPageRequest { after: None, limit })
            .is_err());
    }
    for malformed_key in [false, true] {
        let storage = Arc::new(MemoryStorage::new());
        let key = Key::new(if malformed_key {
            vec![1]
        } else {
            body(1).tribute_id.to_vec()
        })
        .unwrap();
        storage
            .put(ns("tributes"), &key, &Value::new(vec![0]).unwrap())
            .unwrap();
        let reader = TributeRepositoryReader::new(storage);
        assert!(reader
            .scan_stored_bodies(IdPageRequest {
                after: None,
                limit: 2
            })
            .is_err());
        let (_dir, work) = work();
        assert!(reader.audit_indexes(&work).is_err());
    }
}

#[derive(Default)]
struct Collected(Vec<RetainedTributeAuditEntry>);
impl RetainedTributeAuditVisitor for Collected {
    fn visit_retained(&mut self, entry: RetainedTributeAuditEntry) -> Result<(), CeAuditError> {
        self.0.push(entry);
        Ok(())
    }
}

fn retained_fixture() -> (Arc<MemoryStorage>, RetainedTributeReader) {
    let (storage, _, bodies) = fixture();
    let reader = RetainedTributeReader::new(Arc::new(SmallPages(storage.clone())));
    for (index, body) in bodies.iter().enumerate() {
        let pin = RetainedTributePin {
            input_lease_id: B256::repeat_byte(index as u8 + 1),
            worldwide_day: body.worldwide_day,
        };
        storage
            .apply_atomic(&reader.plan_retain_current(pin, body.tribute_id).unwrap())
            .unwrap();
    }
    (storage, reader)
}

#[test]
fn retained_audit_visits_all_leases_and_days_separately_from_live_bodies() {
    let (storage, reader) = retained_fixture();
    for row in rows(&storage, "tributes") {
        storage.delete(ns("tributes"), &row.key).unwrap();
    }
    let before = snapshot(&storage);
    let (_dir, work) = work();
    let mut visitor = Collected::default();
    reader.audit_retained(&work, &mut visitor).unwrap();
    assert_eq!(visitor.0.len(), 4);
    for entry in visitor.0 {
        assert_eq!(
            entry.pin.worldwide_day,
            entry.reference.tribute_id.worldwide_day()
        );
        assert_eq!(
            decode_stored_tribute_v1(&entry.stored_body.encode())
                .unwrap()
                .tribute_id,
            entry.reference.tribute_id
        );
        let key = Key::new(
            [
                entry.pin.input_lease_id.as_slice(),
                &entry.pin.worldwide_day.value().to_be_bytes(),
                entry.reference.tribute_id.as_slice(),
                entry.reference.body_commitment.as_slice(),
            ]
            .concat(),
        )
        .unwrap();
        assert_eq!(
            storage.get(ns(RETAINED), &key).unwrap().unwrap().as_bytes(),
            entry.stored_body.encode()
        );
    }
    assert_eq!(snapshot(&storage), before);
}

#[test]
fn retained_missing_or_dangling_rows_and_changed_selectors_fail() {
    for namespace in [RETAINED, RETAINED_INDEX] {
        for mutation in ["missing", "lease", "day", "commitment", "value"] {
            let (storage, reader) = retained_fixture();
            let row = rows(&storage, namespace).remove(0);
            storage.delete(ns(namespace), &row.key).unwrap();
            if mutation != "missing" {
                let mut key = row.key.as_bytes().to_vec();
                match mutation {
                    "lease" => key[0] ^= 1,
                    "day" => key[32] ^= 1,
                    "commitment" => key[99] ^= 1,
                    _ => {}
                }
                let value = if mutation == "value" {
                    Value::new(vec![1]).unwrap()
                } else {
                    row.value
                };
                storage
                    .put(ns(namespace), &Key::new(key).unwrap(), &value)
                    .unwrap();
            }
            let before = snapshot(&storage);
            let (_dir, work) = work();
            assert!(
                reader
                    .audit_retained(&work, &mut Collected::default())
                    .is_err(),
                "{namespace}/{mutation}"
            );
            assert_eq!(snapshot(&storage), before);
        }
    }
}

#[test]
fn retained_empty_and_partial_gc_residuals_are_structurally_valid() {
    let (storage, reader) = retained_fixture();
    for row in rows(&storage, RETAINED) {
        storage.delete(ns(RETAINED), &row.key).unwrap();
        storage.delete(ns(RETAINED_INDEX), &row.key).unwrap();
        let (_dir, work) = work();
        let mut visitor = Collected::default();
        reader.audit_retained(&work, &mut visitor).unwrap();
        assert_eq!(visitor.0.len(), rows(&storage, RETAINED).len());
    }
}

#[test]
fn retained_callback_error_propagates_without_mutating_source() {
    struct Stop;
    impl RetainedTributeAuditVisitor for Stop {
        fn visit_retained(&mut self, _: RetainedTributeAuditEntry) -> Result<(), CeAuditError> {
            Err(CeAuditError::Invalid("visitor stopped".into()))
        }
    }
    let (storage, reader) = retained_fixture();
    let before = snapshot(&storage);
    let (_dir, work) = work();
    let error = reader.audit_retained(&work, &mut Stop).unwrap_err();
    assert!(error.to_string().contains("visitor stopped"));
    assert_eq!(snapshot(&storage), before);
}

fn add_metadata(storage: &MemoryStorage, name: &str, row: ScanEntry) {
    let metadata = StorageMetadata::new([("block".into(), "17".into())].into()).unwrap();
    storage
        .apply_atomic(&AtomicWriteBatch::from_operations(vec![
            AtomicWriteOperation::put_record(
                ns(name),
                row.key,
                StoredValue::with_metadata(row.value, metadata),
            ),
        ]))
        .unwrap();
}

struct BrokenPages {
    entries: Vec<ScanEntry>,
    fault: &'static str,
}

impl StorageReader for BrokenPages {
    fn get_record(&self, _: Namespace, _: &Key) -> Result<Option<StoredValue>, StorageError> {
        unreachable!("audit must scan complete primary populations")
    }

    fn scan_prefix(
        &self,
        _: Namespace,
        request: ScanRequest<'_>,
    ) -> Result<ScanPage, StorageError> {
        let mut entries = self.entries[..2].to_vec();
        let mut next_after = None;
        match self.fault {
            "nonascending" => entries.reverse(),
            "nonlast" => next_after = Some(entries[0].key.clone()),
            "empty" => {
                next_after = Some(entries[0].key.clone());
                entries.clear();
            }
            "oversized" => entries = vec![entries[0].clone(); MAX_SCAN_ENTRIES + 1],
            "later-error" | "replayed-cursor" => {
                if request.after().is_some() && self.fault == "later-error" {
                    return Err(StorageError::RequestDeadline);
                }
                next_after = Some(entries[1].key.clone());
            }
            _ => unreachable!(),
        }
        Ok(ScanPage {
            entries,
            next_after,
        })
    }
}

#[test]
fn scans_and_index_audits_reject_broken_pages_and_later_storage_errors() {
    let (storage, _, _) = fixture();
    for fault in [
        "nonascending",
        "nonlast",
        "empty",
        "oversized",
        "later-error",
        "replayed-cursor",
    ] {
        let reader = TributeRepositoryReader::new(Arc::new(BrokenPages {
            entries: rows(&storage, "tributes"),
            fault,
        }));
        let first = reader.scan_stored_bodies(IdPageRequest {
            after: None,
            limit: 2,
        });
        if matches!(fault, "later-error" | "replayed-cursor") {
            let first = first.unwrap();
            let error = reader
                .scan_stored_bodies(IdPageRequest {
                    after: first.next_after,
                    limit: 2,
                })
                .unwrap_err();
            if fault == "later-error" {
                assert!(error.to_string().contains("deadline"));
            }
        } else {
            assert!(first.is_err(), "{fault}");
        }
        let (_dir, work) = work();
        let error = reader.audit_indexes(&work).unwrap_err();
        if fault == "later-error" {
            assert!(error.to_string().contains("deadline"));
        }
    }
}

#[test]
fn retained_conflicting_valid_commitments_fail_across_spill_runs() {
    let (storage, reader) = retained_fixture();
    let first = rows(&storage, RETAINED).remove(0);
    let pin = RetainedTributePin {
        input_lease_id: B256::from_slice(&first.key.as_bytes()[..32]),
        worldwide_day: body(2).worldwide_day,
    };
    let mut changed = body(2);
    changed.issuance_amount_minor += U256::from(1);
    let other = Arc::new(MemoryStorage::new());
    TributeRepositoryWriter::new(other.clone(), other.clone())
        .put(&changed)
        .unwrap();
    let other_reader = RetainedTributeReader::new(other);
    storage
        .apply_atomic(
            &other_reader
                .plan_retain_current(pin, changed.tribute_id)
                .unwrap(),
        )
        .unwrap();
    assert_eq!(rows(&storage, RETAINED).len(), 5);
    // Both bodies have valid native commitments; uniqueness is the failing invariant.
    let dir = tempfile::tempdir().unwrap();
    let work = CeAuditWork::create(
        dir.path().join("audit"),
        CeAuditLimits {
            records_per_run: 1,
            merge_fan_in: 2,
        },
    )
    .unwrap();
    let error = reader
        .audit_retained(&work, &mut Collected::default())
        .unwrap_err();
    assert!(error.to_string().contains("duplicate"), "{error}");
}

#[test]
fn retained_malformed_keys_fail_and_metadata_follows_native_policy() {
    for name in [RETAINED, RETAINED_INDEX] {
        let (storage, reader) = retained_fixture();
        let row = rows(&storage, name).remove(0);
        storage.delete(ns(name), &row.key).unwrap();
        storage
            .put(
                ns(name),
                &Key::new(row.key.as_bytes()[..99].to_vec()).unwrap(),
                &row.value,
            )
            .unwrap();
        let (_dir, work) = work();
        assert!(reader
            .audit_retained(&work, &mut Collected::default())
            .is_err());

        let (storage, reader) = retained_fixture();
        add_metadata(&storage, name, rows(&storage, name).remove(0));
        let before = snapshot(&storage);
        let (_dir, work) = self::work();
        let result = reader.audit_retained(&work, &mut Collected::default());
        if name == RETAINED {
            assert!(result.is_err());
        } else {
            result.unwrap();
        }
        assert_eq!(snapshot(&storage), before);
    }
}
