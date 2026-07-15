use std::collections::BTreeMap;
use std::sync::Arc;

use alloy_primitives::{Address, Bytes, LogData, B256, U256};
use alloy_sol_types::SolEvent;
use outbe_common::WorldwideDay;
use outbe_compressed_entities::{
    body_commitment, encode_nod_bucket_v1, encode_nod_item_v1, encode_tribute_v1, CommitmentState,
    EntityId36, ACTIVE_COMMITMENT_SCHEME, BODY_SCHEMA_V1,
};
use outbe_nod::{
    canonical_bucket, canonical_item, precompile::INod, NodBucketState, NodContract, NodItemState,
    NodRepositoryReader,
};
use outbe_offchain_data::{
    FinalizedBlock, FinalizedLog, FinalizedReceipt, OffchainDataProjection, ProjectionConfig,
};
use outbe_offchain_storage::MemoryStorage;
use outbe_primitives::addresses::{NOD_ADDRESS, TRIBUTE_ADDRESS};
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
use outbe_tribute::{
    canonical_body, precompile::ITribute, TributeContract, TributeData, TributeRepositoryReader,
};

#[derive(Default)]
struct ObserverCommitments {
    tributes: BTreeMap<EntityId36, B256>,
    nod_items: BTreeMap<EntityId36, B256>,
    nod_buckets: BTreeMap<EntityId36, B256>,
}

impl ObserverCommitments {
    fn replay_block(&mut self, block: &FinalizedBlock) {
        for receipt in &block.receipts {
            assert!(receipt.success);
            for log in &receipt.logs {
                self.replay_log(log.emitter, &log.data);
            }
        }
    }

    fn replay_log(&mut self, emitter: Address, data: &LogData) {
        let Some(signature) = data.topics().first().copied() else {
            return;
        };
        if emitter == TRIBUTE_ADDRESS && signature == ITribute::TributeBodyStored::SIGNATURE_HASH {
            let event = ITribute::TributeBodyStored::decode_log_data(data).unwrap();
            replay_stored(
                &mut self.tributes,
                parse_id(&event.tributeId),
                event.commitmentSchemeVersion,
                event.schemaVersion,
                event.previousCommitment,
                event.newCommitment,
                &event.canonicalPayload,
            );
        } else if emitter == TRIBUTE_ADDRESS
            && signature == ITribute::TributeBodyDeleted::SIGNATURE_HASH
        {
            let event = ITribute::TributeBodyDeleted::decode_log_data(data).unwrap();
            replay_deleted(
                &mut self.tributes,
                parse_id(&event.tributeId),
                event.previousCommitment,
            );
        } else if emitter == NOD_ADDRESS && signature == INod::NodBodyStored::SIGNATURE_HASH {
            let event = INod::NodBodyStored::decode_log_data(data).unwrap();
            replay_stored(
                &mut self.nod_items,
                parse_id(&event.nodId),
                event.commitmentSchemeVersion,
                event.schemaVersion,
                event.previousCommitment,
                event.newCommitment,
                &event.canonicalPayload,
            );
        } else if emitter == NOD_ADDRESS && signature == INod::NodBodyDeleted::SIGNATURE_HASH {
            let event = INod::NodBodyDeleted::decode_log_data(data).unwrap();
            replay_deleted(
                &mut self.nod_items,
                parse_id(&event.nodId),
                event.previousCommitment,
            );
        } else if emitter == NOD_ADDRESS && signature == INod::NodBucketBodyStored::SIGNATURE_HASH {
            let event = INod::NodBucketBodyStored::decode_log_data(data).unwrap();
            replay_stored(
                &mut self.nod_buckets,
                parse_id(&event.bucketId),
                event.commitmentSchemeVersion,
                event.schemaVersion,
                event.previousCommitment,
                event.newCommitment,
                &event.canonicalPayload,
            );
        } else if emitter == NOD_ADDRESS && signature == INod::NodBucketBodyDeleted::SIGNATURE_HASH
        {
            let event = INod::NodBucketBodyDeleted::decode_log_data(data).unwrap();
            replay_deleted(
                &mut self.nod_buckets,
                parse_id(&event.bucketId),
                event.previousCommitment,
            );
        }
    }
}

fn parse_id(bytes: &Bytes) -> EntityId36 {
    EntityId36::try_from(bytes.as_ref()).unwrap()
}

fn replay_stored(
    commitments: &mut BTreeMap<EntityId36, B256>,
    identity: EntityId36,
    scheme: u32,
    schema: u32,
    previous: B256,
    new: B256,
    payload: &[u8],
) {
    assert_eq!(
        commitments.get(&identity).copied().unwrap_or(B256::ZERO),
        previous
    );
    let recomputed = body_commitment(scheme, schema, identity, payload).unwrap();
    assert_eq!(new, B256::from(*recomputed.as_bytes()));
    assert!(!new.is_zero());
    commitments.insert(identity, new);
}

fn replay_deleted(
    commitments: &mut BTreeMap<EntityId36, B256>,
    identity: EntityId36,
    previous: B256,
) {
    assert!(!previous.is_zero());
    assert_eq!(commitments.remove(&identity), Some(previous));
}

fn finalized_block(execution: &mut HashMapStorageProvider, number: u64) -> FinalizedBlock {
    let logs = execution
        .get_ordered_events()
        .iter()
        .enumerate()
        .map(|(index, event)| FinalizedLog {
            log_index: u64::try_from(index).unwrap(),
            emitter: event.address,
            data: event.data.clone(),
        })
        .collect();
    execution.clear_events(TRIBUTE_ADDRESS);
    execution.clear_events(NOD_ADDRESS);
    FinalizedBlock {
        number,
        hash: B256::from(U256::from(number)),
        receipts: vec![FinalizedReceipt {
            tx_hash: B256::from(U256::from(number + 100)),
            transaction_index: 0,
            success: true,
            logs,
        }],
    }
}

fn emit_tribute_update(storage: &StorageHandle<'_>, body: &TributeData) {
    let commitments = CommitmentState::new(storage.clone());
    let previous = commitments.tribute(body.tribute_id).unwrap().unwrap();
    let payload = encode_tribute_v1(&canonical_body(body)).unwrap();
    let new = body_commitment(
        ACTIVE_COMMITMENT_SCHEME,
        BODY_SCHEMA_V1,
        body.tribute_id,
        &payload,
    )
    .unwrap();
    commitments.set_tribute(body.tribute_id, new).unwrap();
    storage
        .emit_event(
            TRIBUTE_ADDRESS,
            ITribute::TributeBodyStored {
                tributeId: Bytes::copy_from_slice(body.tribute_id.as_bytes()),
                commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
                schemaVersion: BODY_SCHEMA_V1,
                previousCommitment: B256::from(*previous.as_bytes()),
                newCommitment: B256::from(*new.as_bytes()),
                canonicalPayload: Bytes::from(payload),
            }
            .encode_log_data(),
        )
        .unwrap();
}

fn emit_nod_item_update(storage: &StorageHandle<'_>, body: &NodItemState) {
    let commitments = CommitmentState::new(storage.clone());
    let previous = commitments.nod_item(body.nod_id).unwrap().unwrap();
    let payload = encode_nod_item_v1(&canonical_item(body)).unwrap();
    let new = body_commitment(
        ACTIVE_COMMITMENT_SCHEME,
        BODY_SCHEMA_V1,
        body.nod_id,
        &payload,
    )
    .unwrap();
    commitments.set_nod_item(body.nod_id, new).unwrap();
    storage
        .emit_event(
            NOD_ADDRESS,
            INod::NodBodyStored {
                nodId: Bytes::copy_from_slice(body.nod_id.as_bytes()),
                commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
                schemaVersion: BODY_SCHEMA_V1,
                previousCommitment: B256::from(*previous.as_bytes()),
                newCommitment: B256::from(*new.as_bytes()),
                canonicalPayload: Bytes::from(payload),
            }
            .encode_log_data(),
        )
        .unwrap();
}

fn as_b256(commitment: Option<outbe_compressed_entities::Commitment>) -> Option<B256> {
    commitment.map(|value| B256::from(*value.as_bytes()))
}

fn assert_execution_map(
    execution: &mut HashMapStorageProvider,
    observer: &ObserverCommitments,
    tribute_id: EntityId36,
    nod_id: EntityId36,
    bucket_id: EntityId36,
) {
    StorageHandle::enter(execution, |storage| {
        let state = CommitmentState::new(storage);
        assert_eq!(
            as_b256(state.tribute(tribute_id).unwrap()),
            observer.tributes.get(&tribute_id).copied()
        );
        assert_eq!(
            as_b256(state.nod_item(nod_id).unwrap()),
            observer.nod_items.get(&nod_id).copied()
        );
        assert_eq!(
            as_b256(state.nod_bucket(bucket_id).unwrap()),
            observer.nod_buckets.get(&bucket_id).copied()
        );
    });
}

#[test]
fn replay_from_genesis_converges_for_mint_update_and_delete_in_all_namespaces() {
    let projected = Arc::new(MemoryStorage::new());
    let tribute_reader = TributeRepositoryReader::new(projected.clone());
    let nod_reader = NodRepositoryReader::new(projected.clone());
    let mut projection = OffchainDataProjection::open(
        ProjectionConfig {
            chain_id: 91,
            genesis_hash: B256::repeat_byte(0x91),
            start_block: 1,
        },
        projected.clone(),
        projected.clone(),
    )
    .unwrap();

    let owner = Address::repeat_byte(0x41);
    let day = WorldwideDay::new(20_260_716);
    let tribute_id = outbe_compressed_entities::derive_poseidon_entity_id(owner, day).unwrap();
    let mut tribute = TributeData {
        tribute_id,
        owner,
        worldwide_day: day,
        issuance_amount_minor: U256::from(10),
        issuance_currency: 840,
        nominal_amount_minor: U256::from(11),
        reference_currency: 978,
        tribute_price_minor: U256::from(12),
        exclude_from_intex_issuance: false,
    };
    let nod_owner = Address::repeat_byte(0x42);
    let nod_id = outbe_compressed_entities::derive_poseidon_entity_id(nod_owner, day).unwrap();
    let bucket_key = NodContract::bucket_key(day, U256::from(14));
    let mut nod = NodItemState {
        nod_id,
        owner: nod_owner,
        gratis_load_minor: U256::from(13),
        worldwide_day: day,
        league_id: 7,
        floor_price_minor: U256::from(14),
        bucket_key,
        cost_amount_minor: U256::from(15),
        issuance_currency: 840,
        reference_currency: 978,
        issued_at: 1_752_534_000,
    };
    let bucket_id = EntityId36::new(day, bucket_key.0);
    let mut execution = HashMapStorageProvider::new(1);
    let mut observer = ObserverCommitments::default();

    // Block 1: normal domain mint paths emit all three Stored namespaces.
    StorageHandle::enter(&mut execution, |storage| {
        let mut contract = TributeContract::new(storage.clone());
        contract.unseal_day(day).unwrap();
        contract.issue(&tribute_reader, &tribute).unwrap();
        outbe_nod::api::add_nod(&storage, &nod_reader, &nod, U256::from(16)).unwrap();
    });
    let block1 = finalized_block(&mut execution, 1);
    observer.replay_block(&block1);
    projection.project_block(&block1).unwrap();
    assert_execution_map(&mut execution, &observer, tribute_id, nod_id, bucket_id);

    // Block 2: update each namespace. Tribute and Nod item currently have no
    // public domain update method, so the fixture performs their exact journaled
    // mapping/event transition directly. Bucket qualification uses the real hook path.
    tribute.tribute_price_minor += U256::from(1);
    nod.cost_amount_minor += U256::from(1);
    StorageHandle::enter(&mut execution, |storage| {
        emit_tribute_update(&storage, &tribute);
        emit_nod_item_update(&storage, &nod);
        NodContract::new(storage)
            .qualify_bucket_with_reader(&nod_reader, bucket_key)
            .unwrap();
    });
    let block2 = finalized_block(&mut execution, 2);
    observer.replay_block(&block2);
    projection.project_block(&block2).unwrap();
    assert_execution_map(&mut execution, &observer, tribute_id, nod_id, bucket_id);

    let projected_tribute = tribute_reader.get(tribute_id).unwrap().unwrap();
    let projected_nod = nod_reader.get(nod_id).unwrap().unwrap();
    let projected_bucket = nod_reader.get_bucket(bucket_id).unwrap().unwrap();
    let expected_bucket = NodBucketState {
        bucket_key,
        worldwide_day: day,
        floor_price_minor: nod.floor_price_minor,
        is_qualified: true,
        total_nods: 1,
        entry_price_minor: U256::from(16),
    };
    assert_eq!(
        encode_tribute_v1(&canonical_body(&projected_tribute)).unwrap(),
        encode_tribute_v1(&canonical_body(&tribute)).unwrap()
    );
    assert_eq!(
        encode_nod_item_v1(&canonical_item(&projected_nod)).unwrap(),
        encode_nod_item_v1(&canonical_item(&nod)).unwrap()
    );
    assert_eq!(
        encode_nod_bucket_v1(&canonical_bucket(&projected_bucket)).unwrap(),
        encode_nod_bucket_v1(&canonical_bucket(&expected_bucket)).unwrap()
    );

    // Block 3: normal domain delete paths clear all three mappings and emit all
    // three Deleted namespaces from the updated projected bodies.
    StorageHandle::enter(&mut execution, |storage| {
        TributeContract::new(storage.clone())
            .burn(&tribute_reader, tribute_id)
            .unwrap();
        outbe_nod::api::remove_nod(&storage, &nod_reader, &projected_nod).unwrap();
    });
    let block3 = finalized_block(&mut execution, 3);
    observer.replay_block(&block3);
    projection.project_block(&block3).unwrap();
    assert_execution_map(&mut execution, &observer, tribute_id, nod_id, bucket_id);

    assert!(observer.tributes.is_empty());
    assert!(observer.nod_items.is_empty());
    assert!(observer.nod_buckets.is_empty());
    assert!(tribute_reader.get(tribute_id).unwrap().is_none());
    assert!(nod_reader.get(nod_id).unwrap().is_none());
    assert!(nod_reader.get_bucket(bucket_id).unwrap().is_none());
    assert_eq!(projection.state().checkpoint.unwrap().block_number, 3);
}
