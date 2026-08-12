//! Steps for the intex engine deployed onto the committee's own chain.

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
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IParkedProceeds {
        struct ParkedProceeds {
            uint32 worldwideDay;
            uint32 srcChainId;
            uint128 amount;
            bool settled;
        }
        function parkedProceeds(uint256 idx) external view returns (ParkedProceeds memory);
    }
}

/// A day the scenario owns outright, so nothing else can have opened it.
const PROCEEDS_DAY: u32 = 20_260_101;
/// Small enough that the deployer's genesis balance covers it comfortably.
const PROCEEDS_AMOUNT_WEI: u128 = 1_000_000_000_000_000_000;

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

#[when("auction proceeds arrive for a day")]
fn proceeds_arrive(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let contracts = world
        .state
        .origin_contracts
        .clone()
        .expect("a deploy recorded its addresses");
    origin_venue::deliver_proceeds(
        &environment().repo,
        &url,
        &contracts,
        PROCEEDS_DAY,
        PROCEEDS_AMOUNT_WEI,
    )
    .expect("deliver auction proceeds to the origin router");
}

#[then("the router handed those proceeds to the factory")]
fn router_handed_proceeds_on(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let contracts = world
        .state
        .origin_contracts
        .clone()
        .expect("a deploy recorded its addresses");

    // A delivery the factory refused is parked rather than lost, so an empty
    // park is what tells the two apart: the seam carried the value through.
    let parked = eth::read_call(
        &url,
        contracts.origin_router,
        &IParkedProceeds::parkedProceedsCall {
            idx: alloy_primitives::U256::ZERO,
        },
    );
    assert!(
        parked.is_none_or(|entry| entry.amount == 0),
        "proceeds were parked instead of reaching the factory"
    );
}
