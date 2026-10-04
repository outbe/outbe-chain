//! Shared compressed-entity state fixtures for tests and protocol benchmarks.

use alloy_primitives::{B256, U256};
use outbe_primitives::{
    addresses::COMPRESSED_ENTITIES_ADDRESS, error::PrecompileError, storage::StorageHandle,
};
use thiserror::Error;

/// Failure while preparing an empty compressed-entity genesis fixture.
#[derive(Debug, Error)]
pub enum GenesisFixtureError {
    #[error(transparent)]
    Storage(#[from] PrecompileError),
    #[error(transparent)]
    Root(#[from] crate::collection::CollectionError),
}

/// Seeds only the CE initialization marker and wrapped empty catalog root.
/// Domain-specific state and expected test outcomes remain with each caller.
pub fn seed_compressed_entities_genesis(
    storage: &StorageHandle<'_>,
) -> Result<(), GenesisFixtureError> {
    let root = crate::sealed_root(B256::ZERO)?;
    storage.sstore(COMPRESSED_ENTITIES_ADDRESS, U256::ZERO, U256::from(4))?;
    storage.sstore(
        COMPRESSED_ENTITIES_ADDRESS,
        U256::from(1),
        U256::from_be_slice(root.as_slice()),
    )?;
    Ok(())
}
