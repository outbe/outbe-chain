use std::sync::Arc;

use alloy_primitives::{address, Address, Bytes, B256, U256};
use alloy_sol_types::SolCall;
use outbe_common::WorldwideDay;
use outbe_compressed_entities::{
    body_commitment, encode_nod_bucket_v1, encode_nod_item_v1, Commitment, CommitmentState,
    EntityId36, StoredBody, ACTIVE_COMMITMENT_SCHEME, BODY_SCHEMA_V1,
};
use outbe_nod::{
    precompile::{self, INod},
    NodBucketState, NodItemState, NodRepositoryReader, NodRepositoryWriter,
};
use outbe_offchain_storage::{
    Key, MemoryStorage, Namespace, ScanPage, ScanRequest, StorageError, StorageReader,
    StorageWriter, StoredValue, Value,
};
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
use outbe_primitives::{
    block::{BlockContext, BlockRuntimeContext},
    error::PrecompileError,
};

fn entity_id(owner: Address, day: WorldwideDay) -> EntityId36 {
    outbe_nod::NodContract::generate_nod_id(owner, day).unwrap()
}

fn bucket_id(bucket: &NodBucketState) -> EntityId36 {
    EntityId36::new(bucket.worldwide_day, bucket.bucket_key.0)
}

fn copy_item(body: &NodItemState) -> NodItemState {
    NodItemState {
        nod_id: body.nod_id,
        owner: body.owner,
        gratis_load_minor: body.gratis_load_minor,
        worldwide_day: body.worldwide_day,
        league_id: body.league_id,
        floor_price_minor: body.floor_price_minor,
        bucket_key: body.bucket_key,
        cost_amount_minor: body.cost_amount_minor,
        issuance_currency: body.issuance_currency,
        reference_currency: body.reference_currency,
        issued_at: body.issued_at,
    }
}

fn copy_bucket(body: &NodBucketState) -> NodBucketState {
    NodBucketState {
        bucket_key: body.bucket_key,
        worldwide_day: body.worldwide_day,
        floor_price_minor: body.floor_price_minor,
        is_qualified: body.is_qualified,
        total_nods: body.total_nods,
        entry_price_minor: body.entry_price_minor,
    }
}

fn commit_item(storage: StorageHandle<'_>, item: &NodItemState) -> Commitment {
    let payload = encode_nod_item_v1(&outbe_nod::canonical_item(item)).unwrap();
    let commitment = body_commitment(
        ACTIVE_COMMITMENT_SCHEME,
        BODY_SCHEMA_V1,
        item.nod_id,
        &payload,
    )
    .unwrap();
    CommitmentState::new(storage)
        .set_nod_item(item.nod_id, commitment)
        .unwrap();
    commitment
}

fn commit_bucket(storage: StorageHandle<'_>, bucket: &NodBucketState) -> Commitment {
    let identity = bucket_id(bucket);
    let payload = encode_nod_bucket_v1(&outbe_nod::canonical_bucket(bucket)).unwrap();
    let commitment =
        body_commitment(ACTIVE_COMMITMENT_SCHEME, BODY_SCHEMA_V1, identity, &payload).unwrap();
    CommitmentState::new(storage)
        .set_nod_bucket(identity, commitment)
        .unwrap();
    commitment
}

#[test]
fn nod_execution_reads_bodies_from_repository_when_evm_body_indexes_are_empty() {
    let adapter = Arc::new(MemoryStorage::new());
    let reader = NodRepositoryReader::new(adapter.clone());
    let writer = NodRepositoryWriter::new(adapter.clone(), adapter);
    let owner = address!("1111111111111111111111111111111111111111");
    let day = WorldwideDay::new(20260715);
    let nod_id = entity_id(owner, day);
    let bucket_key = B256::repeat_byte(0x42);
    let bucket = NodBucketState {
        bucket_key,
        worldwide_day: day,
        floor_price_minor: U256::from(10),
        is_qualified: true,
        total_nods: 1,
        entry_price_minor: U256::from(9),
    };
    writer.put_bucket(&bucket).unwrap();
    let item = NodItemState {
        nod_id,
        owner,
        gratis_load_minor: U256::from(11),
        worldwide_day: day,
        league_id: 3,
        floor_price_minor: U256::from(10),
        bucket_key,
        cost_amount_minor: U256::from(12),
        issuance_currency: 840,
        reference_currency: 978,
        issued_at: 1_700_000_000,
    };
    writer.put_nod(&item).unwrap();

    let mut evm = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut evm, |storage| {
        let item_commitment = commit_item(storage.clone(), &item);
        let bucket_commitment = commit_bucket(storage.clone(), &bucket);
        let commitments = CommitmentState::new(storage.clone());
        assert_eq!(commitments.nod_item(nod_id).unwrap(), Some(item_commitment));
        assert_eq!(
            commitments.nod_bucket(bucket_id(&bucket)).unwrap(),
            Some(bucket_commitment)
        );

        let owner_call = INod::ownerOfCall {
            nodId: Bytes::copy_from_slice(nod_id.as_bytes()),
        }
        .abi_encode();
        let raw = precompile::dispatch_with_reader(
            storage.clone(),
            &owner_call,
            Address::ZERO,
            U256::ZERO,
            &reader,
        )
        .unwrap();
        assert_eq!(INod::ownerOfCall::abi_decode_returns(&raw).unwrap(), owner);

        let data_call = INod::nodDataCall {
            nodId: Bytes::copy_from_slice(nod_id.as_bytes()),
        }
        .abi_encode();
        let raw = precompile::dispatch_with_reader(
            storage.clone(),
            &data_call,
            Address::ZERO,
            U256::ZERO,
            &reader,
        )
        .unwrap();
        let data = INod::nodDataCall::abi_decode_returns(&raw).unwrap();
        assert_eq!(data.nodId.as_ref(), nod_id.as_bytes());
        assert_eq!(data.owner, owner);
        assert!(data.isQualified);
        assert_eq!(data.costOfGratisMinor, U256::from(9));

        let owner_index_call = INod::tokenOfOwnerByIndexCall {
            owner,
            index: U256::ZERO,
        }
        .abi_encode();
        let raw = precompile::dispatch_with_reader(
            storage.clone(),
            &owner_index_call,
            Address::ZERO,
            U256::ZERO,
            &reader,
        )
        .unwrap();
        assert_eq!(
            INod::tokenOfOwnerByIndexCall::abi_decode_returns(&raw)
                .unwrap()
                .as_ref(),
            nod_id.as_bytes()
        );

        let by_index_call = INod::tokenByIndexCall { index: U256::ZERO }.abi_encode();
        let raw = precompile::dispatch_with_reader(
            storage,
            &by_index_call,
            Address::ZERO,
            U256::ZERO,
            &reader,
        )
        .unwrap();
        assert_eq!(
            INod::tokenByIndexCall::abi_decode_returns(&raw)
                .unwrap()
                .as_ref(),
            nod_id.as_bytes()
        );
    });
}

#[test]
fn missing_nod_is_the_existing_not_found_revert() {
    let adapter = Arc::new(MemoryStorage::new());
    let reader = NodRepositoryReader::new(adapter);
    let nod_id = entity_id(Address::repeat_byte(0x40), WorldwideDay::new(20260715));
    let mut evm = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut evm, |storage| {
        let call = INod::ownerOfCall {
            nodId: Bytes::copy_from_slice(nod_id.as_bytes()),
        }
        .abi_encode();
        let error =
            precompile::dispatch_with_reader(storage, &call, Address::ZERO, U256::ZERO, &reader)
                .unwrap_err();
        assert!(matches!(error, PrecompileError::Revert(reason) if reason == "nod not found"));
    });
}

#[test]
fn corrupt_repository_body_is_fatal_not_an_absence_revert() {
    let adapter = Arc::new(MemoryStorage::new());
    let nod_id = entity_id(Address::repeat_byte(0x07), WorldwideDay::new(20260715));
    adapter
        .put(
            Namespace::new("nods").unwrap(),
            &Key::new(nod_id.as_bytes().to_vec()).unwrap(),
            &Value::new(vec![0xff]).unwrap(),
        )
        .unwrap();
    let reader = NodRepositoryReader::new(adapter);
    let mut evm = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut evm, |storage| {
        let expected = body_commitment(
            ACTIVE_COMMITMENT_SCHEME,
            BODY_SCHEMA_V1,
            nod_id,
            b"expected canonical body",
        )
        .unwrap();
        CommitmentState::new(storage.clone())
            .set_nod_item(nod_id, expected)
            .unwrap();
        let call = INod::ownerOfCall {
            nodId: Bytes::copy_from_slice(nod_id.as_bytes()),
        }
        .abi_encode();
        let error =
            precompile::dispatch_with_reader(storage, &call, Address::ZERO, U256::ZERO, &reader)
                .unwrap_err();
        assert!(matches!(error, PrecompileError::BodyReadCorruption(_)));
    });
}

#[test]
fn every_nod_item_input_schema_envelope_and_evm_leaf_is_authenticated() {
    let owner = Address::repeat_byte(0x71);
    let day = WorldwideDay::new(20_260_716);
    let original = NodItemState {
        nod_id: entity_id(owner, day),
        owner,
        gratis_load_minor: U256::from(1),
        worldwide_day: day,
        league_id: 2,
        floor_price_minor: U256::from(3),
        bucket_key: B256::repeat_byte(0x74),
        cost_amount_minor: U256::from(4),
        issuance_currency: 840,
        reference_currency: 978,
        issued_at: 5,
    };
    let original_payload = encode_nod_item_v1(&outbe_nod::canonical_item(&original)).unwrap();
    let mut mutations = Vec::new();
    let mut changed = copy_item(&original);
    changed.nod_id = EntityId36::new(day, [0x75; 32]);
    mutations.push(("nod_id", changed));
    let mut changed = copy_item(&original);
    changed.owner = Address::repeat_byte(0x76);
    mutations.push(("owner", changed));
    let mut changed = copy_item(&original);
    changed.gratis_load_minor += U256::from(1);
    mutations.push(("gratis_load_minor", changed));
    let mut changed = copy_item(&original);
    changed.worldwide_day = WorldwideDay::new(day.value() + 1);
    mutations.push(("worldwide_day", changed));
    let mut changed = copy_item(&original);
    changed.league_id += 1;
    mutations.push(("league_id", changed));
    let mut changed = copy_item(&original);
    changed.floor_price_minor += U256::from(1);
    mutations.push(("floor_price_minor", changed));
    let mut changed = copy_item(&original);
    changed.bucket_key = B256::repeat_byte(0x77);
    mutations.push(("bucket_key", changed));
    let mut changed = copy_item(&original);
    changed.cost_amount_minor += U256::from(1);
    mutations.push(("cost_amount_minor", changed));
    let mut changed = copy_item(&original);
    changed.issuance_currency += 1;
    mutations.push(("issuance_currency", changed));
    let mut changed = copy_item(&original);
    changed.reference_currency += 1;
    mutations.push(("reference_currency", changed));
    let mut changed = copy_item(&original);
    changed.issued_at += 1;
    mutations.push(("issued_at", changed));

    for (field, changed) in mutations {
        let payload = match encode_nod_item_v1(&outbe_nod::canonical_item(&changed)) {
            Ok(payload) => payload,
            Err(_) => {
                assert_eq!(field, "worldwide_day");
                continue;
            }
        };
        assert_raw_nod_item_is_rejected(
            &original,
            StoredBody::new_v1(payload).unwrap().encode(),
            field,
        );
    }
    assert_raw_nod_item_is_rejected(
        &original,
        StoredBody::new(2, original_payload.clone())
            .unwrap()
            .encode(),
        "schema_version",
    );
    let mut noncanonical = original_payload;
    noncanonical.extend_from_slice(&[0x60, 0x01]);
    assert_raw_nod_item_is_rejected(
        &original,
        StoredBody::new_v1(noncanonical).unwrap().encode(),
        "stored_payload",
    );

    let adapter = Arc::new(MemoryStorage::new());
    let writer = NodRepositoryWriter::new(adapter.clone(), adapter.clone());
    writer.put_nod(&original).unwrap();
    let reader = NodRepositoryReader::new(adapter);
    let mut evm = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut evm, |storage| {
        let mut updated = copy_item(&original);
        updated.cost_amount_minor += U256::from(1);
        let updated_payload = encode_nod_item_v1(&outbe_nod::canonical_item(&updated)).unwrap();
        let updated_commitment = body_commitment(
            ACTIVE_COMMITMENT_SCHEME,
            BODY_SCHEMA_V1,
            original.nod_id,
            &updated_payload,
        )
        .unwrap();
        CommitmentState::new(storage.clone())
            .set_nod_item(original.nod_id, updated_commitment)
            .unwrap();
        assert!(matches!(
            outbe_nod::api::get_item(&storage, &reader, original.nod_id),
            Err(PrecompileError::BodyReadCorruption(_))
        ));
    });
}

fn assert_raw_nod_item_is_rejected(original: &NodItemState, stored: Vec<u8>, field: &str) {
    let adapter = Arc::new(MemoryStorage::new());
    adapter
        .put(
            Namespace::new("nods").unwrap(),
            &Key::new(original.nod_id.as_bytes().to_vec()).unwrap(),
            &Value::new(stored).unwrap(),
        )
        .unwrap();
    let reader = NodRepositoryReader::new(adapter);
    let mut evm = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut evm, |storage| {
        commit_item(storage.clone(), original);
        assert!(
            matches!(
                outbe_nod::api::get_item(&storage, &reader, original.nod_id),
                Err(PrecompileError::BodyReadCorruption(_))
            ),
            "mutation of {field} must fail before domain use"
        );
    });
}

#[test]
fn every_nod_bucket_input_schema_envelope_and_evm_leaf_is_authenticated() {
    let original = NodBucketState {
        bucket_key: B256::repeat_byte(0x81),
        worldwide_day: WorldwideDay::new(20_260_716),
        floor_price_minor: U256::from(1),
        is_qualified: false,
        total_nods: 2,
        entry_price_minor: U256::from(3),
    };
    let original_payload = encode_nod_bucket_v1(&outbe_nod::canonical_bucket(&original)).unwrap();
    let mut mutations = Vec::new();
    let mut changed = copy_bucket(&original);
    changed.bucket_key = B256::repeat_byte(0x82);
    mutations.push(("bucket_key", changed));
    let mut changed = copy_bucket(&original);
    changed.worldwide_day = WorldwideDay::new(original.worldwide_day.value() + 1);
    mutations.push(("worldwide_day", changed));
    let mut changed = copy_bucket(&original);
    changed.floor_price_minor += U256::from(1);
    mutations.push(("floor_price_minor", changed));
    let mut changed = copy_bucket(&original);
    changed.is_qualified = true;
    mutations.push(("is_qualified", changed));
    let mut changed = copy_bucket(&original);
    changed.total_nods += 1;
    mutations.push(("total_nods", changed));
    let mut changed = copy_bucket(&original);
    changed.entry_price_minor += U256::from(1);
    mutations.push(("entry_price_minor", changed));

    for (field, changed) in mutations {
        let payload = encode_nod_bucket_v1(&outbe_nod::canonical_bucket(&changed)).unwrap();
        assert_raw_nod_bucket_is_rejected(
            &original,
            StoredBody::new_v1(payload).unwrap().encode(),
            field,
        );
    }
    assert_raw_nod_bucket_is_rejected(
        &original,
        StoredBody::new(2, original_payload.clone())
            .unwrap()
            .encode(),
        "schema_version",
    );
    let mut noncanonical = original_payload;
    noncanonical.extend_from_slice(&[0x38, 0x01]);
    assert_raw_nod_bucket_is_rejected(
        &original,
        StoredBody::new_v1(noncanonical).unwrap().encode(),
        "stored_payload",
    );

    let adapter = Arc::new(MemoryStorage::new());
    let writer = NodRepositoryWriter::new(adapter.clone(), adapter.clone());
    writer.put_bucket(&original).unwrap();
    let reader = NodRepositoryReader::new(adapter);
    let bucket_id = bucket_id(&original);
    let mut evm = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut evm, |storage| {
        let mut updated = copy_bucket(&original);
        updated.is_qualified = true;
        let updated_payload = encode_nod_bucket_v1(&outbe_nod::canonical_bucket(&updated)).unwrap();
        let updated_commitment = body_commitment(
            ACTIVE_COMMITMENT_SCHEME,
            BODY_SCHEMA_V1,
            bucket_id,
            &updated_payload,
        )
        .unwrap();
        CommitmentState::new(storage.clone())
            .set_nod_bucket(bucket_id, updated_commitment)
            .unwrap();
        assert!(matches!(
            outbe_nod::api::get_bucket(&storage, &reader, bucket_id),
            Err(PrecompileError::BodyReadCorruption(_))
        ));
    });
}

fn assert_raw_nod_bucket_is_rejected(original: &NodBucketState, stored: Vec<u8>, field: &str) {
    let bucket_id = bucket_id(original);
    let adapter = Arc::new(MemoryStorage::new());
    adapter
        .put(
            Namespace::new("nod_buckets").unwrap(),
            &Key::new(bucket_id.as_bytes().to_vec()).unwrap(),
            &Value::new(stored).unwrap(),
        )
        .unwrap();
    let reader = NodRepositoryReader::new(adapter);
    let mut evm = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut evm, |storage| {
        commit_bucket(storage.clone(), original);
        assert!(
            matches!(
                outbe_nod::api::get_bucket(&storage, &reader, bucket_id),
                Err(PrecompileError::BodyReadCorruption(_))
            ),
            "mutation of {field} must fail before domain use"
        );
    });
}

#[test]
fn unclassified_backend_error_is_fatal_not_retryable_unavailability() {
    let reader = NodRepositoryReader::new(Arc::new(BackendErrorReader));
    let error = match reader.get(entity_id(
        Address::repeat_byte(0x07),
        WorldwideDay::new(20260715),
    )) {
        Ok(_) => panic!("unclassified backend error must fail"),
        Err(error) => PrecompileError::from(error),
    };
    assert!(matches!(error, PrecompileError::BodyReadCorruption(_)));
}

struct BackendErrorReader;

impl StorageReader for BackendErrorReader {
    fn get_record(
        &self,
        _namespace: Namespace,
        _key: &Key,
    ) -> Result<Option<StoredValue>, StorageError> {
        Err(StorageError::Backend {
            source: Box::new(std::io::Error::other("deterministic backend failure")),
        })
    }

    fn scan_prefix(
        &self,
        _namespace: Namespace,
        _request: ScanRequest<'_>,
    ) -> Result<ScanPage, StorageError> {
        Err(StorageError::Backend {
            source: Box::new(std::io::Error::other("deterministic backend failure")),
        })
    }
}

#[test]
fn dangling_unqualified_bucket_index_is_body_corruption() {
    let adapter = Arc::new(MemoryStorage::new());
    let reader = NodRepositoryReader::new(adapter);
    let mut evm = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut evm, |storage| {
        let rate = U256::from(10);
        let bucket_key = B256::repeat_byte(0x42);
        let owner = Address::repeat_byte(0x22);
        let day = WorldwideDay::new(20_260_715);
        let item = NodItemState {
            nod_id: entity_id(owner, day),
            owner,
            gratis_load_minor: U256::from(1),
            worldwide_day: day,
            league_id: 1,
            floor_price_minor: U256::from(8),
            bucket_key,
            cost_amount_minor: U256::ZERO,
            issuance_currency: 840,
            reference_currency: 978,
            issued_at: 1_700_000_000,
        };
        outbe_nod::api::add_nod(&storage, &reader, &item, U256::from(5)).unwrap();

        let ctx =
            BlockRuntimeContext::new(BlockContext::empty_for_tests(2, 1_700_000_000, 1), storage);
        let error = outbe_nod::hooks::qualify_buckets_with_rate_and_reader(&ctx, &reader, rate)
            .unwrap_err();
        assert!(matches!(error, PrecompileError::BodyReadCorruption(_)));
    });
}

#[test]
fn reader_backed_qualification_is_bounded_and_resumes_next_block() {
    let adapter = Arc::new(MemoryStorage::new());
    let reader = NodRepositoryReader::new(adapter.clone());
    let writer = NodRepositoryWriter::new(adapter.clone(), adapter);
    let mut evm = HashMapStorageProvider::new(1);
    let limit = outbe_nod::constants::MAX_BUCKET_QUALIFICATIONS_PER_BLOCK;
    let day = WorldwideDay::new(20_260_715);

    StorageHandle::enter(&mut evm, |storage| {
        for offset in 0..=limit {
            let owner = Address::from_word(B256::from(U256::from(offset + 1)));
            let floor = U256::from(offset + 1);
            let bucket_key = outbe_nod::NodContract::bucket_key(day, floor);
            let item = NodItemState {
                nod_id: entity_id(owner, day),
                owner,
                gratis_load_minor: U256::from(1),
                worldwide_day: day,
                league_id: 1,
                floor_price_minor: floor,
                bucket_key,
                cost_amount_minor: U256::ZERO,
                issuance_currency: 840,
                reference_currency: 978,
                issued_at: 1_700_000_000,
            };
            outbe_nod::api::add_nod(&storage, &reader, &item, U256::from(1)).unwrap();
            writer.put_nod(&item).unwrap();
            writer
                .put_bucket(&NodBucketState {
                    bucket_key,
                    worldwide_day: day,
                    floor_price_minor: floor,
                    is_qualified: false,
                    total_nods: 1,
                    entry_price_minor: U256::from(1),
                })
                .unwrap();
        }
    });
    evm.clear_events(outbe_primitives::addresses::NOD_ADDRESS);

    StorageHandle::enter(&mut evm, |storage| {
        let ctx =
            BlockRuntimeContext::new(BlockContext::empty_for_tests(2, 1_700_000_000, 1), storage);
        outbe_nod::hooks::qualify_buckets_with_rate_and_reader(
            &ctx,
            &reader,
            U256::from(limit + 2),
        )
        .unwrap();
    });
    assert_eq!(
        evm.get_events(outbe_primitives::addresses::NOD_ADDRESS)
            .len(),
        usize::try_from(limit).unwrap() * 2,
    );

    evm.clear_events(outbe_primitives::addresses::NOD_ADDRESS);
    StorageHandle::enter(&mut evm, |storage| {
        let ctx =
            BlockRuntimeContext::new(BlockContext::empty_for_tests(3, 1_700_000_001, 1), storage);
        outbe_nod::hooks::qualify_buckets_with_rate_and_reader(
            &ctx,
            &reader,
            U256::from(limit + 2),
        )
        .unwrap();
    });
    assert_eq!(
        evm.get_events(outbe_primitives::addresses::NOD_ADDRESS)
            .len(),
        2,
    );
}
