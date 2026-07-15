use std::sync::Arc;

use alloy_primitives::{Address, U256};
use outbe_common::WorldwideDay;
use outbe_offchain_storage::{
    Key, MemoryStorage, Namespace, ScanPage, ScanRequest, StorageError, StorageReader,
    StorageReaderHandle, StorageWriterHandle, StoredValue, Value,
};
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_tribute::{
    TributeContract, TributeData, TributeRepositoryReader, TributeRepositoryWriter,
};

fn tribute(token_id: u64, owner: Address, day: u32) -> TributeData {
    TributeData {
        token_id: U256::from(token_id),
        owner,
        worldwide_day: WorldwideDay::from(day),
        issuance_amount_minor: U256::from(200),
        issuance_currency: 840,
        nominal_amount_minor: U256::from(100),
        reference_currency: 840,
        tribute_price_minor: U256::from(2),
        exclude_from_intex_issuance: false,
    }
}

fn repository() -> (TributeRepositoryReader, TributeRepositoryWriter) {
    let storage = Arc::new(MemoryStorage::new());
    let reader: StorageReaderHandle = storage.clone();
    let writer: StorageWriterHandle = storage;
    (
        TributeRepositoryReader::new(reader.clone()),
        TributeRepositoryWriter::new(reader, writer),
    )
}

#[test]
fn body_and_index_reads_use_the_repository() {
    let (reader, writer) = repository();
    let owner = Address::repeat_byte(0x11);
    let day = WorldwideDay::from(20_241_220);
    let first = tribute(1, owner, day.value());
    let second = tribute(2, owner, day.value());
    writer.put(&first).unwrap();
    writer.put(&second).unwrap();

    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let contract = TributeContract::new(storage);
        assert_eq!(contract.owner_of(&reader, first.token_id).unwrap(), owner);
        assert_eq!(contract.balance_of(&reader, owner).unwrap(), 2);
        assert_eq!(
            contract.get_tribute_ids_by_owner(&reader, owner).unwrap(),
            vec![first.token_id, second.token_id]
        );
        assert_eq!(
            contract.get_tribute_ids_by_day(&reader, day).unwrap(),
            vec![first.token_id, second.token_id]
        );
        assert!(contract
            .token_uri(&reader, first.token_id)
            .unwrap()
            .contains("Outbe Tribute"));
    });
}

#[test]
fn issue_checks_repository_for_duplicates_and_never_writes_a_body() {
    let (reader, writer) = repository();
    let body = tribute(7, Address::repeat_byte(0x22), 20_241_220);
    let mut provider = HashMapStorageProvider::new(1);

    StorageHandle::enter(&mut provider, |storage| {
        let mut contract = TributeContract::new(storage);
        contract.unseal_day(body.worldwide_day).unwrap();
        contract.issue(&reader, &body).unwrap();
        assert_eq!(contract.total_supply().unwrap(), 1);
        assert!(contract
            .get_tribute(&reader, body.token_id)
            .unwrap()
            .is_none());
    });

    writer.put(&body).unwrap();
    StorageHandle::enter(&mut provider, |storage| {
        let mut contract = TributeContract::new(storage);
        let error = contract.issue(&reader, &body).unwrap_err();
        assert!(
            matches!(error, PrecompileError::Revert(message) if message == "tribute already exists")
        );
        assert_eq!(contract.total_supply().unwrap(), 1);
    });
}

#[test]
fn burn_reads_repository_and_leaves_projection_writes_to_the_projector() {
    let (reader, writer) = repository();
    let body = tribute(9, Address::repeat_byte(0x33), 20_241_220);
    let mut provider = HashMapStorageProvider::new(1);

    StorageHandle::enter(&mut provider, |storage| {
        let mut contract = TributeContract::new(storage);
        contract.unseal_day(body.worldwide_day).unwrap();
        contract.issue(&reader, &body).unwrap();
    });
    writer.put(&body).unwrap();

    StorageHandle::enter(&mut provider, |storage| {
        let mut contract = TributeContract::new(storage);
        contract.burn(&reader, body.token_id).unwrap();
        assert_eq!(contract.total_supply().unwrap(), 0);
        let totals = contract.get_day_totals(body.worldwide_day).unwrap();
        assert_eq!(totals.tribute_count, 0);
        assert_eq!(totals.tribute_nominal_amount, U256::ZERO);
    });

    assert_eq!(
        reader.get(body.token_id).unwrap().unwrap().owner,
        body.owner
    );
}

#[test]
fn absence_corruption_and_unavailability_remain_distinct() {
    let (reader, _) = repository();
    let token_id = U256::from(77);
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let contract = TributeContract::new(storage);
        let error = contract.owner_of(&reader, token_id).unwrap_err();
        assert!(
            matches!(error, PrecompileError::Revert(message) if message == "tribute not found")
        );
    });

    let corrupt = Arc::new(MemoryStorage::new());
    let corrupt_writer: StorageWriterHandle = corrupt.clone();
    corrupt_writer
        .put(
            Namespace::new("tributes").unwrap(),
            &Key::new(token_id.to_be_bytes::<32>()).unwrap(),
            &Value::new([0xff]).unwrap(),
        )
        .unwrap();
    let corrupt_reader = TributeRepositoryReader::new(corrupt);
    let error = match corrupt_reader.get(token_id) {
        Ok(_) => panic!("corrupt body must fail"),
        Err(error) => PrecompileError::from(error),
    };
    assert!(matches!(error, PrecompileError::BodyReadCorruption(_)));

    let unavailable_reader = TributeRepositoryReader::new(Arc::new(UnavailableReader));
    let error = match unavailable_reader.get(token_id) {
        Ok(_) => panic!("unavailable storage must fail"),
        Err(error) => PrecompileError::from(error),
    };
    assert!(matches!(error, PrecompileError::BodyReadUnavailable(_)));

    let backend_reader = TributeRepositoryReader::new(Arc::new(BackendErrorReader));
    let error = match backend_reader.get(token_id) {
        Ok(_) => panic!("unclassified backend error must fail"),
        Err(error) => PrecompileError::from(error),
    };
    assert!(matches!(error, PrecompileError::BodyReadCorruption(_)));
}

struct UnavailableReader;

struct BackendErrorReader;

impl StorageReader for UnavailableReader {
    fn get_record(
        &self,
        _namespace: Namespace,
        _key: &Key,
    ) -> Result<Option<StoredValue>, StorageError> {
        Err(StorageError::Unavailable {
            source: Box::new(std::io::Error::other("test backend unavailable")),
        })
    }

    fn scan_prefix(
        &self,
        _namespace: Namespace,
        _request: ScanRequest<'_>,
    ) -> Result<ScanPage, StorageError> {
        Err(StorageError::Unavailable {
            source: Box::new(std::io::Error::other("test backend unavailable")),
        })
    }
}

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
