use std::{sync::Arc, time::Duration};

use alloy_primitives::{Address, B256, U256};
use outbe_common::WorldwideDay;
use outbe_compressed_entities::EntityId36;
use outbe_nod::{NodBucketState, NodItemState, NodRepositoryWriter};
use outbe_offchain_data::RuntimeBodyReaders;
use outbe_offchain_storage::{
    Key, MemoryStorage, Namespace, ScanPage, ScanRequest, StorageError, StorageErrorKind,
    StorageReader, StorageReaderHandle, StorageWriterHandle, StoredValue,
};
use outbe_tribute::{TributeData, TributeRepositoryWriter};

fn entity(seed: u64) -> EntityId36 {
    EntityId36::new(
        WorldwideDay::new(20_260_715),
        U256::from(seed).to_be_bytes::<32>(),
    )
}

fn tribute(tribute_id: EntityId36) -> TributeData {
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
fn supervised_bundle_reports_read_failures_to_its_lifecycle_owner() {
    let storage = Arc::new(MemoryStorage::new());
    let (failure_tx, failure_rx) = tokio::sync::watch::channel(None);
    let readers = RuntimeBodyReaders::new_supervised(storage, failure_tx);

    readers.report_precompile_error(
        &outbe_primitives::error::PrecompileError::BodyReadUnavailable("replica election".into()),
    );
    readers.report_precompile_error(
        &outbe_primitives::error::PrecompileError::BodyReadCorruption(
            "invalid body identity".into(),
        ),
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

fn nod(nod_id: EntityId36, bucket_key: B256) -> NodItemState {
    NodItemState {
        nod_id,
        owner: Address::repeat_byte(0x22),
        gratis_load_minor: U256::from(55),
        worldwide_day: WorldwideDay::new(20_260_715),
        league_id: 7,
        floor_price_minor: U256::from(8),
        bucket_key,
        cost_amount_minor: U256::from(13),
        issuance_currency: 840,
        reference_currency: 978,
        issued_at: 1_752_534_000,
    }
}

fn bucket(bucket_key: B256) -> NodBucketState {
    NodBucketState {
        bucket_key,
        worldwide_day: WorldwideDay::new(20_260_715),
        floor_price_minor: U256::from(8),
        is_qualified: true,
        total_nods: 1,
        entry_price_minor: U256::from(5),
    }
}

#[test]
fn typed_readers_share_one_memory_adapter() {
    let storage = Arc::new(MemoryStorage::new());
    let reader: StorageReaderHandle = storage.clone();
    let writer: StorageWriterHandle = storage;
    let readers = RuntimeBodyReaders::new(reader.clone());

    let tribute_id = entity(1);
    let nod_id = entity(2);
    let bucket_key = B256::repeat_byte(0x33);
    let bucket_id = EntityId36::new(WorldwideDay::new(20_260_715), bucket_key.0);

    TributeRepositoryWriter::new(reader.clone(), writer.clone())
        .put(&tribute(tribute_id))
        .unwrap();
    let nod_writer = NodRepositoryWriter::new(reader, writer);
    nod_writer.put_nod(&nod(nod_id, bucket_key)).unwrap();
    nod_writer.put_bucket(&bucket(bucket_key)).unwrap();

    let stored_tribute = readers.tribute().get(tribute_id).unwrap().unwrap();
    assert_eq!(stored_tribute.owner, Address::repeat_byte(0x11));

    let stored_nod = readers.nod().get(nod_id).unwrap().unwrap();
    assert_eq!(stored_nod.bucket_key, bucket_key);

    let stored_bucket = readers.nod().get_bucket(bucket_id).unwrap().unwrap();
    assert!(stored_bucket.is_qualified);
}

#[test]
fn cloned_bundle_observes_later_writes_through_typed_readers() {
    let storage = Arc::new(MemoryStorage::new());
    let reader: StorageReaderHandle = storage.clone();
    let writer: StorageWriterHandle = storage;
    let readers = RuntimeBodyReaders::new(reader.clone());
    let cloned = readers.clone();
    let tribute_id = entity(9);

    assert!(cloned.tribute().get(tribute_id).unwrap().is_none());

    TributeRepositoryWriter::new(reader, writer)
        .put(&tribute(tribute_id))
        .unwrap();

    assert_eq!(
        cloned
            .tribute()
            .get(tribute_id)
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
    let readers = RuntimeBodyReaders::new_supervised(storage, failure_tx);
    let request_budget = outbe_primitives::projection::ExecutionReadBudget::new();
    let _budget = readers.enter_execution_budget(request_budget.clone());
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(25));
        request_budget.cancel();
    });

    let started = std::time::Instant::now();
    let error = match readers.tribute().get(entity(1)) {
        Ok(_) => panic!("delayed read must exceed the request budget"),
        Err(error) => error,
    };
    assert!(started.elapsed() < Duration::from_millis(150));
    assert!(matches!(
        error,
        outbe_tribute::TributeRepositoryError::Storage(error)
            if error.kind() == StorageErrorKind::RequestDeadline
    ));

    readers.report_precompile_error(
        &outbe_primitives::error::PrecompileError::BodyReadRequestDeadline,
    );
    assert!(failure_rx.borrow().is_none());
}

#[test]
fn operation_timeout_is_mongo_unavailability_and_not_a_request_deadline() {
    let storage: StorageReaderHandle = Arc::new(DelayedReader {
        inner: MemoryStorage::new(),
        delay: Duration::from_millis(1_200),
    });
    let (failure_tx, failure_rx) = tokio::sync::watch::channel(None);
    let readers = RuntimeBodyReaders::new_supervised(storage, failure_tx);
    let request_budget = outbe_primitives::projection::ExecutionReadBudget::new();
    let _budget = readers.enter_execution_budget(request_budget);

    let error = match readers.tribute().get(entity(1)) {
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
    assert_eq!(
        failure_rx.borrow().clone(),
        Some(outbe_offchain_data::RuntimeBodyFailure::Unavailable)
    );
}
