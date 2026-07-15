use std::sync::Arc;

use alloy_primitives::{Address, U256};
use outbe_common::WorldwideDay;
use outbe_compressed_entities::{
    body_commitment, derive_poseidon_entity_id, encode_tribute_v1, CommitmentState, EntityId36,
    StoredBody, ACTIVE_COMMITMENT_SCHEME, BODY_SCHEMA_V1,
};
use outbe_offchain_storage::{
    Key, MemoryStorage, Namespace, ScanPage, ScanRequest, StorageError, StorageReader,
    StorageReaderHandle, StorageWriter, StorageWriterHandle, StoredValue, Value,
};
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_tribute::{
    TributeContract, TributeData, TributeRepositoryReader, TributeRepositoryWriter,
};

fn tribute(owner: Address, day: u32) -> TributeData {
    let worldwide_day = WorldwideDay::from(day);
    TributeData {
        tribute_id: derive_poseidon_entity_id(owner, worldwide_day).unwrap(),
        owner,
        worldwide_day,
        issuance_amount_minor: U256::from(200),
        issuance_currency: 840,
        nominal_amount_minor: U256::from(100),
        reference_currency: 840,
        tribute_price_minor: U256::from(2),
        exclude_from_intex_issuance: false,
    }
}

fn copy_tribute(body: &TributeData) -> TributeData {
    TributeData {
        tribute_id: body.tribute_id,
        owner: body.owner,
        worldwide_day: body.worldwide_day,
        issuance_amount_minor: body.issuance_amount_minor,
        issuance_currency: body.issuance_currency,
        nominal_amount_minor: body.nominal_amount_minor,
        reference_currency: body.reference_currency,
        tribute_price_minor: body.tribute_price_minor,
        exclude_from_intex_issuance: body.exclude_from_intex_issuance,
    }
}

fn commit(storage: StorageHandle<'_>, body: &TributeData) {
    let payload = encode_tribute_v1(&outbe_tribute::canonical_body(body)).unwrap();
    let commitment = body_commitment(
        ACTIVE_COMMITMENT_SCHEME,
        BODY_SCHEMA_V1,
        body.tribute_id,
        &payload,
    )
    .unwrap();
    CommitmentState::new(storage)
        .set_tribute(body.tribute_id, commitment)
        .unwrap();
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
    let first = tribute(owner, day.value());
    let second = tribute(owner, day.value() + 1);
    let third = tribute(Address::repeat_byte(0x12), day.value());
    writer.put(&first).unwrap();
    writer.put(&second).unwrap();
    writer.put(&third).unwrap();

    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        commit(storage.clone(), &first);
        commit(storage.clone(), &second);
        commit(storage.clone(), &third);
        let contract = TributeContract::new(storage);
        assert_eq!(contract.owner_of(&reader, first.tribute_id).unwrap(), owner);
        assert_eq!(contract.balance_of(&reader, owner).unwrap(), 2);
        assert_eq!(
            contract.get_tribute_ids_by_owner(&reader, owner).unwrap(),
            vec![first.tribute_id, second.tribute_id]
        );
        let mut expected_day = vec![first.tribute_id, third.tribute_id];
        expected_day.sort_unstable();
        assert_eq!(
            contract.get_tribute_ids_by_day(&reader, day).unwrap(),
            expected_day
        );
        assert!(contract
            .token_uri(&reader, first.tribute_id)
            .unwrap()
            .contains("Outbe Tribute"));
    });
}

#[test]
fn issue_checks_repository_for_duplicates_and_never_writes_a_body() {
    let (reader, writer) = repository();
    let body = tribute(Address::repeat_byte(0x22), 20_241_220);
    let mut provider = HashMapStorageProvider::new(1);

    StorageHandle::enter(&mut provider, |storage| {
        let mut contract = TributeContract::new(storage);
        contract.unseal_day(body.worldwide_day).unwrap();
        contract.issue(&reader, &body).unwrap();
        assert_eq!(contract.total_supply().unwrap(), 1);
        assert!(matches!(
            contract.get_tribute(&reader, body.tribute_id),
            Err(PrecompileError::BodyReadCorruption(message))
                if message.contains("CommittedBodyMissing")
        ));
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
    let body = tribute(Address::repeat_byte(0x33), 20_241_220);
    let mut provider = HashMapStorageProvider::new(1);

    StorageHandle::enter(&mut provider, |storage| {
        let mut contract = TributeContract::new(storage);
        contract.unseal_day(body.worldwide_day).unwrap();
        contract.issue(&reader, &body).unwrap();
    });
    writer.put(&body).unwrap();

    StorageHandle::enter(&mut provider, |storage| {
        let mut contract = TributeContract::new(storage);
        contract.burn(&reader, body.tribute_id).unwrap();
        assert_eq!(contract.total_supply().unwrap(), 0);
        let totals = contract.get_day_totals(body.worldwide_day).unwrap();
        assert_eq!(totals.tribute_count, 0);
        assert_eq!(totals.tribute_nominal_amount, U256::ZERO);
    });

    assert_eq!(
        reader.get(body.tribute_id).unwrap().unwrap().owner,
        body.owner
    );
}

#[test]
fn absence_corruption_and_unavailability_remain_distinct() {
    let (reader, _) = repository();
    let tribute_id = EntityId36::new(
        WorldwideDay::from(20_241_220),
        U256::from(77).to_be_bytes::<32>(),
    );
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let contract = TributeContract::new(storage);
        let error = contract.owner_of(&reader, tribute_id).unwrap_err();
        assert!(
            matches!(error, PrecompileError::Revert(message) if message == "tribute not found")
        );
    });

    let corrupt = Arc::new(MemoryStorage::new());
    let corrupt_writer: StorageWriterHandle = corrupt.clone();
    corrupt_writer
        .put(
            Namespace::new("tributes").unwrap(),
            &Key::new(tribute_id.as_bytes().to_vec()).unwrap(),
            &Value::new([0xff]).unwrap(),
        )
        .unwrap();
    let corrupt_reader = TributeRepositoryReader::new(corrupt);
    let error = match corrupt_reader.get(tribute_id) {
        Ok(_) => panic!("corrupt body must fail"),
        Err(error) => PrecompileError::from(error),
    };
    assert!(matches!(error, PrecompileError::BodyReadCorruption(_)));

    let unavailable_reader = TributeRepositoryReader::new(Arc::new(UnavailableReader));
    let error = match unavailable_reader.get(tribute_id) {
        Ok(_) => panic!("unavailable storage must fail"),
        Err(error) => PrecompileError::from(error),
    };
    assert!(matches!(error, PrecompileError::BodyReadUnavailable(_)));

    let backend_reader = TributeRepositoryReader::new(Arc::new(BackendErrorReader));
    let error = match backend_reader.get(tribute_id) {
        Ok(_) => panic!("unclassified backend error must fail"),
        Err(error) => PrecompileError::from(error),
    };
    assert!(matches!(error, PrecompileError::BodyReadCorruption(_)));
}

#[test]
fn every_tribute_body_input_schema_envelope_and_evm_leaf_is_authenticated() {
    let original = tribute(Address::repeat_byte(0x61), 20_260_716);
    let original_payload = encode_tribute_v1(&outbe_tribute::canonical_body(&original)).unwrap();

    let mut mutations = Vec::new();
    let mut changed = copy_tribute(&original);
    changed.tribute_id = EntityId36::new(original.worldwide_day, [0x91; 32]);
    mutations.push(("tribute_id", changed));
    let mut changed = copy_tribute(&original);
    changed.owner = Address::repeat_byte(0x62);
    mutations.push(("owner", changed));
    let mut changed = copy_tribute(&original);
    changed.worldwide_day = WorldwideDay::from(original.worldwide_day.value() + 1);
    mutations.push(("worldwide_day", changed));
    let mut changed = copy_tribute(&original);
    changed.issuance_amount_minor += U256::from(1);
    mutations.push(("issuance_amount_minor", changed));
    let mut changed = copy_tribute(&original);
    changed.issuance_currency += 1;
    mutations.push(("issuance_currency", changed));
    let mut changed = copy_tribute(&original);
    changed.nominal_amount_minor += U256::from(1);
    mutations.push(("nominal_amount_minor", changed));
    let mut changed = copy_tribute(&original);
    changed.reference_currency += 1;
    mutations.push(("reference_currency", changed));
    let mut changed = copy_tribute(&original);
    changed.tribute_price_minor += U256::from(1);
    mutations.push(("tribute_price_minor", changed));
    let mut changed = copy_tribute(&original);
    changed.exclude_from_intex_issuance = !changed.exclude_from_intex_issuance;
    mutations.push(("exclude_from_intex_issuance", changed));

    for (field, changed) in mutations {
        let payload = match encode_tribute_v1(&outbe_tribute::canonical_body(&changed)) {
            Ok(payload) => payload,
            Err(_) => {
                assert_eq!(field, "worldwide_day");
                continue;
            }
        };
        assert_raw_tribute_is_rejected(
            &original,
            StoredBody::new_v1(payload).unwrap().encode(),
            field,
        );
    }

    assert_raw_tribute_is_rejected(
        &original,
        StoredBody::new(2, original_payload.clone())
            .unwrap()
            .encode(),
        "schema_version",
    );
    let mut noncanonical_payload = original_payload.clone();
    noncanonical_payload.extend_from_slice(&[0x50, 0x01]);
    assert_raw_tribute_is_rejected(
        &original,
        StoredBody::new_v1(noncanonical_payload).unwrap().encode(),
        "stored_payload",
    );

    let storage = Arc::new(MemoryStorage::new());
    let writer = TributeRepositoryWriter::new(storage.clone(), storage.clone());
    writer.put(&original).unwrap();
    let reader = TributeRepositoryReader::new(storage);
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |evm| {
        let mut updated = copy_tribute(&original);
        updated.tribute_price_minor += U256::from(1);
        let updated_payload = encode_tribute_v1(&outbe_tribute::canonical_body(&updated)).unwrap();
        let updated_commitment = body_commitment(
            ACTIVE_COMMITMENT_SCHEME,
            BODY_SCHEMA_V1,
            original.tribute_id,
            &updated_payload,
        )
        .unwrap();
        CommitmentState::new(evm.clone())
            .set_tribute(original.tribute_id, updated_commitment)
            .unwrap();
        assert!(matches!(
            TributeContract::new(evm).get_tribute(&reader, original.tribute_id),
            Err(PrecompileError::BodyReadCorruption(_))
        ));
    });
}

fn assert_raw_tribute_is_rejected(original: &TributeData, stored: Vec<u8>, field: &str) {
    let storage = Arc::new(MemoryStorage::new());
    storage
        .put(
            Namespace::new("tributes").unwrap(),
            &Key::new(original.tribute_id.as_bytes().to_vec()).unwrap(),
            &Value::new(stored).unwrap(),
        )
        .unwrap();
    let reader = TributeRepositoryReader::new(storage);
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |evm| {
        commit(evm.clone(), original);
        assert!(
            matches!(
                TributeContract::new(evm).get_tribute(&reader, original.tribute_id),
                Err(PrecompileError::BodyReadCorruption(_))
            ),
            "mutation of {field} must fail before domain use"
        );
    });
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
