use std::{sync::Arc, time::Duration};

use alloy_consensus::Header;
use alloy_eips::BlockNumHash;
use alloy_primitives::B256;
use mongodb::sync::Client;
use outbe_node::projection::{
    prepare_offchain_data_projection, validate_offchain_data_checkpoint,
    OffchainDataProjectionConfig,
};
use outbe_offchain_data::{FinalizedBlock, OffchainDataProjection, ProjectionConfig};
use outbe_offchain_storage::{MongoStorage, MongoStorageConfig};
use outbe_primitives::chain::DEVNET_CHAIN_ID;
use reth_chainspec::ChainInfo;
use reth_ethereum::Block;
use reth_provider::{
    test_utils::MockEthProvider, BlockHashReader, BlockIdReader, BlockNumReader, ProviderResult,
};

#[test]
#[ignore = "requires OUTBE_TEST_STANDALONE_MONGODB_URI"]
fn standalone_mongodb_is_rejected_during_startup_preparation() {
    let uri = std::env::var("OUTBE_TEST_STANDALONE_MONGODB_URI")
        .expect("set OUTBE_TEST_STANDALONE_MONGODB_URI before running this test");
    drop(
        prepare_offchain_data_projection(config(
            uri,
            isolated_database("standalone_rejected"),
            DEVNET_CHAIN_ID,
        ))
        .err()
        .expect("standalone MongoDB must not produce a ready projection"),
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

    let canonical_mock = MockEthProvider::new();
    let checkpoint_hash = add_empty_block(&canonical_mock, 1, 1);
    let canonical_provider =
        FinalizedMockProvider::new(canonical_mock, BlockNumHash::new(1, checkpoint_hash));
    let first = config(uri.clone(), database.clone(), DEVNET_CHAIN_ID);
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

    let wrong_canonical_mock = MockEthProvider::new();
    let wrong_hash = add_empty_block(&wrong_canonical_mock, 1, 2);
    let wrong_canonical_provider =
        FinalizedMockProvider::new(wrong_canonical_mock, BlockNumHash::new(1, wrong_hash));
    assert_ne!(checkpoint_hash, wrong_hash);
    let prepared = prepare_offchain_data_projection(first)
        .expect("matching managed state must pass MongoDB startup preparation");
    drop(
        validate_offchain_data_checkpoint(prepared, &wrong_canonical_provider)
            .err()
            .expect("mismatched canonical checkpoint hash must stop startup"),
    );

    let mut wrong_identity = config(uri, database.clone(), DEVNET_CHAIN_ID);
    wrong_identity.genesis_hash = B256::repeat_byte(0x22);
    drop(
        prepare_offchain_data_projection(wrong_identity)
            .err()
            .expect("mismatched persisted chain identity must stop startup"),
    );

    client.database(&database).drop().run().unwrap();
}

#[test]
#[ignore = "requires OUTBE_TEST_MONGODB_URI"]
fn second_active_projection_writer_is_rejected_until_the_first_releases_its_lease() {
    let uri = std::env::var("OUTBE_TEST_MONGODB_URI")
        .expect("set OUTBE_TEST_MONGODB_URI before running this test");
    let database = isolated_database("single_writer");
    let client = Client::with_uri_str(&uri).unwrap();
    client.database(&database).drop().run().unwrap();
    let projection_config = config(uri, database.clone(), DEVNET_CHAIN_ID);

    let first = prepare_offchain_data_projection(projection_config.clone())
        .expect("first projection writer must acquire the database lease");
    let second_error = prepare_offchain_data_projection(projection_config.clone())
        .err()
        .expect("second active writer must be rejected");
    assert!(second_error
        .to_string()
        .contains("eight-second total deadline"));

    drop(first);
    let restarted_at = std::time::Instant::now();
    prepare_offchain_data_projection(projection_config)
        .map(drop)
        .expect("clean writer shutdown must release the database lease");
    assert!(
        restarted_at.elapsed() < Duration::from_secs(3),
        "clean shutdown must release ownership without waiting for lease expiry"
    );
    client.database(&database).drop().run().unwrap();
}

struct FinalizedMockProvider {
    inner: MockEthProvider,
    finalized: BlockNumHash,
}

impl FinalizedMockProvider {
    fn new(inner: MockEthProvider, finalized: BlockNumHash) -> Self {
        Self { inner, finalized }
    }
}

impl BlockHashReader for FinalizedMockProvider {
    fn block_hash(&self, number: u64) -> ProviderResult<Option<B256>> {
        self.inner.block_hash(number)
    }

    fn canonical_hashes_range(&self, start: u64, end: u64) -> ProviderResult<Vec<B256>> {
        self.inner.canonical_hashes_range(start, end)
    }
}

impl BlockNumReader for FinalizedMockProvider {
    fn chain_info(&self) -> ProviderResult<ChainInfo> {
        self.inner.chain_info()
    }

    fn best_block_number(&self) -> ProviderResult<u64> {
        self.inner.best_block_number()
    }

    fn last_block_number(&self) -> ProviderResult<u64> {
        self.inner.last_block_number()
    }

    fn block_number(&self, hash: B256) -> ProviderResult<Option<u64>> {
        self.inner.block_number(hash)
    }
}

impl BlockIdReader for FinalizedMockProvider {
    fn pending_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(None)
    }

    fn safe_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(Some(self.finalized))
    }

    fn finalized_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(Some(self.finalized))
    }
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
