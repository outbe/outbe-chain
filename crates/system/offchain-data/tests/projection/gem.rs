use super::gem_support::*;
use super::support::{log, nod_stored, open, single_receipt_block, RecordingStorage};
use alloy_primitives::{Address, Bytes, B256};
use alloy_sol_types::SolEvent;
use outbe_compressed_entities::{encode_gem_v1, IdPageRequest, StoredBody};
use outbe_offchain_data::{ProjectionError, ProjectionSource};
use outbe_primitives::addresses::{GEM_ADDRESS, NOD_ADDRESS};
use std::sync::Arc;

#[test]
fn gem_log_projects_canonical_body_and_rejects_tampered_commitment() {
    let body = item(0xff);
    let valid = stored(&body, B256::ZERO);
    for mutation in [
        "commitment",
        "identity",
        "scheme",
        "schema",
        "payload",
        "failed",
    ] {
        let storage = Arc::new(RecordingStorage::default());
        let mut projection = open(&storage, 1);
        let before = storage.batches();
        let mut event = valid.clone();
        match mutation {
            "commitment" => event.newCommitment = B256::ZERO,
            "identity" => event.gemId += alloy_primitives::U256::from(1),
            "scheme" => event.commitmentSchemeVersion += 1,
            "schema" => event.schemaVersion += 1,
            "payload" => {
                event.canonicalPayload =
                    Bytes::from([event.canonicalPayload.as_ref(), &[0]].concat())
            }
            "failed" => {}
            _ => unreachable!(),
        }
        let mut block =
            single_receipt_block(1, 1, 2, vec![log(0, GEM_ADDRESS, event.encode_log_data())]);
        block.receipts[0].success = mutation != "failed";
        assert!(projection.project_block(&block).is_err(), "{mutation}");
        assert_eq!(storage.batches(), before);
        assert!(projection.state().checkpoint.is_none());
    }
    let storage = Arc::new(RecordingStorage::default());
    let mut projection = open(&storage, 1);
    projection
        .project_block(&single_receipt_block(
            1,
            1,
            2,
            vec![log(0, GEM_ADDRESS, valid.encode_log_data())],
        ))
        .unwrap();
    let reader = outbe_gem::gem_reader(storage.clone());
    assert_eq!(
        reader.get_stored_item(body.gem_id).unwrap().unwrap(),
        StoredBody::new(1, encode_gem_v1(&body).unwrap()).unwrap()
    );
    let (_, metadata) = reader.get_with_metadata(body.gem_id).unwrap().unwrap();
    assert_eq!(
        ProjectionSource::from_storage_metadata(&metadata.unwrap())
            .unwrap()
            .emitter,
        GEM_ADDRESS
    );
    assert_eq!(
        storage.batches().last().unwrap().last().unwrap(),
        "projection_state"
    );
}

#[test]
fn gem_id_is_not_a_worldwide_day_and_still_projects() {
    let storage = Arc::new(RecordingStorage::default());
    let mut projection = open(&storage, 1);
    let body = item(0xff);
    projection
        .project_block(&single_receipt_block(
            1,
            1,
            1,
            vec![log(
                0,
                GEM_ADDRESS,
                stored(&body, B256::ZERO).encode_log_data(),
            )],
        ))
        .unwrap();
    assert!(outbe_gem::gem_reader(storage.clone())
        .get_item(body.gem_id)
        .unwrap()
        .is_some());
    let before = storage.batches();
    let valid_id = super::support::poseidon_entity(body.owner, 20260715);
    let mut bad_nod = outbe_nod::precompile::INod::NodBodyStored::decode_log_data(&nod_stored(
        valid_id,
        body.owner,
        body.bucket_key,
    ))
    .unwrap();
    bad_nod.nodId = body.gem_id.to_u256();
    assert!(matches!(
        projection.project_block(&single_receipt_block(
            2,
            2,
            2,
            vec![log(0, NOD_ADDRESS, bad_nod.encode_log_data())]
        )),
        Err(ProjectionError::MalformedProjectionEvent { .. })
    ));
    assert_eq!(storage.batches(), before);
}

#[test]
fn gem_item_and_bucket_share_one_receipt_batch_and_all_four_events_decode() {
    let storage = Arc::new(RecordingStorage::default());
    let mut projection = open(&storage, 1);
    let body = item(7);
    let bucket = bucket();
    for data in [
        stored(&body, B256::ZERO).encode_log_data(),
        bucket_stored(&bucket, B256::ZERO).encode_log_data(),
        deleted(&body),
        bucket_deleted(&bucket),
    ] {
        let before = storage.batches();
        let mut failed = single_receipt_block(1, 1, 1, vec![log(0, GEM_ADDRESS, data)]);
        failed.receipts[0].success = false;
        assert!(matches!(
            projection.project_block(&failed),
            Err(ProjectionError::ProjectionLogInFailedReceipt(_))
        ));
        assert_eq!(storage.batches(), before);
    }
    let prepared = projection
        .prepare_block(&single_receipt_block(
            1,
            1,
            1,
            vec![
                log(0, GEM_ADDRESS, stored(&body, B256::ZERO).encode_log_data()),
                log(
                    1,
                    GEM_ADDRESS,
                    bucket_stored(&bucket, B256::ZERO).encode_log_data(),
                ),
            ],
        ))
        .unwrap();
    assert_eq!(prepared.receipts().len(), 1);
    let names: Vec<_> = prepared.receipts()[0]
        .batch()
        .operations()
        .iter()
        .map(|op| match op {
            outbe_offchain_storage::AtomicWriteOperation::Put { namespace, .. }
            | outbe_offchain_storage::AtomicWriteOperation::Delete { namespace, .. } => {
                namespace.as_str()
            }
        })
        .collect();
    assert_eq!(names, ["gems", "gems_by_owner", "gem_buckets"]);
    projection.apply_prepared(prepared).unwrap();
    projection
        .project_block(&single_receipt_block(
            2,
            2,
            2,
            vec![
                log(0, GEM_ADDRESS, deleted(&body)),
                log(1, GEM_ADDRESS, bucket_deleted(&bucket)),
            ],
        ))
        .unwrap();
    let reader = outbe_gem::gem_reader(storage);
    assert!(reader.get_item(body.gem_id).unwrap().is_none());
    assert!(reader.get_bucket(bucket.entity_id()).unwrap().is_none());
    assert!(reader
        .list_ids_by_owner(
            body.owner,
            IdPageRequest {
                after: None,
                limit: 10
            }
        )
        .unwrap()
        .ids
        .is_empty());
}

#[test]
fn gem_transitions_validate_the_in_block_overlay_and_accept_first_finalized_previous() {
    let body = item(7);
    let mut updated = body.clone();
    updated.owner = Address::repeat_byte(0x55);
    for valid in [false, true] {
        let storage = Arc::new(RecordingStorage::default());
        let mut projection = open(&storage, 1);
        let first = stored(&body, B256::with_last_byte(1));
        let second = stored(
            &updated,
            if valid {
                first.newCommitment
            } else {
                B256::ZERO
            },
        );
        let before = storage.batches();
        let result = projection.project_block(&single_receipt_block(
            1,
            1,
            1,
            vec![
                log(0, GEM_ADDRESS, first.encode_log_data()),
                log(1, GEM_ADDRESS, second.encode_log_data()),
            ],
        ));
        if valid {
            result.unwrap();
            let reader = outbe_gem::gem_reader(storage);
            assert_eq!(
                reader.get_item(body.gem_id).unwrap().unwrap().owner,
                updated.owner
            );
            assert!(reader
                .list_ids_by_owner(
                    body.owner,
                    IdPageRequest {
                        after: None,
                        limit: 1
                    }
                )
                .unwrap()
                .ids
                .is_empty());
        } else {
            assert!(matches!(
                result,
                Err(ProjectionError::CommitmentTransitionMismatch { .. })
            ));
            assert_eq!(storage.batches(), before);
        }
    }
}

#[test]
fn malformed_gem_bucket_and_delete_inputs_reject_the_whole_block() {
    use outbe_gem::precompile::IGem;
    let body = item(7);
    let bucket = bucket();
    let mut events = Vec::new();
    for mutation in ["identity", "commitment", "schema", "scheme", "payload"] {
        let mut event = bucket_stored(&bucket, B256::ZERO);
        match mutation {
            "identity" => event.bucketId += alloy_primitives::U256::from(1),
            "commitment" => event.newCommitment = B256::ZERO,
            "schema" => event.schemaVersion += 1,
            "scheme" => event.commitmentSchemeVersion += 1,
            "payload" => event.canonicalPayload = Bytes::new(),
            _ => unreachable!(),
        }
        events.push(event.encode_log_data());
    }
    events.push(
        IGem::GemBodyDeleted {
            gemId: body.gem_id.to_u256(),
            previousCommitment: B256::ZERO,
        }
        .encode_log_data(),
    );
    events.push(
        IGem::GemBucketBodyDeleted {
            bucketId: bucket.entity_id().to_u256(),
            previousCommitment: B256::ZERO,
        }
        .encode_log_data(),
    );
    for data in events {
        let storage = Arc::new(RecordingStorage::default());
        let mut projection = open(&storage, 1);
        let before = storage.batches();
        let block = single_receipt_block(
            1,
            1,
            1,
            vec![
                log(0, GEM_ADDRESS, stored(&body, B256::ZERO).encode_log_data()),
                log(1, GEM_ADDRESS, data),
            ],
        );
        assert!(matches!(
            projection.project_block(&block),
            Err(ProjectionError::MalformedProjectionEvent { .. })
        ));
        assert_eq!(storage.batches(), before);
        assert!(projection.state().checkpoint.is_none());
    }
}
