use std::{
    io,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use alloy_primitives::{Address, Bytes, LogData, B256, U256};
use alloy_sol_types::SolEvent;
use outbe_compressed_entities::{
    body_commitment, derive_poseidon_entity_id, encode_nod_bucket_v1, encode_nod_item_v2,
    encode_tribute_v1, WwdEntityId, ACTIVE_COMMITMENT_SCHEME, BODY_SCHEMA_V1,
};
use outbe_nod::{canonical_bucket, canonical_item, precompile::INod, NodBucketState, NodItemState};
use outbe_offchain_data::{
    FinalizedLog, FinalizedReceipt, OffchainDataProjection, ProjectionConfig,
    TributeRetentionSelector,
};
use outbe_offchain_storage::{
    AtomicWriteBatch, AtomicWriteOperation, MemoryStorage, StorageError, StorageWriter,
};
use outbe_primitives::time::WorldwideDay;
use outbe_tribute::{canonical_body, precompile::ITribute, RetainedTributePin, TributeData};

#[derive(Default)]
pub(crate) struct RecordingStorage {
    pub(crate) inner: MemoryStorage,
    pub(crate) applied_namespaces: Mutex<Vec<Vec<String>>>,
}

impl RecordingStorage {
    pub(crate) fn batches(&self) -> Vec<Vec<String>> {
        self.applied_namespaces.lock().unwrap().clone()
    }
}

outbe_offchain_storage::impl_test_storage_reader!(RecordingStorage, inner);

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

pub(crate) fn config(start_block: u64) -> ProjectionConfig {
    ProjectionConfig {
        chain_id: 91,
        genesis_hash: B256::repeat_byte(0x91),
        start_block,
    }
}

pub(crate) fn open(storage: &Arc<RecordingStorage>, start_block: u64) -> OffchainDataProjection {
    outbe_offchain_data::open_projection(config(start_block), storage.clone(), storage.clone())
        .unwrap()
}

#[derive(Clone, Copy)]
pub(crate) struct FixedRetentionSelector {
    pub(crate) pin: RetainedTributePin,
}

impl TributeRetentionSelector for FixedRetentionSelector {
    fn active_pin_for(
        &self,
        worldwide_day: WorldwideDay,
    ) -> Result<Option<RetainedTributePin>, String> {
        Ok((self.pin.worldwide_day == worldwide_day).then_some(self.pin))
    }
}

pub(crate) fn receipt(index: u64, hash_byte: u8, logs: Vec<FinalizedLog>) -> FinalizedReceipt {
    FinalizedReceipt {
        tx_hash: B256::repeat_byte(hash_byte),
        transaction_index: index,
        success: true,
        logs,
    }
}

pub(crate) fn log(index: u64, emitter: Address, data: LogData) -> FinalizedLog {
    FinalizedLog {
        log_index: index,
        emitter,
        data,
    }
}

pub(crate) fn entity(seed: u64, day: u32) -> WwdEntityId {
    WwdEntityId::from_day_and_digest(WorldwideDay::new(day), U256::from(seed).to_be_bytes::<32>())
}

pub(crate) fn poseidon_entity(owner: Address, day: u32) -> WwdEntityId {
    derive_poseidon_entity_id(owner, WorldwideDay::new(day)).unwrap()
}

pub(crate) fn tribute_body(tribute_id: WwdEntityId, owner: Address, day: u32) -> TributeData {
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

pub(crate) fn tribute_commitment(body: &TributeData) -> B256 {
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

pub(crate) fn tribute_stored(tribute_id: WwdEntityId, owner: Address, day: u32) -> LogData {
    tribute_stored_after(tribute_id, owner, day, B256::ZERO)
}

pub(crate) fn tribute_stored_after(
    tribute_id: WwdEntityId,
    owner: Address,
    day: u32,
    previous: B256,
) -> LogData {
    let body = tribute_body(tribute_id, owner, day);
    tribute_stored_body_after(&body, previous)
}

pub(crate) fn tribute_stored_body_after(body: &TributeData, previous: B256) -> LogData {
    let payload = encode_tribute_v1(&canonical_body(body)).unwrap();
    let new_commitment = tribute_commitment(body);
    ITribute::TributeBodyStored {
        tributeId: body.tribute_id.to_u256(),
        commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
        schemaVersion: BODY_SCHEMA_V1,
        previousCommitment: previous,
        newCommitment: new_commitment,
        canonicalPayload: Bytes::from(payload),
    }
    .encode_log_data()
}

pub(crate) fn tribute_partition_retired(day: u32) -> LogData {
    ITribute::TributePartitionRetired { worldwideDay: day }.encode_log_data()
}

pub(crate) fn nod_body(nod_id: WwdEntityId, owner: Address, bucket_key: B256) -> NodItemState {
    outbe_nod::test_support::item(
        outbe_nod::test_support::NodItemFixture {
            is_settled: false,
            nod_id,
            owner,
            gratis_load_minor: U256::from(101),
            worldwide_day: WorldwideDay::new(20260715),
            league_id: 7,
            bucket_key,
            issuance_currency: 840,
            reference_currency: 978,
            issued_at: 123_456,
        },
        U256::from(5),
    )
}

pub(crate) fn nod_deleted(nod_id: WwdEntityId, owner: Address, bucket_key: B256) -> LogData {
    let body = nod_body(nod_id, owner, bucket_key);
    let payload = encode_nod_item_v2(&canonical_item(&body)).unwrap();
    let commitment = body_commitment(
        ACTIVE_COMMITMENT_SCHEME,
        outbe_compressed_entities::NOD_BODY_SCHEMA_V2,
        nod_id,
        &payload,
    )
    .unwrap();
    INod::NodBodyDeleted {
        nodId: nod_id.to_u256(),
        previousCommitment: B256::from(*commitment.as_bytes()),
    }
    .encode_log_data()
}

pub(crate) fn nod_stored(nod_id: WwdEntityId, owner: Address, bucket_key: B256) -> LogData {
    let body = nod_body(nod_id, owner, bucket_key);
    let payload = encode_nod_item_v2(&canonical_item(&body)).unwrap();
    let commitment = body_commitment(
        ACTIVE_COMMITMENT_SCHEME,
        outbe_compressed_entities::NOD_BODY_SCHEMA_V2,
        nod_id,
        &payload,
    )
    .unwrap();
    INod::NodBodyStored {
        nodId: nod_id.to_u256(),
        commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
        schemaVersion: outbe_compressed_entities::NOD_BODY_SCHEMA_V2,
        previousCommitment: B256::ZERO,
        newCommitment: B256::from(*commitment.as_bytes()),
        canonicalPayload: Bytes::from(payload),
    }
    .encode_log_data()
}

pub(crate) fn bucket_body(bucket_key: B256) -> NodBucketState {
    NodBucketState {
        settled_nods: 0,
        bucket_key,
        worldwide_day: WorldwideDay::new(20260715),
        entry_price_minor: U256::from(104),
        reference_currency: 978,
    }
}

pub(crate) fn bucket_stored(bucket_key: B256) -> LogData {
    let body = bucket_body(bucket_key);
    let bucket_id = WwdEntityId::from_day_and_digest(body.worldwide_day, bucket_key.0);
    let payload = encode_nod_bucket_v1(&canonical_bucket(&body)).unwrap();
    let commitment = body_commitment(
        ACTIVE_COMMITMENT_SCHEME,
        BODY_SCHEMA_V1,
        bucket_id,
        &payload,
    )
    .unwrap();
    INod::NodBucketBodyStored {
        bucketId: bucket_id.to_u256(),
        commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
        schemaVersion: BODY_SCHEMA_V1,
        previousCommitment: B256::ZERO,
        newCommitment: B256::from(*commitment.as_bytes()),
        canonicalPayload: Bytes::from(payload),
    }
    .encode_log_data()
}

#[derive(Default)]
pub(crate) struct FailOnceStorage {
    pub(crate) inner: MemoryStorage,
    pub(crate) call: AtomicUsize,
    pub(crate) fail_on: AtomicUsize,
}

impl FailOnceStorage {
    pub(crate) fn arm(&self, fail_on: usize) {
        self.call.store(0, Ordering::SeqCst);
        self.fail_on.store(fail_on, Ordering::SeqCst);
    }
}

outbe_offchain_storage::impl_test_storage_reader!(FailOnceStorage, inner);

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
