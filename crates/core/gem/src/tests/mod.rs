//! Shared native state helpers for the Gem read tests. GemFactory decides
//! issuance, calls and settlement; these helpers write the records it would.

use alloy_primitives::{address, Address, B256, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::error::Result;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::api;
use crate::precompile::{dispatch, IGem};
use crate::schema::{CallTerms, GemAddParams, GemContract, GemItemState, GemView};

mod views;

pub(crate) const T_NOW: u64 = 1_700_000_000;
pub(crate) const ALICE: Address = address!("0x1111111111111111111111111111111111111111");
pub(crate) const BOB: Address = address!("0x2222222222222222222222222222222222222222");
pub(crate) const USD: u16 = 840;

/// Construct native contract state at the requested timestamp.
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
        promis_load_minor: U256::from(1_000_000u64),
        entry_price_minor: U256::from(500_000u64),
        issuance_currency: USD,
        reference_currency: USD,
        issued_at: T_NOW,
        call_terms: None,
        entry_at_series_vwap: true,
    }
}

/// Records a gem whose group seals the live profile's call terms.
pub(crate) fn add(storage: &StorageHandle, mut params: GemAddParams) -> U256 {
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
    api::add_gem(storage, params).unwrap()
}

pub(crate) fn item(storage: &StorageHandle, gem_id: U256) -> Option<GemItemState> {
    api::get_item(storage, gem_id).unwrap()
}

pub(crate) fn view(storage: &StorageHandle, gem_id: U256) -> GemView {
    api::get_view(storage, gem_id).unwrap().unwrap()
}

pub(crate) fn group_of(storage: &StorageHandle, gem_id: U256) -> B256 {
    item(storage, gem_id).unwrap().group_key
}

/// Calls the gem's whole group at `at`.
pub(crate) fn call_gem(storage: &StorageHandle, gem_id: U256, at: u64) {
    let group = group_of(storage, gem_id);
    let mut gem = GemContract::new(storage.clone());
    let notice = gem
        .read_call_terms(group)
        .unwrap()
        .call_notice_period_seconds;
    gem.record_group_called(group, at, at + u64::from(notice))
        .unwrap();
}

/// Records a gem settled, as GemFactory does once payment is accepted.
pub(crate) fn settle(storage: &StorageHandle, gem_id: U256) {
    let (item, group) = api::load_pair(storage, gem_id).unwrap().unwrap();
    api::record_settled(storage, item, group).unwrap();
}

pub(crate) fn burn_settled(storage: &StorageHandle, gem_id: U256) {
    let (item, group) = api::load_pair(storage, gem_id).unwrap().unwrap();
    api::remove_gem(storage, item, group).unwrap();
}

pub(crate) fn set_profile(storage: &StorageHandle, profile: u8) {
    GemContract::new(storage.clone())
        .config_profile
        .write(profile)
        .unwrap();
}

// --- Precompile -----------------------------------------------------------------

pub(crate) fn call(storage: &StorageHandle, data: Vec<u8>) -> Result<alloy_primitives::Bytes> {
    dispatch(storage.clone(), &data, Address::ZERO, U256::ZERO)
}

pub(crate) fn status(storage: &StorageHandle, gem_id: U256) -> IGem::GemData {
    let out = call(storage, IGem::getGemCall { gemId: gem_id }.abi_encode()).unwrap();
    IGem::getGemCall::abi_decode_returns(&out).unwrap()
}
