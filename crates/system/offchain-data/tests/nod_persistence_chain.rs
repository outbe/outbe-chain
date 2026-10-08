//! Exercise production NOD mutations, emitted receipts, CE sealing and RocksDB projection.
//! Only the EVM storage host and finalized receipt/header envelopes are test fixtures.

use outbe_offchain_data::{runtime_body_readers, supervised_runtime_body_readers};
use std::sync::Arc;

use alloy_primitives::{Address, B256, U256};
use outbe_compressed_entities::{
    begin_block, derive_poseidon_entity_id, encode_nod_item_v2, end_block, execution_scope, CeMdbx,
    CeWorkConfig, EntityRef, EnvironmentIdentity, ExactParentIdentity, FinalizedMarker,
    MdbxAuthenticatedTree, WwdEntityId, ACTIVE_COMMITMENT_SCHEME, LOCAL_STORAGE_SCHEMA_VERSION,
};
use outbe_nod::{api, canonical_item, NodContract, NodItemState};
use outbe_offchain_data::{
    open_projection, FinalizedBlock, FinalizedLog, FinalizedReceipt, OffchainDataProjection,
    ProjectionConfig, RuntimeBodyReaders,
};
use outbe_offchain_storage::RocksDbStorage;
use outbe_primitives::{
    addresses::COMPRESSED_ENTITIES_ADDRESS,
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
    time::WorldwideDay,
};

fn assert_authenticated_nods(
    evm: &mut HashMapStorageProvider,
    ce: &Arc<CeMdbx>,
    identity: ExactParentIdentity,
    readers: &RuntimeBodyReaders,
    items: &[NodItemState],
) {
    let scope = execution_scope::with_parent_tree(
        Arc::new(MdbxAuthenticatedTree::open(ce.clone(), identity).unwrap()),
        CeWorkConfig::new(0, 0, u64::MAX),
    );
    // A fresh scope cannot read a mutation left in the preceding block's overlay.
    StorageHandle::enter(evm, |storage| {
        begin_block(storage.clone(), &scope).unwrap();
        for item in items {
            let loaded = api::load_item(&storage, &scope, readers, item.nod_id)
                .expect("authenticate projected NOD against its persisted CE leaf")
                .expect("NOD exists");
            assert_eq!(canonical_item(loaded.body()), canonical_item(item));
            let verified = outbe_compressed_entities::read(
                storage.clone(),
                &scope,
                readers,
                EntityRef::NodItem(item.nod_id),
            )
            .unwrap()
            .unwrap();
            assert_eq!(
                verified.stored_body().payload(),
                encode_nod_item_v2(&canonical_item(item)).unwrap(),
                "production write and projected read must preserve exact canonical bytes"
            );
        }
        let first = &items[0];
        let bucket_id = WwdEntityId::from_day_and_digest(first.worldwide_day, first.bucket_key);
        let bucket = api::load_bucket(&storage, &scope, readers, bucket_id)
            .expect("authenticate the shared bucket against its persisted CE leaf")
            .unwrap();
        assert_eq!(bucket.body().entry_price_minor, U256::from(5));
        assert_eq!(bucket.body().reference_currency, first.reference_currency);
    });
}

struct NativeStores {
    ce: Arc<CeMdbx>,
    rocks: Arc<RocksDbStorage>,
    environment: EnvironmentIdentity,
    genesis_marker: FinalizedMarker,
    config: ProjectionConfig,
    evm: HashMapStorageProvider,
}

fn prepare_stores(directory: &std::path::Path) -> NativeStores {
    let ce_path = directory.join("ce");
    let rocks_path = directory.join("projection");
    let genesis = B256::repeat_byte(0x42);
    let empty_root = outbe_compressed_entities::sealed_root(B256::ZERO).unwrap();
    let environment = EnvironmentIdentity {
        local_storage_schema_version: LOCAL_STORAGE_SCHEMA_VERSION,
        chain_id: 91,
        genesis_hash: genesis,
        commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
        topology: outbe_compressed_entities::CeTopologyV1.encode(),
        tree_format: "ckb-smt-v0.6.1-poseidon-catalog-v3".into(),
        vendor_revision: "nod-persistence-chain-test".into(),
    };
    let genesis_marker = FinalizedMarker {
        commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
        height: 0,
        block_hash: genesis,
        parent_block_hash: B256::ZERO,
        parent_root: B256::ZERO,
        new_root: empty_root,
    };
    let ce = Arc::new(CeMdbx::open(&ce_path, environment.clone(), genesis_marker).unwrap());
    let rocks = Arc::new(RocksDbStorage::open(&rocks_path).unwrap());
    let config = ProjectionConfig {
        chain_id: 91,
        genesis_hash: genesis,
        start_block: 1,
    };
    let mut evm = HashMapStorageProvider::new_with_chain_identity(91, genesis);
    // Seed only the empty genesis CE state. All later roots/leaves come from end_block.
    StorageHandle::enter(&mut evm, |storage| {
        storage
            .sstore(COMPRESSED_ENTITIES_ADDRESS, U256::ZERO, U256::from(4))
            .unwrap();
        storage
            .sstore(
                COMPRESSED_ENTITIES_ADDRESS,
                U256::from(1),
                U256::from_be_bytes(empty_root.0),
            )
            .unwrap();
    });
    NativeStores {
        ce,
        rocks,
        environment,
        genesis_marker,
        config,
        evm,
    }
}

fn project_mutation_receipt(
    projector: &mut OffchainDataProjection,
    evm: &HashMapStorageProvider,
    first_event: usize,
    identity: ExactParentIdentity,
) {
    let height = identity.block_number;
    let hash = identity.block_hash;
    let logs = evm.get_ordered_events()[first_event..]
        .iter()
        .enumerate()
        .map(|(index, log)| FinalizedLog {
            log_index: index as u64,
            emitter: log.address,
            data: log.data.clone(),
        })
        .collect::<Vec<_>>();
    assert!(
        !logs.is_empty(),
        "production mutation must emit projection events"
    );
    projector
        .project_block(&FinalizedBlock {
            number: height,
            hash,
            receipts: vec![FinalizedReceipt {
                tx_hash: B256::repeat_byte(0x70 + height as u8),
                transaction_index: 0,
                success: true,
                logs,
            }],
        })
        .unwrap();
}

fn genesis_parent_identity(genesis: B256, empty_root: B256) -> ExactParentIdentity {
    ExactParentIdentity {
        commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
        block_number: 0,
        block_hash: genesis,
        root: empty_root,
    }
}

#[test]
fn production_nod_receipts_and_ce_seal_agree_with_rocksdb_after_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let NativeStores {
        ce,
        rocks,
        environment,
        genesis_marker,
        config,
        mut evm,
    } = prepare_stores(directory.path());
    let ce_path = directory.path().join("ce");
    let rocks_path = directory.path().join("projection");
    let genesis = config.genesis_hash;
    let empty_root = genesis_marker.new_root;
    let mut projector = open_projection(config, rocks.clone(), rocks.clone()).unwrap();
    let readers = runtime_body_readers(rocks.clone());
    let mut items = Vec::new();
    let mut identity = genesis_parent_identity(genesis, empty_root);

    for height in 1..=2_u64 {
        let (item, next_identity, first_event) =
            mint_authenticated_nod(&mut evm, &ce, identity, &readers, height);
        identity = next_identity;
        assert_bucket_membership(&mut evm, &item, height);
        if height == 2 {
            // CE already includes the second member, while RocksDB is still at block 1.
            // The existing NOD and its shared bucket remain readable without a bucket rewrite.
            assert_projection_checkpoint(&projector, 1);
            assert_authenticated_nods(&mut evm, &ce, identity, &readers, &items);
        }
        project_mutation_receipt(&mut projector, &evm, first_event, identity);
        items.push(item);
        assert_authenticated_nods(&mut evm, &ce, identity, &readers, &items);
    }

    drop(readers);
    drop(projector);
    drop(rocks);
    drop(ce);
    let reopened_ce = Arc::new(CeMdbx::open(&ce_path, environment, genesis_marker).unwrap());
    let reopened_rocks = Arc::new(RocksDbStorage::open(&rocks_path).unwrap());
    let reopened_projector =
        open_projection(config, reopened_rocks.clone(), reopened_rocks.clone()).unwrap();
    assert_projection_checkpoint(&reopened_projector, 2);
    let (failure_sender, failures) = tokio::sync::watch::channel(None);
    let reopened_readers = supervised_runtime_body_readers(reopened_rocks.clone(), failure_sender);
    assert_authenticated_nods(&mut evm, &reopened_ce, identity, &reopened_readers, &items);

    // A changed field must fail authentication.
    // This read error must not stop the node.
    assert_corrupt_read_is_nonfatal(
        &mut evm,
        &reopened_readers,
        CorruptNodFixture {
            ce: reopened_ce,
            rocks: reopened_rocks,
            identity,
            item: items.remove(0),
        },
        &failures,
    );
}

fn assert_bucket_membership(evm: &mut HashMapStorageProvider, item: &NodItemState, height: u64) {
    StorageHandle::enter(evm, |storage| {
        assert_eq!(
            NodContract::new(storage)
                .bucket_nod_count
                .read(&item.bucket_key)
                .unwrap(),
            height as u32
        );
    });
}

fn assert_projection_checkpoint(projector: &OffchainDataProjection, height: u64) {
    assert_eq!(projector.state().checkpoint.unwrap().block_number, height);
}

fn assert_corrupt_read_is_nonfatal(
    evm: &mut HashMapStorageProvider,
    readers: &RuntimeBodyReaders,
    fixture: CorruptNodFixture,
    failures: &tokio::sync::watch::Receiver<Option<outbe_offchain_data::RuntimeBodyFailure>>,
) {
    let error = read_corrupt_nod(evm, readers, fixture);
    assert_corruption_diagnostics(&error);
    readers.report_precompile_error(&error);
    assert!(failures.borrow().is_none());
}

fn assert_corruption_diagnostics(error: &outbe_primitives::error::PrecompileError) {
    let outbe_primitives::error::PrecompileError::BodyReadCorruption(message) = error else {
        panic!("body mismatch must remain corruption, got {error:?}");
    };
    for field in [
        "expected=0x",
        "actual=0x",
        "payload_hex=",
        "stored_body_hex=",
        "decoded=",
        "evm_block=",
        "evm_ce_root=",
        "binding=",
    ] {
        assert!(message.contains(field), "missing {field}: {message}");
    }
}

fn mint_authenticated_nod(
    evm: &mut HashMapStorageProvider,
    ce: &Arc<CeMdbx>,
    identity: ExactParentIdentity,
    readers: &impl outbe_compressed_entities::ParentBodySource,
    height: u64,
) -> (outbe_nod::NodItemState, ExactParentIdentity, usize) {
    let day = WorldwideDay::new(20260906);
    let owner = Address::repeat_byte(height as u8);
    let item = outbe_nod::test_support::item(
        outbe_nod::test_support::NodItemFixture {
            is_settled: false,
            nod_id: derive_poseidon_entity_id(owner, day).unwrap(),
            owner,
            gratis_load_minor: U256::from(123_456),
            worldwide_day: day,
            league_id: 7,
            bucket_key: outbe_nod::identity::bucket_key(day, U256::from(5), 978),
            issuance_currency: 840,
            reference_currency: 978,
            issued_at: 1_788_652_800 + height,
        },
        U256::from(5),
    );
    evm.set_block_number(height);
    let first_event = evm.get_ordered_events().len();
    let scope = execution_scope::with_parent_tree(
        Arc::new(MdbxAuthenticatedTree::open(ce.clone(), identity).unwrap()),
        CeWorkConfig::new(0, 0, u64::MAX),
    );
    let seal = StorageHandle::enter(evm, |storage| {
        begin_block(storage.clone(), &scope).unwrap();
        api::add_nod(&storage, &scope, readers, &item, U256::from(5)).unwrap();
        end_block(storage, &scope).unwrap()
    });
    let hash = B256::repeat_byte(height as u8);
    let batch = seal.staged_tree_batch.freeze(hash);
    ce.apply_finalized(&batch).unwrap();
    let identity = ExactParentIdentity {
        commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
        block_number: height,
        block_hash: hash,
        root: seal.new_root,
    };
    (item, identity, first_event)
}

struct CorruptNodFixture {
    ce: Arc<CeMdbx>,
    rocks: Arc<RocksDbStorage>,
    identity: ExactParentIdentity,
    item: outbe_nod::NodItemState,
}
fn read_corrupt_nod(
    evm: &mut HashMapStorageProvider,
    readers: &outbe_offchain_data::RuntimeBodyReaders,
    fixture: CorruptNodFixture,
) -> outbe_primitives::error::PrecompileError {
    let CorruptNodFixture {
        ce,
        rocks,
        identity,
        mut item,
    } = fixture;
    let next_amount = outbe_nod::api::calculation_amount(&item).unwrap() + U256::from(1);
    outbe_nod::test_support::set_amount(&mut item, next_amount);
    outbe_nod::nod_writer(rocks.clone(), rocks)
        .put_nod(&item)
        .unwrap();
    let scope = execution_scope::with_parent_tree(
        Arc::new(MdbxAuthenticatedTree::open(ce, identity).unwrap()),
        CeWorkConfig::new(0, 0, u64::MAX),
    );
    StorageHandle::enter(evm, |storage| {
        begin_block(storage.clone(), &scope).unwrap();
        outbe_compressed_entities::read(storage, &scope, readers, EntityRef::NodItem(item.nod_id))
            .unwrap_err()
    })
}
