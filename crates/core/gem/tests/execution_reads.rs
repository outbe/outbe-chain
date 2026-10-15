//! Reads during execution: a reverted issuance leaves nothing behind, and a removal
//! consumes the capabilities it was handed instead of reading the parent again.
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use alloy_primitives::{Address, U256};
use outbe_compressed_entities::test_support::seed_compressed_entities_genesis;
use outbe_compressed_entities::{
    begin_block, EntityRef, ExecutionScope, IdPage, IdPageRequest, ParentBodySource,
    ParentBodySourceError, QueryRef, StoredBody,
};
use outbe_gem::{api, GemAddParams, GemContract, GemRepositoryReader};
use outbe_offchain_storage::MemoryStorage;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};

const ISSUED_AT: u64 = 1_704_067_200;

fn params(owner: Address) -> GemAddParams {
    GemAddParams {
        owner,
        gem_type: 2,
        promis_load_minor: U256::from(1_000_000u64),
        entry_price_minor: U256::from(500_000u64),
        floor_price_minor: U256::from(540_000u64),
        issuance_currency: 840,
        reference_currency: 840,
        issued_at: ISSUED_AT,
        call_terms: None,
    }
}

fn active_world() -> (HashMapStorageProvider, ExecutionScope, GemRepositoryReader) {
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_block_number(1);
    provider.set_timestamp(U256::from(ISSUED_AT));
    let scope = ExecutionScope::default();
    StorageHandle::enter(&mut provider, |storage| {
        seed_compressed_entities_genesis(&storage).unwrap();
        begin_block(storage, &scope).unwrap();
    });
    let parent = outbe_gem::gem_reader(Arc::new(MemoryStorage::new()));
    (provider, scope, parent)
}

#[test]
fn reverted_issuance_rolls_back_overlay_compact_state_and_events() {
    let (mut provider, scope, parent) = active_world();
    let owner = Address::repeat_byte(0x66);
    StorageHandle::enter(&mut provider, |storage| {
        let outcome: Result<U256> = storage.with_checkpoint(|| {
            let gem_id = api::add_gem(&storage, &scope, &parent, params(owner))?;
            assert!(api::get_item(&storage, &scope, &parent, gem_id)?.is_some());
            Err(PrecompileError::Revert("nested caller reverted".into()))
        });
        assert!(outcome.is_err());
        let gem = GemContract::new(storage.clone());
        assert_eq!(gem.total_supply().unwrap(), 0);
        assert!(api::list_by_owner(&storage, &scope, &parent, owner)
            .unwrap()
            .is_empty());
    });
}

struct CountingParent {
    inner: GemRepositoryReader,
    gets: AtomicUsize,
}

impl ParentBodySource for CountingParent {
    fn get(
        &self,
        entity: EntityRef,
    ) -> std::result::Result<Option<StoredBody>, ParentBodySourceError> {
        self.gets.fetch_add(1, Ordering::SeqCst);
        ParentBodySource::get(&self.inner, entity)
    }

    fn list(
        &self,
        query: QueryRef,
        request: IdPageRequest,
    ) -> std::result::Result<IdPage, ParentBodySourceError> {
        ParentBodySource::list(&self.inner, query, request)
    }
}

#[test]
fn removal_consumes_loaded_capabilities_without_a_second_parent_read() {
    let (mut provider, scope, reader) = active_world();
    let parent = CountingParent {
        inner: reader,
        gets: AtomicUsize::new(0),
    };
    let owner = Address::repeat_byte(0x41);
    StorageHandle::enter(&mut provider, |storage| {
        let gem_id = api::add_gem(&storage, &scope, &parent, params(owner)).unwrap();
        let (item, bucket) = api::load_pair(&storage, &scope, &parent, gem_id)
            .unwrap()
            .unwrap();
        let reads_before_remove = parent.gets.load(Ordering::SeqCst);
        api::remove_gem(&storage, &scope, item, bucket).unwrap();
        assert_eq!(parent.gets.load(Ordering::SeqCst), reads_before_remove);
        assert!(api::get_item(&storage, &scope, &parent, gem_id)
            .unwrap()
            .is_none());
        assert_eq!(GemContract::new(storage).total_supply().unwrap(), 0);
    });
}
