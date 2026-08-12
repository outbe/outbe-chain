//! Steps for the intex engine deployed onto the committee's own chain.

use std::thread::sleep;
use std::time::{Duration, Instant};

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
            "day {worldwide_day} settled but no auction ever opened on the venue at {}",
            contracts.intex_auction
        );
        sleep(Duration::from_secs(2));
    }
}
