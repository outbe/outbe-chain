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
