use alloy_primitives::{address, Address, U256};
use alloy_sol_types::SolCall;
use outbe_nod::NodRepositoryReader;
use outbe_offchain_storage::{MemoryStorage, StorageReaderHandle};
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

fn with_empty_body_readers<R>(
    f: impl FnOnce(&TributeRepositoryReader, &NodRepositoryReader) -> R,
) -> R {
    let storage: StorageReaderHandle = Arc::new(MemoryStorage::new());
    f(
        &TributeRepositoryReader::new(storage.clone()),
        &NodRepositoryReader::new(storage),
    )
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
    with_empty_body_readers(|tribute_bodies, nod_bodies| {
        crate::runtime::start_metadosis(&ctx, tribute_bodies, nod_bodies)
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
