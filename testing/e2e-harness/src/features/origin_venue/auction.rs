//! Auction stage advancement across the committee chain and every named venue.

use std::thread::sleep;
use std::time::{Duration, Instant};

use alloy_primitives::{Address, U256};
use cucumber::{then, when};

use crate::internal::eth;
use crate::world::venue_probes;
use crate::world::venue_probes::{IAuctionStage, IParkedWork, IVenueSchedule};
use crate::world::World;

pub(super) const AUCTION_STAGE_TIMEOUT: Duration = Duration::from_secs(2400);

pub(super) fn advance_past_window_to_stage(world: &mut World, target_stage: u8) {
    jump_committee_past_window(world, target_stage);
    for side in venue_sides(world) {
        advance_one_venue_to_stage(world, &side, target_stage);
    }
}

/// Jump the committee to the end of the window a stage waits on instead of waiting for that end.
/// The e2e windows lie inside one day, so the jump crosses nothing the wait would not.
fn jump_committee_past_window(world: &mut World, target_stage: u8) {
    let side = venue_side(world);
    let schedule = eth::read_call(
        &side.url,
        side.auction,
        &IVenueSchedule::getAuctionInfoCall {
            worldwideDay: settled_day(world),
        },
    )
    .expect("the venue reports the day's schedule")
    .schedule;
    let window_end = match target_stage {
        1 => schedule.commitEnd,
        2 => schedule.revealEnd,
        _ => return,
    };
    let target = u64::from(window_end) + 1;
    let port = world.validators.primary_port();
    let now = world
        .rpc
        .latest_block_timestamp(port)
        .expect("committee head timestamp");
    if now >= target {
        return;
    }
    let (height, pending) =
        crate::features::common::restart_committee_at_logical_time(world, target);
    for peer in world.validators.committee_ports() {
        assert!(
            world.rpc.wait_finalized_at_least(peer, height, 240),
            "validator on port {peer} did not finalize past the window jump"
        );
    }
    if let Some(pending) = pending {
        let deadline = Instant::now() + Duration::from_secs(120);
        while !crate::features::price_oracle::observe_pending_publication(world, &pending) {
            assert!(
                Instant::now() < deadline,
                "post-jump feeder did not finalize"
            );
            sleep(Duration::from_millis(500));
        }
    }
    committee_clock_settles(world);
}

/// Each chain runs the day on its own clock, so every venue has to reach the
/// stage before the bidders may act on any of them.
fn advance_one_venue_to_stage(world: &World, venue: &VenueSide, target_stage: u8) {
    let worldwide_day = settled_day(world);
    let url = venue.url.clone();
    let home = world.rpc.url(world.validators.primary_port());
    let deadline = Instant::now() + AUCTION_STAGE_TIMEOUT;
    loop {
        if venue.is_target_chain {
            let now = eth::latest_block_timestamp(&home)
                .expect("read committee timestamp before syncing target venue");
            world
                .target_chain
                .sync_clock_to(now)
                .expect("sync target venue clock before checking its stage");
        }
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
        sleep(Duration::from_secs(2));
    }
}

/// Where the venue actually lives. With a second chain started, the auction runs
/// there and its messages ride the relay home. Without one, the committee is its
/// own target and everything stays on one chain.
pub(in crate::features) struct VenueSide {
    pub(in crate::features) url: String,
    pub(in crate::features) chain_id: u64,
    pub(in crate::features) auction: Address,
    pub(in crate::features) escrow: Address,
    pub(in crate::features) payment_token: Address,
    pub(in crate::features) intex_nft: Address,
    pub(in crate::features) target_router: Address,
    pub(in crate::features) is_target_chain: bool,
}

pub(super) fn venue_sides(world: &World) -> Vec<VenueSide> {
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

pub(super) fn venue_side(world: &World) -> VenueSide {
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

pub(super) const AUCTION_START_TIMEOUT: Duration = Duration::from_secs(1200);

#[then("the auction for that day opens on every chain it named")]
fn auction_opens_on_target(world: &mut World) {
    // The start message goes to every chain the day names, so every chain
    // has to answer for it.
    for venue in venue_sides(world) {
        auction_opened_on(world, &venue);
    }
}

fn auction_opened_on(world: &World, venue: &VenueSide) {
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
/// a minute-long auction window would end in seconds. Wait for blocks that
/// carry ordinary time: a chain that has not resumed yet looks identical to a
/// settled one if only the clock is sampled.
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

/// A loopback delivery runs inside the sending transaction on the gas its message type budgets
/// for it. So a park means a wrong budget or a reverting handler. The run must report it, not
/// retry it.
pub(super) fn assert_no_parked_deliveries(world: &World) {
    let url = world.rpc.url(world.validators.primary_port());
    let Some(loopback) = world
        .state
        .origin_contracts
        .clone()
        .map(|contracts| contracts.loopback)
    else {
        return;
    };
    let parked = eth::read_call(&url, loopback, &IParkedWork::nextParkedIdxCall {})
        .expect("the loopback adapter's parked count");
    assert_eq!(
        parked,
        U256::ZERO,
        "a loopback delivery parked during this run: {}",
        venue_probes::parked_deliveries(world)
    );
}

pub(super) fn flush_parked_bid_relays(world: &mut World, worldwide_day: u32) {
    for venue in venue_sides(world) {
        push_one_venue_bid_relay(&venue, worldwide_day);
    }
}

/// A relay that stopped part way continues from the chunk it left. The routers share an address
/// across chains. So the push request has to go to the chain that is relaying.
fn push_one_venue_bid_relay(venue: &VenueSide, worldwide_day: u32) {
    let progress = eth::read_call(
        &venue.url,
        venue.target_router,
        &IParkedWork::bidsRelayCall {
            worldwideDay: worldwide_day,
        },
    );
    if progress.is_none_or(|p| p.done) {
        return;
    }
    // A fixed limit: the relay stops quietly when gas runs short, so an estimate buys a no-op.
    eth::send_call_outcome(
        &venue.url,
        venue.target_router,
        crate::world::forge::DEPLOYER_KEY,
        &IParkedWork::relayBidsCall {
            worldwideDay: worldwide_day,
        },
        None,
    )
    .ok();
}

pub(super) fn settled_day(world: &World) -> u32 {
    world
        .state
        .wwd
        .as_deref()
        .expect("the settled day's WorldwideDay")
        .parse::<u32>()
        .expect("numeric WorldwideDay")
}
