use alloy_primitives::{Address, B256};
use outbe_primitives::{
    block::{BlockContext, BlockContextInput, ZeroGenesisTimestamp},
    error::Result,
    storage::{
        hashmap::HashMapStorageProvider,
        readonly::{ReadOnlyBlockContext, ReadOnlyStorageProvider, StorageReader},
        StorageHandle,
    },
};

struct EmptyReader;

impl StorageReader for EmptyReader {
    fn read_storage(&self, _address: Address, _key: B256) -> Result<alloy_primitives::U256> {
        Ok(alloy_primitives::U256::ZERO)
    }
}

#[test]
fn block_context_carries_explicit_genesis_hash_beside_chain_id() {
    let genesis_hash = B256::repeat_byte(0x42);
    let context = BlockContext::try_new_with_genesis_hash(BlockContextInput {
        block_number: 7,
        timestamp: 1_700_000_000,
        chain_id: 54_322_345,
        genesis_hash,
        genesis_timestamp: 1_600_000_000,
        proposer: Address::repeat_byte(0x11),
        validators: vec![Address::repeat_byte(0x22)],
    })
    .unwrap();

    assert_eq!(context.chain_id, 54_322_345);
    assert_eq!(context.genesis_hash, genesis_hash);
    assert_eq!(context.genesis_timestamp, 1_600_000_000);

    assert_eq!(
        BlockContext::try_new_with_genesis_hash(BlockContextInput {
            block_number: 7,
            timestamp: 1_700_000_000,
            chain_id: 54_322_345,
            genesis_hash,
            genesis_timestamp: 0,
            proposer: Address::repeat_byte(0x11),
            validators: vec![Address::repeat_byte(0x22)],
        })
        .unwrap_err(),
        ZeroGenesisTimestamp
    );
}

#[test]
fn block_context_new_copies_timestamp_into_genesis_timestamp() {
    let context = BlockContext::new(7, 1_700_000_000, 1, Address::ZERO, Vec::new());
    assert_eq!(context.genesis_timestamp, 1_700_000_000);
    assert_eq!(
        context
            .with_genesis_timestamp(1_600_000_000)
            .genesis_timestamp,
        1_600_000_000
    );
}

#[test]
fn storage_handle_exposes_the_provider_chain_identity() {
    let genesis_hash = B256::repeat_byte(0x43);
    let mut provider = HashMapStorageProvider::new_with_chain_identity(54_322_345, genesis_hash)
        .with_genesis_timestamp(1_600_000_000);
    provider.set_genesis_timestamp(1_600_000_001);

    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(storage.chain_id().unwrap(), 54_322_345);
        assert_eq!(storage.genesis_hash().unwrap(), genesis_hash);
        assert_eq!(storage.genesis_timestamp().unwrap(), 1_600_000_001);
    });
}

#[test]
fn follower_read_only_context_can_be_bound_to_the_same_chain_identity() {
    let genesis_hash = B256::repeat_byte(0x44);
    let mut provider =
        ReadOnlyStorageProvider::new_with_chain_identity(EmptyReader, 54_322_345, genesis_hash);

    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(storage.chain_id().unwrap(), 54_322_345);
        assert_eq!(storage.genesis_hash().unwrap(), genesis_hash);
        assert_eq!(storage.genesis_timestamp().unwrap(), 0);
    });

    let context =
        ReadOnlyBlockContext::try_new(54_322_345, genesis_hash, 1_600_000_000, 7, 1_700_000_000)
            .unwrap();
    assert!(context.is_complete());
    assert_eq!(
        ReadOnlyBlockContext::try_new(54_322_345, genesis_hash, 0, 7, 1_700_000_000).unwrap_err(),
        ZeroGenesisTimestamp
    );
    assert!(!ReadOnlyBlockContext {
        genesis_timestamp: 0,
        ..context
    }
    .is_complete());
    let mut bound = ReadOnlyStorageProvider::new_with_block_context(EmptyReader, context);
    StorageHandle::enter(&mut bound, |storage| {
        assert_eq!(storage.genesis_timestamp().unwrap(), 1_600_000_000);
    });
}
