use std::sync::Arc;

use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_common::WorldwideDay;
use outbe_compressed_entities::{
    begin_block, derive_poseidon_entity_id, end_block, mint, BodyInput, CandidateCacheLimits,
    CeMdbx, CeWorkConfig, CompressedTreeService, EntityId36, EnvironmentIdentity,
    ExactParentIdentity, ExecutionScope, FinalizedMarker, StoredBody, ACTIVE_COMMITMENT_SCHEME,
    LOCAL_STORAGE_SCHEMA_VERSION,
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

fn repository() -> (TributeRepositoryReader, TributeRepositoryWriter) {
    let storage = Arc::new(MemoryStorage::new());
    let reader: StorageReaderHandle = storage.clone();
    let writer: StorageWriterHandle = storage;
    (
        TributeRepositoryReader::new(reader.clone()),
        TributeRepositoryWriter::new(reader, writer),
    )
}

struct TreeHarness {
    _directory: tempfile::TempDir,
    service: Arc<CompressedTreeService>,
}

impl TreeHarness {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let genesis_hash = B256::repeat_byte(0xa0);
        let db = CeMdbx::open(
            directory.path(),
            EnvironmentIdentity {
                local_storage_schema_version: LOCAL_STORAGE_SCHEMA_VERSION,
                chain_id: 1,
                genesis_hash,
                commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                tree_format: "ckb-smt-v0.6.1-poseidon".to_owned(),
                vendor_revision: "ad555350c866b2265d87d2d7fbd146fbc918bfe5".to_owned(),
            },
            FinalizedMarker {
                commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                height: 0,
                block_hash: genesis_hash,
                parent_block_hash: B256::ZERO,
                parent_root: B256::ZERO,
                new_root: B256::ZERO,
            },
        )
        .unwrap();
        let service = Arc::new(
            CompressedTreeService::new(
                db,
                CandidateCacheLimits {
                    max_candidates: 4,
                    max_encoded_bytes: 1_000_000,
                },
            )
            .unwrap(),
        );
        Self {
            _directory: directory,
            service,
        }
    }

    fn activate(&self, provider: &mut HashMapStorageProvider) -> ExecutionScope {
        let marker = self.service.finalized_marker().unwrap();
        provider.set_block_number(marker.height + 1);
        let parent = self
            .service
            .open_parent(ExactParentIdentity {
                commitment_scheme_version: marker.commitment_scheme_version,
                block_number: marker.height,
                block_hash: marker.block_hash,
                root: marker.new_root,
            })
            .unwrap();
        let scope = ExecutionScope::with_parent_tree(parent, CeWorkConfig::new(0, 0, u64::MAX));
        StorageHandle::enter(provider, |storage| begin_block(storage, &scope).unwrap());
        scope
    }

    fn finish(&self, provider: &mut HashMapStorageProvider, scope: &ExecutionScope) {
        let output = StorageHandle::enter(provider, |storage| end_block(storage, scope).unwrap());
        let block_number = output.staged_tree_batch.block_number;
        let block_hash = keccak256(block_number.to_be_bytes());
        self.service
            .publish_candidate(block_hash, output.staged_tree_batch)
            .unwrap();
        self.service
            .apply_finalized(block_number, block_hash, output.new_root)
            .unwrap();
    }
}

fn activate(provider: &mut HashMapStorageProvider, tree: &TreeHarness) -> ExecutionScope {
    tree.activate(provider)
}

fn finish(provider: &mut HashMapStorageProvider, scope: &ExecutionScope, tree: &TreeHarness) {
    tree.finish(provider, scope);
}

fn seed_parent_commitments(
    provider: &mut HashMapStorageProvider,
    tree: &TreeHarness,
    bodies: &[&TributeData],
) {
    let scope = activate(provider, tree);
    StorageHandle::enter(provider, |storage| {
        for body in bodies {
            let canonical = outbe_tribute::canonical_body(body);
            mint(storage.clone(), &scope, BodyInput::Tribute(&canonical)).unwrap();
        }
    });
    finish(provider, &scope, tree);
}

#[test]
fn body_and_index_reads_use_the_finalized_parent_repository() {
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
    let tree = TreeHarness::new();
    seed_parent_commitments(&mut provider, &tree, &[&first, &second, &third]);
    let scope = activate(&mut provider, &tree);
    StorageHandle::enter(&mut provider, |storage| {
        let contract = TributeContract::new(storage);
        assert_eq!(
            contract
                .owner_of(&scope, &reader, first.tribute_id)
                .unwrap(),
            owner
        );
        assert_eq!(contract.balance_of(&scope, &reader, owner).unwrap(), 2);
        assert_eq!(
            contract
                .get_tribute_ids_by_owner(&scope, &reader, owner)
                .unwrap(),
            vec![first.tribute_id, second.tribute_id]
        );
        let mut expected_day = vec![first.tribute_id, third.tribute_id];
        expected_day.sort_unstable();
        assert_eq!(
            contract
                .get_tribute_ids_by_day(&scope, &reader, day)
                .unwrap(),
            expected_day
        );
        assert!(contract
            .token_uri(&scope, &reader, first.tribute_id)
            .unwrap()
            .contains("Outbe Tribute"));
    });
    finish(&mut provider, &scope, &tree);
}

#[test]
fn issue_is_visible_and_rejects_duplicates_before_projection() {
    let (reader, _) = repository();
    let body = tribute(Address::repeat_byte(0x22), 20_241_220);
    let mut provider = HashMapStorageProvider::new(1);
    let tree = TreeHarness::new();
    let scope = activate(&mut provider, &tree);

    StorageHandle::enter(&mut provider, |storage| {
        let mut contract = TributeContract::new(storage);
        contract.unseal_day(body.worldwide_day).unwrap();
        contract.issue(&scope, &reader, &body).unwrap();
        assert_eq!(contract.total_supply().unwrap(), 1);
        let visible = contract
            .get_tribute(&scope, &reader, body.tribute_id)
            .unwrap()
            .expect("same-block mint must be readable");
        assert_eq!(visible.tribute_id, body.tribute_id);
        assert_eq!(visible.owner, body.owner);
        assert_eq!(visible.nominal_amount_minor, body.nominal_amount_minor);
        let error = contract.issue(&scope, &reader, &body).unwrap_err();
        assert!(
            matches!(error, PrecompileError::Revert(message) if message == "tribute already exists")
        );
        assert_eq!(contract.total_supply().unwrap(), 1);
    });
    assert!(reader.get(body.tribute_id).unwrap().is_none());
    finish(&mut provider, &scope, &tree);
}

#[test]
fn burn_observes_same_block_mint_and_leaves_projection_to_the_projector() {
    let (reader, _) = repository();
    let body = tribute(Address::repeat_byte(0x33), 20_241_220);
    let mut provider = HashMapStorageProvider::new(1);
    let tree = TreeHarness::new();
    let scope = activate(&mut provider, &tree);

    StorageHandle::enter(&mut provider, |storage| {
        let mut contract = TributeContract::new(storage);
        contract.unseal_day(body.worldwide_day).unwrap();
        contract.issue(&scope, &reader, &body).unwrap();
        contract.burn(&scope, &reader, body.tribute_id).unwrap();
        assert_eq!(contract.total_supply().unwrap(), 0);
        assert!(contract
            .get_tribute(&scope, &reader, body.tribute_id)
            .unwrap()
            .is_none());
        let totals = contract.get_day_totals(body.worldwide_day).unwrap();
        assert_eq!(totals.tribute_count, 0);
        assert_eq!(totals.tribute_nominal_amount, U256::ZERO);
    });
    assert!(reader.get(body.tribute_id).unwrap().is_none());
    finish(&mut provider, &scope, &tree);
}

#[test]
fn absence_corruption_and_unavailability_remain_distinct() {
    let (reader, _) = repository();
    let body = tribute(Address::repeat_byte(0x44), 20_241_220);
    let mut provider = HashMapStorageProvider::new(1);
    let tree = TreeHarness::new();
    let scope = activate(&mut provider, &tree);
    StorageHandle::enter(&mut provider, |storage| {
        let error = TributeContract::new(storage)
            .owner_of(&scope, &reader, body.tribute_id)
            .unwrap_err();
        assert!(
            matches!(error, PrecompileError::Revert(message) if message == "tribute not found")
        );
    });
    finish(&mut provider, &scope, &tree);

    seed_parent_commitments(&mut provider, &tree, &[&body]);

    let corrupt = Arc::new(MemoryStorage::new());
    let corrupt_writer: StorageWriterHandle = corrupt.clone();
    corrupt_writer
        .put(
            Namespace::new("tributes").unwrap(),
            &Key::new(body.tribute_id.as_bytes().to_vec()).unwrap(),
            &Value::new([0xff]).unwrap(),
        )
        .unwrap();
    let corrupt_reader = TributeRepositoryReader::new(corrupt);
    let scope = activate(&mut provider, &tree);
    StorageHandle::enter(&mut provider, |storage| {
        assert!(matches!(
            TributeContract::new(storage).get_tribute(&scope, &corrupt_reader, body.tribute_id),
            Err(PrecompileError::BodyReadCorruption(_))
        ));
    });
    finish(&mut provider, &scope, &tree);

    let unavailable_reader = TributeRepositoryReader::new(Arc::new(UnavailableReader));
    let scope = activate(&mut provider, &tree);
    StorageHandle::enter(&mut provider, |storage| {
        assert!(matches!(
            TributeContract::new(storage).get_tribute(&scope, &unavailable_reader, body.tribute_id),
            Err(PrecompileError::BodyReadUnavailable(_))
        ));
    });
    finish(&mut provider, &scope, &tree);

    let backend_reader = TributeRepositoryReader::new(Arc::new(BackendErrorReader));
    let scope = activate(&mut provider, &tree);
    StorageHandle::enter(&mut provider, |storage| {
        assert!(matches!(
            TributeContract::new(storage).get_tribute(&scope, &backend_reader, body.tribute_id),
            Err(PrecompileError::BodyReadCorruption(_))
        ));
    });
    finish(&mut provider, &scope, &tree);
}

#[test]
fn every_tribute_body_input_schema_envelope_and_evm_leaf_is_authenticated() {
    let original = tribute(Address::repeat_byte(0x61), 20_260_716);
    let original_payload =
        outbe_compressed_entities::encode_tribute_v1(&outbe_tribute::canonical_body(&original))
            .unwrap();

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
        let payload = match outbe_compressed_entities::encode_tribute_v1(
            &outbe_tribute::canonical_body(&changed),
        ) {
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
    let mut noncanonical_payload = original_payload;
    noncanonical_payload.extend_from_slice(&[0x50, 0x01]);
    assert_raw_tribute_is_rejected(
        &original,
        StoredBody::new_v1(noncanonical_payload).unwrap().encode(),
        "stored_payload",
    );

    let storage = Arc::new(MemoryStorage::new());
    let writer = TributeRepositoryWriter::new(storage.clone(), storage.clone());
    let mut updated = copy_tribute(&original);
    updated.tribute_price_minor += U256::from(1);
    writer.put(&updated).unwrap();
    let reader = TributeRepositoryReader::new(storage);
    let mut provider = HashMapStorageProvider::new(1);
    let tree = TreeHarness::new();
    seed_parent_commitments(&mut provider, &tree, &[&original]);
    let scope = activate(&mut provider, &tree);
    StorageHandle::enter(&mut provider, |evm| {
        assert!(matches!(
            TributeContract::new(evm).get_tribute(&scope, &reader, original.tribute_id),
            Err(PrecompileError::BodyReadCorruption(_))
        ));
    });
    finish(&mut provider, &scope, &tree);
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
    let tree = TreeHarness::new();
    seed_parent_commitments(&mut provider, &tree, &[original]);
    let scope = activate(&mut provider, &tree);
    StorageHandle::enter(&mut provider, |evm| {
        assert!(
            matches!(
                TributeContract::new(evm).get_tribute(&scope, &reader, original.tribute_id),
                Err(PrecompileError::BodyReadCorruption(_))
            ),
            "mutation of {field} must fail before domain use"
        );
    });
    finish(&mut provider, &scope, &tree);
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
