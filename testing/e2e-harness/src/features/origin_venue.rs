//! Steps for the intex engine deployed onto the committee's own chain.

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
    ] {
        assert!(
            eth::code(&url, address).is_some_and(|code| !code.is_empty()),
            "{name} reported at {address} but the committee chain holds no code there"
        );
    }
}
