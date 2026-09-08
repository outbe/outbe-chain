//! Exact two-chain auction settlement checks, independent of Desis clearing.

use std::collections::BTreeSet;

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use alloy_sol_types::sol;
use serde_json::{json, Value};

use super::origin_venue::VenueSide;
use crate::internal::{auction_reference, eth};
use crate::world::venue_probes::{IIssuedSeries, IOriginTargets, IPaymentToken, IVenueSchedule};
use crate::world::{bidders::Bidder, venue_probes, World};

sol! {
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IBidSettlement {
        function bidLocks(uint32 worldwideDay, address bidder) external view returns (
            uint128 lockedAmount, uint32 lockedAt, uint8 status, uint128 failedRefund, bool splitRecorded);
    }
}

fn word(value: U256) -> Value {
    json!(format!("0x{value:064x}"))
}

fn logs(url: &str, address: Address, height: u64, topics: Vec<Value>) -> Vec<Value> {
    eth::raw_json_with_params(url, "eth_getLogs", json!([{
        "fromBlock": "0x0", "toBlock": format!("0x{height:x}"), "address": address, "topics": topics,
    }])).expect("auction evidence logs").as_array().expect("logs array").clone()
}

fn number(value: &Value) -> u64 {
    u64::from_str_radix(
        value
            .as_str()
            .expect("hex block quantity")
            .strip_prefix("0x")
            .expect("quantity prefix"),
        16,
    )
    .expect("u64 block quantity")
}

fn data(log: &Value) -> Vec<u8> {
    serde_json::from_value::<Bytes>(log["data"].clone())
        .expect("event data")
        .to_vec()
}

/// Committee observations use a common finalized checkpoint. The Anvil target
/// has its own pinned mined head; it does not claim Outbe committee finality.
fn view(world: &World, side: &VenueSide) -> (Vec<String>, u64, B256, String) {
    let height = eth::block_number(&side.url).expect("auction observation height");
    let urls = if side.is_target_chain {
        vec![side.url.clone()]
    } else {
        let ports = world.validators.committee_ports();
        world
            .rpc
            .wait_finalized_checkpoint(&ports, height, 120)
            .expect("auction common finality");
        ports.into_iter().map(|port| world.rpc.url(port)).collect()
    };
    let hash: B256 = eth::block_hash(&urls[0], height)
        .expect("auction observation block")
        .parse()
        .expect("block hash");
    let root = eth::state_root(&urls[0], height).expect("auction observation state root");
    for url in &urls {
        assert_eq!(
            eth::block_hash(url, height)
                .expect("peer block")
                .parse::<B256>()
                .unwrap(),
            hash
        );
        assert_eq!(eth::state_root(url, height).expect("peer root"), root);
    }
    (urls, height, hash, root)
}

fn revealed_inputs(
    world: &World,
    side: &VenueSide,
    bidders: &[Bidder],
    day: u32,
) -> (Vec<auction_reference::Bid>, u64) {
    let (urls, height, _, _) = view(world, side);
    let topics = vec![
        json!(keccak256(
            "BidRevealed(uint32,address,uint16,uint32,uint16,uint16)"
        )),
        word(U256::from(day)),
    ];
    let events = logs(&urls[0], side.auction, height, topics.clone());
    assert_eq!(
        events.len(),
        bidders.len(),
        "exact public reveal population"
    );
    for url in &urls {
        assert_eq!(logs(url, side.auction, height, topics.clone()), events);
    }
    let input_height = events
        .iter()
        .map(|event| number(&event["blockNumber"]))
        .min()
        .expect("accepted reveal before clearing");
    let bids = bidders
        .iter()
        .map(|bidder| {
            let bidder_topic = word(U256::from_be_slice(bidder.address.as_slice()));
            let matching: Vec<_> = events
                .iter()
                .filter(|event| event["topics"][2] == bidder_topic)
                .collect();
            let [event] = matching.as_slice() else {
                panic!("one reveal per fixture bidder")
            };
            assert_eq!(event["topics"][3], word(U256::from(bidder.quantity)));
            let expected_data: Vec<_> = [
                U256::from(bidder.bid_rate),
                U256::from(840),
                U256::from(840),
            ]
            .into_iter()
            .flat_map(|value| value.to_be_bytes::<32>())
            .collect();
            assert_eq!(
                data(event),
                expected_data,
                "reveal contains submitted bid terms"
            );
            let block_number = number(&event["blockNumber"]);
            let block = eth::raw_json_with_params(
                &urls[0],
                "eth_getBlockByNumber",
                json!([format!("0x{block_number:x}"), false]),
            )
            .expect("reveal's mined block");
            assert_eq!(block["hash"], event["blockHash"]);
            auction_reference::Bid {
                chain_id: side.chain_id,
                bidder: bidder.address,
                quantity: bidder.quantity,
                rate: bidder.bid_rate,
                timestamp: u32::try_from(number(&block["timestamp"])).expect("reveal timestamp"),
            }
        })
        .collect();
    (bids, input_height)
}

pub(super) fn assert_clearing(
    world: &World,
    sides: &[VenueSide],
    bidders: &[Bidder],
    allowance: U256,
) {
    let request = world
        .state
        .ocomp_job_request
        .as_ref()
        .expect("auction input JobIntent");
    let day = request.worldwide_day;
    let ports = world.validators.committee_ports();
    world
        .rpc
        .wait_finalized_checkpoint(&ports, request.request_height, 120)
        .expect("auction request common finality");
    let input_checkpoint = world
        .rpc
        .checkpoint_at(ports[0], request.request_height)
        .expect("auction input checkpoint");
    assert_eq!(input_checkpoint.block_hash, request.request_block_hash);
    let input = world
        .rpc
        .ocomp_job_record_at_on(ports[0], request.intent_id, request.request_height)
        .expect("canonical auction input")
        .intent;
    for &port in &ports {
        assert_eq!(
            world
                .rpc
                .ocomp_job_record_at_on(port, request.intent_id, request.request_height)
                .expect("peer canonical auction input")
                .intent,
            input
        );
        assert_eq!(
            world
                .rpc
                .checkpoint_at(port, request.request_height)
                .expect("recheck auction input checkpoint"),
            input_checkpoint
        );
    }
    let origin = world
        .state
        .origin_contracts
        .as_ref()
        .expect("origin router deployment");
    let targets = eth::read_call(
        &world.rpc.url(ports[0]),
        origin.origin_router,
        &IOriginTargets::targetsOfCall { worldwideDay: day },
    )
    .expect("frozen auction target order");
    assert_eq!(
        targets
            .iter()
            .map(|id| u64::from(*id))
            .collect::<BTreeSet<_>>(),
        sides
            .iter()
            .map(|side| side.chain_id)
            .collect::<BTreeSet<_>>()
    );
    assert_eq!(targets.len(), sides.len(), "no duplicate frozen targets");
    let mut bids = Vec::new();
    let mut params = None;
    for target in targets {
        let side = sides
            .iter()
            .find(|side| side.chain_id == u64::from(target))
            .unwrap();
        let (submitted, input_height) = revealed_inputs(world, side, bidders, day);
        let (urls, _, _, _) = view(world, side);
        for url in &urls {
            let auction = eth::read_call_at(
                url,
                side.auction,
                &IVenueSchedule::auctionsCall { worldwideDay: day },
                input_height,
            )
            .expect("auction input terms");
            assert_eq!(
                auction.params.promisLoadMinor, 1,
                "declared e2e PROMIS load override"
            );
            if let Some(expected) = &params {
                assert_eq!(&auction.params, expected);
            } else {
                params = Some(auction.params);
            }
        }
        bids.extend(submitted);
    }
    let params = params.expect("auction inputs on named chains");
    let supply = input.frozen_metadosis_values.auction_base / U256::from(params.promisLoadMinor);
    let supply = u32::try_from(supply.min(U256::from(u32::MAX))).expect("capped auction units");
    let expected = auction_reference::clear(
        bids,
        supply,
        params.promisLoadMinor,
        params.minIntexBidRate,
        params.minIntexBidQuantity,
    );
    assert!(expected.units > 0, "fixture exercises actual issuance");
    for side in sides {
        let (urls, height, block_hash, state_root) = view(world, side);
        let clearing_data: Vec<_> = [
            U256::from(expected.units),
            U256::from(expected.rate),
            U256::from(expected.demand),
        ]
        .into_iter()
        .flat_map(|v| v.to_be_bytes::<32>())
        .collect();
        let chain_allocations: Vec<_> = expected
            .allocations
            .iter()
            .filter(|a| a.bid.chain_id == side.chain_id)
            .collect();
        for url in &urls {
            if !side.is_target_chain {
                let event = logs(
                    url,
                    crate::internal::addresses::DESIS_ADDR,
                    height,
                    vec![
                        json!(keccak256("AuctionCleared(uint32,uint32,uint32,uint64)")),
                        word(U256::from(day)),
                    ],
                );
                assert_eq!(event.len(), 1, "one canonical clearing event");
                assert_eq!(
                    data(&event[0]),
                    clearing_data,
                    "exact clearing rate, units and demand"
                );
            }
            let auction = eth::read_call_at(
                url,
                side.auction,
                &IVenueSchedule::auctionsCall { worldwideDay: day },
                height,
            )
            .expect("final venue result");
            assert_eq!(auction.result.auctionClearingRate, u64::from(expected.rate));
            assert_eq!(auction.result.issuedIntexCount, expected.units);
            assert_eq!(
                auction.result.issuedIntexLoadedPromis,
                u128::from(expected.units) * params.promisLoadMinor
            );
            assert_eq!(
                auction.result.wonBidsCount as usize,
                chain_allocations.iter().filter(|a| a.units > 0).count()
            );
            let series = venue_probes::issued_series(url, side.target_router)
                .expect("issued fixture series");
            let token_id = eth::read_call_at(
                url,
                side.intex_nft,
                &IIssuedSeries::issuedTokenIdCall { seriesId: series },
                height,
            )
            .expect("issued token ID");
            for allocation in &chain_allocations {
                let bidder = allocation.bid.bidder;
                assert_eq!(
                    eth::read_call_at(
                        url,
                        side.intex_nft,
                        &IIssuedSeries::balanceOfCall {
                            account: bidder,
                            id: token_id
                        },
                        height
                    )
                    .expect("bidder NFT units"),
                    U256::from(allocation.units),
                    "exact winner units"
                );
                assert_eq!(
                    eth::read_call_at(
                        url,
                        side.payment_token,
                        &IPaymentToken::balanceOfCall { account: bidder },
                        height
                    )
                    .expect("bidder payment balance"),
                    allowance - allocation.paid,
                    "exact payment token debit after refund"
                );
                let lock = eth::read_call_at(
                    url,
                    side.escrow,
                    &IBidSettlement::bidLocksCall {
                        worldwideDay: day,
                        bidder,
                    },
                    height,
                )
                .expect("settled bid lock");
                assert_eq!(U256::from(lock.lockedAmount), allocation.locked);
                assert_eq!(lock.status, 2, "bid finalized");
                assert_eq!(lock.failedRefund, 0);
                assert!(!lock.splitRecorded);
                let refund = logs(
                    url,
                    side.escrow,
                    height,
                    vec![
                        json!(keccak256("FundsRefunded(bytes32,uint32,address,uint128)")),
                        Value::Null,
                        word(U256::from(day)),
                        word(U256::from_be_slice(bidder.as_slice())),
                    ],
                );
                if allocation.refund.is_zero() {
                    assert!(refund.is_empty(), "no zero refund event");
                } else {
                    assert_eq!(refund.len(), 1, "one exact refund event");
                    assert_eq!(data(&refund[0]), allocation.refund.to_be_bytes::<32>());
                }
            }
            assert_eq!(
                eth::block_hash(url, height)
                    .expect("recheck auction block")
                    .parse::<B256>()
                    .unwrap(),
                block_hash
            );
            assert_eq!(
                eth::state_root(url, height).expect("recheck auction state root"),
                state_root
            );
        }
        eprintln!(
            "AUCTION_EXPECTATION {}",
            json!({"day": day, "chain_id": side.chain_id,
                "height": height, "block_hash": block_hash, "input_auction_base": input.frozen_metadosis_values.auction_base,
                "input_supply_units": supply, "expected": expected,
            })
        );
    }
}
