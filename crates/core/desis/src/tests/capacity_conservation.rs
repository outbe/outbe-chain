//! Real clearing checkpoints conserve sold units, unused capacity and dust.
use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{SolCall, SolEvent};
use outbe_oracle::{api, schema::OracleContract};
use outbe_primitives::time::SECONDS_PER_DAY;
use outbe_primitives::{
    address_pair::AddressPair,
    addresses::DESIS_ADDRESS,
    block::{BlockContext, BlockRuntimeContext},
    error::Result,
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
const CLEAR_AT: u64 = ANCHOR + 2 * SECONDS_PER_DAY;
const DAY: WorldwideDay = WorldwideDay::new(20_240_101);
const LOAD: u128 = 1_000_000_000_000;
const LIMIT: u128 = 3 * LOAD + 7;
const CHAIN: u32 = 1;

fn world(sale: bool) -> HashMapStorageProvider {
    world_selling(u16::from(sale))
}

/// The day with one bid for `units` Intex, none when zero.
fn world_selling(units: u16) -> HashMapStorageProvider {
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
        outbe_intex::IntexContract::new(s.clone())
            .config_profile
            .write(outbe_intex::config::PROFILE_PROD)
            .unwrap();
        let pair = api::register_pair(s.clone(), AddressPair::new_coen_to(840)).unwrap();
        let oracle = OracleContract::new(s.clone());
        oracle.reference_currencies.push(840).unwrap();
        for offset in 0..5 {
            let day = previous_date_key(timestamp_to_date_key(NOW + offset * SECONDS_PER_DAY));
            oracle
                .record_utc_day_vwap(day, pair, U256::from(2_000_000))
                .unwrap();
            oracle.utc_day_vwap_last_finalized.write(day).unwrap();
        }
        crate::api::dispatch_auction_brief(
            s.clone(),
            crate::api::AuctionBrief {
                worldwide_day: DAY,
                desis_limit_minor: U256::from(LIMIT),
                has_limit: true,
            },
            NOW,
            crate::api::BriefOverflowPolicy::ReturnToPromisLimit,
        )
        .unwrap();
    });
    for at in [NOW, ANCHOR + SECONDS_PER_DAY, CLEAR_AT] {
        p.set_timestamp(U256::from(at));
        StorageHandle::enter(&mut p, |s| runtime::schedule_tick(&s, at).unwrap());
    }
    StorageHandle::enter(&mut p, |s| {
        let bids = if units > 0 {
            vec![BidData {
                bidder_address: Address::repeat_byte(1),
                intex_bid_rate: 200,
                timestamp: 0,
                intex_quantity: units,
                issuance_currency: 840,
                reference_currency: 840,
            }]
        } else {
            vec![]
        };
        runtime::process_bids_batch(
            s.clone(),
            runtime::Inbound {
                caller: ORIGIN_ROUTER_ADDRESS,
                worldwide_day: DAY,
                src_chain_id: CHAIN,
            },
            0,
            1,
            bids,
        )
        .unwrap();
        runtime::process_bids_done(
            s,
            runtime::Inbound {
                caller: ORIGIN_ROUTER_ADDRESS,
                worldwide_day: DAY,
                src_chain_id: CHAIN,
            },
            1,
            u32::from(units > 0),
        )
        .unwrap();
    });
    p.clear_mutation_failure();
    p
}

fn gate(p: &mut HashMapStorageProvider) -> Result<()> {
    p.set_timestamp(U256::from(CLEAR_AT));
    StorageHandle::enter(p, |s| {
        runtime::tick_gate(&BlockRuntimeContext::new(
            BlockContext::empty_for_tests(2, CLEAR_AT, 1),
            s,
        ))
    })
}

fn clear(p: &mut HashMapStorageProvider) {
    gate(p).unwrap();
}

fn ledger(p: &mut HashMapStorageProvider, sold: bool, terminal: bool) {
    ledger_selling(p, u32::from(sold), terminal);
}

fn ledger_selling(p: &mut HashMapStorageProvider, sold: u32, terminal: bool) {
    StorageHandle::enter(p, |s| {
        let day = DesisContract::new(s.clone());
        let returned = PromisLimitContract::new(s).promis_limit().unwrap();
        let pending = day.pending_desis_limit_minor.read(&DAY).unwrap();
        let issued = day.last_clearing_issued_count.read().unwrap();
        assert_eq!(
            pending + returned + U256::from(u128::from(issued) * LOAD),
            U256::from(LIMIT)
        );
        if terminal {
            assert_eq!(day.read_stage(DAY).unwrap(), AuctionStage::Cleared);
            assert_eq!(issued, sold);
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
fn no_sale_returns_all_capacity_and_dust_once_across_repeated_clears() {
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

/// Selling every unit returns only the dust, and the day's one series holds all three.
#[test]
fn full_sale_returns_only_the_dust_once() {
    let mut p = world_selling(3);
    clear(&mut p);
    ledger_selling(&mut p, 3, true);
    clear(&mut p);
    ledger_selling(&mut p, 3, true);
    let unused = p
        .get_events(DESIS_ADDRESS)
        .iter()
        .filter_map(|e| IDesis::UnusedDesisLimitReported::decode_log_data(e).ok())
        .map(|e| e.unusedDesisLimitMinor)
        .collect::<Vec<_>>();
    assert_eq!(unused, vec![U256::from(7u64)]);
    let cleared = p
        .get_events(DESIS_ADDRESS)
        .iter()
        .filter_map(|e| IDesis::AuctionCleared::decode_log_data(e).ok())
        .map(|e| e.issuedUnits)
        .collect::<Vec<_>>();
    assert_eq!(cleared, vec![3]);
    StorageHandle::enter(&mut p, |s| {
        assert_eq!(outbe_intex::api::total_series(&s).unwrap(), 1);
        let series = outbe_intex::api::series_id_at(&s, 0).unwrap();
        let counts = outbe_intex::api::unit_counts(&s, series).unwrap();
        assert_eq!((counts.issued, counts.active, counts.forfeited), (3, 3, 0));
    });
    assert!(p
        .get_events(outbe_primitives::addresses::INTEX_ADDRESS)
        .iter()
        .all(|e| outbe_intex::precompile::IIntex::IntexGroupExpired::decode_log_data(e).is_err()));
}

#[test]
fn every_clearing_write_failure_rolls_back_allocation_and_return_then_retries_once() {
    for units in [0, 1, 3] {
        let mut baseline = world_selling(units);
        clear(&mut baseline);
        let count = baseline.clear_mutation_failure();
        ledger_selling(&mut baseline, u32::from(units), true);
        assert!(count > 0);
        for after in [false, true] {
            for point in 0..count {
                let mut p = world_selling(units);
                if after {
                    p.fail_after_mutation_at(point);
                } else {
                    p.fail_mutation_at(point);
                }
                // The gate may swallow or surface the injected fault. The test checks only state.
                let _ = gate(&mut p);
                let observed = p.clear_mutation_failure();
                let reached = if after {
                    observed > point
                } else {
                    observed == point
                };
                assert!(reached, "unreached fault {point}, after={after}");
                ledger_selling(&mut p, u32::from(units), false);
                clear(&mut p);
                clear(&mut p);
                ledger_selling(&mut p, u32::from(units), true);
            }
        }
    }
}
