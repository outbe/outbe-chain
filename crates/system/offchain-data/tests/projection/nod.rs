use std::sync::Arc;

use alloy_primitives::{Address, B256};
use alloy_sol_types::SolEvent;
use outbe_compressed_entities::{
    body_commitment, encode_nod_bucket_v1, encode_nod_item_v2, WwdEntityId,
    ACTIVE_COMMITMENT_SCHEME, BODY_SCHEMA_V1,
};
use outbe_nod::{
    canonical_bucket, canonical_item, precompile::INod, NodPageRequest, NodRepositoryReader,
};
use outbe_offchain_data::FinalizedBlock;
use outbe_primitives::addresses::{NOD_ADDRESS, TRIBUTE_ADDRESS};
use outbe_primitives::time::WorldwideDay;
use outbe_tribute::precompile::ITribute;

use super::support::*;

#[test]
fn nod_item_and_bucket_share_one_receipt_batch_and_all_six_events_decode() {
    let storage = Arc::new(RecordingStorage::default());
    let mut projection = open(&storage, 30);
    let bucket_key = B256::repeat_byte(0xbc);
    let owner = Address::repeat_byte(0x77);
    let nod_id = poseidon_entity(owner, 20260715);
    let bucket_id = WwdEntityId::from_day_and_digest(WorldwideDay::new(20260715), bucket_key.0);
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
                        nodId: nod_id.to_u256(),
                        previousCommitment: {
                            let body = nod_body(nod_id, owner, bucket_key);
                            let payload = encode_nod_item_v2(&canonical_item(&body)).unwrap();
                            B256::from(
                                *body_commitment(
                                    ACTIVE_COMMITMENT_SCHEME,
                                    outbe_compressed_entities::NOD_BODY_SCHEMA_V2,
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
                        bucketId: bucket_id.to_u256(),
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
                        tributeId: entity(1234, 20260715).to_u256(),
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
