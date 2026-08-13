//! Steps for the intex engine deployed onto the committee's own chain.

use std::thread::sleep;
use std::time::{Duration, Instant};

use alloy_primitives::Address;
use alloy_sol_types::sol;
use cucumber::{then, when};

use crate::env::environment;
use crate::internal::eth;
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

    // Bytecode at each address is the only claim a deploy can make on its own;
    // whether the engine is wired is a separate step.
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

    // Without the route an inbound delivery reverts as an unauthorised caller,
    // and the day never opens a payout round.
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
}

/// The targets frozen for the day. An empty snapshot means the start had nowhere
/// to go, which is a different fault from one that was dispatched and lost.
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

/// What Desis itself thinks the day reached, so a venue that never heard of the
/// auction can be told apart from a day Desis never scheduled.
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
        Some(6) => "Desis cancelled the day as red".to_owned(),
        Some(other) => format!("Desis stage {other}"),
        None => "Desis did not answer".to_owned(),
    }
}

/// Whether Metadosis ever closed the day. Closing it is what applies the budget
/// split, and that is the only thing that briefs Desis for a day that ran a job,
/// so a missing receipt places the gap before Desis rather than inside it.
/// A red day briefs no supply, so Desis is left with nothing to start. Reading
/// the type separates that from a green day whose closure simply never ran.
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

/// Desis starts a briefed auction from its own schedule tick, which fires every
/// twelve hours of logical time.
#[cfg(feature = "ocomp-integration")]
const AUCTION_TICK_PERIOD_SECS: u64 = 43_200;

#[cfg(feature = "ocomp-integration")]
#[when("the committee logical clock reaches the next auction schedule tick")]
fn committee_clock_reaches_next_auction_tick(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let now = eth::latest_block_timestamp(&url).expect("committee block timestamp");
    let target = now
        .checked_div(AUCTION_TICK_PERIOD_SECS)
        .and_then(|periods| periods.checked_add(1))
        .and_then(|periods| periods.checked_mul(AUCTION_TICK_PERIOD_SECS))
        .expect("auction tick boundary after the committee clock");
    crate::features::ocomp::restart_committee_at_logical_time(world, target.saturating_add(60));
}

/// Desis dispatches the start on its own schedule tick, which lands some blocks
/// after the day settles.
const AUCTION_START_TIMEOUT: Duration = Duration::from_secs(300);

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
            assert_ne!(
                stage, 4,
                "day {worldwide_day} reached the venue already cancelled"
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
