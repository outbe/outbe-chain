//! Real clearing checkpoints conserve sold units, unused capacity and dust.
use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{SolCall, SolEvent};
use outbe_oracle::{api, schema::OracleContract};
use outbe_primitives::{
    address_pair::AddressPair,
    addresses::DESIS_ADDRESS,
    block::{BlockContext, BlockRuntimeContext},
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
    time::{previous_date_key, timestamp_to_date_key, WorldwideDay},
};
use outbe_promislimit::PromisLimitContract;

use crate::{
    constants::ORIGIN_ROUTER_ADDRESS,
    precompile::IDesis,
    runtime,
    schema::{AuctionStage, BidData, DesisContract},
};

const NOW: u64 = 1_704_067_205;
const ANCHOR: u64 = NOW - 5;
const DAY: WorldwideDay = WorldwideDay::new(20_240_101);
const LOAD: u128 = 100_000_000_000;
const LIMIT: u128 = 3 * LOAD + 7;
const CHAIN: u32 = 1;

fn world(sale: bool) -> HashMapStorageProvider {
    let mut p = HashMapStorageProvider::new(1);
    p.set_timestamp(U256::from(NOW));
    p.stub_sub_call_at(
        ORIGIN_ROUTER_ADDRESS,
        Bytes::from(crate::sol_ext::IOriginRouter::targetsOfCall::abi_encode_returns(&vec![CHAIN])),
    );
    p.stub_sub_call_at(
        outbe_intexfactory::constants::INTEX_NFT1155_ADDRESS,
        Bytes::from(vec![0; 32]),
    );
    StorageHandle::enter(&mut p, |s| {
        outbe_intexfactory::schema::IntexFactoryContract::new(s.clone())
            .config_profile
            .write(outbe_intexfactory::config::PROFILE_PROD)
            .unwrap();
        let pair = api::register_pair(s.clone(), AddressPair::new_coen_to(840)).unwrap();
        let oracle = OracleContract::new(s.clone());
        oracle.reference_currencies.push(840).unwrap();
        for offset in 0..5 {
            let day = previous_date_key(timestamp_to_date_key(NOW + offset * 86_400));
            oracle
                .record_utc_day_vwap(day, pair, U256::from(2_000_000))
                .unwrap();
            oracle.utc_day_vwap_last_finalized.write(day).unwrap();
        }
        crate::api::dispatch_auction_brief(
            s.clone(),
            DAY,
            U256::from(LIMIT),
            true,
            NOW,
            crate::api::BriefOverflowPolicy::CarryOver,
        )
        .unwrap();
        for at in [NOW, ANCHOR + 86_400, ANCHOR + 2 * 86_400] {
            runtime::schedule_tick(&s, at).unwrap();
        }
        let bids = if sale {
            vec![BidData {
                bidder_address: Address::repeat_byte(1),
                intex_bid_rate: 200,
                timestamp: 0,
                intex_quantity: 1,
                issuance_currency: 840,
                reference_currency: 840,
            }]
        } else {
            vec![]
        };
        runtime::process_bids_batch(s.clone(), ORIGIN_ROUTER_ADDRESS, DAY, CHAIN, 0, 1, bids)
            .unwrap();
        runtime::process_bids_done(s, ORIGIN_ROUTER_ADDRESS, DAY, CHAIN, 1, u32::from(sale))
            .unwrap();
    });
    p.clear_mutation_failure();
    p
}

fn clear(p: &mut HashMapStorageProvider) {
    StorageHandle::enter(p, |s| {
        runtime::tick_gate(&BlockRuntimeContext::new(
            BlockContext::empty_for_tests(2, ANCHOR + 2 * 86_400, 1),
            s,
        ))
        .unwrap();
    });
}

fn ledger(p: &mut HashMapStorageProvider, sold: bool, terminal: bool) {
    StorageHandle::enter(p, |s| {
        let day = DesisContract::new(s.clone());
        let returned = PromisLimitContract::new(s).get_total_unallocated().unwrap();
        let pending = day.pending_desis_limit_minor.read(&DAY).unwrap();
        let issued = day.last_clearing_issued_count.read().unwrap();
        assert_eq!(
            pending + returned + U256::from(u128::from(issued) * LOAD),
            U256::from(LIMIT)
        );
        if terminal {
            assert_eq!(day.read_stage(DAY).unwrap(), AuctionStage::Cleared);
            assert_eq!(issued, u32::from(sold));
            assert_eq!(pending, U256::ZERO);
            assert_eq!(returned, U256::from(LIMIT - u128::from(sold) * LOAD));
            assert_eq!(day.gate_active_count.read().unwrap(), 0);
        } else {
            assert_eq!(day.read_stage(DAY).unwrap(), AuctionStage::Clearing);
            assert_eq!(pending, U256::from(LIMIT));
            assert_eq!(returned, U256::ZERO);
            assert_eq!(issued, 0);
        }
    });
    let records = p
        .get_events(DESIS_ADDRESS)
        .iter()
        .filter_map(|e| IDesis::DesisAllocationRecorded::decode_log_data(e).ok())
        .collect::<Vec<_>>();
    assert_eq!(records.len(), usize::from(terminal));
    if terminal {
        assert_eq!(
            records[0].desisAllocationMinor,
            U256::from(u128::from(sold) * LOAD)
        );
        assert_eq!(records[0].desisLimitMinor, U256::from(LIMIT));
    }
}

#[test]
fn no_sale_returns_all_capacity_and_dust_once_after_cleanup() {
    let mut p = world(false);
    clear(&mut p);
    ledger(&mut p, false, true);
    clear(&mut p);
    ledger(&mut p, false, true);
}

#[test]
fn partial_sale_returns_only_two_unsold_loads_and_dust_once() {
    let mut p = world(true);
    clear(&mut p);
    ledger(&mut p, true, true);
    clear(&mut p);
    ledger(&mut p, true, true);
}

#[test]
fn every_clearing_write_failure_rolls_back_allocation_and_return_then_retries_once() {
    for sale in [false, true] {
        let mut baseline = world(sale);
        clear(&mut baseline);
        let count = baseline.clear_mutation_failure();
        assert!(count > 0);
        for after in [false, true] {
            for point in 0..count {
                let mut p = world(sale);
                if after {
                    p.fail_after_mutation_at(point);
                } else {
                    p.fail_mutation_at(point);
                }
                clear(&mut p);
                let observed = p.clear_mutation_failure();
                let reached = if after {
                    observed > point
                } else {
                    observed == point
                };
                assert!(reached, "unreached fault {point}, after={after}");
                ledger(&mut p, sale, false);
                clear(&mut p);
                clear(&mut p);
                ledger(&mut p, sale, true);
            }
        }
    }
}
