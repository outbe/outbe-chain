//! The Gem lifecycle as GemFactory runs it: issuance on the live profile, settlement and
//! burn rules, the daily call sweep and the forfeit sweep. The harness is a block-scoped
//! compressed-entity world over an in-memory body store.

use std::sync::Arc;

use alloy_primitives::{address, Address, B256, U256};
use outbe_compressed_entities::test_support::seed_compressed_entities_genesis;
use outbe_compressed_entities::{begin_block, ExecutionScope};
use outbe_offchain_storage::MemoryStorage;
use outbe_oracle::schema::OracleContract;
use outbe_primitives::address_pair::AddressPair;
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
use outbe_primitives::error::Result;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::SECONDS_PER_DAY;
use outbe_primitives::time::SECONDS_PER_DAY_U32;
use outbe_primitives::time::{first_full_day, previous_date_key, timestamp_to_date_key};

use outbe_gem::api;
use outbe_gem::precompile::IGem;
use outbe_gem::schema::{GemAddParams, GemContract, GemItemState, GemView};
use outbe_gem::GemRepositoryReader;

mod call_sweep;
mod expiry;
mod lifecycle;

pub(crate) const T_NOW: u64 = 1_700_000_000;
pub(crate) const ALICE: Address = address!("0x1111111111111111111111111111111111111111");
pub(crate) const BOB: Address = address!("0x2222222222222222222222222222222222222222");
pub(crate) const USD: u16 = 840;
pub(crate) const EUR: u16 = 978;

/// Two days after the sample gems were issued: the day before it they held in full.
pub(crate) const QUALIFY_TS: u64 = T_NOW + 2 * SECONDS_PER_DAY;

/// One block's worth of compressed-entity authority: the executor scope and the
/// finalized parent bodies. Tests never cross a block, so the overlay is the truth.
pub(crate) struct World {
    pub(crate) scope: ExecutionScope,
    pub(crate) parent: GemRepositoryReader,
}

/// A provider at `T_NOW` whose compressed-entity lifecycle is open for the block.
pub(crate) fn provider_at(timestamp: u64) -> (HashMapStorageProvider, World) {
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_timestamp(U256::from(timestamp));
    let world = World {
        scope: ExecutionScope::default(),
        parent: outbe_gem::gem_reader(Arc::new(MemoryStorage::new())),
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
        gem_type: 2, // SRA
        promis_load_minor: U256::from(1_000_000u64),
        entry_price_minor: U256::from(500_000u64),
        floor_price_minor: U256::from(540_000u64),
        issuance_currency: USD,
        reference_currency: USD,
        issued_at: T_NOW,
        call_terms: None,
    }
}

pub(crate) fn add(storage: &StorageHandle, world: &World, params: GemAddParams) -> U256 {
    crate::runtime::add_gem(storage, &world.scope, &world.parent, params).unwrap()
}

pub(crate) fn item(storage: &StorageHandle, world: &World, gem_id: U256) -> Option<GemItemState> {
    api::get_item(storage, &world.scope, &world.parent, gem_id).unwrap()
}

pub(crate) fn view(storage: &StorageHandle, world: &World, gem_id: U256) -> GemView {
    api::get_view(storage, &world.scope, &world.parent, gem_id)
        .unwrap()
        .unwrap()
}

/// Lifecycle state as the view derives it at the block's timestamp.
pub(crate) fn gem_state(storage: &StorageHandle, world: &World, gem_id: U256) -> u8 {
    let now = storage.timestamp().unwrap().to::<u64>();
    view(storage, world, gem_id).effective_state(now)
}

pub(crate) fn bucket_of(storage: &StorageHandle, world: &World, gem_id: U256) -> B256 {
    item(storage, world, gem_id).unwrap().bucket_key
}

pub(crate) fn owned(storage: &StorageHandle, world: &World, owner: Address) -> Vec<U256> {
    api::list_by_owner(storage, &world.scope, &world.parent, owner)
        .unwrap()
        .into_iter()
        .map(|item| item.gem_id.to_u256())
        .collect()
}

pub(crate) fn alice_gems(storage: &StorageHandle, world: &World, loads: &[u64]) -> Vec<U256> {
    loads
        .iter()
        .map(|load| {
            let mut params = sample_params(ALICE);
            params.promis_load_minor = U256::from(*load);
            add(storage, world, params)
        })
        .collect()
}

/// Calls the gem's whole bucket at `at`.
pub(crate) fn call_gem(storage: &StorageHandle, world: &World, gem_id: U256, at: u64) {
    let bucket = bucket_of(storage, world, gem_id);
    let mut gem = GemContract::new(storage.clone());
    let terms = gem.read_call_terms(bucket).unwrap();
    crate::called::mark_called(&mut gem, bucket, &terms, at).unwrap();
}

/// Settles a gem the way the factory does once payment is accepted: a called gem
/// before its deadline, or a qualified one.
pub(crate) fn settle(storage: &StorageHandle, world: &World, gem_id: U256) {
    try_settle(storage, world, gem_id).unwrap();
}

/// Settles a gem if the factory's rules allow it, without taking a payment.
pub(crate) fn try_settle(storage: &StorageHandle, world: &World, gem_id: U256) -> Result<()> {
    crate::runtime::require_settleable(storage, &view(storage, world, gem_id))?;
    let (item, bucket) = api::load_pair(storage, &world.scope, &world.parent, gem_id)?.unwrap();
    api::record_settled(storage, &world.scope, item, bucket)
}

/// Qualifies then settles an uncalled gem.
pub(crate) fn qualify_and_settle(storage: &StorageHandle, world: &World, gem_id: U256) {
    let gem = item(storage, world, gem_id).unwrap();
    close_day_above(
        storage,
        gem.reference_currency,
        gem.floor_price_minor,
        first_full_day(gem.issued_at),
    );
    settle(storage, world, gem_id);
}

pub(crate) fn burn_settled(storage: &StorageHandle, world: &World, gem_id: U256) {
    crate::runtime::burn_settled(storage, &world.scope, &world.parent, gem_id).unwrap();
}

/// Whether the gem has qualified, as settlement and the view read it.
pub(crate) fn is_qualified(storage: &StorageHandle, world: &World, gem_id: U256) -> bool {
    api::is_qualified(storage, &item(storage, world, gem_id).unwrap()).unwrap()
}

pub(crate) fn unallocated(storage: &StorageHandle) -> U256 {
    outbe_promislimit::PromisLimitContract::new(storage.clone())
        .get_total_unallocated()
        .unwrap()
}

pub(crate) fn set_profile(storage: &StorageHandle, profile: u8) {
    GemContract::new(storage.clone())
        .config_profile
        .write(profile)
        .unwrap();
}

// --- Oracle ---------------------------------------------------------------------

/// Closes the day before `QUALIFY_TS` at `vwap` for the pair at `index`.
pub(crate) fn close_day(storage: &StorageHandle, index: u32, vwap: U256) {
    let oracle = OracleContract::new(storage.clone());
    let day = previous_date_key(timestamp_to_date_key(QUALIFY_TS));
    oracle.record_utc_day_vwap(day, index, vwap).unwrap();
    if oracle.utc_day_vwap_last_finalized.read().unwrap() < day {
        oracle.utc_day_vwap_last_finalized.write(day).unwrap();
    }
}

/// Lists `iso_code` and, given a price, registers its pair and closes the day at it.
pub(crate) fn seed_day_price(storage: &StorageHandle, iso_code: u16, vwap: Option<U256>) -> u32 {
    let oracle = OracleContract::new(storage.clone());
    oracle.reference_currencies.push(iso_code).unwrap();
    let Some(vwap) = vwap else {
        return 0;
    };
    let index =
        outbe_oracle::api::register_pair(storage.clone(), AddressPair::new_coen_to(iso_code))
            .unwrap();
    close_day(storage, index, vwap);
    index
}

/// Closes `day` just above `floor` on `COEN/<iso>`, registering the pair on first use.
pub(crate) fn close_day_above(storage: &StorageHandle, iso: u16, floor: U256, day: u32) {
    let pair = AddressPair::new_coen_to(iso);
    let oracle = OracleContract::new(storage.clone());
    let mut index = oracle.pair_index_of(pair).unwrap();
    if index == 0 {
        index = outbe_oracle::api::register_pair(storage.clone(), pair).unwrap();
    }
    oracle
        .record_utc_day_vwap(day, index, floor + U256::from(1))
        .unwrap();
    if oracle.utc_day_vwap_last_finalized.read().unwrap() < day {
        oracle.utc_day_vwap_last_finalized.write(day).unwrap();
    }
}

/// Registers `iso_code` as a reference currency, and its `COEN/<iso>` pair when
/// `rate` is given. Returns the pair index, or 0 when no pair was registered.
pub(crate) fn seed_currency(storage: &StorageHandle, iso_code: u16, rate: Option<U256>) -> u32 {
    let oracle = OracleContract::new(storage.clone());
    oracle.reference_currencies.push(iso_code).unwrap();
    let Some(rate) = rate else {
        return 0;
    };
    let index =
        outbe_oracle::api::register_pair(storage.clone(), AddressPair::new_coen_to(iso_code))
            .unwrap();
    oracle.exchange_rate.write(&index, rate).unwrap();
    oracle.exchange_rate_timestamp.write(&index, T_NOW).unwrap();
    index
}

/// Prices the call window back from `latest` at `vwap` and finalizes through it.
pub(crate) fn priced_window(storage: &StorageHandle, pair: u32, latest: u32, vwap: U256) {
    let oracle = OracleContract::new(storage.clone());
    let mut day = latest;
    for _ in 0..(outbe_gem::constants::CALL_WINDOW / SECONDS_PER_DAY_U32) {
        oracle.record_utc_day_vwap(day, pair, vwap).unwrap();
        day = previous_date_key(day);
    }
    if oracle.utc_day_vwap_last_finalized.read().unwrap() < latest {
        oracle.utc_day_vwap_last_finalized.write(latest).unwrap();
    }
}

/// Build a full-window (newest-first) list with `breach_days` entries above the
/// gem's call threshold, the rest at zero.
pub(crate) fn breach_window(
    now: u64,
    breach: U256,
    breach_days: usize,
) -> outbe_oracle::call_window::CallWindow {
    let window_days = (outbe_gem::constants::CALL_WINDOW / SECONDS_PER_DAY_U32) as usize;
    let mut window = Vec::with_capacity(window_days);
    let mut day = timestamp_to_date_key(now);
    for i in 0..window_days {
        let v = if i < breach_days { breach } else { U256::ZERO };
        window.push((day, Some(v)));
        day = previous_date_key(day);
    }
    outbe_oracle::call_window::CallWindow::from_vwaps(
        window,
        outbe_gem::constants::CALL_THRESHOLD / SECONDS_PER_DAY_U32,
    )
}

// --- Sweeps ------------------------------------------------------------------------

pub(crate) fn block_ctx_at<'s>(
    storage: &StorageHandle<'s>,
    timestamp: u64,
) -> BlockRuntimeContext<'s> {
    BlockRuntimeContext::new(
        BlockContext::empty_for_tests(1, timestamp, 1),
        storage.clone(),
    )
}

pub(crate) fn block_ctx<'s>(storage: &StorageHandle<'s>) -> BlockRuntimeContext<'s> {
    block_ctx_at(storage, storage.timestamp().unwrap().to::<u64>())
}

/// One block of the Gem sweeps as CycleTick runs them: what fell due, then a slice.
pub(crate) fn continue_sweeps(ctx: &BlockRuntimeContext, world: &World) -> Result<()> {
    crate::hooks::sweep_forfeits(ctx, &world.scope, &world.parent)?;
    crate::called::run_call_slice(ctx).map(drop)
}

pub(crate) fn begin_block_at(storage: &StorageHandle, world: &World, ts: u64) {
    continue_sweeps(&block_ctx_at(storage, ts), world).unwrap();
}

/// A mature gem on the PROD call terms, issued well before the window so no day is
/// skipped as pre-issuance.
pub(crate) fn mature_gem(storage: &StorageHandle, world: &World) -> U256 {
    set_profile(storage, outbe_gem::config::PROFILE_PROD);
    let mut p = sample_params(ALICE);
    p.issued_at = T_NOW - 100 * SECONDS_PER_DAY;
    add(storage, world, p)
}

/// A gem of `iso` issued at `issued_at` whose own entry price opens its own bucket;
/// `nonce` keeps the ids and the buckets apart.
pub(crate) fn callable_gem_of(
    storage: &StorageHandle,
    world: &World,
    iso: u16,
    nonce: u64,
    issued_at: u64,
    entry_price: U256,
) -> U256 {
    let mut p = sample_params(ALICE);
    p.promis_load_minor = U256::from(1_000_000 + u64::from(iso) * 1_000 + nonce);
    p.reference_currency = iso;
    p.issued_at = issued_at;
    p.entry_price_minor = entry_price + U256::from(nonce);
    add(storage, world, p)
}
