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
use outbe_compressed_entities::{
    body_commitment, derive_poseidon_entity_id, encode_nod_bucket_v1, encode_nod_item_v1,
    encode_tribute_v1, EntityId36, StoredBody, ACTIVE_COMMITMENT_SCHEME, BODY_SCHEMA_V1,
};
use outbe_nod::{
    canonical_bucket, canonical_item, precompile::INod, NodBucketState, NodItemState,
    NodPageRequest, NodRepositoryReader,
};
use outbe_offchain_data::{
    FinalizedBlock, FinalizedLog, FinalizedReceipt, OffchainDataProjection, ProjectionConfig,
    ProjectionError, ProjectionOutcome, ProjectionSource, PROJECTION_STATE_NAMESPACE,
};
use outbe_offchain_storage::{
    AtomicWriteBatch, AtomicWriteOperation, Key, MemoryStorage, Namespace, ScanPage, ScanRequest,
    StorageError, StorageMetadata, StorageReader, StorageWriter, StoredValue,
};
use outbe_primitives::addresses::{NOD_ADDRESS, TRIBUTE_ADDRESS};
use outbe_tribute::{
    canonical_body, precompile::ITribute, TributeData, TributePageRequest, TributeRepositoryReader,
};

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

fn entity(seed: u64, day: u32) -> EntityId36 {
    EntityId36::new(WorldwideDay::new(day), U256::from(seed).to_be_bytes::<32>())
}

fn poseidon_entity(owner: Address, day: u32) -> EntityId36 {
    derive_poseidon_entity_id(owner, WorldwideDay::new(day)).unwrap()
}

fn tribute_body(tribute_id: EntityId36, owner: Address, day: u32) -> TributeData {
    TributeData {
        tribute_id,
        owner,
        worldwide_day: WorldwideDay::new(day),
        issuance_amount_minor: U256::from(10),
        issuance_currency: 840,
        nominal_amount_minor: U256::from(11),
        reference_currency: 978,
        tribute_price_minor: U256::from(12),
        exclude_from_intex_issuance: true,
    }
}

fn tribute_commitment(body: &TributeData) -> B256 {
    let payload = encode_tribute_v1(&canonical_body(body)).unwrap();
    B256::from(
        *body_commitment(
            ACTIVE_COMMITMENT_SCHEME,
            BODY_SCHEMA_V1,
            body.tribute_id,
            &payload,
        )
        .unwrap()
        .as_bytes(),
    )
}

fn tribute_stored(tribute_id: EntityId36, owner: Address, day: u32) -> LogData {
    tribute_stored_after(tribute_id, owner, day, B256::ZERO)
}

fn tribute_stored_after(
    tribute_id: EntityId36,
    owner: Address,
    day: u32,
    previous: B256,
) -> LogData {
    let body = tribute_body(tribute_id, owner, day);
    tribute_stored_body_after(&body, previous)
}

fn tribute_stored_body_after(body: &TributeData, previous: B256) -> LogData {
    let payload = encode_tribute_v1(&canonical_body(body)).unwrap();
    let new_commitment = tribute_commitment(body);
    ITribute::TributeBodyStored {
        tributeId: Bytes::copy_from_slice(body.tribute_id.as_bytes()),
        commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
        schemaVersion: BODY_SCHEMA_V1,
        previousCommitment: previous,
        newCommitment: new_commitment,
        canonicalPayload: Bytes::from(payload),
    }
    .encode_log_data()
}

fn nod_body(nod_id: EntityId36, owner: Address, bucket_key: B256) -> NodItemState {
    NodItemState {
        nod_id,
        owner,
        gratis_load_minor: U256::from(101),
        worldwide_day: WorldwideDay::new(20260715),
        league_id: 7,
        floor_price_minor: U256::from(102),
        bucket_key,
        cost_amount_minor: U256::from(103),
        issuance_currency: 840,
        reference_currency: 978,
        issued_at: 123_456,
    }
}

fn nod_stored(nod_id: EntityId36, owner: Address, bucket_key: B256) -> LogData {
    let body = nod_body(nod_id, owner, bucket_key);
    let payload = encode_nod_item_v1(&canonical_item(&body)).unwrap();
    let commitment =
        body_commitment(ACTIVE_COMMITMENT_SCHEME, BODY_SCHEMA_V1, nod_id, &payload).unwrap();
    INod::NodBodyStored {
        nodId: Bytes::copy_from_slice(nod_id.as_bytes()),
        commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
        schemaVersion: BODY_SCHEMA_V1,
        previousCommitment: B256::ZERO,
        newCommitment: B256::from(*commitment.as_bytes()),
        canonicalPayload: Bytes::from(payload),
    }
    .encode_log_data()
}

fn bucket_body(bucket_key: B256) -> NodBucketState {
    NodBucketState {
        bucket_key,
        worldwide_day: WorldwideDay::new(20260715),
        floor_price_minor: U256::from(102),
        is_qualified: true,
        total_nods: 3,
        entry_price_minor: U256::from(104),
    }
}

fn bucket_stored(bucket_key: B256) -> LogData {
    let body = bucket_body(bucket_key);
    let bucket_id = EntityId36::new(body.worldwide_day, bucket_key.0);
    let payload = encode_nod_bucket_v1(&canonical_bucket(&body)).unwrap();
    let commitment = body_commitment(
        ACTIVE_COMMITMENT_SCHEME,
        BODY_SCHEMA_V1,
        bucket_id,
        &payload,
    )
    .unwrap();
    INod::NodBucketBodyStored {
        bucketId: Bytes::copy_from_slice(bucket_id.as_bytes()),
        commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
        schemaVersion: BODY_SCHEMA_V1,
        previousCommitment: B256::ZERO,
        newCommitment: B256::from(*commitment.as_bytes()),
        canonicalPayload: Bytes::from(payload),
    }
    .encode_log_data()
}

#[test]
fn projects_primary_indexes_provenance_and_writes_checkpoint_last() {
    let storage = Arc::new(RecordingStorage::default());
    let mut projection = open(&storage, 10);
    let owner = Address::repeat_byte(0xa1);
    let token_id = poseidon_entity(owner, 20260715);
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
    let raw_primary = storage
        .get_record(
            Namespace::new("tributes").unwrap(),
            &Key::new(token_id.as_bytes().to_vec()).unwrap(),
        )
        .unwrap()
        .unwrap();
    let emitted_payload =
        encode_tribute_v1(&canonical_body(&tribute_body(token_id, owner, 20260715))).unwrap();
    assert_eq!(
        raw_primary.value.as_bytes(),
        StoredBody::new_v1(emitted_payload).unwrap().encode()
    );
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
fn full_block_overlay_applies_successive_canonical_updates_across_receipts() {
    let storage = Arc::new(RecordingStorage::default());
    let mut projection = open(&storage, 5);
    let owner = Address::repeat_byte(0x11);
    let token_id = poseidon_entity(owner, 20260715);
    let first_body = tribute_body(token_id, owner, 20260715);
    let mut final_body = tribute_body(token_id, owner, 20260715);
    final_body.tribute_price_minor = U256::from(99);
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
                    tribute_stored_body_after(&first_body, B256::ZERO),
                )],
            ),
            receipt(
                1,
                2,
                vec![log(
                    1,
                    TRIBUTE_ADDRESS,
                    tribute_stored_body_after(&final_body, tribute_commitment(&first_body)),
                )],
            ),
        ],
    };

    projection.project_block(&block).unwrap();
    let repository = TributeRepositoryReader::new(storage.clone());
    let final_page = repository
        .list_by_owner(
            owner,
            TributePageRequest {
                after: None,
                limit: 10,
            },
        )
        .unwrap();
    assert_eq!(final_page.records.len(), 1);
    assert_eq!(final_page.records[0].tribute_price_minor, U256::from(99));
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
    let ignored_id = entity(1, 20260715);
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

    let valid_owner = Address::repeat_byte(2);
    let valid_id = poseidon_entity(valid_owner, 20260715);
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
                    tribute_stored(valid_id, valid_owner, 20260715),
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
fn rejects_tampered_commitment_identity_version_and_transition_before_any_domain_write() {
    let owner = Address::repeat_byte(0x81);
    let tribute_id = poseidon_entity(owner, 20260715);
    let body = tribute_body(tribute_id, owner, 20260715);
    let payload = encode_tribute_v1(&canonical_body(&body)).unwrap();
    let mut noncanonical_payload = payload.clone();
    noncanonical_payload.extend_from_slice(&[0x60, 0x01]);

    let malformed_events = [
        ITribute::TributeBodyStored {
            tributeId: Bytes::copy_from_slice(tribute_id.as_bytes()),
            commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
            schemaVersion: BODY_SCHEMA_V1,
            previousCommitment: B256::ZERO,
            newCommitment: B256::repeat_byte(0x44),
            canonicalPayload: Bytes::copy_from_slice(&payload),
        }
        .encode_log_data(),
        ITribute::TributeBodyStored {
            tributeId: Bytes::copy_from_slice(entity(82, 20260715).as_bytes()),
            commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
            schemaVersion: BODY_SCHEMA_V1,
            previousCommitment: B256::ZERO,
            newCommitment: tribute_commitment(&body),
            canonicalPayload: Bytes::copy_from_slice(&payload),
        }
        .encode_log_data(),
        ITribute::TributeBodyStored {
            tributeId: Bytes::copy_from_slice(tribute_id.as_bytes()),
            commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME + 1,
            schemaVersion: BODY_SCHEMA_V1,
            previousCommitment: B256::ZERO,
            newCommitment: tribute_commitment(&body),
            canonicalPayload: Bytes::copy_from_slice(&payload),
        }
        .encode_log_data(),
        ITribute::TributeBodyStored {
            tributeId: Bytes::copy_from_slice(tribute_id.as_bytes()),
            commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
            schemaVersion: BODY_SCHEMA_V1 + 1,
            previousCommitment: B256::ZERO,
            newCommitment: tribute_commitment(&body),
            canonicalPayload: Bytes::copy_from_slice(&payload),
        }
        .encode_log_data(),
        ITribute::TributeBodyStored {
            tributeId: Bytes::copy_from_slice(tribute_id.as_bytes()),
            commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
            schemaVersion: BODY_SCHEMA_V1,
            previousCommitment: B256::ZERO,
            newCommitment: tribute_commitment(&body),
            canonicalPayload: Bytes::from(noncanonical_payload),
        }
        .encode_log_data(),
    ];

    for (case, event) in malformed_events.into_iter().enumerate() {
        let storage = Arc::new(RecordingStorage::default());
        let mut projection = open(&storage, 80);
        let batches_before = storage.batches().len();
        let block = FinalizedBlock {
            number: 80,
            hash: B256::repeat_byte(0x80 + case as u8),
            receipts: vec![receipt(
                0,
                0x80 + case as u8,
                vec![log(0, TRIBUTE_ADDRESS, event)],
            )],
        };
        assert!(matches!(
            projection.project_block(&block),
            Err(ProjectionError::MalformedProjectionEvent { .. })
        ));
        assert_eq!(storage.batches().len(), batches_before);
        assert_eq!(projection.state().checkpoint, None);
        assert!(TributeRepositoryReader::new(storage)
            .get(tribute_id)
            .unwrap()
            .is_none());
    }

    let storage = Arc::new(RecordingStorage::default());
    let mut projection = open(&storage, 80);
    let first_owner = owner;
    let final_owner = owner;
    let wrong_previous = tribute_commitment(&tribute_body(
        tribute_id,
        Address::repeat_byte(0x55),
        20260715,
    ));
    let block = FinalizedBlock {
        number: 80,
        hash: B256::repeat_byte(0x8f),
        receipts: vec![
            receipt(
                0,
                0x8e,
                vec![log(
                    0,
                    TRIBUTE_ADDRESS,
                    tribute_stored(tribute_id, first_owner, 20260715),
                )],
            ),
            receipt(
                1,
                0x8f,
                vec![log(
                    1,
                    TRIBUTE_ADDRESS,
                    tribute_stored_after(tribute_id, final_owner, 20260715, wrong_previous),
                )],
            ),
        ],
    };
    let batches_before = storage.batches().len();
    assert!(matches!(
        projection.project_block(&block),
        Err(ProjectionError::CommitmentTransitionMismatch { .. })
    ));
    assert_eq!(storage.batches().len(), batches_before);
    assert_eq!(projection.state().checkpoint, None);
    assert!(TributeRepositoryReader::new(storage)
        .get(tribute_id)
        .unwrap()
        .is_none());
}

#[test]
fn every_typed_store_and_delete_event_rejects_its_malformed_protocol_inputs_atomically() {
    let day = 20260715;
    let owner = Address::repeat_byte(0x91);
    let nod_id = poseidon_entity(owner, day);
    let nod = nod_body(nod_id, owner, B256::repeat_byte(0xa1));
    let nod_payload = encode_nod_item_v1(&canonical_item(&nod)).unwrap();
    let nod_commitment = B256::from(
        *body_commitment(
            ACTIVE_COMMITMENT_SCHEME,
            BODY_SCHEMA_V1,
            nod_id,
            &nod_payload,
        )
        .unwrap()
        .as_bytes(),
    );

    let bucket_key = B256::repeat_byte(0xa2);
    let bucket_id = EntityId36::new(WorldwideDay::new(day), bucket_key.0);
    let bucket = bucket_body(bucket_key);
    let bucket_payload = encode_nod_bucket_v1(&canonical_bucket(&bucket)).unwrap();
    let bucket_commitment = B256::from(
        *body_commitment(
            ACTIVE_COMMITMENT_SCHEME,
            BODY_SCHEMA_V1,
            bucket_id,
            &bucket_payload,
        )
        .unwrap()
        .as_bytes(),
    );

    let tribute_id = poseidon_entity(Address::repeat_byte(0x92), day);
    let tribute = tribute_body(tribute_id, Address::repeat_byte(0x92), day);
    let tribute_commitment = tribute_commitment(&tribute);

    let mut noncanonical_nod = nod_payload.clone();
    noncanonical_nod.extend_from_slice(&[0x60, 0x01]);
    let mut noncanonical_bucket = bucket_payload.clone();
    noncanonical_bucket.extend_from_slice(&[0x38, 0x01]);

    let malformed_events = vec![
        (
            NOD_ADDRESS,
            INod::NodBodyStored {
                nodId: Bytes::copy_from_slice(nod_id.as_bytes()),
                commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME + 1,
                schemaVersion: BODY_SCHEMA_V1,
                previousCommitment: B256::ZERO,
                newCommitment: nod_commitment,
                canonicalPayload: Bytes::copy_from_slice(&nod_payload),
            }
            .encode_log_data(),
        ),
        (
            NOD_ADDRESS,
            INod::NodBodyStored {
                nodId: Bytes::copy_from_slice(nod_id.as_bytes()),
                commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
                schemaVersion: BODY_SCHEMA_V1 + 1,
                previousCommitment: B256::ZERO,
                newCommitment: nod_commitment,
                canonicalPayload: Bytes::copy_from_slice(&nod_payload),
            }
            .encode_log_data(),
        ),
        (
            NOD_ADDRESS,
            INod::NodBodyStored {
                nodId: Bytes::copy_from_slice(nod_id.as_bytes()),
                commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
                schemaVersion: BODY_SCHEMA_V1,
                previousCommitment: B256::ZERO,
                newCommitment: nod_commitment,
                canonicalPayload: Bytes::from(noncanonical_nod),
            }
            .encode_log_data(),
        ),
        (
            NOD_ADDRESS,
            INod::NodBodyStored {
                nodId: Bytes::copy_from_slice(entity(0x93, day).as_bytes()),
                commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
                schemaVersion: BODY_SCHEMA_V1,
                previousCommitment: B256::ZERO,
                newCommitment: nod_commitment,
                canonicalPayload: Bytes::copy_from_slice(&nod_payload),
            }
            .encode_log_data(),
        ),
        (
            NOD_ADDRESS,
            INod::NodBodyStored {
                nodId: Bytes::copy_from_slice(nod_id.as_bytes()),
                commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
                schemaVersion: BODY_SCHEMA_V1,
                previousCommitment: B256::ZERO,
                newCommitment: B256::ZERO,
                canonicalPayload: Bytes::copy_from_slice(&nod_payload),
            }
            .encode_log_data(),
        ),
        (
            NOD_ADDRESS,
            INod::NodBucketBodyStored {
                bucketId: Bytes::copy_from_slice(bucket_id.as_bytes()),
                commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME + 1,
                schemaVersion: BODY_SCHEMA_V1,
                previousCommitment: B256::ZERO,
                newCommitment: bucket_commitment,
                canonicalPayload: Bytes::copy_from_slice(&bucket_payload),
            }
            .encode_log_data(),
        ),
        (
            NOD_ADDRESS,
            INod::NodBucketBodyStored {
                bucketId: Bytes::copy_from_slice(bucket_id.as_bytes()),
                commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
                schemaVersion: BODY_SCHEMA_V1 + 1,
                previousCommitment: B256::ZERO,
                newCommitment: bucket_commitment,
                canonicalPayload: Bytes::copy_from_slice(&bucket_payload),
            }
            .encode_log_data(),
        ),
        (
            NOD_ADDRESS,
            INod::NodBucketBodyStored {
                bucketId: Bytes::copy_from_slice(bucket_id.as_bytes()),
                commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
                schemaVersion: BODY_SCHEMA_V1,
                previousCommitment: B256::ZERO,
                newCommitment: bucket_commitment,
                canonicalPayload: Bytes::from(noncanonical_bucket),
            }
            .encode_log_data(),
        ),
        (
            NOD_ADDRESS,
            INod::NodBucketBodyStored {
                bucketId: Bytes::copy_from_slice(entity(0x94, day).as_bytes()),
                commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
                schemaVersion: BODY_SCHEMA_V1,
                previousCommitment: B256::ZERO,
                newCommitment: bucket_commitment,
                canonicalPayload: Bytes::copy_from_slice(&bucket_payload),
            }
            .encode_log_data(),
        ),
        (
            NOD_ADDRESS,
            INod::NodBucketBodyStored {
                bucketId: Bytes::copy_from_slice(bucket_id.as_bytes()),
                commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
                schemaVersion: BODY_SCHEMA_V1,
                previousCommitment: B256::ZERO,
                newCommitment: B256::ZERO,
                canonicalPayload: Bytes::copy_from_slice(&bucket_payload),
            }
            .encode_log_data(),
        ),
        (
            TRIBUTE_ADDRESS,
            ITribute::TributeBodyDeleted {
                tributeId: Bytes::copy_from_slice(tribute_id.as_bytes()),
                previousCommitment: B256::ZERO,
            }
            .encode_log_data(),
        ),
        (
            TRIBUTE_ADDRESS,
            ITribute::TributeBodyDeleted {
                tributeId: Bytes::from(vec![0x95; 35]),
                previousCommitment: tribute_commitment,
            }
            .encode_log_data(),
        ),
        (
            NOD_ADDRESS,
            INod::NodBodyDeleted {
                nodId: Bytes::copy_from_slice(nod_id.as_bytes()),
                previousCommitment: B256::ZERO,
            }
            .encode_log_data(),
        ),
        (
            NOD_ADDRESS,
            INod::NodBodyDeleted {
                nodId: Bytes::from(vec![0x96; 35]),
                previousCommitment: nod_commitment,
            }
            .encode_log_data(),
        ),
        (
            NOD_ADDRESS,
            INod::NodBucketBodyDeleted {
                bucketId: Bytes::copy_from_slice(bucket_id.as_bytes()),
                previousCommitment: B256::ZERO,
            }
            .encode_log_data(),
        ),
        (
            NOD_ADDRESS,
            INod::NodBucketBodyDeleted {
                bucketId: Bytes::from(vec![0x97; 35]),
                previousCommitment: bucket_commitment,
            }
            .encode_log_data(),
        ),
    ];

    for (case, (emitter, event)) in malformed_events.into_iter().enumerate() {
        let storage = Arc::new(RecordingStorage::default());
        let mut projection = open(&storage, 80);
        let batches_before = storage.batches().len();
        let block = FinalizedBlock {
            number: 80,
            hash: B256::repeat_byte(0xa0 + case as u8),
            receipts: vec![receipt(0, 0xa0 + case as u8, vec![log(0, emitter, event)])],
        };

        assert!(projection.project_block(&block).is_err(), "case {case}");
        assert_eq!(storage.batches().len(), batches_before, "case {case}");
        assert_eq!(projection.state().checkpoint, None, "case {case}");
    }

    for (case, (emitter, first, conflicting)) in [
        (
            NOD_ADDRESS,
            INod::NodBodyStored {
                nodId: Bytes::copy_from_slice(nod_id.as_bytes()),
                commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
                schemaVersion: BODY_SCHEMA_V1,
                previousCommitment: B256::ZERO,
                newCommitment: nod_commitment,
                canonicalPayload: Bytes::copy_from_slice(&nod_payload),
            }
            .encode_log_data(),
            INod::NodBodyStored {
                nodId: Bytes::copy_from_slice(nod_id.as_bytes()),
                commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
                schemaVersion: BODY_SCHEMA_V1,
                previousCommitment: bucket_commitment,
                newCommitment: nod_commitment,
                canonicalPayload: Bytes::copy_from_slice(&nod_payload),
            }
            .encode_log_data(),
        ),
        (
            NOD_ADDRESS,
            INod::NodBucketBodyStored {
                bucketId: Bytes::copy_from_slice(bucket_id.as_bytes()),
                commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
                schemaVersion: BODY_SCHEMA_V1,
                previousCommitment: B256::ZERO,
                newCommitment: bucket_commitment,
                canonicalPayload: Bytes::copy_from_slice(&bucket_payload),
            }
            .encode_log_data(),
            INod::NodBucketBodyStored {
                bucketId: Bytes::copy_from_slice(bucket_id.as_bytes()),
                commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
                schemaVersion: BODY_SCHEMA_V1,
                previousCommitment: nod_commitment,
                newCommitment: bucket_commitment,
                canonicalPayload: Bytes::copy_from_slice(&bucket_payload),
            }
            .encode_log_data(),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let storage = Arc::new(RecordingStorage::default());
        let mut projection = open(&storage, 90);
        let batches_before = storage.batches().len();
        let block = FinalizedBlock {
            number: 90,
            hash: B256::repeat_byte(0xe0 + case as u8),
            receipts: vec![
                receipt(0, 0xe0 + case as u8, vec![log(0, emitter, first)]),
                receipt(1, 0xe2 + case as u8, vec![log(1, emitter, conflicting)]),
            ],
        };

        let result = projection.project_block(&block);
        assert!(
            matches!(
                result,
                Err(ProjectionError::CommitmentTransitionMismatch { .. })
            ),
            "case {case}: {result:?}"
        );
        assert_eq!(storage.batches().len(), batches_before, "case {case}");
        assert_eq!(projection.state().checkpoint, None, "case {case}");
    }
}

#[test]
fn nod_item_and_bucket_share_one_receipt_batch_and_all_six_events_decode() {
    let storage = Arc::new(RecordingStorage::default());
    let mut projection = open(&storage, 30);
    let bucket_key = B256::repeat_byte(0xbc);
    let owner = Address::repeat_byte(0x77);
    let nod_id = poseidon_entity(owner, 20260715);
    let bucket_id = EntityId36::new(WorldwideDay::new(20260715), bucket_key.0);
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
    assert!(repository.get_bucket(bucket_id).unwrap().is_some());
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
                    INod::NodBodyDeleted {
                        nodId: Bytes::copy_from_slice(nod_id.as_bytes()),
                        previousCommitment: {
                            let body = nod_body(nod_id, owner, bucket_key);
                            let payload = encode_nod_item_v1(&canonical_item(&body)).unwrap();
                            B256::from(
                                *body_commitment(
                                    ACTIVE_COMMITMENT_SCHEME,
                                    BODY_SCHEMA_V1,
                                    nod_id,
                                    &payload,
                                )
                                .unwrap()
                                .as_bytes(),
                            )
                        },
                    }
                    .encode_log_data(),
                ),
                log(
                    1,
                    NOD_ADDRESS,
                    INod::NodBucketBodyDeleted {
                        bucketId: Bytes::copy_from_slice(bucket_id.as_bytes()),
                        previousCommitment: {
                            let body = bucket_body(bucket_key);
                            let payload = encode_nod_bucket_v1(&canonical_bucket(&body)).unwrap();
                            B256::from(
                                *body_commitment(
                                    ACTIVE_COMMITMENT_SCHEME,
                                    BODY_SCHEMA_V1,
                                    bucket_id,
                                    &payload,
                                )
                                .unwrap()
                                .as_bytes(),
                            )
                        },
                    }
                    .encode_log_data(),
                ),
                log(
                    2,
                    TRIBUTE_ADDRESS,
                    ITribute::TributeBodyDeleted {
                        tributeId: Bytes::copy_from_slice(entity(1234, 20260715).as_bytes()),
                        previousCommitment: tribute_commitment(&tribute_body(
                            entity(1234, 20260715),
                            Address::repeat_byte(1),
                            20260715,
                        )),
                    }
                    .encode_log_data(),
                ),
            ],
        )],
    };
    projection.project_block(&delete_block).unwrap();
    assert!(repository.get(nod_id).unwrap().is_none());
    assert!(repository.get_bucket(bucket_id).unwrap().is_none());
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
    let owner = Address::repeat_byte(0x51);
    let token_id = poseidon_entity(owner, 20260715);
    let mut bodies: [TributeData; 4] =
        std::array::from_fn(|_| tribute_body(token_id, owner, 20260715));
    for (index, body) in bodies.iter_mut().enumerate() {
        body.tribute_price_minor = U256::from(index + 1);
    }
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
                    tribute_stored_body_after(&bodies[0], B256::ZERO),
                )],
            ),
            receipt(
                1,
                0x62,
                vec![log(
                    1,
                    TRIBUTE_ADDRESS,
                    tribute_stored_body_after(&bodies[1], tribute_commitment(&bodies[0])),
                )],
            ),
            receipt(
                2,
                0x63,
                vec![log(
                    2,
                    TRIBUTE_ADDRESS,
                    tribute_stored_body_after(&bodies[2], tribute_commitment(&bodies[1])),
                )],
            ),
            receipt(
                3,
                0x64,
                vec![log(
                    3,
                    TRIBUTE_ADDRESS,
                    tribute_stored_body_after(&bodies[3], tribute_commitment(&bodies[2])),
                )],
            ),
        ],
    };

    for fail_on in 1..=5 {
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
        assert_eq!(final_body.owner, owner);
        assert_eq!(final_body.tribute_price_minor, U256::from(4));
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
            tribute_stored(
                poseidon_entity(Address::repeat_byte(0x70), 20260715),
                Address::repeat_byte(0x70),
                20260715,
            ),
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
