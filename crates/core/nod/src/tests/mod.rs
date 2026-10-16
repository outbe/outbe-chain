//! Shared fixtures for the Nod tests.

mod adr006;
mod buckets;
mod cost;
mod layout;
mod nft;
mod views;

use alloy_primitives::{Address, B256, U256};
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{
    math::{constants::MAX_BIN_ID, tree_math},
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
};

use crate::{api, state::CallBins, NodBucketState, NodContract, NodItemState};

const USD: u16 = 840;

const EUR: u16 = 978;

fn seed_production_nod_genesis(storage: &StorageHandle<'_>) {
    // These tests exercise the production call terms.
    NodContract::new(storage.clone())
        .config_profile
        .write(crate::config::PROFILE_PROD)
        .unwrap();
}

fn bucket_of(storage: &StorageHandle<'_>, item: &NodItemState) -> NodBucketState {
    api::get_bucket(storage, item.bucket_key).unwrap().unwrap()
}

/// A Nod whose `bucket_key` is derived the way `record_nod_issued` requires.
fn item(owner: Address, entry: U256, reference_currency: u16) -> NodItemState {
    let worldwide_day = WorldwideDay::new(20_260_715);
    crate::test_support::item(
        crate::test_support::NodItemFixture {
            is_settled: false,
            nod_id: crate::identity::generate_nod_id(owner, worldwide_day).unwrap(),
            owner,
            gratis_load_minor: U256::from(11),
            worldwide_day,
            league_id: 4,
            bucket_key: crate::identity::bucket_key(worldwide_day, entry, reference_currency),
            issuance_currency: 840,
            reference_currency,
            issued_at: 1_752_534_000,
        },
        entry,
    )
}
