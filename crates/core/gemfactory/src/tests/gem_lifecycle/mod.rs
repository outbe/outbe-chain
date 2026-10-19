//! The Gem lifecycle as GemFactory runs it: issuance on the live profile, settlement and
//! burn rules, the daily call sweep and the forfeit sweep in EVM storage.

use alloy_primitives::{address, Address, B256, U256};
use outbe_oracle::schema::OracleContract;
use outbe_primitives::address_pair::AddressPair;
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
use outbe_primitives::error::Result;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::SECONDS_PER_DAY;
use outbe_primitives::time::SECONDS_PER_DAY_U32;
use outbe_primitives::time::{previous_date_key, timestamp_to_date_key};

use outbe_gem::api;
use outbe_gem::precompile::IGem;
use outbe_gem::schema::{GemAddParams, GemContract, GemItemState, GemView};

mod call_sweep;
mod expiry;
mod lifecycle;

pub(crate) const T_NOW: u64 = 1_700_000_000;
pub(crate) const ALICE: Address = address!("0x1111111111111111111111111111111111111111");
pub(crate) const BOB: Address = address!("0x2222222222222222222222222222222222222222");
pub(crate) const USD: u16 = 840;
pub(crate) const EUR: u16 = 978;

/// Creates an EVM storage provider at the specified timestamp.
pub(crate) fn provider_at(timestamp: u64) -> HashMapStorageProvider {
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_timestamp(U256::from(timestamp));
    provider
}

pub(crate) fn with_storage<R>(f: impl FnOnce(&StorageHandle) -> R) -> R {
    let mut provider = provider_at(T_NOW);
    StorageHandle::enter(&mut provider, |handle| f(&handle))
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

pub(crate) fn add(storage: &StorageHandle, params: GemAddParams) -> U256 {
    crate::runtime::add_gem(storage, params).unwrap()
}

pub(crate) fn item(storage: &StorageHandle, gem_id: U256) -> Option<GemItemState> {
    api::get_item(storage, gem_id).unwrap()
}

pub(crate) fn view(storage: &StorageHandle, gem_id: U256) -> GemView {
    api::get_view(storage, gem_id).unwrap().unwrap()
}

/// Lifecycle state as the view derives it at the block's timestamp.
pub(crate) fn gem_state(storage: &StorageHandle, gem_id: U256) -> u8 {
    let now = storage.timestamp().unwrap().to::<u64>();
    view(storage, gem_id).effective_state(now)
}

pub(crate) fn bucket_of(storage: &StorageHandle, gem_id: U256) -> B256 {
    item(storage, gem_id).unwrap().group_key
}

pub(crate) fn owned(storage: &StorageHandle, owner: Address) -> Vec<U256> {
    api::list_by_owner(storage, owner)
        .unwrap()
        .into_iter()
        .map(|item| item.gem_id.to_u256())
        .collect()
}

pub(crate) fn alice_gems(storage: &StorageHandle, loads: &[u64]) -> Vec<U256> {
    loads
        .iter()
        .map(|load| {
            let mut params = sample_params(ALICE);
            params.promis_load_minor = U256::from(*load);
            add(storage, params)
        })
        .collect()
}

/// Calls the gem's whole group at `at`.
pub(crate) fn call_gem(storage: &StorageHandle, gem_id: U256, at: u64) {
    let group = bucket_of(storage, gem_id);
    let mut gem = GemContract::new(storage.clone());
    let terms = gem.read_call_terms(group).unwrap();
    crate::called::mark_called(&mut gem, group, &terms, at).unwrap();
}

/// Settles a gem the way the factory does once payment is accepted: an uncalled gem,
/// or a called one before its deadline.
pub(crate) fn settle(storage: &StorageHandle, gem_id: U256) {
    try_settle(storage, gem_id).unwrap();
}

/// Settles a gem if the factory's rules allow it, without taking a payment.
pub(crate) fn try_settle(storage: &StorageHandle, gem_id: U256) -> Result<()> {
    crate::runtime::require_settleable(storage, &view(storage, gem_id))?;
    let (item, group) = api::load_pair(storage, gem_id)?.unwrap();
    api::record_settled(storage, item, group)
}

pub(crate) fn burn_settled(storage: &StorageHandle, gem_id: U256) {
    crate::runtime::burn_settled(storage, gem_id).unwrap();
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
pub(crate) fn continue_sweeps(ctx: &BlockRuntimeContext) -> Result<()> {
    crate::hooks::sweep_forfeits(ctx)?;
    crate::called::run_call_slice(ctx).map(drop)
}

pub(crate) fn begin_block_at(storage: &StorageHandle, ts: u64) {
    continue_sweeps(&block_ctx_at(storage, ts)).unwrap();
}

/// A mature gem on the PROD call terms, issued well before the window so no day is
/// skipped as pre-issuance.
pub(crate) fn mature_gem(storage: &StorageHandle) -> U256 {
    set_profile(storage, outbe_gem::config::PROFILE_PROD);
    let mut p = sample_params(ALICE);
    p.issued_at = T_NOW - 100 * SECONDS_PER_DAY;
    add(storage, p)
}

/// A gem of `iso` issued at `issued_at` whose own entry price opens its own group;
/// `nonce` keeps the ids and the groups apart.
pub(crate) fn callable_gem_of(
    storage: &StorageHandle,
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
    add(storage, p)
}
