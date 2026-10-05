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

/// Seeds the marker before computing the root, preserving transaction fixture write order.
pub fn seed_compressed_entities_genesis_after_marker(storage: &StorageHandle<'_>) {
    storage
        .sstore(COMPRESSED_ENTITIES_ADDRESS, U256::ZERO, U256::from(4))
        .unwrap();
    storage
        .sstore(
            COMPRESSED_ENTITIES_ADDRESS,
            U256::from(1),
            U256::from_be_slice(crate::sealed_root(B256::ZERO).unwrap().as_slice()),
        )
        .unwrap();
}

/// Opens the empty CE database used by single-chain execution fixtures.
pub fn open_empty_ce_database(
    path: &std::path::Path,
    genesis_hash: B256,
) -> Result<crate::CeMdbx, crate::PersistenceError> {
    crate::CeMdbx::open(
        path,
        crate::EnvironmentIdentity {
            local_storage_schema_version: crate::LOCAL_STORAGE_SCHEMA_VERSION,
            chain_id: 1,
            genesis_hash,
            commitment_scheme_version: crate::ACTIVE_COMMITMENT_SCHEME,
            topology: crate::CeTopologyV1.encode(),
            tree_format: "ckb-smt-v0.6.1-poseidon-catalog-v3".to_owned(),
            vendor_revision: "ad555350c866b2265d87d2d7fbd146fbc918bfe5".to_owned(),
        },
        crate::FinalizedMarker {
            commitment_scheme_version: crate::ACTIVE_COMMITMENT_SCHEME,
            height: 0,
            block_hash: genesis_hash,
            parent_block_hash: B256::ZERO,
            parent_root: B256::ZERO,
            new_root: crate::sealed_root(B256::ZERO).unwrap(),
        },
    )
}
