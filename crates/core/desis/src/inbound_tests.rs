//! Relayed bids that can never apply are acknowledged, not rejected: an unknown day, a
//! superseded generation, a BIDS_DONE marker disagreeing with the one already recorded.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{SolCall, SolEvent};
use outbe_common::WorldwideDay;
use outbe_primitives::addresses::DESIS_ADDRESS;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::api::AuctionBriefReceipt;
use crate::constants::ORIGIN_ROUTER_ADDRESS;
use crate::precompile::IDesis;
use crate::runtime;
use crate::schema::{BidData, DesisContract};

const CHAIN_ID: u64 = 1;
const REFERENCE_ISO: u16 = 840;
const WORLDWIDE_DAY: WorldwideDay = WorldwideDay::new(20260101);
const UNBRIEFED_DAY: WorldwideDay = WorldwideDay::new(20260202);
const SRC_CHAIN: u32 = 1;
const NOW: u64 = 1_699_920_000 + 5;
const ANCHOR: u64 = NOW - NOW % 86_400;
const LOAD_MINOR: u128 = crate::constants::PROMIS_LOAD * 1_000_000;

const IGNORED_STALE_GENERATION: u8 = 1;
const IGNORED_UNKNOWN_DAY: u8 = 2;
const IGNORED_CONFLICTING_MARKER: u8 = 3;

fn storage() -> HashMapStorageProvider {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(NOW));
    storage.stub_sub_call_at(
        ORIGIN_ROUTER_ADDRESS,
        Bytes::from(
            crate::sol_ext::IOriginRouter::targetsOfCall::abi_encode_returns(&vec![SRC_CHAIN]),
        ),
    );
    storage.stub_sub_call_at(
        outbe_intexfactory::constants::INTEX_NFT1155_ADDRESS,
        Bytes::from(vec![0u8; 32]),
    );
    storage
}

fn open_revealing(s: &StorageHandle) {
    assert_eq!(
        crate::api::dispatch_auction_brief(
            s.clone(),
            WORLDWIDE_DAY,
            U256::from(10 * LOAD_MINOR),
            vec![crate::schema::ReferenceCurrencyPrice {
                iso_code: REFERENCE_ISO,
                entry_price_minor: U256::from(2_000_000u64),
            }],
            true,
            NOW,
        )
        .unwrap(),
        AuctionBriefReceipt::Accepted
    );
    runtime::schedule_tick(s, NOW).unwrap();
    runtime::schedule_tick(s, ANCHOR + 86_400).unwrap();
}

fn bids(n: u8) -> Vec<BidData> {
    (0..n)
        .map(|i| BidData {
            bidder_address: Address::from([i + 1; 20]),
            intex_bid_rate: 200,
            timestamp: i as u32,
            intex_quantity: 1,
            issuance_currency: REFERENCE_ISO,
            reference_currency: REFERENCE_ISO,
        })
        .collect()
}

fn ignored_reasons(storage: &HashMapStorageProvider) -> Vec<u8> {
    storage
        .get_events(DESIS_ADDRESS)
        .iter()
        .filter(|log| log.topics().first() == Some(&IDesis::InboundIgnored::SIGNATURE_HASH))
        .map(|log| IDesis::InboundIgnored::decode_log_data(log).unwrap().reason)
        .collect()
}

#[test]
fn a_batch_for_a_day_never_briefed_is_acknowledged() {
    let mut storage = storage();
    StorageHandle::enter(&mut storage, |s| {
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            UNBRIEFED_DAY,
            SRC_CHAIN,
            1,
            0,
            1,
            bids(1),
        )
        .unwrap();
        let contract = DesisContract::new(s.clone());
        let key = DesisContract::chain_key(UNBRIEFED_DAY, SRC_CHAIN);
        assert_eq!(contract.chain_bid_count.read(&key).unwrap(), 0);
    });
    assert_eq!(ignored_reasons(&storage), vec![IGNORED_UNKNOWN_DAY]);
}

#[test]
fn a_batch_before_reveal_still_reverts_so_the_transport_redelivers() {
    let mut storage = storage();
    StorageHandle::enter(&mut storage, |s| {
        assert_eq!(
            crate::api::dispatch_auction_brief(
                s.clone(),
                WORLDWIDE_DAY,
                U256::from(10 * LOAD_MINOR),
                vec![crate::schema::ReferenceCurrencyPrice {
                    iso_code: REFERENCE_ISO,
                    entry_price_minor: U256::from(2_000_000u64),
                }],
                true,
                NOW,
            )
            .unwrap(),
            AuctionBriefReceipt::Accepted
        );
        runtime::schedule_tick(&s, NOW).unwrap(); // Started, commit stage running
        assert!(runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            WORLDWIDE_DAY,
            SRC_CHAIN,
            1,
            0,
            1,
            bids(1),
        )
        .is_err());
    });
    assert!(ignored_reasons(&storage).is_empty());
}

#[test]
fn a_stale_marker_is_acknowledged() {
    let mut storage = storage();
    StorageHandle::enter(&mut storage, |s| {
        open_revealing(&s);
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            WORLDWIDE_DAY,
            SRC_CHAIN,
            2,
            0,
            1,
            bids(1),
        )
        .unwrap();
        runtime::process_bids_done(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            WORLDWIDE_DAY,
            SRC_CHAIN,
            1,
            1,
            1,
        )
        .unwrap();
        let contract = DesisContract::new(s.clone());
        let key = DesisContract::chain_key(WORLDWIDE_DAY, SRC_CHAIN);
        assert_eq!(
            contract.chain_done_batches.read(&key).unwrap(),
            0,
            "stale marker not recorded"
        );
    });
    assert_eq!(ignored_reasons(&storage), vec![IGNORED_STALE_GENERATION]);
}

#[test]
fn a_repeated_marker_is_a_no_op_and_a_differing_one_is_reported() {
    let mut storage = storage();
    StorageHandle::enter(&mut storage, |s| {
        open_revealing(&s);
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            WORLDWIDE_DAY,
            SRC_CHAIN,
            1,
            0,
            2,
            bids(1),
        )
        .unwrap();
        // Marker claims 2 batches / 3 bids; the second batch is still missing.
        runtime::process_bids_done(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            WORLDWIDE_DAY,
            SRC_CHAIN,
            1,
            2,
            3,
        )
        .unwrap();
        // The same marker again: nothing to report.
        runtime::process_bids_done(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            WORLDWIDE_DAY,
            SRC_CHAIN,
            1,
            2,
            3,
        )
        .unwrap();
        // A marker that disagrees: the first one stands.
        runtime::process_bids_done(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            WORLDWIDE_DAY,
            SRC_CHAIN,
            1,
            2,
            5,
        )
        .unwrap();

        let contract = DesisContract::new(s.clone());
        let key = DesisContract::chain_key(WORLDWIDE_DAY, SRC_CHAIN);
        assert_eq!(contract.chain_done_batches.read(&key).unwrap(), 2);
        assert_eq!(
            contract.chain_done_bids.read(&key).unwrap(),
            3,
            "the first marker stands"
        );
    });
    assert_eq!(ignored_reasons(&storage), vec![IGNORED_CONFLICTING_MARKER]);
}
