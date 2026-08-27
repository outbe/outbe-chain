//! Steps for the intex engine deployed onto the committee's own chain.

use std::thread::sleep;
use std::time::{Duration, Instant};

#[cfg(feature = "ocomp-integration")]
use alloy_primitives::{Address, U256};
use cucumber::{then, when};

use crate::env::environment;
use crate::internal::eth;
#[cfg(feature = "ocomp-integration")]
use crate::world::bidders;
use crate::world::venue_probes::{IAuctionStage, IProceedsRoute};
#[cfg(feature = "ocomp-integration")]
use crate::world::venue_probes::{IIssuedSeries, IParkedWork, IPaymentToken};
use crate::world::{origin_venue, venue_probes, World};

const ORIGIN_DEPLOY_FUNDING_COEN: u64 = 5_400;

fn origin_deploy_funding_plan(
    capacity_owner_keys: &[String],
    fallback_operator_key: String,
) -> Vec<(String, u64)> {
    let funders = capacity_owner_keys
        .iter()
        .rev()
        .take(6)
        .cloned()
        .collect::<Vec<_>>();
    if funders.is_empty() {
        return vec![(fallback_operator_key, ORIGIN_DEPLOY_FUNDING_COEN)];
    }
    let share = ORIGIN_DEPLOY_FUNDING_COEN
        / u64::try_from(funders.len()).expect("at most six origin deploy funders");
    funders.into_iter().map(|key| (key, share)).collect()
}

#[when("the intex engine is deployed on the committee chain")]
fn deploy_origin_venue(world: &mut World) {
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let chain_id = world.rpc.chain_id(port).expect("committee chain id");
    // Genesis funds validators and Tribute owners, so the deploy account starts
    // empty on this chain and cannot even pay for its own scripts.
    // One owner's balance does not cover the whole deploy, so several chip in.
    // Taken from the tail: the scenario's Tribute submitters are the head of the
    // same list, and they still need their own gas.
    let fallback_operator_key = world
        .validators
        .get(0)
        .evm_key()
        .expect("read validator-0 deploy funding key");
    let funding_plan = origin_deploy_funding_plan(
        &world.state.ocomp_capacity_tribute_private_keys,
        fallback_operator_key,
    );
    for (funder, amount_coen) in funding_plan {
        crate::internal::eth::send_value(
            &url,
            origin_venue::deployer_address(),
            &funder,
            crate::internal::eth::coen(amount_coen),
        )
        .expect("fund the deploy account on the committee chain");
    }
    // The committee is always a target of its own day; a started second chain
    // joins it, so the day fans out to both.
    let targets: Vec<u64> = std::iter::once(chain_id)
        .chain(
            world
                .target_chain
                .port()
                .map(|_| world.target_chain.chain_id()),
        )
        .collect();
    let contracts = origin_venue::deploy(&environment().repo, &url, chain_id, &targets)
        .expect("deploy the intex engine on the committee chain");
    world.state.origin_contracts = Some(contracts);
}

#[then("the committee chain hosts the intex engine")]
fn committee_chain_hosts_engine(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let contracts = world
        .state
        .origin_contracts
        .clone()
        .expect("a deploy recorded its addresses");

    for (name, address) in [
        ("ERC7786Bridge", contracts.bridge),
        ("LoopbackGatewayAdapter", contracts.loopback),
        ("OriginRouter", contracts.origin_router),
        ("IntexNFT1155", contracts.intex_nft),
        ("TargetRouter", contracts.target_router),
    ] {
        assert!(
            eth::code(&url, address).is_some_and(|code| !code.is_empty()),
            "{name} reported at {address} but the committee chain holds no code there"
        );
    }
}

#[then("the origin router knows where proceeds come from")]
fn origin_router_knows_proceeds_route(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let contracts = world
        .state
        .origin_contracts
        .clone()
        .expect("a deploy recorded its addresses");

    let bridge = eth::read_call(
        &url,
        contracts.origin_router,
        &IProceedsRoute::tokenBridgeCall {},
    )
    .expect("read tokenBridge from the origin router");
    let token = eth::read_call(&url, contracts.origin_router, &IProceedsRoute::wcoenCall {})
        .expect("read wcoen from the origin router");
    assert!(
        !bridge.is_zero() && token == contracts.wcoen,
        "proceeds route is unset: bridge {bridge}, token {token}"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_deploy_funding_uses_the_operator_without_a_capacity_fixture() {
        assert_eq!(
            origin_deploy_funding_plan(&[], "validator-0".to_owned()),
            vec![("validator-0".to_owned(), 5_400)]
        );
    }

    #[test]
    fn origin_deploy_funding_preserves_the_capacity_owner_pool() {
        let keys = (0..8)
            .map(|index| format!("owner-{index}"))
            .collect::<Vec<_>>();
        let plan = origin_deploy_funding_plan(&keys, "validator-0".to_owned());
        assert_eq!(plan.len(), 6);
        assert_eq!(plan.iter().map(|(_, amount)| amount).sum::<u64>(), 5_400);
        assert_eq!(
            plan.iter().map(|(key, _)| key.as_str()).collect::<Vec<_>>(),
            vec!["owner-7", "owner-6", "owner-5", "owner-4", "owner-3", "owner-2"]
        );
    }
}

/// An e2e build runs the auction on minute-long windows, so the run waits the
/// stages out rather than moving the clock across a day it never formed.
#[cfg(feature = "ocomp-integration")]
const AUCTION_STAGE_TIMEOUT: Duration = Duration::from_secs(2400);

#[cfg(feature = "ocomp-integration")]
fn advance_past_window_to_stage(world: &mut World, target_stage: u8) {
    for side in venue_sides(world) {
        advance_one_venue_to_stage(world, &side, target_stage);
    }
}

/// Each chain runs the day on its own clock, so every venue has to reach the
/// stage before the bidders may act on any of them.
#[cfg(feature = "ocomp-integration")]
fn advance_one_venue_to_stage(world: &World, venue: &VenueSide, target_stage: u8) {
    let worldwide_day = settled_day(world);
    let url = venue.url.clone();
    let home = world.rpc.url(world.validators.primary_port());
    let deadline = Instant::now() + AUCTION_STAGE_TIMEOUT;
    loop {
        let stage = eth::read_call(
            &url,
            venue.auction,
            &IAuctionStage::getAuctionStageCall {
                worldwideDay: worldwide_day,
            },
        )
        .expect("the venue knows the auction");
        assert_ne!(stage, 4, "day {worldwide_day} was cancelled on the venue");
        if stage >= target_stage {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "day {worldwide_day} stalled at venue stage {stage} short of {target_stage} on {}",
            venue.url
        );
        if venue.is_target_chain {
            if let Some(now) = eth::latest_block_timestamp(&home) {
                let _ = world.target_chain.sync_clock_to(now);
            }
        }
        sleep(Duration::from_secs(2));
    }
}

/// Where the venue actually lives. With a second chain started the auction runs
/// there and its messages ride the relay home; without one the committee is its
/// own target and everything stays on one chain.
#[cfg(feature = "ocomp-integration")]
struct VenueSide {
    url: String,
    chain_id: u64,
    auction: Address,
    escrow: Address,
    payment_token: Address,
    intex_nft: Address,
    target_router: Address,
    is_target_chain: bool,
}

#[cfg(feature = "ocomp-integration")]
fn venue_sides(world: &World) -> Vec<VenueSide> {
    let origin = world
        .state
        .origin_contracts
        .clone()
        .expect("a deploy recorded its addresses");
    let mut sides = vec![VenueSide {
        url: world.rpc.url(world.validators.primary_port()),
        chain_id: world
            .rpc
            .chain_id(world.validators.primary_port())
            .expect("committee chain id"),
        auction: origin.intex_auction,
        escrow: origin.escrow,
        payment_token: origin.payment_token,
        intex_nft: origin.intex_nft,
        target_router: origin.target_router,
        is_target_chain: false,
    }];
    if let (Some(_), Some(target)) = (
        world.target_chain.port(),
        world.state.target_contracts.clone(),
    ) {
        sides.push(VenueSide {
            url: world.target_chain.rpc_url().expect("target chain started"),
            chain_id: world.target_chain.chain_id(),
            auction: target.auction,
            escrow: target.escrow,
            payment_token: target.payment_token,
            intex_nft: target.intex_nft,
            target_router: target.target_router,
            is_target_chain: true,
        });
    }
    sides
}

#[cfg(feature = "ocomp-integration")]
fn venue_side(world: &World) -> VenueSide {
    let origin = world
        .state
        .origin_contracts
        .clone()
        .expect("a deploy recorded its addresses");
    match (
        world.target_chain.port(),
        world.state.target_contracts.clone(),
    ) {
        (Some(_), Some(target)) => VenueSide {
            url: world.target_chain.rpc_url().expect("target chain started"),
            chain_id: world.target_chain.chain_id(),
            auction: target.auction,
            escrow: target.escrow,
            payment_token: target.payment_token,
            intex_nft: target.intex_nft,
            target_router: target.target_router,
            is_target_chain: true,
        },
        _ => VenueSide {
            url: world.rpc.url(world.validators.primary_port()),
            chain_id: world
                .rpc
                .chain_id(world.validators.primary_port())
                .expect("committee chain id"),
            auction: origin.intex_auction,
            escrow: origin.escrow,
            payment_token: origin.payment_token,
            intex_nft: origin.intex_nft,
            target_router: origin.target_router,
            is_target_chain: false,
        },
    }
}

const AUCTION_START_TIMEOUT: Duration = Duration::from_secs(1200);

#[then("the auction for that day opens on the target chain")]
fn auction_opens_on_target(world: &mut World) {
    let venue = venue_side(world);
    let url = venue.url.clone();
    // Desis and the origin router answer on the committee, whichever chain the
    // venue itself sits on.
    let home = world.rpc.url(world.validators.primary_port());
    let origin_router = world
        .state
        .origin_contracts
        .clone()
        .expect("a deploy recorded its addresses")
        .origin_router;
    let worldwide_day = world
        .state
        .wwd
        .as_deref()
        .expect("the settled day's WorldwideDay")
        .parse::<u32>()
        .expect("numeric WorldwideDay");

    // The venue reverts with AuctionNotFound until the start message lands, so a
    // reply at all is the proof that the whole origin-to-target path ran.
    let deadline = Instant::now() + AUCTION_START_TIMEOUT;
    loop {
        if let Some(stage) = eth::read_call(
            &url,
            venue.auction,
            &IAuctionStage::getAuctionStageCall {
                worldwideDay: worldwide_day,
            },
        ) {
            assert!(
                stage < 2,
                "day {worldwide_day} reached the venue at stage {stage}: {}",
                venue_probes::venue_schedule(&url, venue.auction, worldwide_day)
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "day {worldwide_day} settled but no auction ever opened on the venue at {}: {}; {}; {}; {}",
            venue.auction,
            venue_probes::desis_stage(&home, worldwide_day),
            venue_probes::frozen_targets(&home, origin_router, worldwide_day),
            venue_probes::day_closure(world, worldwide_day),
            venue_probes::day_colour(world, worldwide_day)
        );
        sleep(Duration::from_secs(2));
    }
}

/// After a logical-time jump the ratchet lets each block carry up to an hour, so
/// a minute-long auction window would burn out in seconds. Wait for blocks that
/// carry ordinary time: a chain that has not resumed yet looks identical to a
/// settled one if only the clock is sampled.
#[cfg(feature = "ocomp-integration")]
#[when("the committee clock settles after the jump")]
fn committee_clock_settles(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let deadline = Instant::now() + AUCTION_STAGE_TIMEOUT;
    let mut calm_samples = 0;
    let mut previous: Option<(u64, u64)> = None;
    loop {
        sleep(Duration::from_secs(6));
        let height = eth::block_number(&url);
        let stamp = eth::latest_block_timestamp(&url);
        if let (Some(height), Some(stamp), Some((last_height, last_stamp))) =
            (height, stamp, previous)
        {
            let blocks = height.saturating_sub(last_height);
            let seconds = stamp.saturating_sub(last_stamp);
            if blocks > 0 && seconds / blocks < 120 {
                calm_samples += 1;
                if calm_samples >= 2 {
                    return;
                }
            } else {
                calm_samples = 0;
            }
        }
        previous = height.zip(stamp);
        assert!(
            Instant::now() < deadline,
            "the committee clock never settled after the jump"
        );
    }
}

/// A loopback venue cannot relay its bids from inside the clearing delivery —
/// that would be a nested send in the same transaction — so it parks the relay
/// for a permissionless retry. Production has a keeper for this; the run does it
/// itself.
/// The loopback adapter isolates a failed delivery by parking it, and a real
/// transport is what retries. Nothing plays that part in a localnet, so the
/// scenario does: retrying is permissionless and a still-broken delivery simply
/// parks again.
#[cfg(feature = "ocomp-integration")]
fn flush_parked_deliveries(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let Some(loopback) = world
        .state
        .origin_contracts
        .clone()
        .map(|contracts| contracts.loopback)
    else {
        return;
    };
    let parked =
        eth::read_call(&url, loopback, &IParkedWork::nextParkedIdxCall {}).unwrap_or_default();
    let mut idx = U256::ZERO;
    while idx < parked {
        let _ = eth::send_call(
            &url,
            loopback,
            crate::world::forge::DEPLOYER_KEY,
            &IParkedWork::retryDeliveryCall { idx },
            None,
        );
        idx += U256::from(1);
    }
}

#[cfg(feature = "ocomp-integration")]
fn flush_parked_bid_relays(world: &mut World) {
    for venue in venue_sides(world) {
        flush_one_venue_bid_relays(&venue);
    }
}

/// Each venue keeps its own deferred-relay queue, and the routers share an
/// address across chains — so a flush has to be asked of the chain that parked it.
#[cfg(feature = "ocomp-integration")]
fn flush_one_venue_bid_relays(venue: &VenueSide) {
    let url = venue.url.clone();
    let venue_router = venue.target_router;
    let parked = eth::read_call(
        &url,
        venue_router,
        &IParkedWork::nextPendingBidsRelayIdxCall {},
    )
    .unwrap_or_default();
    let mut idx = U256::ZERO;
    while idx < parked {
        eth::send_call(
            &url,
            venue_router,
            crate::world::forge::DEPLOYER_KEY,
            &IParkedWork::flushPendingBidsRelayCall { idx },
            None,
        )
        .ok();
        idx += U256::from(1);
    }
}

#[cfg(feature = "ocomp-integration")]
fn settled_day(world: &World) -> u32 {
    world
        .state
        .wwd
        .as_deref()
        .expect("the settled day's WorldwideDay")
        .parse::<u32>()
        .expect("numeric WorldwideDay")
}

#[cfg(feature = "ocomp-integration")]
const BIDDER_ALLOWANCE: u128 = 1_000_000_000 * 1_000_000_000_000_000_000;

#[cfg(feature = "ocomp-integration")]
const BIDS: [(u16, u32); 2] = [(30, 800_000), (40, 700_000)];

#[cfg(feature = "ocomp-integration")]
#[when("two bidders commit their bids")]
fn bidders_commit(world: &mut World) {
    let worldwide_day = settled_day(world);
    // The same two bidders trade on every chain the day opened on: one keypair
    // funded on each, so clearing has to fan their bids in from both.
    let bidders = bidders::derive(&BIDS).expect("derive the bidders");
    for side in venue_sides(world) {
        bidders::fund(
            &side.url,
            side.payment_token,
            side.escrow,
            &bidders,
            U256::from(BIDDER_ALLOWANCE),
        )
        .expect("fund the bidders");
        bidders::commit(
            &side.url,
            side.auction,
            side.chain_id,
            worldwide_day,
            &bidders,
        )
        .expect("commit the bids");
    }
    world.state.auction_bidders = bidders;
}

#[cfg(feature = "ocomp-integration")]
#[when("those bidders reveal their bids once the venue is revealing")]
fn bidders_reveal(world: &mut World) {
    advance_past_window_to_stage(world, 1);

    let worldwide_day = settled_day(world);
    let bidders = world.state.auction_bidders.clone();
    for side in venue_sides(world) {
        bidders::reveal(
            &side.url,
            side.auction,
            side.chain_id,
            worldwide_day,
            &bidders,
        )
        .expect("reveal the bids");
    }
}

#[cfg(feature = "ocomp-integration")]
#[then("the auction clears and the venue moves past its reveal window")]
fn auction_clears(world: &mut World) {
    advance_past_window_to_stage(world, 2);

    let deployed = world
        .state
        .origin_contracts
        .clone()
        .expect("a deploy recorded its addresses");
    let side = venue_side(world);
    let venue_url = side.url.clone();
    let venue = side.auction;
    let router = deployed.origin_router;
    let venue_router = side.target_router;
    let worldwide_day = settled_day(world);
    let deadline = Instant::now() + AUCTION_START_TIMEOUT;
    loop {
        let url = world.rpc.url(world.validators.primary_port());
        let desis = origin_venue::DESIS
            .parse()
            .expect("desis precompile address");
        let stage = eth::read_call(
            &url,
            desis,
            &IAuctionStage::getAuctionStageCall {
                worldwideDay: worldwide_day,
            },
        );
        if stage == Some(5) {
            return;
        }
        flush_parked_bid_relays(world);
        flush_parked_deliveries(world);
        assert_ne!(stage, Some(6), "Desis cancelled day {worldwide_day}");
        assert!(
            Instant::now() < deadline,
            "day {worldwide_day} never cleared on Desis: {}; {}; venue stage {:?}, {}; {}; {}; {}; {}; {}; {}; {}",
            venue_probes::desis_stage(&url, worldwide_day),
            venue_probes::relayed_bids(&url, worldwide_day, side.chain_id as u32),
            eth::read_call(
                &url,
                venue,
                &IAuctionStage::getAuctionStageCall {
                    worldwideDay: worldwide_day
                },
            ),
            venue_probes::venue_bid_counts(&venue_url, venue, worldwide_day),
            venue_probes::parked_work(&url, &venue_url, router, venue_router),
            venue_probes::stages_received(&venue_url, venue_router, worldwide_day),
            venue_probes::ignored_inbound(&url, router, "origin"),
            venue_probes::ignored_inbound(&venue_url, venue_router, "venue"),
            venue_probes::ignored_by_desis(&url, worldwide_day),
            venue_probes::bid_relay_traffic(&url, &venue_url, router, venue_router, worldwide_day),
            venue_probes::parked_deliveries(world)
        );
        sleep(Duration::from_secs(2));
    }
}

#[cfg(feature = "ocomp-integration")]
#[then("the cleared day mints the Intex on the target chain")]
fn issuance_mints_intex(world: &mut World) {
    let bidders = world.state.auction_bidders.clone();
    let home = world.rpc.url(world.validators.primary_port());
    let worldwide_day = settled_day(world);
    assert_ne!(
        venue_probes::cleared_empty(&home, worldwide_day),
        Some(true),
        "day {worldwide_day} cleared empty: the day's supply bought no whole Intex, so there is \
         nothing to issue and this scenario cannot cover minting"
    );
    // The day issues to every chain it opened on, so every chain has to show the
    // series and the units its own bidders won.
    for side in venue_sides(world) {
        mints_landed_on(&side, &bidders, worldwide_day);
    }
}

#[cfg(feature = "ocomp-integration")]
fn mints_landed_on(side: &VenueSide, bidders: &[bidders::Bidder], worldwide_day: u32) {
    let url = side.url.clone();
    let deadline = Instant::now() + AUCTION_STAGE_TIMEOUT;
    let series = loop {
        if let Some(series) = venue_probes::issued_series(&url, side.target_router) {
            break series;
        }
        assert!(
            Instant::now() < deadline,
            "day {worldwide_day} cleared but {url} never received issuance instructions"
        );
        sleep(Duration::from_secs(2));
    };
    let exists = eth::read_call(
        &url,
        side.intex_nft,
        &IIssuedSeries::seriesExistsCall { seriesId: series },
    )
    .expect("ask the collection whether the series exists");
    assert!(
        exists,
        "series {series} was instructed but never created on {url}"
    );
    let token_id = eth::read_call(
        &url,
        side.intex_nft,
        &IIssuedSeries::issuedTokenIdCall { seriesId: series },
    )
    .expect("derive the token id of the issued series");
    let minted: U256 = bidders
        .iter()
        .map(|bidder| {
            eth::read_call(
                &url,
                side.intex_nft,
                &IIssuedSeries::balanceOfCall {
                    account: bidder.address,
                    id: token_id,
                },
            )
            .expect("read a bidder's Intex balance")
        })
        .sum();
    assert!(
        !minted.is_zero(),
        "series {series} exists on {url} but no bidder holds any Intex ({} mints were deferred)",
        venue_probes::deferred_mints(&url, side.target_router)
    );
}

#[cfg(feature = "ocomp-integration")]
#[then("the escrow settles the day and returns what the bids did not buy")]
fn escrow_refunds_the_rest(world: &mut World) {
    // Each chain settled its own bids, so each has to give back what it did
    // not buy.
    for side in venue_sides(world) {
        refunds_landed_on(world, &side);
    }
}

#[cfg(feature = "ocomp-integration")]
fn refunds_landed_on(world: &World, side: &VenueSide) {
    let url = side.url.clone();
    let worldwide_day = settled_day(world);
    let topic0 = alloy_primitives::keccak256(
        b"RefundInstructionsReceived(uint32,uint32,uint256)".as_slice(),
    );
    let day_topic = format!("0x{worldwide_day:064x}");

    let deadline = Instant::now() + AUCTION_STAGE_TIMEOUT;
    loop {
        let received = eth::raw_json_with_params(
            &url,
            "eth_getLogs",
            serde_json::json!([{
                "fromBlock": "0x0",
                "toBlock": "latest",
                "address": format!("{:?}", side.target_router),
                "topics": [format!("{topic0:?}"), serde_json::Value::Null, day_topic],
            }]),
        )
        .as_ref()
        .and_then(|value| value.as_array())
        .is_some_and(|entries| !entries.is_empty());
        if received {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "day {worldwide_day} cleared but the escrow never received refund instructions \
             (sent by Desis: {}; {} origin sends are parked; {})",
            venue_probes::refunds_were_sent(world, worldwide_day),
            venue_probes::parked_origin_sends(world),
            venue_probes::parked_deliveries(world)
        );
        sleep(Duration::from_secs(2));
    }

    // Settlement either pays a bid or gives it back, so the day leaves nothing behind.
    let held = eth::read_call(
        &url,
        side.payment_token,
        &IPaymentToken::balanceOfCall {
            account: side.escrow,
        },
    )
    .expect("read what the escrow still holds");
    assert!(
        held.is_zero(),
        "escrow still holds {held} of the payment token after settling day {worldwide_day}"
    );
}
