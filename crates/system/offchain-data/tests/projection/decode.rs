use std::sync::Arc;

use alloy_primitives::{Address, Bytes, LogData, B256};
use alloy_sol_types::SolEvent;
use outbe_compressed_entities::{
    body_commitment, encode_nod_bucket_v1, encode_nod_item_v2, encode_tribute_v1, WwdEntityId,
    ACTIVE_COMMITMENT_SCHEME, BODY_SCHEMA_V1,
};
use outbe_nod::{canonical_bucket, canonical_item, precompile::INod};
use outbe_offchain_data::{FinalizedBlock, ProjectionError};
use outbe_primitives::addresses::{NOD_ADDRESS, TRIBUTE_ADDRESS};
use outbe_primitives::time::WorldwideDay;
use outbe_tribute::{canonical_body, precompile::ITribute, TributeRepositoryReader};

use super::support::*;

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
            vec![
                log(
                    0,
                    NOD_ADDRESS,
                    tribute_stored(ignored_id, Address::ZERO, 20260715),
                ),
                log(1, NOD_ADDRESS, tribute_partition_retired(20260715)),
            ],
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
            tributeId: tribute_id.to_u256(),
            commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
            schemaVersion: BODY_SCHEMA_V1,
            previousCommitment: B256::ZERO,
            newCommitment: B256::repeat_byte(0x44),
            canonicalPayload: Bytes::copy_from_slice(&payload),
        }
        .encode_log_data(),
        ITribute::TributeBodyStored {
            tributeId: entity(82, 20260715).to_u256(),
            commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
            schemaVersion: BODY_SCHEMA_V1,
            previousCommitment: B256::ZERO,
            newCommitment: tribute_commitment(&body),
            canonicalPayload: Bytes::copy_from_slice(&payload),
        }
        .encode_log_data(),
        ITribute::TributeBodyStored {
            tributeId: tribute_id.to_u256(),
            commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME + 1,
            schemaVersion: BODY_SCHEMA_V1,
            previousCommitment: B256::ZERO,
            newCommitment: tribute_commitment(&body),
            canonicalPayload: Bytes::copy_from_slice(&payload),
        }
        .encode_log_data(),
        ITribute::TributeBodyStored {
            tributeId: tribute_id.to_u256(),
            commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
            schemaVersion: BODY_SCHEMA_V1 + 1,
            previousCommitment: B256::ZERO,
            newCommitment: tribute_commitment(&body),
            canonicalPayload: Bytes::copy_from_slice(&payload),
        }
        .encode_log_data(),
        ITribute::TributeBodyStored {
            tributeId: tribute_id.to_u256(),
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
    let nod_payload = encode_nod_item_v2(&canonical_item(&nod)).unwrap();
    let nod_commitment = B256::from(
        *body_commitment(
            ACTIVE_COMMITMENT_SCHEME,
            outbe_compressed_entities::NOD_BODY_SCHEMA_V2,
            nod_id,
            &nod_payload,
        )
        .unwrap()
        .as_bytes(),
    );

    let bucket_key = B256::repeat_byte(0xa2);
    let bucket_id = WwdEntityId::from_day_and_digest(WorldwideDay::new(day), bucket_key.0);
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

    let mut noncanonical_nod = nod_payload.clone();
    noncanonical_nod.extend_from_slice(&[0x60, 0x01]);
    let mut noncanonical_bucket = bucket_payload.clone();
    noncanonical_bucket.extend_from_slice(&[0x38, 0x01]);

    let malformed_events = vec![
        (
            NOD_ADDRESS,
            INod::NodBodyStored {
                nodId: nod_id.to_u256(),
                commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME + 1,
                schemaVersion: outbe_compressed_entities::NOD_BODY_SCHEMA_V2,
                previousCommitment: B256::ZERO,
                newCommitment: nod_commitment,
                canonicalPayload: Bytes::copy_from_slice(&nod_payload),
            }
            .encode_log_data(),
        ),
        (
            NOD_ADDRESS,
            INod::NodBodyStored {
                nodId: nod_id.to_u256(),
                commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
                schemaVersion: outbe_compressed_entities::NOD_BODY_SCHEMA_V2 + 1,
                previousCommitment: B256::ZERO,
                newCommitment: nod_commitment,
                canonicalPayload: Bytes::copy_from_slice(&nod_payload),
            }
            .encode_log_data(),
        ),
        (
            NOD_ADDRESS,
            INod::NodBodyStored {
                nodId: nod_id.to_u256(),
                commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
                schemaVersion: outbe_compressed_entities::NOD_BODY_SCHEMA_V2,
                previousCommitment: B256::ZERO,
                newCommitment: nod_commitment,
                canonicalPayload: Bytes::from(noncanonical_nod),
            }
            .encode_log_data(),
        ),
        (
            NOD_ADDRESS,
            INod::NodBodyStored {
                nodId: entity(0x93, day).to_u256(),
                commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
                schemaVersion: outbe_compressed_entities::NOD_BODY_SCHEMA_V2,
                previousCommitment: B256::ZERO,
                newCommitment: nod_commitment,
                canonicalPayload: Bytes::copy_from_slice(&nod_payload),
            }
            .encode_log_data(),
        ),
        (
            NOD_ADDRESS,
            INod::NodBodyStored {
                nodId: nod_id.to_u256(),
                commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
                schemaVersion: outbe_compressed_entities::NOD_BODY_SCHEMA_V2,
                previousCommitment: B256::ZERO,
                newCommitment: B256::ZERO,
                canonicalPayload: Bytes::copy_from_slice(&nod_payload),
            }
            .encode_log_data(),
        ),
        (
            NOD_ADDRESS,
            INod::NodBucketBodyStored {
                bucketId: bucket_id.to_u256(),
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
                bucketId: bucket_id.to_u256(),
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
                bucketId: bucket_id.to_u256(),
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
                bucketId: entity(0x94, day).to_u256(),
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
                bucketId: bucket_id.to_u256(),
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
                tributeId: tribute_id.to_u256(),
                previousCommitment: B256::ZERO,
            }
            .encode_log_data(),
        ),
        (
            NOD_ADDRESS,
            INod::NodBodyDeleted {
                nodId: nod_id.to_u256(),
                previousCommitment: B256::ZERO,
            }
            .encode_log_data(),
        ),
        (
            NOD_ADDRESS,
            INod::NodBucketBodyDeleted {
                bucketId: bucket_id.to_u256(),
                previousCommitment: B256::ZERO,
            }
            .encode_log_data(),
        ),
    ];

    // Three cases that fed a wrong-width `bytes` identity are gone. A uint256
    // identity has no malformed encoding. The ABI type, not this projector,
    // now enforces the rejection that those cases proved.
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
                nodId: nod_id.to_u256(),
                commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
                schemaVersion: outbe_compressed_entities::NOD_BODY_SCHEMA_V2,
                previousCommitment: B256::ZERO,
                newCommitment: nod_commitment,
                canonicalPayload: Bytes::copy_from_slice(&nod_payload),
            }
            .encode_log_data(),
            INod::NodBodyStored {
                nodId: nod_id.to_u256(),
                commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
                schemaVersion: outbe_compressed_entities::NOD_BODY_SCHEMA_V2,
                previousCommitment: bucket_commitment,
                newCommitment: nod_commitment,
                canonicalPayload: Bytes::copy_from_slice(&nod_payload),
            }
            .encode_log_data(),
        ),
        (
            NOD_ADDRESS,
            INod::NodBucketBodyStored {
                bucketId: bucket_id.to_u256(),
                commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
                schemaVersion: BODY_SCHEMA_V1,
                previousCommitment: B256::ZERO,
                newCommitment: bucket_commitment,
                canonicalPayload: Bytes::copy_from_slice(&bucket_payload),
            }
            .encode_log_data(),
            INod::NodBucketBodyStored {
                bucketId: bucket_id.to_u256(),
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
