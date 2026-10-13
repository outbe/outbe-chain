//! Test-only compressed-entity world for crates that issue, settle or read gems.
//!
//! A gem lives in the block's compressed-entity overlay and, once finalized, in the
//! projected body store. Tests stay inside one block, so an in-memory parent plus an
//! open [`ExecutionScope`] is the complete authority.

use std::sync::Arc;

use outbe_compressed_entities::test_support::seed_compressed_entities_genesis;
use outbe_compressed_entities::{begin_block, ExecutionReaders, ExecutionScope};
use outbe_offchain_storage::MemoryStorage;
use outbe_primitives::storage::StorageHandle;

use crate::GemRepositoryReader;

/// The block's scope and an empty finalized parent.
pub struct GemWorld {
    pub scope: ExecutionScope,
    pub parent: GemRepositoryReader,
}

impl GemWorld {
    /// Seeds the compressed-entity genesis slots and opens the block on `storage`.
    /// Call it once per provider, before the first gem mutation.
    pub fn open(storage: &StorageHandle<'_>) -> Self {
        let world = Self::unopened();
        seed_compressed_entities_genesis(storage).expect("compressed-entity genesis fixture");
        begin_block(storage.clone(), &world.scope).expect("open the compressed-entity block");
        world
    }

    /// A scope that is not active yet: for tests that open the block themselves.
    pub fn unopened() -> Self {
        Self {
            scope: ExecutionScope::default(),
            parent: crate::gem_reader(Arc::new(MemoryStorage::new())),
        }
    }

    pub fn readers(&self) -> ExecutionReaders<'_, '_, GemRepositoryReader> {
        ExecutionReaders {
            scope: &self.scope,
            parent: &self.parent,
        }
    }
}
