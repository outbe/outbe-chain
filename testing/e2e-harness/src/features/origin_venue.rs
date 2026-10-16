//! Steps for the intex engine deployed onto the committee's own chain.

use cucumber::{then, when};

use crate::env::environment;
use crate::internal::eth;
use crate::world::venue_probes::IProceedsRoute;
use crate::world::{origin_venue, World};

mod auction;

mod bidding;

pub(super) use auction::VenueSide;

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
    // One owner's balance does not cover the whole deploy, so several owners
    // contribute. They come from the tail of the list. The scenario's Tribute
    // submitters are the head of the same list, and they still need their own gas.
    let fallback_operator_key = world
        .validators
        .get(0)
        .evm_key()
        .expect("read validator-0 deploy funding key");
    let funding_plan =
        origin_deploy_funding_plan(&world.state.tribute_owner_keys, fallback_operator_key);
    for (funder, amount_coen) in funding_plan {
        crate::internal::eth::send_value(
            &url,
            origin_venue::deployer_address(),
            &funder,
            crate::internal::eth::coen(amount_coen),
        )
        .expect("fund the deploy account on the committee chain");
    }
    // The committee is always a target of its own day. A started second chain
    // joins it, so the day goes to both.
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
