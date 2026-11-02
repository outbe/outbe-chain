//! Generic proposal approval step used by `features/governance.feature`.

use cucumber::then;

use crate::world::World;

#[then(expr = "proposal {int} is approved")]
fn proposal_approved(world: &mut World, id: u64) {
    assert!(
        world
            .rpc
            .wait_vote_status(id, "approved", 60)
            .expect("observe proposal approval"),
        "proposal #{id} not approved after deadline"
    );
}
