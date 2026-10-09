//! Fixtures shared by the Rewards metadata-fingerprint test binaries.

use alloy_primitives::Address;
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;

const CHAIN_ID: u64 = 1;
const GENESIS_TS: u64 = 1_704_067_200;

/// Runs `f` on fresh storage in block `block_number`, one minute after
/// genesis.
pub fn with_block(block_number: u64, f: impl FnOnce(BlockRuntimeContext<'_>)) {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.enter(|handle| {
        let block = BlockContext::new(
            block_number,
            GENESIS_TS + 60,
            CHAIN_ID,
            Address::ZERO,
            Vec::new(),
        );
        f(BlockRuntimeContext::new(block, handle));
    });
}
