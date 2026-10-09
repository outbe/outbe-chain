use std::sync::Arc;

use alloy_primitives::{keccak256, Address, B256};
use alloy_sol_types::SolEvent;
use outbe_compressed_entities::{encode_tribute_v1, StoredBody};
use outbe_offchain_data::{read_projection_state, ProjectionOutcome, PROJECTION_STATE_NAMESPACE};
use outbe_primitives::addresses::TRIBUTE_ADDRESS;
use outbe_primitives::time::WorldwideDay;
use outbe_tribute::{
    canonical_body, precompile::ITribute, RetainedTributePin, RetainedTributeReader,
    TributeRepositoryReader, OCOMP_RETAINED_TRIBUTES_BY_DAY_NAMESPACE,
    OCOMP_RETAINED_TRIBUTES_NAMESPACE,
};

use super::support::*;

#[test]
fn tribute_partition_retirement_atomically_deletes_only_the_selected_day() {
    assert_eq!(
        ITribute::TributePartitionRetired::SIGNATURE_HASH,
        keccak256("TributePartitionRetired(uint32)")
    );
    let storage = Arc::new(RecordingStorage::default());
    let mut projection = open(&storage, 10);
    let owner = Address::repeat_byte(0x71);
    let retired_day = 20260715;
    let retained_day = 20260716;
    let retired_id = poseidon_entity(owner, retired_day);
    let retained_id = poseidon_entity(owner, retained_day);

    projection
        .project_block(&single_receipt_block(
            10,
            0x70,
            0x71,
            vec![
                log(
                    0,
                    TRIBUTE_ADDRESS,
                    tribute_stored(retired_id, owner, retired_day),
                ),
                log(
                    1,
                    TRIBUTE_ADDRESS,
                    tribute_stored(retained_id, owner, retained_day),
                ),
            ],
        ))
        .unwrap();

    let retirement = single_receipt_block(
        11,
        0x72,
        0x73,
        vec![log(
            0,
            TRIBUTE_ADDRESS,
            tribute_partition_retired(retired_day),
        )],
    );
    projection.project_block(&retirement).unwrap();

    let repository = TributeRepositoryReader::new(storage.clone());
    assert!(repository.get(retired_id).unwrap().is_none());
    assert!(repository.get(retained_id).unwrap().is_some());
    assert!(repository
        .list_by_day(WorldwideDay::new(retired_day), FIRST_TRIBUTE_PAGE,)
        .unwrap()
        .records
        .is_empty());
    assert_eq!(
        projection.state().checkpoint.unwrap().block_hash,
        retirement.hash
    );
    assert!(matches!(
        projection.project_block(&retirement).unwrap(),
        ProjectionOutcome::AlreadyApplied(_)
    ));
}

#[test]
fn pinned_tribute_partition_retirement_atomically_moves_exact_body_to_job_retention() {
    let storage = Arc::new(RecordingStorage::default());
    let day = 20260715;
    let pin = RetainedTributePin {
        input_lease_id: B256::repeat_byte(0x51),
        worldwide_day: WorldwideDay::new(day),
    };
    let selector = Arc::new(FixedRetentionSelector { pin });
    let mut projection = outbe_offchain_data::open_projection_with_retention_selector(
        config(10),
        storage.clone(),
        storage.clone(),
        selector,
    )
    .unwrap();
    let owner = Address::repeat_byte(0x52);
    let tribute_id = poseidon_entity(owner, day);
    let body = tribute_body(tribute_id, owner, day);

    projection
        .project_block(&single_receipt_block(
            10,
            0x53,
            0x54,
            vec![log(
                0,
                TRIBUTE_ADDRESS,
                tribute_stored_body_after(&body, B256::ZERO),
            )],
        ))
        .unwrap();
    projection
        .project_block(&single_receipt_block(
            11,
            0x55,
            0x56,
            vec![log(0, TRIBUTE_ADDRESS, tribute_partition_retired(day))],
        ))
        .unwrap();

    assert!(TributeRepositoryReader::new(storage.clone())
        .get(tribute_id)
        .unwrap()
        .is_none());
    let retained = RetainedTributeReader::new(storage.clone())
        .get_current_or_retained(pin, tribute_id, tribute_commitment(&body))
        .unwrap()
        .unwrap();
    let payload = encode_tribute_v1(&canonical_body(&body)).unwrap();
    assert_eq!(
        retained.encode(),
        StoredBody::new(outbe_compressed_entities::BODY_SCHEMA_V1, payload)
            .unwrap()
            .encode()
    );

    let batches = storage.batches();
    let retirement_batch = batches.last().unwrap();
    assert!(retirement_batch.contains(&OCOMP_RETAINED_TRIBUTES_NAMESPACE.to_owned()));
    assert!(retirement_batch.contains(&OCOMP_RETAINED_TRIBUTES_BY_DAY_NAMESPACE.to_owned()));
    assert!(retirement_batch.contains(&"tributes".to_owned()));
    assert!(retirement_batch.contains(&"tributes_by_day".to_owned()));
    assert!(retirement_batch.contains(&PROJECTION_STATE_NAMESPACE.to_owned()));
}

#[test]
fn rocksdb_retirement_and_gc_preserve_open_export_session_and_durable_checkpoint() {
    use outbe_offchain_storage::{RocksDbReader, RocksDbStorage};
    use outbe_tribute::RetainedTributeWriter;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("primary");
    let storage = Arc::new(RocksDbStorage::open(&path).unwrap());
    let day = 20260715;
    let pin = RetainedTributePin {
        input_lease_id: B256::repeat_byte(0x51),
        worldwide_day: WorldwideDay::new(day),
    };
    let owner = Address::repeat_byte(0x52);
    let id = poseidon_entity(owner, day);
    let body = tribute_body(id, owner, day);
    let commitment = tribute_commitment(&body);
    let mut projection = outbe_offchain_data::open_projection_with_retention_selector(
        config(10),
        storage.clone(),
        storage.clone(),
        Arc::new(FixedRetentionSelector { pin }),
    )
    .unwrap();
    projection
        .project_block(&single_receipt_block(
            10,
            0x53,
            0x54,
            vec![log(
                0,
                TRIBUTE_ADDRESS,
                tribute_stored_body_after(&body, B256::ZERO),
            )],
        ))
        .unwrap();
    let before = Arc::new(RocksDbReader::open(&path, &root.path().join("before")).unwrap());
    projection
        .project_block(&single_receipt_block(
            11,
            0x55,
            0x56,
            vec![log(0, TRIBUTE_ADDRESS, tribute_partition_retired(day))],
        ))
        .unwrap();
    let retired = Arc::new(RocksDbReader::open(&path, &root.path().join("retired")).unwrap());
    assert!(TributeRepositoryReader::new(before.clone())
        .get(id)
        .unwrap()
        .is_some());
    assert!(TributeRepositoryReader::new(retired.clone())
        .get(id)
        .unwrap()
        .is_none());
    assert_eq!(
        read_projection_state(config(10), before.clone())
            .unwrap()
            .unwrap()
            .checkpoint
            .unwrap()
            .block_number,
        10
    );
    assert_eq!(
        read_projection_state(config(10), retired.clone())
            .unwrap()
            .unwrap()
            .checkpoint
            .unwrap()
            .block_number,
        11
    );
    let expected = RetainedTributeReader::new(before.clone())
        .get_current_or_retained(pin, id, commitment)
        .unwrap()
        .unwrap()
        .encode();
    assert_eq!(
        RetainedTributeReader::new(retired.clone())
            .get_current_or_retained(pin, id, commitment)
            .unwrap()
            .unwrap()
            .encode(),
        expected
    );
    // Projection and post-release GC use the same primary capability.
    let gc = RetainedTributeWriter::new(storage.clone(), storage.clone());
    assert!(gc.release_input_lease_page(pin.input_lease_id).unwrap());
    assert!(gc.release_input_lease_page(pin.input_lease_id).unwrap());
    // GC does not change an already-open export inventory's view.
    assert_eq!(
        RetainedTributeReader::new(retired.clone())
            .get_current_or_retained(pin, id, commitment)
            .unwrap()
            .unwrap()
            .encode(),
        expected
    );
    let after = Arc::new(RocksDbReader::open(&path, &root.path().join("after")).unwrap());
    assert!(RetainedTributeReader::new(after)
        .get_current_or_retained(pin, id, commitment)
        .unwrap()
        .is_none());
    drop((gc, projection, storage, before, retired));
    let reopened = Arc::new(RocksDbStorage::open(&path).unwrap());
    assert_eq!(
        read_projection_state(config(10), reopened.clone())
            .unwrap()
            .unwrap()
            .checkpoint
            .unwrap()
            .block_number,
        11
    );
    assert!(RetainedTributeReader::new(reopened)
        .get_current_or_retained(pin, id, commitment)
        .unwrap()
        .is_none());
}
