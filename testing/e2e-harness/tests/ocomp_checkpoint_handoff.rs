use std::sync::Arc;

use alloy_consensus::Header;
use alloy_eips::BlockNumHash;
use alloy_primitives::B256;
use eyre::Result;
use mongodb::sync::Client;
use outbe_e2e_harness::mongo_fixture::ManagedMongoReplicaSet;
use outbe_node::projection::{ocomp_projection_contains, OcompProjectionContainment};
use outbe_offchain_data::{FinalizedBlock, OffchainDataProjection, ProjectionConfig};
use outbe_offchain_storage::{MongoStorage, MongoStorageConfig};
use outbe_primitives::{chain::DEVNET_CHAIN_ID, projection::ProjectionCheckpoint};
use reth_chainspec::ChainInfo;
use reth_ethereum::Block;
use reth_provider::{
    test_utils::MockEthProvider, BlockHashReader, BlockIdReader, BlockNumReader, ProviderResult,
};

#[test]
fn checkpoint_handoff_uses_real_mongodb_for_behind_exact_and_ahead() -> Result<()> {
    let mongo = ManagedMongoReplicaSet::start("ocomp-checkpoint-handoff", false)?;
    let uri = mongo.uri().to_owned();
    let database = format!("outbe_ocomp_checkpoint_handoff_{}", std::process::id());
    let client = Client::with_uri_str(&uri)?;
    client.database(&database).drop().run()?;

    let storage = Arc::new(MongoStorage::connect(MongoStorageConfig {
        uri,
        database: database.clone(),
    })?);
    let projection_config = ProjectionConfig {
        chain_id: DEVNET_CHAIN_ID,
        genesis_hash: B256::repeat_byte(0x11),
        start_block: 1,
    };
    let mut projector =
        OffchainDataProjection::open(projection_config, storage.clone(), storage.clone())?;

    let canonical = MockEthProvider::new();
    let block_1 = add_empty_block(&canonical, 1, 1);
    let block_2 = add_empty_block(&canonical, 2, 2);
    let block_3 = add_empty_block(&canonical, 3, 3);
    let canonical = FinalizedMockProvider::new(canonical, BlockNumHash::new(3, block_3));
    let required = ProjectionCheckpoint {
        block_number: 2,
        block_hash: block_2,
    };

    projector.project_block(&FinalizedBlock {
        number: 1,
        hash: block_1,
        receipts: Vec::new(),
    })?;
    let behind = projector
        .state()
        .checkpoint
        .expect("block 1 projection has a durable checkpoint");
    assert_eq!(
        ocomp_projection_contains(behind, required, &canonical)?,
        OcompProjectionContainment::Behind {
            checkpoint: behind,
            required,
        }
    );

    projector.project_block(&FinalizedBlock {
        number: 2,
        hash: block_2,
        receipts: Vec::new(),
    })?;
    let exact = projector
        .state()
        .checkpoint
        .expect("block 2 projection has a durable checkpoint");
    assert_eq!(
        ocomp_projection_contains(exact, required, &canonical)?,
        OcompProjectionContainment::Contains {
            checkpoint: exact,
            required,
        }
    );

    projector.project_block(&FinalizedBlock {
        number: 3,
        hash: block_3,
        receipts: Vec::new(),
    })?;
    drop(projector);

    let reopened = OffchainDataProjection::open(projection_config, storage.clone(), storage)?;
    let ahead = reopened
        .state()
        .checkpoint
        .expect("reopened projection retains the durable block 3 checkpoint");
    assert_eq!(
        ocomp_projection_contains(ahead, required, &canonical)?,
        OcompProjectionContainment::Contains {
            checkpoint: ahead,
            required,
        }
    );

    drop(reopened);
    client.database(&database).drop().run()?;
    Ok(())
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
