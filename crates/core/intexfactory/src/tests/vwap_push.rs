//! The daily VWAP sender: which days it sends, what a day carries, and where its mark stops.

use alloy_primitives::U256;
use outbe_oracle::api::AddressPair;
use outbe_oracle::schema::OracleContract;
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::{next_date_key, previous_date_key};

use super::{factory_provider, CHAIN_ID, EUR_ISO, EUR_PAIR_ID, ISSUED_AT, PAIR_ID, REFERENCE_ISO};
use crate::constants::MAX_VWAP_DAYS_PER_FIRING;
use crate::schema::IntexFactoryContract;
use crate::vwap_push::{self, backfill_start, day_rows};

const FINALIZED: u32 = 20_260_301;

fn list(oracle: &OracleContract, iso_code: u16, pair_id: u32) {
    oracle.reference_currencies.push(iso_code).unwrap();
    oracle
        .pair_to_index
        .write(&AddressPair::new_coen_to(iso_code), pair_id)
        .unwrap();
}

fn close_day(oracle: &OracleContract, day: u32, pair_id: u32, vwap: U256) {
    oracle
        .utc_day_vwap_value
        .get_nested(&day)
        .write(&pair_id, vwap)
        .unwrap();
    if oracle.utc_day_vwap_last_finalized.read().unwrap() < day {
        oracle.utc_day_vwap_last_finalized.write(day).unwrap();
    }
}

fn fire(storage: &StorageHandle<'_>) {
    let ctx = BlockRuntimeContext::new(
        BlockContext::empty_for_tests(1, ISSUED_AT as u64, CHAIN_ID),
        storage.clone(),
    );
    vwap_push::run(&ctx).expect("the sender never fails the block");
}

fn sent_day(storage: &StorageHandle<'_>) -> u32 {
    IntexFactoryContract::new(storage.clone())
        .vwap_sent_day
        .read()
        .unwrap()
}

#[test]
fn nothing_goes_out_before_the_oracle_finalizes_a_day() {
    let mut provider = factory_provider();
    StorageHandle::enter(&mut provider, |storage| {
        list(
            &OracleContract::new(storage.clone()),
            REFERENCE_ISO,
            PAIR_ID,
        );
        fire(&storage);
        assert_eq!(sent_day(&storage), 0);
    });
}

#[test]
fn a_fresh_sender_backfills_the_window_a_few_days_per_firing() {
    // Jan 26 to Mar 1 is 35 days in a common year.
    assert_eq!(backfill_start(FINALIZED), 20_260_126);

    let mut provider = factory_provider();
    StorageHandle::enter(&mut provider, |storage| {
        let oracle = OracleContract::new(storage.clone());
        list(&oracle, REFERENCE_ISO, PAIR_ID);
        let mut day = backfill_start(FINALIZED);
        while day <= FINALIZED {
            close_day(&oracle, day, PAIR_ID, U256::from(1_000_000));
            day = next_date_key(day);
        }

        fire(&storage);
        let first_firing = (1..MAX_VWAP_DAYS_PER_FIRING)
            .fold(backfill_start(FINALIZED), |day, _| next_date_key(day));
        assert_eq!(sent_day(&storage), first_firing);

        for _ in 0..4 {
            fire(&storage);
        }
        assert_eq!(sent_day(&storage), FINALIZED, "caught up");

        close_day(
            &oracle,
            next_date_key(FINALIZED),
            PAIR_ID,
            U256::from(1_000_000),
        );
        fire(&storage);
        assert_eq!(
            sent_day(&storage),
            next_date_key(FINALIZED),
            "the next day follows"
        );
    });
}

#[test]
fn an_unpriced_day_moves_the_mark_and_a_refused_send_holds_it() {
    // No router stub: every send is refused.
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut provider, |storage| {
        let oracle = OracleContract::new(storage.clone());
        list(&oracle, REFERENCE_ISO, PAIR_ID);
        close_day(&oracle, FINALIZED, PAIR_ID, U256::from(1_000_000));

        for _ in 0..6 {
            fire(&storage);
        }
        assert_eq!(sent_day(&storage), previous_date_key(FINALIZED));
    });
}

#[test]
fn a_router_that_answers_nothing_holds_the_mark() {
    // An address without code answers a call with nothing.
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.stub_sub_call_at(
        crate::constants::ORIGIN_ROUTER_ADDRESS,
        alloy_primitives::Bytes::new(),
    );
    StorageHandle::enter(&mut provider, |storage| {
        let oracle = OracleContract::new(storage.clone());
        list(&oracle, REFERENCE_ISO, PAIR_ID);
        close_day(&oracle, FINALIZED, PAIR_ID, U256::from(1_000_000));

        for _ in 0..6 {
            fire(&storage);
        }
        assert_eq!(sent_day(&storage), previous_date_key(FINALIZED));
    });
}

#[test]
fn a_day_carries_its_priced_currencies_and_saturates_past_the_wire_type() {
    let mut provider = factory_provider();
    StorageHandle::enter(&mut provider, |storage| {
        let oracle = OracleContract::new(storage.clone());
        list(&oracle, REFERENCE_ISO, PAIR_ID);
        list(&oracle, EUR_ISO, EUR_PAIR_ID);
        // Listed, but never given a pair.
        oracle.reference_currencies.push(392).unwrap();
        close_day(&oracle, FINALIZED, PAIR_ID, U256::from(u128::MAX));
        close_day(&oracle, FINALIZED, EUR_PAIR_ID, U256::from(7));

        let rows: Vec<_> = day_rows(&storage, FINALIZED)
            .unwrap()
            .into_iter()
            .map(|row| (row.isoCode, row.vwapMinor))
            .collect();
        assert_eq!(rows, [(REFERENCE_ISO, u64::MAX), (EUR_ISO, 7)]);
        assert!(day_rows(&storage, previous_date_key(FINALIZED))
            .unwrap()
            .is_empty());
    });
}
