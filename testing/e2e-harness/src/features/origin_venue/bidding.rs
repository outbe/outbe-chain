//! Bids, clearing, issuance and refunds of one auction day on every venue.

use std::thread::sleep;
use std::time::{Duration, Instant};

use alloy_primitives::U256;
use cucumber::{then, when};

use super::auction::{
    advance_past_window_to_stage, assert_no_parked_deliveries, flush_parked_bid_relays,
    settled_day, venue_side, venue_sides, VenueSide, AUCTION_STAGE_TIMEOUT, AUCTION_START_TIMEOUT,
};
use crate::internal::eth;
use crate::world::venue_probes::{IAuctionStage, IIssuedSeries, IPaymentToken, IRefundClaim};
use crate::world::{bidders, origin_venue, venue_probes, World};

const BIDDER_ALLOWANCE: u128 = 1_000_000_000 * 1_000_000_000_000_000_000;

const BIDS: [(u16, u32); 2] = [(30, 800_000), (40, 700_000)];

#[when("two bidders commit their bids on every chain")]
fn bidders_commit(world: &mut World) {
    let worldwide_day = settled_day(world);
    // The same two bidders trade on every chain the day opened on: one keypair
    // funded on each, so clearing has to collect their bids from both.
    let bidders = bidders::derive(&BIDS).expect("derive the bidders");
    for side in venue_sides(world) {
        for bidder in &bidders {
            assert_eq!(
                eth::read_call(
                    &side.url,
                    side.payment_token,
                    &IPaymentToken::balanceOfCall {
                        account: bidder.address
                    }
                ),
                Some(U256::ZERO),
                "fresh auction bidder starts with no payment tokens"
            );
        }
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

#[when("those bidders reveal their bids once the venues are revealing")]
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
        assert_no_parked_deliveries(world);
        if stage == Some(5) {
            return;
        }
        flush_parked_bid_relays(world, worldwide_day);
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
            venue_probes::parked_work(&url, &venue_url, router, venue_router, worldwide_day),
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

#[then("the cleared day issues the Intex on every chain it reached")]
fn cleared_day_issues_intex(world: &mut World) {
    let bidders = world.state.auction_bidders.clone();
    let home = world.rpc.url(world.validators.primary_port());
    let worldwide_day = settled_day(world);
    assert_ne!(
        venue_probes::cleared_empty(&home, worldwide_day),
        Some(true),
        "day {worldwide_day} cleared empty: the day's Desis Limit bought no whole Intex, so there is \
         nothing to issue and this scenario cannot cover minting"
    );
    // The day issues to every chain it opened on, so every chain has to show the
    // series and the units its own bidders won.
    for side in venue_sides(world) {
        issuances_landed_on(&side, &bidders, worldwide_day);
    }
}

fn issuances_landed_on(side: &VenueSide, bidders: &[bidders::Bidder], worldwide_day: u32) {
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
        "series {series} exists on {url} but no bidder holds any Intex ({} issuances were deferred)",
        venue_probes::deferred_issuances(&url, side.target_router)
    );
}

#[then("each escrow settles the day and returns what the bids did not buy")]
fn escrow_refunds_the_rest(world: &mut World) {
    // Each chain settled its own bids, so each has to return what it did
    // not buy, and each bidder collects it.
    for side in venue_sides(world) {
        refunds_landed_on(world, &side);
        claim_refunds_on(world, &side);
    }
    crate::features::auction_expectations::assert_clearing(
        world,
        &venue_sides(world),
        &world.state.auction_bidders,
        U256::from(BIDDER_ALLOWANCE),
    );
}

fn claim_refunds_on(world: &World, side: &VenueSide) {
    let worldwide_day = settled_day(world);
    for bidder in &world.state.auction_bidders {
        let claimable = eth::read_call(
            &side.url,
            side.escrow,
            &IRefundClaim::getClaimableRefundCall {
                worldwideDay: worldwide_day,
                bidder: bidder.address,
            },
        )
        .expect("read what the bidder can claim");
        if claimable.amount == 0 {
            continue;
        }
        eth::send_call(
            &side.url,
            side.escrow,
            &bidder.key,
            &IRefundClaim::claimRefundCall {
                worldwideDay: worldwide_day,
                bidder: bidder.address,
            },
            None,
        )
        .expect("claim the bidder's refund");
    }
}

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
        assert_no_parked_deliveries(world);
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

    // Settlement either pays a bid or returns it, so the day leaves nothing behind.
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
