//! Shared fixtures for the Nod tests.

mod adr006;
mod buckets;
mod layout;
mod nft;
mod views;

use outbe_compressed_entities::test_support::seed_compressed_entities_genesis;
use std::sync::Arc;

use alloy_primitives::{Address, U256};
use outbe_compressed_entities::{begin_block, ExecutionReaders, ExecutionScope, WwdEntityId};
use outbe_offchain_storage::MemoryStorage;
use outbe_primitives::time::{first_full_day, timestamp_to_date_key, WorldwideDay};
use outbe_primitives::{
    math::{constants::MAX_BIN_ID, tree_math},
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
};

use crate::{api, state::CallBins, NodBucketState, NodContract, NodItemState, NodRepositoryReader};

const USD: u16 = 840;

const EUR: u16 = 978;

fn seed_production_nod_genesis(storage: &StorageHandle<'_>) {
    seed_compressed_entities_genesis(storage).expect("CE genesis fixture");
    // These tests exercise the production call terms.
    NodContract::new(storage.clone())
        .config_profile
        .write(crate::config::PROFILE_PROD)
        .unwrap();
}

/// Close `day` just above `floor` on `COEN/<iso>`, registering the pair on first use.
pub(crate) fn close_day_above(storage: &StorageHandle<'_>, iso: u16, floor: U256, day: u32) {
    let pair = outbe_oracle::api::AddressPair::new_coen_to(iso);
    let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
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

/// Qualify the Nod's bucket: close its first full day above the floor of `entry`.
pub(crate) fn qualify(storage: &StorageHandle<'_>, item: &NodItemState, entry: U256) {
    close_day_above(
        storage,
        item.reference_currency,
        crate::pricing::floor_price_minor(entry).unwrap(),
        first_full_day(item.issued_at),
    );
}

fn bucket_of(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &NodRepositoryReader,
    item: &NodItemState,
) -> NodBucketState {
    api::get_bucket(
        storage,
        scope,
        parent,
        WwdEntityId::from_day_and_digest(item.worldwide_day, item.bucket_key),
    )
    .unwrap()
    .unwrap()
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
