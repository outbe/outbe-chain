//! Steps for the intex engine deployed onto the committee's own chain.

use std::thread::sleep;
use std::time::{Duration, Instant};

use alloy_primitives::Address;
#[cfg(feature = "ocomp-integration")]
use alloy_primitives::U256;
use alloy_sol_types::sol;
use cucumber::{then, when};

use crate::env::environment;
use crate::internal::eth;
#[cfg(feature = "ocomp-integration")]
use crate::world::bidders;
use crate::world::{origin_venue, World};

#[when("the intex engine is deployed on the committee chain")]
fn deploy_origin_venue(world: &mut World) {
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let chain_id = world.rpc.chain_id(port).expect("committee chain id");
    let contracts = origin_venue::deploy(&environment().repo, &url, chain_id)
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

sol! {
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IProceedsRoute {
        function tokenBridge() external view returns (address);
        function wcoen() external view returns (address);
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

sol! {
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IAuctionStage {
        function getAuctionStage(uint32 worldwideDay) external view returns (uint8);
    }

    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IOriginTargets {
        function targetsOf(uint32 worldwideDay) external view returns (uint32[] memory);
    }

    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IVenueSchedule {
        struct IntexCallTrigger { uint32 callWindow; uint32 callThreshold; uint32 callNoticePeriod; }
        struct AuctionSchedule { uint32 commitEnd; uint32 revealEnd; uint32 issuanceEnd; }
        struct AuctionParams {
            uint16 issuanceCurrency; uint16 referenceCurrency; uint128 promisLoadMinor;
            IntexCallTrigger callTrigger; uint32 minIntexBidRate; uint16 minIntexBidQuantity;
            uint64 entryPriceMinor; uint64 floorPriceMinor; uint64 callPriceMinor; uint128 commitBondMinor;
        }
        struct AuctionResult {
            uint64 auctionClearingRate; uint32 wonBidsCount; uint32 issuedIntexCount; uint128 issuedIntexLoadedPromis;
        }
        function auctions(uint32 worldwideDay)
            external view
            returns (uint8 worldwideDayState, AuctionSchedule memory schedule, AuctionParams memory params, AuctionResult memory result);
    }

    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IParkedWork {
        struct ParkedSend { uint32 dstChainId; uint64 gasLimit; bool sent; bytes payload; }
        function nextPendingBidsRelayIdx() external view returns (uint256);
        function parkedSend(uint256 idx) external view returns (ParkedSend memory);
        function flushPendingSend(uint256 idx) external;
    }

    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IVenueCounts {
        function auctionRunningCounts(uint32 worldwideDay)
            external view returns (uint32 committedBidsCount, uint32 revealedBidsCount);
    }

    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IDesisBids {
        function getBidsCount(uint32 worldwideDay) external view returns (uint256);
        function isChainDone(uint32 worldwideDay, uint32 srcChainId) external view returns (bool);
    }
}

fn frozen_targets(url: &str, router: Address, worldwide_day: u32) -> String {
    match eth::read_call(
        url,
        router,
        &IOriginTargets::targetsOfCall {
            worldwideDay: worldwide_day,
        },
    ) {
        Some(targets) if targets.is_empty() => "the router froze no targets for the day".to_owned(),
        Some(targets) => format!("the router froze targets {targets:?}"),
        None => "the router did not answer".to_owned(),
    }
}

fn desis_stage(url: &str, worldwide_day: u32) -> String {
    let desis = origin_venue::DESIS
        .parse()
        .expect("desis precompile address");
    match eth::read_call(
        url,
        desis,
        &IAuctionStage::getAuctionStageCall {
            worldwideDay: worldwide_day,
        },
    ) {
        Some(0) => "no brief ever reached Desis".to_owned(),
        Some(1) => "Desis holds the brief but never started the auction".to_owned(),
        Some(2) => "Desis started it, so the message never reached the venue".to_owned(),
        Some(6) => "Desis cancelled the day".to_owned(),
        Some(other) => format!("Desis stage {other}"),
        None => "Desis did not answer".to_owned(),
    }
}

#[cfg(feature = "ocomp-integration")]
fn day_colour(world: &World, worldwide_day: u32) -> String {
    match world
        .rpc
        .metadosis_wwd_state_on(world.validators.primary_port(), worldwide_day)
    {
        Some(state) => format!("day type {} in status {}", state.day_type, state.status),
        None => "day state unreadable".to_owned(),
    }
}

#[cfg(not(feature = "ocomp-integration"))]
fn day_colour(_world: &World, _worldwide_day: u32) -> String {
    "day type unreadable without ocomp-integration".to_owned()
}

#[cfg(feature = "ocomp-integration")]
fn day_closure(world: &World, worldwide_day: u32) -> String {
    match world
        .rpc
        .metadosis_terminal_receipt_on(world.validators.primary_port(), worldwide_day)
    {
        // An absent receipt reads back zeroed rather than reverting, so the
        // block number is what tells a real closure from no closure at all.
        Some(receipt) if receipt.block_number > 0 => format!(
            "Metadosis closed the day at block {} with outcome {}",
            receipt.block_number, receipt.outcome
        ),
        _ => "Metadosis never closed the day, so nothing ever briefed Desis".to_owned(),
    }
}

#[cfg(not(feature = "ocomp-integration"))]
fn day_closure(_world: &World, _worldwide_day: u32) -> String {
    "day closure unreadable without ocomp-integration".to_owned()
}

/// An e2e build runs the auction on minute-long windows, so the run waits the
/// stages out rather than moving the clock across a day it never formed.
#[cfg(feature = "ocomp-integration")]
const AUCTION_STAGE_TIMEOUT: Duration = Duration::from_secs(2400);

#[cfg(feature = "ocomp-integration")]
fn advance_past_window_to_stage(world: &mut World, target_stage: u8) {
    let contracts = world
        .state
        .origin_contracts
        .clone()
        .expect("a deploy recorded its addresses");
    let worldwide_day = settled_day(world);
    let url = world.rpc.url(world.validators.primary_port());
    let deadline = Instant::now() + AUCTION_STAGE_TIMEOUT;
    loop {
        let stage = eth::read_call(
            &url,
            contracts.intex_auction,
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
            "day {worldwide_day} stalled at venue stage {stage} short of {target_stage}"
        );
        sleep(Duration::from_secs(2));
    }
}

const AUCTION_START_TIMEOUT: Duration = Duration::from_secs(1200);

#[then("the auction for that day opens on the target chain")]
fn auction_opens_on_target(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let contracts = world
        .state
        .origin_contracts
        .clone()
        .expect("a deploy recorded its addresses");
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
            contracts.intex_auction,
            &IAuctionStage::getAuctionStageCall {
                worldwideDay: worldwide_day,
            },
        ) {
            assert!(
                stage < 2,
                "day {worldwide_day} reached the venue at stage {stage}: {}",
                venue_schedule(&url, contracts.intex_auction, worldwide_day)
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "day {worldwide_day} settled but no auction ever opened on the venue at {}: {}; {}; {}; {}",
            contracts.intex_auction,
            desis_stage(&url, worldwide_day),
            frozen_targets(&url, contracts.origin_router, worldwide_day),
            day_closure(world, worldwide_day),
            day_colour(world, worldwide_day)
        );
        sleep(Duration::from_secs(2));
    }
}

fn venue_schedule(url: &str, venue: Address, worldwide_day: u32) -> String {
    let now = eth::latest_block_timestamp(url).unwrap_or_default();
    match eth::read_call(
        url,
        venue,
        &IVenueSchedule::auctionsCall {
            worldwideDay: worldwide_day,
        },
    ) {
        Some(a) => format!(
            "commitEnd {} revealEnd {} issuanceEnd {} against block time {now}",
            a.schedule.commitEnd, a.schedule.revealEnd, a.schedule.issuanceEnd
        ),
        None => "venue did not report the schedule".to_owned(),
    }
}

/// Both sides swallow a failed send by parking it, so the parked entry is the
/// only trace of work that never left.
#[cfg(feature = "ocomp-integration")]
fn parked_work(url: &str, router: Address, venue_router: Address) -> String {
    let relays = eth::read_call(
        url,
        venue_router,
        &IParkedWork::nextPendingBidsRelayIdxCall {},
    );
    let parked = eth::read_call(
        url,
        router,
        &IParkedWork::parkedSendCall { idx: U256::ZERO },
    );
    let parked_note = match parked {
        Some(send) if !send.payload.is_empty() => {
            // Retrying a parked send is permissionless, and its revert carries
            // the reason the router swallowed when it parked.
            let flush = eth::send_call(
                url,
                router,
                crate::world::forge::DEPLOYER_KEY,
                &IParkedWork::flushPendingSendCall { idx: U256::ZERO },
                None,
            );
            format!(
                "origin parked a send to chain {} ({} bytes, gas {}), retry says {:?}",
                send.dstChainId,
                send.payload.len(),
                send.gasLimit,
                flush.err().map(|error| error.to_string())
            )
        }
        Some(_) => "origin parked nothing".to_owned(),
        None => "origin did not report parked sends".to_owned(),
    };
    format!("{parked_note}, venue parked bid relays {relays:?}")
}

#[cfg(feature = "ocomp-integration")]
fn venue_bid_counts(url: &str, venue: Address, worldwide_day: u32) -> String {
    match eth::read_call(
        url,
        venue,
        &IVenueCounts::auctionRunningCountsCall {
            worldwideDay: worldwide_day,
        },
    ) {
        Some(counts) => format!(
            "venue holds {} committed and {} revealed bids",
            counts.committedBidsCount, counts.revealedBidsCount
        ),
        None => "venue did not report its bid counts".to_owned(),
    }
}

#[cfg(feature = "ocomp-integration")]
fn relayed_bids(url: &str, worldwide_day: u32, chain_id: u32) -> String {
    let desis = origin_venue::DESIS
        .parse()
        .expect("desis precompile address");
    let count = eth::read_call(
        url,
        desis,
        &IDesisBids::getBidsCountCall {
            worldwideDay: worldwide_day,
        },
    );
    let done = eth::read_call(
        url,
        desis,
        &IDesisBids::isChainDoneCall {
            worldwideDay: worldwide_day,
            srcChainId: chain_id,
        },
    );
    match (count, done) {
        (Some(count), Some(done)) => {
            format!("Desis holds {count} relayed bids, chain {chain_id} done={done}")
        }
        _ => "Desis did not report its relayed bids".to_owned(),
    }
}

/// After a logical-time jump the ratchet lets each block carry up to an hour, so
/// a minute-long auction window would burn out in seconds. Let that catch-up end.
#[cfg(feature = "ocomp-integration")]
#[when("the committee clock settles after the jump")]
fn committee_clock_settles(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let deadline = Instant::now() + AUCTION_STAGE_TIMEOUT;
    let mut previous = eth::latest_block_timestamp(&url).expect("committee block timestamp");
    loop {
        sleep(Duration::from_secs(6));
        let current = eth::latest_block_timestamp(&url).expect("committee block timestamp");
        let step = current.saturating_sub(previous);
        if step < 120 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the committee clock is still catching up at {step}s per six seconds"
        );
        previous = current;
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
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let chain_id = world.rpc.chain_id(port).expect("committee chain id");
    let contracts = world
        .state
        .origin_contracts
        .clone()
        .expect("a deploy recorded its addresses");
    let worldwide_day = settled_day(world);

    let bidders = bidders::derive(&BIDS).expect("derive the bidders");
    bidders::fund(
        &url,
        contracts.payment_token,
        contracts.escrow,
        &bidders,
        U256::from(BIDDER_ALLOWANCE),
    )
    .expect("fund the bidders");
    bidders::commit(
        &url,
        contracts.intex_auction,
        chain_id,
        worldwide_day,
        &bidders,
    )
    .expect("commit the bids");
    world.state.auction_bidders = bidders;
}

#[cfg(feature = "ocomp-integration")]
#[when("those bidders reveal their bids once the venue is revealing")]
fn bidders_reveal(world: &mut World) {
    advance_past_window_to_stage(world, 1);

    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let chain_id = world.rpc.chain_id(port).expect("committee chain id");
    let contracts = world
        .state
        .origin_contracts
        .clone()
        .expect("a deploy recorded its addresses");
    let worldwide_day = settled_day(world);
    let bidders = world.state.auction_bidders.clone();

    bidders::reveal(
        &url,
        contracts.intex_auction,
        chain_id,
        worldwide_day,
        &bidders,
    )
    .expect("reveal the bids");
}

#[cfg(feature = "ocomp-integration")]
#[then("the auction clears and the venue moves past its reveal window")]
fn auction_clears(world: &mut World) {
    advance_past_window_to_stage(world, 2);

    let chain_id = world
        .rpc
        .chain_id(world.validators.primary_port())
        .expect("committee chain id");
    let deployed = world
        .state
        .origin_contracts
        .clone()
        .expect("a deploy recorded its addresses");
    let venue = deployed.intex_auction;
    let router = deployed.origin_router;
    let venue_router = deployed.target_router;
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
        assert_ne!(stage, Some(6), "Desis cancelled day {worldwide_day}");
        assert!(
            Instant::now() < deadline,
            "day {worldwide_day} never cleared on Desis: {}; {}; venue stage {:?}, {}; {}",
            desis_stage(&url, worldwide_day),
            relayed_bids(&url, worldwide_day, chain_id as u32),
            eth::read_call(
                &url,
                venue,
                &IAuctionStage::getAuctionStageCall {
                    worldwideDay: worldwide_day
                },
            ),
            venue_bid_counts(&url, venue, worldwide_day),
            parked_work(&url, router, venue_router)
        );
        sleep(Duration::from_secs(2));
    }
}
