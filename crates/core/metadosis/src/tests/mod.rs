use alloy_primitives::{address, Address, B256, U256};
use alloy_sol_types::SolCall;
use outbe_compressed_entities::{
    begin_block, end_block, EntityRef, ExecutionScope, IdPage, IdPageRequest, ParentBodySource,
    ParentBodySourceError, QueryRef, StoredBody,
};
use outbe_nod::NodRepositoryReader;
use outbe_offchain_storage::{MemoryStorage, StorageReaderHandle};
use outbe_primitives::addresses::COMPRESSED_ENTITIES_ADDRESS;
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
use outbe_primitives::storage::dsl::StorageRecord;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_promislimit::PromisLimitContract;
use outbe_tribute::{TributeContract, TributeData, TributeRepositoryReader};
use std::sync::Arc;

use crate::constants::*;
use crate::precompile::{dispatch as metadosis_dispatch, IMetadosis};
use crate::runtime::timestamp_to_date_key;
use crate::schema::{day_type, status, MetadosisContract, WorldwideDay, WorldwideDayEntryExt};

const CHAIN_ID: u64 = 1;

fn with_contract<R>(f: impl FnOnce(&mut MetadosisContract) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut contract = MetadosisContract::new(storage.clone());
        f(&mut contract)
    })
}

fn with_storage<R>(f: impl FnOnce(StorageHandle) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| f(storage.clone()))
}

struct TestParent {
    tribute: TributeRepositoryReader,
    nod: NodRepositoryReader,
}

impl TestParent {
    fn empty() -> Self {
        let storage: StorageReaderHandle = Arc::new(MemoryStorage::new());
        Self {
            tribute: TributeRepositoryReader::new(storage.clone()),
            nod: NodRepositoryReader::new(storage),
        }
    }
}

impl ParentBodySource for TestParent {
    fn get(&self, entity: EntityRef) -> Result<Option<StoredBody>, ParentBodySourceError> {
        match entity {
            EntityRef::Tribute(_) => ParentBodySource::get(&self.tribute, entity),
            EntityRef::NodItem(_) | EntityRef::NodBucket(_) => {
                ParentBodySource::get(&self.nod, entity)
            }
        }
    }

    fn list(
        &self,
        query: QueryRef,
        request: IdPageRequest,
    ) -> Result<IdPage, ParentBodySourceError> {
        match query {
            QueryRef::TributeByOwner(_) | QueryRef::TributeByDay(_) => {
                ParentBodySource::list(&self.tribute, query, request)
            }
            QueryRef::NodByOwner(_) | QueryRef::NodAll => {
                ParentBodySource::list(&self.nod, query, request)
            }
        }
    }
}

fn with_active_scope<R>(
    storage: StorageHandle,
    f: impl FnOnce(&ExecutionScope, &TestParent) -> R,
) -> R {
    let parent = TestParent::empty();
    let scope = ExecutionScope::new();
    if storage
        .sload(COMPRESSED_ENTITIES_ADDRESS, U256::ZERO)
        .unwrap()
        .is_zero()
    {
        storage
            .sstore(COMPRESSED_ENTITIES_ADDRESS, U256::ZERO, U256::from(3))
            .unwrap();
        storage
            .sstore(
                COMPRESSED_ENTITIES_ADDRESS,
                U256::from(1),
                U256::from_be_slice(
                    outbe_compressed_entities::sealed_root(B256::ZERO)
                        .unwrap()
                        .as_slice(),
                ),
            )
            .unwrap();
    }
    begin_block(storage.clone(), &scope).unwrap();
    let result = f(&scope, &parent);
    end_block(storage, &scope).unwrap();
    result
}

/// Drive the WWD lifecycle the way the daily Cycle handler does:
/// invoke `start_metadosis` on a synthetic context. Production no
/// longer drives Metadosis through a per-block lifecycle hook (see
/// ), but these tests intentionally exercise the state
/// machine sub-day, so they call `start_metadosis` directly.
fn run_begin_block_with_chain_id(
    storage: StorageHandle,
    block_number: u64,
    timestamp: u64,
    chain_id: u64,
) {
    let ctx = BlockRuntimeContext::new(
        BlockContext::empty_for_tests(block_number, timestamp, chain_id),
        storage,
    );
    with_active_scope(ctx.storage.clone(), |scope, parent| {
        crate::runtime::start_metadosis(&ctx, scope, parent)
    })
    .unwrap();
}

fn run_begin_block(storage: StorageHandle, block_number: u64, timestamp: u64) {
    run_begin_block_with_chain_id(
        storage,
        block_number,
        timestamp,
        outbe_primitives::chain::CHAIN_ID,
    );
}

mod lifecycle;
mod state;
