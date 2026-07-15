use std::sync::Arc;

use alloy_primitives::{address, Address, B256, U256};
use alloy_sol_types::SolCall;
use outbe_common::WorldwideDay;
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

#[test]
fn nod_execution_reads_bodies_from_repository_when_evm_body_indexes_are_empty() {
    let adapter = Arc::new(MemoryStorage::new());
    let reader = NodRepositoryReader::new(adapter.clone());
    let writer = NodRepositoryWriter::new(adapter.clone(), adapter);
    let owner = address!("1111111111111111111111111111111111111111");
    let nod_id = U256::from(7);
    let bucket_key = B256::repeat_byte(0x42);
    writer
        .put_bucket(&NodBucketState {
            bucket_key,
            worldwide_day: WorldwideDay::new(20260715),
            floor_price_minor: U256::from(10),
            is_qualified: true,
            total_nods: 1,
            entry_price_minor: U256::from(9),
        })
        .unwrap();
    writer
        .put_nod(&NodItemState {
            nod_id,
            owner,
            gratis_load_minor: U256::from(11),
            worldwide_day: WorldwideDay::new(20260715),
            league_id: 3,
            floor_price_minor: U256::from(10),
            bucket_key,
            cost_amount_minor: U256::from(12),
            issuance_currency: 840,
            reference_currency: 978,
            issued_at: 1_700_000_000,
        })
        .unwrap();

    let mut evm = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut evm, |storage| {
        let owner_call = INod::ownerOfCall { nodId: nod_id }.abi_encode();
        let raw = precompile::dispatch_with_reader(
            storage.clone(),
            &owner_call,
            Address::ZERO,
            U256::ZERO,
            &reader,
        )
        .unwrap();
        assert_eq!(INod::ownerOfCall::abi_decode_returns(&raw).unwrap(), owner);

        let data_call = INod::nodDataCall { nodId: nod_id }.abi_encode();
        let raw = precompile::dispatch_with_reader(
            storage,
            &data_call,
            Address::ZERO,
            U256::ZERO,
            &reader,
        )
        .unwrap();
        let data = INod::nodDataCall::abi_decode_returns(&raw).unwrap();
        assert_eq!(data.nodId, nod_id);
        assert_eq!(data.owner, owner);
        assert!(data.isQualified);
        assert_eq!(data.costOfGratisMinor, U256::from(9));
    });
}

#[test]
fn missing_nod_is_the_existing_not_found_revert() {
    let adapter = Arc::new(MemoryStorage::new());
    let reader = NodRepositoryReader::new(adapter);
    let mut evm = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut evm, |storage| {
        let call = INod::ownerOfCall {
            nodId: U256::from(404),
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
    let nod_id = U256::from(7);
    adapter
        .put(
            Namespace::new("nods").unwrap(),
            &Key::new(nod_id.to_be_bytes::<32>()).unwrap(),
            &Value::new(vec![0xff]).unwrap(),
        )
        .unwrap();
    let reader = NodRepositoryReader::new(adapter);
    let mut evm = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut evm, |storage| {
        let call = INod::ownerOfCall { nodId: nod_id }.abi_encode();
        let error =
            precompile::dispatch_with_reader(storage, &call, Address::ZERO, U256::ZERO, &reader)
                .unwrap_err();
        assert!(matches!(error, PrecompileError::BodyReadCorruption(_)));
    });
}

#[test]
fn unclassified_backend_error_is_fatal_not_retryable_unavailability() {
    let reader = NodRepositoryReader::new(Arc::new(BackendErrorReader));
    let error = match reader.get(U256::from(7)) {
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
        let item = NodItemState {
            nod_id: U256::from(7),
            owner: Address::repeat_byte(0x22),
            gratis_load_minor: U256::from(1),
            worldwide_day: WorldwideDay::new(20_260_715),
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
