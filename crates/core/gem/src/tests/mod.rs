//! Shared harness: a block-scoped compressed-entity world over an in-memory body
//! store, plus the gem helpers the read tests build on. GemFactory decides
//! issuance, calls and settlement; these helpers write the records it would.

use std::sync::Arc;

use alloy_primitives::{address, Address, B256, U256};
use alloy_sol_types::SolCall;
use outbe_compressed_entities::test_support::seed_compressed_entities_genesis;
use outbe_compressed_entities::{begin_block, ExecutionReaders, ExecutionScope};
use outbe_offchain_storage::MemoryStorage;
use outbe_primitives::error::Result;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::api;
use crate::precompile::{dispatch, IGem};
use crate::schema::{CallTerms, GemAddParams, GemContract, GemItemState, GemView};
use crate::GemRepositoryReader;

mod views;

pub(crate) const T_NOW: u64 = 1_700_000_000;
pub(crate) const ALICE: Address = address!("0x1111111111111111111111111111111111111111");
pub(crate) const BOB: Address = address!("0x2222222222222222222222222222222222222222");
pub(crate) const USD: u16 = 840;

/// One block's worth of compressed-entity authority: the executor scope and the
/// finalized parent bodies. Tests never cross a block, so the overlay is the truth.
pub(crate) struct World {
    pub(crate) scope: ExecutionScope,
    pub(crate) parent: GemRepositoryReader,
}

impl World {
    pub(crate) fn readers(&self) -> ExecutionReaders<'_, '_, GemRepositoryReader> {
        ExecutionReaders {
            scope: &self.scope,
            parent: &self.parent,
        }
    }
}

/// A provider at `T_NOW` whose compressed-entity lifecycle is open for the block.
pub(crate) fn provider_at(timestamp: u64) -> (HashMapStorageProvider, World) {
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_timestamp(U256::from(timestamp));
    let world = World {
        scope: ExecutionScope::default(),
        parent: crate::gem_reader(Arc::new(MemoryStorage::new())),
    };
    StorageHandle::enter(&mut provider, |storage| {
        seed_compressed_entities_genesis(&storage).expect("CE genesis fixture");
        begin_block(storage, &world.scope).expect("open the CE block");
    });
    (provider, world)
}

pub(crate) fn with_storage<R>(f: impl FnOnce(&StorageHandle, &World) -> R) -> R {
    let (mut provider, world) = provider_at(T_NOW);
    StorageHandle::enter(&mut provider, |handle| f(&handle, &world))
}

pub(crate) fn sample_params(owner: Address) -> GemAddParams {
    GemAddParams {
        owner,
        gem_type: 1, // SRA
        promis_load_minor: U256::from(1_000_000u64),
        entry_price_minor: U256::from(500_000u64),
        issuance_currency: USD,
        reference_currency: USD,
        issued_at: T_NOW,
        call_terms: None,
    }
}

/// Records a gem whose bucket seals the live profile's call terms.
pub(crate) fn add(storage: &StorageHandle, world: &World, mut params: GemAddParams) -> U256 {
    let profile = crate::config::read(storage).unwrap();
    params.call_terms = Some(CallTerms {
        call_price_minor: outbe_primitives::markup::marked_up(
            params.entry_price_minor,
            profile.call_rate,
        )
        .unwrap(),
        reference_currency: params.reference_currency,
        call_rate: profile.call_rate,
        call_window_seconds: profile.call_window_seconds,
        call_threshold_seconds: profile.call_threshold_seconds,
        call_notice_period_seconds: profile.call_notice_period_seconds,
    });
    api::add_gem(storage, &world.scope, &world.parent, params).unwrap()
}

pub(crate) fn item(storage: &StorageHandle, world: &World, gem_id: U256) -> Option<GemItemState> {
    api::get_item(storage, &world.scope, &world.parent, gem_id).unwrap()
}

pub(crate) fn view(storage: &StorageHandle, world: &World, gem_id: U256) -> GemView {
    api::get_view(storage, &world.scope, &world.parent, gem_id)
        .unwrap()
        .unwrap()
}

pub(crate) fn bucket_of(storage: &StorageHandle, world: &World, gem_id: U256) -> B256 {
    item(storage, world, gem_id).unwrap().bucket_key
}

/// Calls the gem's whole bucket at `at`.
pub(crate) fn call_gem(storage: &StorageHandle, world: &World, gem_id: U256, at: u64) {
    let bucket = bucket_of(storage, world, gem_id);
    let mut gem = GemContract::new(storage.clone());
    let notice = gem
        .read_call_terms(bucket)
        .unwrap()
        .call_notice_period_seconds;
    gem.record_bucket_called(bucket, at, at + u64::from(notice))
        .unwrap();
}

/// Records a gem settled, as GemFactory does once payment is accepted.
pub(crate) fn settle(storage: &StorageHandle, world: &World, gem_id: U256) {
    let (item, bucket) = api::load_pair(storage, &world.scope, &world.parent, gem_id)
        .unwrap()
        .unwrap();
    api::record_settled(storage, &world.scope, item, bucket).unwrap();
}

pub(crate) fn burn_settled(storage: &StorageHandle, world: &World, gem_id: U256) {
    let (item, bucket) = api::load_pair(storage, &world.scope, &world.parent, gem_id)
        .unwrap()
        .unwrap();
    api::remove_gem(storage, &world.scope, item, bucket).unwrap();
}

pub(crate) fn set_profile(storage: &StorageHandle, profile: u8) {
    GemContract::new(storage.clone())
        .config_profile
        .write(profile)
        .unwrap();
}

// --- Precompile -----------------------------------------------------------------

pub(crate) fn call(
    storage: &StorageHandle,
    world: &World,
    data: Vec<u8>,
) -> Result<alloy_primitives::Bytes> {
    dispatch(
        storage.clone(),
        world.readers(),
        &data,
        Address::ZERO,
        U256::ZERO,
    )
}

pub(crate) fn status(storage: &StorageHandle, world: &World, gem_id: U256) -> IGem::GemData {
    let out = call(
        storage,
        world,
        IGem::getGemCall { gemId: gem_id }.abi_encode(),
    )
    .unwrap();
    IGem::getGemCall::abi_decode_returns(&out).unwrap()
}
