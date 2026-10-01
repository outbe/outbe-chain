use std::{collections::BTreeMap, sync::Arc};

use alloy_primitives::{Address, Bytes, LogData, B256, U256};
use alloy_sol_types::SolEvent;
use outbe_compressed_entities::{encode_tribute_v1, StoredBody};
use outbe_offchain_data::{
    read_projection_state, FinalizedBlock, OffchainDataProjection, ProjectionConfig,
    ProjectionError, ProjectionOutcome, ProjectionSource, ProjectionState, PROJECTION_STATE_KEY,
    PROJECTION_STATE_NAMESPACE,
};
use outbe_offchain_storage::{
    AtomicWriteBatch, AtomicWriteOperation, Key, MemoryStorage, Namespace, PendingOverlayStorage,
    StorageMetadata, StorageReader, StorageWriter,
};
use outbe_primitives::addresses::TRIBUTE_ADDRESS;
use outbe_primitives::time::WorldwideDay;
use outbe_tribute::{
    canonical_body, precompile::ITribute, TributeData, TributePageRequest, TributeRepositoryReader,
};

use super::support::*;

#[test]
fn projection_state_can_be_read_without_a_writer_capability() {
    let storage = Arc::new(RecordingStorage::default());
    let projection = open(&storage, 1);
    let expected = projection.state().clone();
    let batches_before = storage.batches();

    let actual = read_projection_state(config(1), storage.clone()).unwrap();

    assert_eq!(actual, Some(expected));
    assert_eq!(storage.batches(), batches_before);
}

#[test]
fn pre_ocomp_projection_schema_cannot_open_the_retained_namespace_layout() {
    let storage = Arc::new(MemoryStorage::new());
    let legacy = ProjectionState {
        chain_id: 91,
        genesis_hash: B256::repeat_byte(0x91),
        storage_schema_version: 1,
        start_block: 7,
        checkpoint: None,
    };
    storage
        .put(
            Namespace::new(PROJECTION_STATE_NAMESPACE).unwrap(),
            &Key::new(PROJECTION_STATE_KEY.to_vec()).unwrap(),
            &outbe_offchain_storage::Value::new(postcard::to_stdvec(&legacy).unwrap()).unwrap(),
        )
        .unwrap();

    assert!(matches!(
        OffchainDataProjection::open(config(7), storage.clone(), storage),
        Err(ProjectionError::ProjectionSchemaMismatch {
            expected: 3,
            actual: 1
        })
    ));
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
            &Key::new(token_id.as_slice().to_vec()).unwrap(),
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
    assert_eq!(batches.len(), 2); // initial state, then one block+checkpoint transaction
    assert!(batches[1].contains(&"tributes".to_owned()));
    assert!(batches[1].contains(&"tributes_by_owner".to_owned()));
    assert!(batches[1].contains(&"tributes_by_day".to_owned()));
    assert!(batches[1].contains(&PROJECTION_STATE_NAMESPACE.to_owned()));
}

#[test]
fn logical_projection_advances_before_the_durable_batch_is_written() {
    let durable = Arc::new(MemoryStorage::new());
    let bootstrap =
        OffchainDataProjection::open(config(10), durable.clone(), durable.clone()).unwrap();
    assert_eq!(bootstrap.state().checkpoint, None);

    let overlay = Arc::new(PendingOverlayStorage::new(durable.clone()));
    let mut logical =
        OffchainDataProjection::open(config(10), overlay.clone(), overlay.clone()).unwrap();
    let owner = Address::repeat_byte(0xa2);
    let tribute_id = poseidon_entity(owner, 20260715);
    let block = FinalizedBlock {
        number: 10,
        hash: B256::repeat_byte(0x42),
        receipts: vec![receipt(
            0,
            0x43,
            vec![log(
                0,
                TRIBUTE_ADDRESS,
                tribute_stored(tribute_id, owner, 20260715),
            )],
        )],
    };

    let prepared = logical.prepare_block(&block).unwrap();
    let (outcome, durable_batch) = logical.apply_prepared_with_batch(prepared).unwrap();

    assert!(matches!(
        outcome,
        ProjectionOutcome::Applied { checkpoint, .. }
            if checkpoint.block_number == 10 && checkpoint.block_hash == block.hash
    ));
    assert_eq!(
        read_projection_state(config(10), overlay.clone())
            .unwrap()
            .unwrap()
            .checkpoint,
        Some(outbe_offchain_data::ProjectionCheckpoint {
            block_number: 10,
            block_hash: block.hash,
        })
    );
    assert_eq!(
        read_projection_state(config(10), durable.clone())
            .unwrap()
            .unwrap()
            .checkpoint,
        None,
        "Mongo base must remain behind until its writer applies the returned batch"
    );
    assert!(TributeRepositoryReader::new(overlay)
        .get(tribute_id)
        .unwrap()
        .is_some());

    durable.apply_atomic(&durable_batch).unwrap();
    assert_eq!(
        read_projection_state(config(10), durable)
            .unwrap()
            .unwrap()
            .checkpoint,
        Some(outbe_offchain_data::ProjectionCheckpoint {
            block_number: 10,
            block_hash: block.hash,
        })
    );
}

#[test]
fn restart_replays_pending_finalized_receipts_from_the_durable_checkpoint() {
    let durable = Arc::new(MemoryStorage::new());
    let owner = Address::repeat_byte(0xb2);
    let durable_id = poseidon_entity(owner, 20260715);
    let pending_id = poseidon_entity(owner, 20260716);
    let durable_block = FinalizedBlock {
        number: 10,
        hash: B256::repeat_byte(0x51),
        receipts: vec![receipt(
            0,
            0x52,
            vec![log(
                0,
                TRIBUTE_ADDRESS,
                tribute_stored(durable_id, owner, 20260715),
            )],
        )],
    };
    let pending_block = FinalizedBlock {
        number: 11,
        hash: B256::repeat_byte(0x53),
        receipts: vec![receipt(
            0,
            0x54,
            vec![log(
                0,
                TRIBUTE_ADDRESS,
                tribute_stored(pending_id, owner, 20260716),
            )],
        )],
    };
    let mut durable_projection =
        OffchainDataProjection::open(config(10), durable.clone(), durable.clone()).unwrap();
    durable_projection.project_block(&durable_block).unwrap();

    let lost_overlay = Arc::new(PendingOverlayStorage::new(durable.clone()));
    let mut before_kill =
        OffchainDataProjection::open(config(10), lost_overlay.clone(), lost_overlay.clone())
            .unwrap();
    before_kill.project_block(&pending_block).unwrap();
    assert!(TributeRepositoryReader::new(lost_overlay)
        .get(pending_id)
        .unwrap()
        .is_some());
    drop(before_kill);

    let rebuilt_overlay = Arc::new(PendingOverlayStorage::new(durable.clone()));
    let mut after_restart =
        OffchainDataProjection::open(config(10), rebuilt_overlay.clone(), rebuilt_overlay.clone())
            .unwrap();
    assert_eq!(after_restart.state().checkpoint.unwrap().block_number, 10);
    after_restart.project_block(&pending_block).unwrap();

    assert_eq!(after_restart.state().checkpoint.unwrap().block_number, 11);
    assert!(TributeRepositoryReader::new(rebuilt_overlay)
        .get(pending_id)
        .unwrap()
        .is_some());
    assert_eq!(
        read_projection_state(config(10), durable)
            .unwrap()
            .unwrap()
            .checkpoint
            .unwrap()
            .block_number,
        10,
        "replay must rebuild RAM state without pretending Mongo already committed it"
    );
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

#[test]
fn replay_after_atomic_block_boundary_converges() {
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

    for fail_on in 1..=1 {
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
