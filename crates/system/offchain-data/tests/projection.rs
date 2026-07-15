use std::{
    collections::BTreeMap,
    io,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use alloy_primitives::{Address, Bytes, LogData, B256, U256};
use alloy_sol_types::SolEvent;
use outbe_common::WorldwideDay;
use outbe_nod::{precompile::INod, NodPageRequest, NodRepositoryReader};
use outbe_offchain_data::{
    FinalizedBlock, FinalizedLog, FinalizedReceipt, OffchainDataProjection, ProjectionConfig,
    ProjectionError, ProjectionOutcome, ProjectionSource, PROJECTION_STATE_NAMESPACE,
};
use outbe_offchain_storage::{
    AtomicWriteBatch, AtomicWriteOperation, Key, MemoryStorage, Namespace, ScanPage, ScanRequest,
    StorageError, StorageMetadata, StorageReader, StorageWriter, StoredValue,
};
use outbe_primitives::addresses::{NOD_ADDRESS, TRIBUTE_ADDRESS};
use outbe_tribute::{precompile::ITribute, TributePageRequest, TributeRepositoryReader};

#[derive(Default)]
struct RecordingStorage {
    inner: MemoryStorage,
    applied_namespaces: Mutex<Vec<Vec<String>>>,
}

impl RecordingStorage {
    fn batches(&self) -> Vec<Vec<String>> {
        self.applied_namespaces.lock().unwrap().clone()
    }
}

impl StorageReader for RecordingStorage {
    fn get_record(
        &self,
        namespace: Namespace,
        key: &Key,
    ) -> Result<Option<StoredValue>, StorageError> {
        self.inner.get_record(namespace, key)
    }

    fn get_records(
        &self,
        namespace: Namespace,
        keys: &[Key],
    ) -> Result<Vec<Option<StoredValue>>, StorageError> {
        self.inner.get_records(namespace, keys)
    }

    fn scan_prefix(
        &self,
        namespace: Namespace,
        request: ScanRequest<'_>,
    ) -> Result<ScanPage, StorageError> {
        self.inner.scan_prefix(namespace, request)
    }
}

impl StorageWriter for RecordingStorage {
    fn apply_atomic(&self, batch: &AtomicWriteBatch) -> Result<(), StorageError> {
        let namespaces = batch
            .operations()
            .iter()
            .map(|operation| match operation {
                AtomicWriteOperation::Put { namespace, .. }
                | AtomicWriteOperation::Delete { namespace, .. } => namespace.as_str().to_owned(),
            })
            .collect();
        self.inner.apply_atomic(batch)?;
        self.applied_namespaces.lock().unwrap().push(namespaces);
        Ok(())
    }
}

fn config(start_block: u64) -> ProjectionConfig {
    ProjectionConfig {
        chain_id: 91,
        genesis_hash: B256::repeat_byte(0x91),
        start_block,
    }
}

fn open(storage: &Arc<RecordingStorage>, start_block: u64) -> OffchainDataProjection {
    OffchainDataProjection::open(config(start_block), storage.clone(), storage.clone()).unwrap()
}

fn receipt(index: u64, hash_byte: u8, logs: Vec<FinalizedLog>) -> FinalizedReceipt {
    FinalizedReceipt {
        tx_hash: B256::repeat_byte(hash_byte),
        transaction_index: index,
        success: true,
        logs,
    }
}

fn log(index: u64, emitter: Address, data: LogData) -> FinalizedLog {
    FinalizedLog {
        log_index: index,
        emitter,
        data,
    }
}

fn tribute_stored(token_id: U256, owner: Address, day: u32) -> LogData {
    ITribute::TributeBodyStored {
        tokenId: token_id,
        owner,
        worldwideDay: day,
        issuanceAmountMinor: U256::from(10),
        issuanceCurrency: 840,
        nominalAmountMinor: U256::from(11),
        referenceCurrency: 978,
        tributePriceMinor: U256::from(12),
        excludeFromIntexIssuance: true,
    }
    .encode_log_data()
}

fn nod_stored(nod_id: U256, owner: Address, bucket_key: B256) -> LogData {
    INod::NodBodyStored {
        nodId: nod_id,
        owner,
        gratisLoadMinor: U256::from(101),
        worldwideDay: 20260715,
        leagueId: 7,
        floorPriceMinor: U256::from(102),
        bucketKey: bucket_key,
        costAmountMinor: U256::from(103),
        issuanceCurrency: 840,
        referenceCurrency: 978,
        issuedAt: 123_456,
    }
    .encode_log_data()
}

fn bucket_stored(bucket_key: B256) -> LogData {
    INod::NodBucketBodyStored {
        bucketKey: bucket_key,
        worldwideDay: 20260715,
        floorPriceMinor: U256::from(102),
        isQualified: true,
        totalNods: 3,
        entryPriceMinor: U256::from(104),
    }
    .encode_log_data()
}

#[test]
fn projects_primary_indexes_provenance_and_writes_checkpoint_last() {
    let storage = Arc::new(RecordingStorage::default());
    let mut projection = open(&storage, 10);
    let token_id = U256::from(42);
    let owner = Address::repeat_byte(0xa1);
    let block = FinalizedBlock {
        number: 10,
        hash: B256::repeat_byte(0x10),
        receipts: vec![receipt(
            0,
            0x20,
            vec![
                log(
                    0,
                    Address::repeat_byte(0xee),
                    LogData::new(
                        vec![B256::repeat_byte(0xee)],
                        Bytes::from_static(b"ignored"),
                    )
                    .unwrap(),
                ),
                log(
                    1,
                    TRIBUTE_ADDRESS,
                    tribute_stored(token_id, owner, 20260715),
                ),
            ],
        )],
    };

    let outcome = projection.project_block(&block).unwrap();
    assert_eq!(
        outcome,
        ProjectionOutcome::Applied {
            checkpoint: projection.state().checkpoint.unwrap(),
            receipt_batches: 1,
        }
    );

    let repository = TributeRepositoryReader::new(storage.clone());
    let (body, metadata) = repository.get_with_metadata(token_id).unwrap().unwrap();
    assert_eq!(body.owner, owner);
    assert_eq!(body.worldwide_day, WorldwideDay::new(20260715));
    let source = ProjectionSource::from_storage_metadata(&metadata.unwrap()).unwrap();
    assert_eq!(source.block_number, 10);
    assert_eq!(source.block_hash, block.hash);
    assert_eq!(source.tx_hash, block.receipts[0].tx_hash);
    assert_eq!(source.transaction_index, 0);
    assert_eq!(source.log_index, 1);
    assert_eq!(source.emitter, TRIBUTE_ADDRESS);
    assert_eq!(
        source.event_signature,
        ITribute::TributeBodyStored::SIGNATURE_HASH
    );
    assert_eq!(
        repository
            .list_by_owner(
                owner,
                TributePageRequest {
                    after: None,
                    limit: 10,
                },
            )
            .unwrap()
            .records
            .len(),
        1
    );

    let batches = storage.batches();
    assert_eq!(batches.len(), 3); // initial state, receipt, checkpoint
    assert!(batches[1].contains(&"tributes".to_owned()));
    assert!(batches[1].contains(&"tributes_by_owner".to_owned()));
    assert!(batches[1].contains(&"tributes_by_day".to_owned()));
    assert_eq!(batches[2], vec![PROJECTION_STATE_NAMESPACE.to_owned()]);
}

#[test]
fn full_block_overlay_removes_stale_indexes_across_receipts() {
    let storage = Arc::new(RecordingStorage::default());
    let mut projection = open(&storage, 5);
    let token_id = U256::from(7);
    let first_owner = Address::repeat_byte(0x11);
    let final_owner = Address::repeat_byte(0x22);
    let block = FinalizedBlock {
        number: 5,
        hash: B256::repeat_byte(5),
        receipts: vec![
            receipt(
                0,
                1,
                vec![log(
                    0,
                    TRIBUTE_ADDRESS,
                    tribute_stored(token_id, first_owner, 20260714),
                )],
            ),
            receipt(
                1,
                2,
                vec![log(
                    1,
                    TRIBUTE_ADDRESS,
                    tribute_stored(token_id, final_owner, 20260715),
                )],
            ),
        ],
    };

    projection.project_block(&block).unwrap();
    let repository = TributeRepositoryReader::new(storage.clone());
    assert!(repository
        .list_by_owner(
            first_owner,
            TributePageRequest {
                after: None,
                limit: 10,
            },
        )
        .unwrap()
        .records
        .is_empty());
    let final_page = repository
        .list_by_owner(
            final_owner,
            TributePageRequest {
                after: None,
                limit: 10,
            },
        )
        .unwrap();
    assert_eq!(final_page.records.len(), 1);
    assert_eq!(
        final_page.records[0].worldwide_day,
        WorldwideDay::new(20260715)
    );
    let (_, metadata) = repository.get_with_metadata(token_id).unwrap().unwrap();
    assert_eq!(
        ProjectionSource::from_storage_metadata(&metadata.unwrap())
            .unwrap()
            .transaction_index,
        1
    );
}

#[test]
fn exact_pair_filtering_and_full_block_prepare_failure_do_not_write_domain_data() {
    let storage = Arc::new(RecordingStorage::default());
    let mut projection = open(&storage, 20);
    let ignored_id = U256::from(1);
    let ignored = FinalizedBlock {
        number: 20,
        hash: B256::repeat_byte(20),
        receipts: vec![receipt(
            0,
            3,
            vec![log(
                0,
                NOD_ADDRESS,
                tribute_stored(ignored_id, Address::ZERO, 20260715),
            )],
        )],
    };
    projection.project_block(&ignored).unwrap();
    assert!(TributeRepositoryReader::new(storage.clone())
        .get(ignored_id)
        .unwrap()
        .is_none());

    let valid_id = U256::from(2);
    let malformed = LogData::new(
        vec![ITribute::TributeBodyStored::SIGNATURE_HASH],
        Bytes::new(),
    )
    .unwrap();
    let bad_block = FinalizedBlock {
        number: 21,
        hash: B256::repeat_byte(21),
        receipts: vec![
            receipt(
                0,
                4,
                vec![log(
                    0,
                    TRIBUTE_ADDRESS,
                    tribute_stored(valid_id, Address::repeat_byte(2), 20260715),
                )],
            ),
            receipt(1, 5, vec![log(1, TRIBUTE_ADDRESS, malformed)]),
        ],
    };
    let batches_before = storage.batches().len();
    assert!(matches!(
        projection.project_block(&bad_block),
        Err(ProjectionError::MalformedProjectionEvent { .. })
    ));
    assert_eq!(storage.batches().len(), batches_before);
    assert!(TributeRepositoryReader::new(storage.clone())
        .get(valid_id)
        .unwrap()
        .is_none());
    assert_eq!(projection.state().checkpoint.unwrap().block_number, 20);
}

#[test]
fn nod_item_and_bucket_share_one_receipt_batch_and_all_six_events_decode() {
    let storage = Arc::new(RecordingStorage::default());
    let mut projection = open(&storage, 30);
    let nod_id = U256::from(99);
    let bucket_key = B256::repeat_byte(0xbc);
    let owner = Address::repeat_byte(0x77);
    let store_block = FinalizedBlock {
        number: 30,
        hash: B256::repeat_byte(30),
        receipts: vec![receipt(
            0,
            6,
            vec![
                log(0, NOD_ADDRESS, nod_stored(nod_id, owner, bucket_key)),
                log(1, NOD_ADDRESS, bucket_stored(bucket_key)),
            ],
        )],
    };
    projection.project_block(&store_block).unwrap();
    let repository = NodRepositoryReader::new(storage.clone());
    assert!(repository.get(nod_id).unwrap().is_some());
    assert!(repository.get_bucket(bucket_key).unwrap().is_some());
    assert_eq!(
        repository
            .list_by_owner(
                owner,
                NodPageRequest {
                    after: None,
                    limit: 10,
                },
            )
            .unwrap()
            .records
            .len(),
        1
    );
    let store_batches = storage.batches();
    assert!(store_batches[1].contains(&"nods".to_owned()));
    assert!(store_batches[1].contains(&"nod_buckets".to_owned()));

    let delete_block = FinalizedBlock {
        number: 31,
        hash: B256::repeat_byte(31),
        receipts: vec![receipt(
            0,
            7,
            vec![
                log(
                    0,
                    NOD_ADDRESS,
                    INod::NodBodyDeleted { nodId: nod_id }.encode_log_data(),
                ),
                log(
                    1,
                    NOD_ADDRESS,
                    INod::NodBucketBodyDeleted {
                        bucketKey: bucket_key,
                    }
                    .encode_log_data(),
                ),
                log(
                    2,
                    TRIBUTE_ADDRESS,
                    ITribute::TributeBodyDeleted {
                        tokenId: U256::from(1234),
                    }
                    .encode_log_data(),
                ),
            ],
        )],
    };
    projection.project_block(&delete_block).unwrap();
    assert!(repository.get(nod_id).unwrap().is_none());
    assert!(repository.get_bucket(bucket_key).unwrap().is_none());
}

#[test]
fn rejects_unmanaged_data_and_validates_persisted_identity() {
    let storage = Arc::new(RecordingStorage::default());
    storage
        .apply_atomic(&AtomicWriteBatch::from_operations(vec![
            AtomicWriteOperation::put(
                Namespace::new("tributes").unwrap(),
                Key::new(vec![1]).unwrap(),
                outbe_offchain_storage::Value::new(vec![1]).unwrap(),
            ),
        ]))
        .unwrap();
    assert!(matches!(
        OffchainDataProjection::open(config(1), storage.clone(), storage.clone()),
        Err(ProjectionError::UnmanagedProjectionData)
    ));

    let managed = Arc::new(RecordingStorage::default());
    let _projection = open(&managed, 1);
    let wrong = ProjectionConfig {
        chain_id: 92,
        ..config(1)
    };
    assert!(matches!(
        OffchainDataProjection::open(wrong, managed.clone(), managed.clone()),
        Err(ProjectionError::ProjectionIdentityMismatch { .. })
    ));
}

#[test]
fn duplicate_delivery_is_idempotent_and_conflicting_hash_is_rejected() {
    let storage = Arc::new(RecordingStorage::default());
    let mut projection = open(&storage, 40);
    let block = FinalizedBlock {
        number: 40,
        hash: B256::repeat_byte(40),
        receipts: vec![],
    };
    projection.project_block(&block).unwrap();
    let count = storage.batches().len();
    assert_eq!(
        projection.project_block(&block).unwrap(),
        ProjectionOutcome::AlreadyApplied(projection.state().checkpoint.unwrap())
    );
    assert_eq!(storage.batches().len(), count);

    let conflict = FinalizedBlock {
        hash: B256::repeat_byte(41),
        ..block
    };
    assert!(matches!(
        projection.project_block(&conflict),
        Err(ProjectionError::CheckpointMismatch { .. })
    ));
}

#[derive(Default)]
struct FailOnceStorage {
    inner: MemoryStorage,
    call: AtomicUsize,
    fail_on: AtomicUsize,
}

impl FailOnceStorage {
    fn arm(&self, fail_on: usize) {
        self.call.store(0, Ordering::SeqCst);
        self.fail_on.store(fail_on, Ordering::SeqCst);
    }
}

impl StorageReader for FailOnceStorage {
    fn get_record(
        &self,
        namespace: Namespace,
        key: &Key,
    ) -> Result<Option<StoredValue>, StorageError> {
        self.inner.get_record(namespace, key)
    }

    fn get_records(
        &self,
        namespace: Namespace,
        keys: &[Key],
    ) -> Result<Vec<Option<StoredValue>>, StorageError> {
        self.inner.get_records(namespace, keys)
    }

    fn scan_prefix(
        &self,
        namespace: Namespace,
        request: ScanRequest<'_>,
    ) -> Result<ScanPage, StorageError> {
        self.inner.scan_prefix(namespace, request)
    }
}

impl StorageWriter for FailOnceStorage {
    fn apply_atomic(&self, batch: &AtomicWriteBatch) -> Result<(), StorageError> {
        let call = self.call.fetch_add(1, Ordering::SeqCst) + 1;
        if self
            .fail_on
            .compare_exchange(call, 0, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            return Err(StorageError::Backend {
                source: Box::new(io::Error::other("injected projection crash boundary")),
            });
        }
        self.inner.apply_atomic(batch)
    }
}

#[test]
fn replay_after_each_receipt_and_checkpoint_boundary_converges() {
    let token_id = U256::from(500);
    let first_owner = Address::repeat_byte(0x51);
    let final_owner = Address::repeat_byte(0x52);
    let block = FinalizedBlock {
        number: 60,
        hash: B256::repeat_byte(0x60),
        receipts: vec![
            receipt(
                0,
                0x61,
                vec![log(
                    0,
                    TRIBUTE_ADDRESS,
                    tribute_stored(token_id, first_owner, 20260714),
                )],
            ),
            receipt(
                1,
                0x62,
                vec![log(
                    1,
                    TRIBUTE_ADDRESS,
                    tribute_stored(token_id, final_owner, 20260715),
                )],
            ),
        ],
    };

    for fail_on in 1..=3 {
        let storage = Arc::new(FailOnceStorage::default());
        let mut projection =
            OffchainDataProjection::open(config(60), storage.clone(), storage.clone()).unwrap();
        storage.arm(fail_on);
        assert!(projection.project_block(&block).is_err());
        drop(projection);

        let mut restarted =
            OffchainDataProjection::open(config(60), storage.clone(), storage.clone()).unwrap();
        restarted.project_block(&block).unwrap();
        let repository = TributeRepositoryReader::new(storage.clone());
        let final_body = repository.get(token_id).unwrap().unwrap();
        assert_eq!(final_body.owner, final_owner);
        assert!(repository
            .list_by_owner(
                first_owner,
                TributePageRequest {
                    after: None,
                    limit: 10,
                },
            )
            .unwrap()
            .records
            .is_empty());
        assert_eq!(restarted.state().checkpoint.unwrap().block_hash, block.hash);
    }
}

#[test]
fn failed_recognized_receipt_and_noncanonical_metadata_stall_without_checkpoint() {
    let storage = Arc::new(RecordingStorage::default());
    let mut projection = open(&storage, 70);
    let mut failed_receipt = receipt(
        0,
        0x70,
        vec![log(
            0,
            TRIBUTE_ADDRESS,
            tribute_stored(U256::from(70), Address::repeat_byte(0x70), 20260715),
        )],
    );
    failed_receipt.success = false;
    let block = FinalizedBlock {
        number: 70,
        hash: B256::repeat_byte(0x70),
        receipts: vec![failed_receipt],
    };
    let batches_before = storage.batches().len();
    assert!(matches!(
        projection.project_block(&block),
        Err(ProjectionError::ProjectionLogInFailedReceipt(_))
    ));
    assert_eq!(storage.batches().len(), batches_before);
    assert_eq!(projection.state().checkpoint, None);

    let metadata = StorageMetadata::new(BTreeMap::from([
        ("block_number".to_owned(), "070".to_owned()),
        (
            "block_hash".to_owned(),
            format!("{:#x}", B256::repeat_byte(1)),
        ),
        ("tx_hash".to_owned(), format!("{:#x}", B256::repeat_byte(2))),
        ("transaction_index".to_owned(), "0".to_owned()),
        ("log_index".to_owned(), "0".to_owned()),
        ("emitter".to_owned(), format!("{:#x}", TRIBUTE_ADDRESS)),
        (
            "event_signature".to_owned(),
            format!("{:#x}", ITribute::TributeBodyStored::SIGNATURE_HASH),
        ),
    ]))
    .unwrap();
    assert!(matches!(
        ProjectionSource::from_storage_metadata(&metadata),
        Err(ProjectionError::MalformedProjectionMetadata(_))
    ));
}
