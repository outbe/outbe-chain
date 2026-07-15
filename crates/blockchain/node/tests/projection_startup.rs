use std::sync::Arc;

use alloy_consensus::Header;
use alloy_primitives::B256;
use mongodb::sync::Client;
use outbe_node::projection::{
    prepare_offchain_data_projection, validate_offchain_data_checkpoint,
    OffchainDataProjectionConfig,
};
use outbe_offchain_data::{FinalizedBlock, OffchainDataProjection, ProjectionConfig};
use outbe_offchain_storage::{MongoStorage, MongoStorageConfig};
use reth_ethereum::Block;
use reth_provider::test_utils::MockEthProvider;

#[test]
#[ignore = "requires OUTBE_TEST_STANDALONE_MONGODB_URI"]
fn standalone_mongodb_is_rejected_during_startup_preparation() {
    let uri = std::env::var("OUTBE_TEST_STANDALONE_MONGODB_URI")
        .expect("set OUTBE_TEST_STANDALONE_MONGODB_URI before running this test");
    let error =
        prepare_offchain_data_projection(config(uri, isolated_database("standalone_rejected"), 1))
            .err()
            .expect("standalone MongoDB must not produce a ready projection");

    let error = format!("{error:#}");
    assert!(error.contains("transaction support"), "error: {error}");
    assert!(
        error.contains("requires a replica set or sharded"),
        "error: {error}"
    );
}

#[test]
#[ignore = "requires OUTBE_TEST_MONGODB_URI"]
fn replica_set_passes_startup_and_persisted_identity_is_validated() {
    let uri = std::env::var("OUTBE_TEST_MONGODB_URI")
        .expect("set OUTBE_TEST_MONGODB_URI before running this test");
    let database = isolated_database("replica_ready");
    let client = Client::with_uri_str(&uri).unwrap();
    client.database(&database).drop().run().unwrap();

    let canonical_provider = MockEthProvider::new();
    let checkpoint_hash = add_empty_block(&canonical_provider, 1, 1);
    let first = config(uri.clone(), database.clone(), 1);
    let storage = Arc::new(
        MongoStorage::connect(MongoStorageConfig {
            uri: uri.clone(),
            database: database.clone(),
        })
        .unwrap(),
    );
    let mut projector = OffchainDataProjection::open(
        ProjectionConfig {
            chain_id: first.chain_id,
            genesis_hash: first.genesis_hash,
            start_block: first.start_block,
        },
        storage.clone(),
        storage,
    )
    .unwrap();
    projector
        .project_block(&FinalizedBlock {
            number: 1,
            hash: checkpoint_hash,
            receipts: Vec::new(),
        })
        .unwrap();

    let prepared = prepare_offchain_data_projection(first.clone())
        .expect("transaction-capable replica set must pass MongoDB startup preparation");
    validate_offchain_data_checkpoint(prepared, &canonical_provider)
        .map(drop)
        .expect("transaction-capable replica set must pass startup preparation");
    let prepared = prepare_offchain_data_projection(first.clone())
        .expect("matching managed state must pass MongoDB startup preparation");
    validate_offchain_data_checkpoint(prepared, &canonical_provider)
        .map(drop)
        .expect("matching managed state must reopen successfully");

    let wrong_canonical_provider = MockEthProvider::new();
    let wrong_hash = add_empty_block(&wrong_canonical_provider, 1, 2);
    assert_ne!(checkpoint_hash, wrong_hash);
    let prepared = prepare_offchain_data_projection(first)
        .expect("matching managed state must pass MongoDB startup preparation");
    let error = validate_offchain_data_checkpoint(prepared, &wrong_canonical_provider)
        .err()
        .expect("mismatched canonical checkpoint hash must stop startup");
    let error = format!("{error:#}");
    assert!(
        error.contains("checkpoint identity mismatch"),
        "error: {error}"
    );

    let mut wrong_identity = config(uri, database.clone(), 1);
    wrong_identity.chain_id = 2;
    let error = prepare_offchain_data_projection(wrong_identity)
        .err()
        .expect("mismatched persisted chain identity must stop startup");
    let error = format!("{error:#}");
    assert!(error.contains("MongoDB state"), "error: {error}");
    assert!(
        error.contains("identity does not match configured chain"),
        "error: {error}"
    );

    client.database(&database).drop().run().unwrap();
}

fn add_empty_block(provider: &MockEthProvider, number: u64, timestamp: u64) -> B256 {
    let header = Header {
        number,
        timestamp,
        ..Default::default()
    };
    let hash = header.hash_slow();
    provider.add_block(hash, Block::new(header, Default::default()));
    hash
}

fn config(uri: String, database: String, chain_id: u64) -> OffchainDataProjectionConfig {
    OffchainDataProjectionConfig {
        chain_id,
        genesis_hash: B256::repeat_byte(0x11),
        start_block: 1,
        mongodb_uri: uri,
        mongodb_database: database,
    }
}

fn isolated_database(test_name: &str) -> String {
    format!("outbe_node_startup_{}_{}", std::process::id(), test_name)
}
