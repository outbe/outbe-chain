//! Steps for the local target chain cross-chain scenarios bridge to.

use cucumber::{then, when};

use crate::internal::eth;
use crate::world::World;

#[when("a local target chain is started")]
fn start_target_chain(world: &mut World) {
    world
        .target_chain
        .start()
        .expect("start the local target chain");
}

#[then("the target chain answers with its own chain id")]
fn target_chain_answers(world: &mut World) {
    let url = world
        .target_chain
        .rpc_url()
        .expect("target chain exposes an RPC endpoint once started");
    let observed = eth::chain_id(&url).unwrap_or_else(|| panic!("read chain id at {url}"));
    assert_eq!(
        observed,
        world.target_chain.chain_id(),
        "target chain reports a different chain id than it was started with"
    );
}

#[when("the intex venue is deployed on the target chain")]
fn deploy_intex_venue(world: &mut World) {
    let origin_chain_id = world
        .rpc
        .chain_id(world.validators.primary_port())
        .expect("committee chain id");
    let contracts = world
        .target_chain
        .deploy(origin_chain_id)
        .expect("deploy the intex venue on the target chain");
    world.state.target_contracts = Some(contracts);
}

#[then("the target chain hosts the intex venue")]
fn target_chain_hosts_venue(world: &mut World) {
    let url = world.target_chain.rpc_url().expect("target chain started");
    let contracts = world
        .state
        .target_contracts
        .clone()
        .expect("a deploy recorded its addresses");
    for (name, address) in [
        ("ERC7786Bridge", contracts.bridge),
        ("IntexNFT1155", contracts.intex_nft),
        ("TargetRouter", contracts.target_router),
    ] {
        assert!(
            eth::code(&url, address).is_some_and(|code| !code.is_empty()),
            "{name} reported at {address} but the target chain holds no code there"
        );
    }
}

#[when("the intex venue is wired")]
fn wire_intex_venue(world: &mut World) {
    let contracts = world
        .state
        .target_contracts
        .clone()
        .expect("a deploy recorded its addresses");
    world
        .target_chain
        .wire(&contracts)
        .expect("wire the intex venue");
}

#[then("the target router may issue on the venue")]
fn router_may_mint(world: &mut World) {
    let contracts = world
        .state
        .target_contracts
        .clone()
        .expect("a deploy recorded its addresses");
    let relayer = world
        .target_chain
        .relayer_role(contracts.intex_nft)
        .expect("read the relayer role id");
    assert!(
        world
            .target_chain
            .holds_role(contracts.intex_nft, relayer, contracts.target_router)
            .expect("read hasRole on the collection"),
        "the router cannot mint: an inbound issuance would arrive and do nothing"
    );
}

#[then("the committee is still producing blocks")]
fn committee_still_producing(world: &mut World) {
    let port = world.validators.primary_port();
    let height = world
        .rpc
        .finalized(port)
        .expect("committee finalized height before the target chain check");
    assert!(
        world.rpc.wait_block_gt(port, height, 30).is_some(),
        "committee stopped finalizing while the target chain was running"
    );
}
