#[path = "../../../../testing/fixtures/storage_failures.rs"]
mod storage_failures;
use outbe_offchain_data::{
    runtime_body_readers, supervised_runtime_body_readers, RuntimeBodyReaders,
};
use std::{sync::Arc, time::Duration};
use storage_failures::FailingStorageReader;

use alloy_primitives::{Address, B256, U256};
use outbe_compressed_entities::{
    decode_stored_nod_bucket_v1, decode_stored_nod_item_v2, decode_stored_tribute_v1, EntityRef,
    IdPageRequest, ParentBodySource, ParentBodySourceError, QueryRef, WwdEntityId,
};
use outbe_nod::{NodBucketState, NodItemState};
use outbe_offchain_storage::{
    Key, MemoryStorage, Namespace, ScanEntry, ScanPage, ScanRequest, StorageError,
    StorageErrorKind, StorageReader, StorageReaderHandle, StorageWriter, StorageWriterHandle,
    StoredValue, Value,
};
use outbe_primitives::time::WorldwideDay;
use outbe_tribute::{TributeData, TributeReadView, TributeRepositoryWriter};
use tracing_subscriber::{layer::SubscriberExt, Layer};

fn entity(seed: u64) -> WwdEntityId {
    WwdEntityId::from_day_and_digest(
        WorldwideDay::new(20_260_715),
        U256::from(seed).to_be_bytes::<32>(),
    )
}

fn tribute(tribute_id: WwdEntityId) -> TributeData {
    TributeData {
        tribute_id,
        owner: Address::repeat_byte(0x11),
        worldwide_day: WorldwideDay::new(20_260_715),
        issuance_amount_minor: U256::from(100),
        issuance_currency: 840,
        nominal_amount_minor: U256::from(90),
        reference_currency: 978,
        tribute_price_minor: U256::from(3),
        exclude_from_intex_issuance: true,
    }
}

#[test]
fn supervised_body_mismatch_does_not_publish_a_node_failure() {
    let storage = Arc::new(MemoryStorage::new());
    let (failure_tx, failure_rx) = tokio::sync::watch::channel(None);
    let readers = supervised_runtime_body_readers(storage, failure_tx);
    readers.report_precompile_error(
        &outbe_primitives::error::PrecompileError::BodyReadCorruption(
            "body commitment mismatch".into(),
        ),
    );
    assert!(failure_rx.borrow().is_none());
}

#[test]
fn supervised_bundle_reports_read_failures_to_its_lifecycle_owner() {
    let storage = Arc::new(MemoryStorage::new());
    let (failure_tx, failure_rx) = tokio::sync::watch::channel(None);
    let readers = supervised_runtime_body_readers(storage, failure_tx);

    readers.report_precompile_error(
        &outbe_primitives::error::PrecompileError::BodyReadUnavailable("replica election".into()),
    );
    readers.report_fatal(
        outbe_primitives::projection::ProjectionFailureClass::CorruptBody,
        "invalid body identity",
    );

    assert!(matches!(
        failure_rx.borrow().clone(),
        Some(outbe_offchain_data::RuntimeBodyFailure::Fatal(_))
    ));

    readers.report_precompile_error(
        &outbe_primitives::error::PrecompileError::BodyReadUnavailable(
            "later replica election".into(),
        ),
    );
    assert!(matches!(
        failure_rx.borrow().clone(),
        Some(outbe_offchain_data::RuntimeBodyFailure::Fatal(_))
    ));

    readers.report_precompile_error(&outbe_primitives::error::PrecompileError::Revert(
        "ordinary domain absence".into(),
    ));
    assert!(matches!(
        failure_rx.borrow().clone(),
        Some(outbe_offchain_data::RuntimeBodyFailure::Fatal(_))
    ));
}

fn nod(nod_id: WwdEntityId, bucket_key: B256) -> NodItemState {
    outbe_nod::test_support::item(
        outbe_nod::test_support::NodItemFixture {
            is_settled: false,
            nod_id,
            owner: Address::repeat_byte(0x22),
            gratis_load_minor: U256::from(55),
            worldwide_day: WorldwideDay::new(20_260_715),
            league_id: 7,
            bucket_key,
            issuance_currency: 840,
            reference_currency: 978,
            issued_at: 1_752_534_000,
        },
        U256::from(5),
    )
}

fn bucket(bucket_key: B256) -> NodBucketState {
    NodBucketState {
        settled_nods: 0,
        bucket_key,
        worldwide_day: WorldwideDay::new(20_260_715),
        entry_price_minor: U256::from(5),
        reference_currency: 978,
    }
}

/// One memory adapter: its reader, its writer and the runtime readers on it.
fn memory_runtime() -> (StorageReaderHandle, StorageWriterHandle, RuntimeBodyReaders) {
    let storage = Arc::new(MemoryStorage::new());
    let reader: StorageReaderHandle = storage.clone();
    let writer: StorageWriterHandle = storage;
    let readers = runtime_body_readers(reader.clone());
    (reader, writer, readers)
}

#[test]
fn typed_readers_share_one_memory_adapter() {
    let (reader, writer, readers) = memory_runtime();

    let tribute_id = entity(1);
    let nod_id = entity(2);
    let bucket_key = B256::repeat_byte(0x33);
    let bucket_id = WwdEntityId::from_day_and_digest(WorldwideDay::new(20_260_715), bucket_key.0);

    TributeRepositoryWriter::new(reader.clone(), writer.clone())
        .put(&tribute(tribute_id))
        .unwrap();
    let nod_writer = outbe_nod::nod_writer(reader, writer);
    nod_writer.put_nod(&nod(nod_id, bucket_key)).unwrap();
    nod_writer.put_bucket(&bucket(bucket_key)).unwrap();

    let stored_tribute = TributeReadView::get(readers.tribute(), tribute_id)
        .unwrap()
        .unwrap();
    assert_eq!(stored_tribute.owner, Address::repeat_byte(0x11));

    let stored_nod = readers.nod().get(nod_id).unwrap().unwrap();
    assert_eq!(stored_nod.bucket_key, bucket_key);

    let stored_bucket = readers.nod().get_bucket(bucket_id).unwrap().unwrap();
    assert_eq!(stored_bucket.entry_price_minor, U256::from(5));
}

#[test]
fn parent_body_source_gets_exact_bodies_and_lists_strict_id_pages() {
    let (reader, writer, readers) = memory_runtime();
    let tribute_owner = Address::repeat_byte(0x11);
    let nod_owner = Address::repeat_byte(0x22);
    let tribute_ids = [entity(1), entity(2), entity(3)];
    let nod_ids = [entity(11), entity(12), entity(13)];
    let bucket_key = B256::repeat_byte(0x33);
    let bucket_id = WwdEntityId::from_day_and_digest(WorldwideDay::new(20_260_715), bucket_key.0);

    let tribute_writer = TributeRepositoryWriter::new(reader.clone(), writer.clone());
    let nod_writer = outbe_nod::nod_writer(reader, writer);
    for id in [tribute_ids[2], tribute_ids[0], tribute_ids[1]] {
        tribute_writer.put(&tribute(id)).unwrap();
    }
    for id in [nod_ids[2], nod_ids[0], nod_ids[1]] {
        nod_writer.put_nod(&nod(id, bucket_key)).unwrap();
    }
    nod_writer.put_bucket(&bucket(bucket_key)).unwrap();

    assert_exact_bodies(&readers, tribute_ids[0], nod_ids[0], bucket_id);

    for (query, expected) in [
        (QueryRef::TributeByOwner(tribute_owner), &tribute_ids[..]),
        (
            QueryRef::TributeByDay(WorldwideDay::new(20_260_715)),
            &tribute_ids[..],
        ),
        (QueryRef::NodByOwner(nod_owner), &nod_ids[..]),
        (QueryRef::NodAll, &nod_ids[..]),
    ] {
        assert_strict_id_pages(&readers, query, expected);
    }
}

fn assert_exact_bodies(
    readers: &impl ParentBodySource,
    tribute_id: WwdEntityId,
    nod_id: WwdEntityId,
    bucket_id: WwdEntityId,
) {
    let stored_tribute = ParentBodySource::get(readers, EntityRef::Tribute(tribute_id))
        .unwrap()
        .unwrap();
    assert_eq!(
        decode_stored_tribute_v1(&stored_tribute.encode())
            .unwrap()
            .tribute_id,
        tribute_id
    );
    let stored_nod = ParentBodySource::get(readers, EntityRef::NodItem(nod_id))
        .unwrap()
        .unwrap();
    assert_eq!(
        decode_stored_nod_item_v2(&stored_nod.encode())
            .unwrap()
            .encrypted
            .terms
            .nod_id,
        nod_id
    );
    let stored_bucket = ParentBodySource::get(readers, EntityRef::NodBucket(bucket_id))
        .unwrap()
        .unwrap();
    assert_eq!(
        decode_stored_nod_bucket_v1(&stored_bucket.encode())
            .unwrap()
            .entity_id(),
        bucket_id
    );
    assert!(
        ParentBodySource::get(readers, EntityRef::Tribute(entity(99)))
            .unwrap()
            .is_none()
    );
}

fn assert_strict_id_pages(
    readers: &impl ParentBodySource,
    query: QueryRef,
    expected: &[WwdEntityId],
) {
    let first = ParentBodySource::list(
        readers,
        query,
        IdPageRequest {
            after: None,
            limit: 2,
        },
    )
    .unwrap();
    assert_eq!(first.ids, expected[..2]);
    assert_eq!(first.next_after, Some(expected[1]));
    let second = ParentBodySource::list(
        readers,
        query,
        IdPageRequest {
            after: first.next_after,
            limit: 2,
        },
    )
    .unwrap();
    assert_eq!(second.ids, expected[2..]);
    assert_eq!(second.next_after, None);
}

#[derive(Clone)]
struct ScriptedScanReader {
    page: ScanPage,
}

impl StorageReader for ScriptedScanReader {
    fn get_record(
        &self,
        _namespace: Namespace,
        _key: &Key,
    ) -> Result<Option<StoredValue>, StorageError> {
        Ok(None)
    }

    fn scan_prefix(
        &self,
        _namespace: Namespace,
        _request: ScanRequest<'_>,
    ) -> Result<ScanPage, StorageError> {
        Ok(self.page.clone())
    }
}

fn scan_entry(id: WwdEntityId) -> ScanEntry {
    ScanEntry {
        key: Key::new(id.as_slice().to_vec()).unwrap(),
        value: Value::new(Vec::new()).unwrap(),
        metadata: None,
    }
}

#[test]
fn parent_body_source_classifies_backend_absence_and_canonical_failures() {
    let unavailable = runtime_body_readers(Arc::new(FailingStorageReader(|| {
        StorageError::Unavailable {
            source: Box::new(std::io::Error::other("replica election")),
        }
    })));
    assert!(matches!(
        ParentBodySource::get(&unavailable, EntityRef::Tribute(entity(1))),
        Err(ParentBodySourceError::Unavailable(_))
    ));
    assert!(matches!(
        ParentBodySource::list(
            &unavailable,
            QueryRef::NodAll,
            IdPageRequest {
                after: None,
                limit: 1,
            },
        ),
        Err(ParentBodySourceError::Unavailable(_))
    ));

    let corrupt_storage = Arc::new(MemoryStorage::new());
    corrupt_storage
        .put(
            Namespace::new("tributes").unwrap(),
            &Key::new(entity(1).as_slice().to_vec()).unwrap(),
            &Value::new([0xff]).unwrap(),
        )
        .unwrap();
    let corrupt = runtime_body_readers(corrupt_storage);
    assert!(matches!(
        ParentBodySource::get(&corrupt, EntityRef::Tribute(entity(1))),
        Err(ParentBodySourceError::Corruption(_))
    ));
    assert!(matches!(
        ParentBodySource::list(
            &corrupt,
            QueryRef::NodAll,
            IdPageRequest {
                after: None,
                limit: 0,
            },
        ),
        Err(ParentBodySourceError::Corruption(_))
    ));

    let descending = runtime_body_readers(Arc::new(ScriptedScanReader {
        page: ScanPage {
            entries: vec![scan_entry(entity(2)), scan_entry(entity(1))],
            next_after: None,
        },
    }));
    assert!(matches!(
        ParentBodySource::list(
            &descending,
            QueryRef::NodAll,
            IdPageRequest {
                after: None,
                limit: 2,
            },
        ),
        Err(ParentBodySourceError::Corruption(_))
    ));

    let invalid_continuation = runtime_body_readers(Arc::new(ScriptedScanReader {
        page: ScanPage {
            entries: vec![scan_entry(entity(1))],
            next_after: Some(Key::new(entity(2).as_slice().to_vec()).unwrap()),
        },
    }));
    assert!(matches!(
        ParentBodySource::list(
            &invalid_continuation,
            QueryRef::NodAll,
            IdPageRequest {
                after: None,
                limit: 1,
            },
        ),
        Err(ParentBodySourceError::Corruption(_))
    ));
}

#[test]
fn cloned_bundle_observes_later_writes_through_typed_readers() {
    let (reader, writer, readers) = memory_runtime();
    let cloned = readers.clone();
    let tribute_id = entity(9);

    assert!(TributeReadView::get(cloned.tribute(), tribute_id)
        .unwrap()
        .is_none());

    TributeRepositoryWriter::new(reader, writer)
        .put(&tribute(tribute_id))
        .unwrap();

    assert_eq!(
        TributeReadView::get(cloned.tribute(), tribute_id)
            .unwrap()
            .unwrap()
            .tribute_id,
        tribute_id
    );
}

struct DelayedReader {
    inner: MemoryStorage,
    delay: Duration,
}

impl StorageReader for DelayedReader {
    fn get_record(
        &self,
        namespace: Namespace,
        key: &Key,
    ) -> Result<Option<StoredValue>, StorageError> {
        std::thread::sleep(self.delay);
        self.inner.get_record(namespace, key)
    }

    fn scan_prefix(
        &self,
        namespace: Namespace,
        request: ScanRequest<'_>,
    ) -> Result<ScanPage, StorageError> {
        std::thread::sleep(self.delay);
        self.inner.scan_prefix(namespace, request)
    }
}

#[test]
fn execution_read_uses_remaining_request_budget_without_reporting_mongo_outage() {
    let storage: StorageReaderHandle = Arc::new(DelayedReader {
        inner: MemoryStorage::new(),
        delay: Duration::from_millis(200),
    });
    let (failure_tx, failure_rx) = tokio::sync::watch::channel(None);
    let readers = supervised_runtime_body_readers(storage, failure_tx);
    let request_budget = outbe_primitives::projection::ExecutionReadBudget::new();
    let _budget = readers.enter_execution_budget(request_budget.clone());
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(25));
        request_budget.cancel();
    });

    let started = std::time::Instant::now();
    let capture = DiagnosticCapture::default();
    let error = match tracing::subscriber::with_default(capture.subscriber(), || {
        TributeReadView::get(readers.tribute(), entity(1))
    }) {
        Ok(_) => panic!("delayed read must exceed the request budget"),
        Err(error) => error,
    };
    assert!(started.elapsed() < Duration::from_millis(150));
    assert!(matches!(
        error,
        outbe_tribute::TributeRepositoryError::Storage(error)
            if error.kind() == StorageErrorKind::RequestDeadline
    ));
    let parent_error = ParentBodySource::get(&readers, EntityRef::Tribute(entity(1)))
        .expect_err("request-budget expiry must remain distinct from backend unavailability");
    assert!(matches!(
        parent_error,
        ParentBodySourceError::RequestDeadline(_)
    ));
    let precompile_error = outbe_primitives::error::PrecompileError::from(parent_error);
    assert!(matches!(
        precompile_error,
        outbe_primitives::error::PrecompileError::BodyReadRequestDeadline
    ));

    readers.report_precompile_error(&precompile_error);
    assert!(failure_rx.borrow().is_none());
    let diagnostic = capture.0.lock().unwrap();
    assert!(
        diagnostic.contains("stage=\"request_cancelled\""),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains("error_kind=RequestDeadline"),
        "{diagnostic}"
    );
    assert!(!diagnostic.contains("operation_timeout"));
}

#[test]
fn operation_timeout_is_mongo_unavailability_and_not_a_request_deadline() {
    let storage: StorageReaderHandle = Arc::new(DelayedReader {
        inner: MemoryStorage::new(),
        delay: Duration::from_millis(1_200),
    });
    let (failure_tx, failure_rx) = tokio::sync::watch::channel(None);
    let readers = supervised_runtime_body_readers(storage, failure_tx);
    let request_budget = outbe_primitives::projection::ExecutionReadBudget::new();
    let _budget = readers.enter_execution_budget(request_budget);

    let capture = DiagnosticCapture::default();
    let error = match tracing::subscriber::with_default(capture.subscriber(), || {
        TributeReadView::get(readers.tribute(), entity(1))
    }) {
        Ok(_) => panic!("read exceeding the MongoDB operation limit must fail"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        outbe_tribute::TributeRepositoryError::Storage(error)
            if error.kind() == StorageErrorKind::Unavailable
    ));

    readers.report_precompile_error(
        &outbe_primitives::error::PrecompileError::BodyReadUnavailable(
            "operation timeout".to_owned(),
        ),
    );
    assert!(matches!(
        failure_rx.borrow().clone(),
        Some(outbe_offchain_data::RuntimeBodyFailure::Unavailable { .. })
    ));
    let diagnostic = capture.0.lock().unwrap();
    assert!(
        diagnostic.contains("stage=\"operation_timeout\""),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains("error_kind=Unavailable"),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains("source_io_kind=Some(TimedOut)"),
        "{diagnostic}"
    );
    assert!(!diagnostic.contains("request_cancelled"));
}

#[derive(Clone, Default)]
struct DiagnosticCapture(Arc<std::sync::Mutex<String>>);

impl tracing::field::Visit for DiagnosticCapture {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write;
        writeln!(self.0.lock().unwrap(), "{}={value:?}", field.name()).unwrap();
    }
}

impl DiagnosticCapture {
    fn subscriber(&self) -> impl tracing::Subscriber + Send + Sync + 'static {
        tracing_subscriber::registry().with(self.clone())
    }
}

impl<S: tracing::Subscriber> Layer<S> for DiagnosticCapture {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        event.record(&mut self.clone());
    }
}

#[test]
fn backend_failure_diagnostic_identifies_operation_without_record_or_error_contents() {
    let capture = DiagnosticCapture::default();
    let readers = runtime_body_readers(Arc::new(FailingStorageReader(|| {
        StorageError::Unavailable {
            source: Box::new(std::io::Error::other("replica election")),
        }
    })));
    let error = tracing::subscriber::with_default(capture.subscriber(), || {
        ParentBodySource::get(&readers, EntityRef::Tribute(entity(1))).unwrap_err()
    });
    assert!(matches!(error, ParentBodySourceError::Unavailable(_)));
    let diagnostic = capture.0.lock().unwrap();
    assert!(
        diagnostic.contains("operation=\"get_record\""),
        "{diagnostic}"
    );
    assert!(diagnostic.contains("stage=\"backend\""), "{diagnostic}");
    assert!(
        diagnostic.contains("error_kind=Unavailable"),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains("source_io_kind=Some(Other)"),
        "{diagnostic}"
    );
    assert!(diagnostic.contains("elapsed_ms="), "{diagnostic}");
    assert!(!diagnostic.contains("replica election"));
    assert!(!diagnostic.contains(&alloy_primitives::hex::encode(entity(1).as_slice())));
    assert!(!diagnostic.contains("namespace="));
    assert!(!diagnostic.contains("record="));
}
