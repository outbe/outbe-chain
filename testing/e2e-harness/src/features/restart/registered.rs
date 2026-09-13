use super::restart_activation;
use super::restart_capture_incarnations;
use super::restart_check_logs;
use super::restart_finalize_hash;
use super::restart_fresh_checkpoint;
use super::restart_joiner_pair;
use super::restart_pin_before;
use super::restart_prove_signing;
use super::restart_require_membership;
use super::restart_snapshot;

use crate::world::World;

use cucumber::then;
use cucumber::when;

/// Restart at the earliest durable join checkpoint: registration, P2P identity
/// and enclave join are committed, but no stake/readiness or DKG side effect is.
#[when("a registered joining node and enclave restart before staking")]
fn restart_registered_joiner_before_staking(world: &mut World) {
    let idx = world.validators.joiner_index();
    world
        .localnet
        .provision_joiner(idx)
        .expect("provision joiner");
    let keys = world.localnet.keys_dir(idx);
    world
        .localnet
        .launch_caught_up_joiner(idx, &["--consensus.keys-dir", &keys])
        .expect("launch joiner");
    let key = world.validators.joiner().evm_key().expect("joiner key");
    world.state.joiner_addr = Some(world.rpc.address_of(&key).expect("joiner identity"));
    restart_capture_incarnations(world, idx).expect("capture registered identity and owners");
    restart_pin_before(world, 0, idx).expect("canonical zero-stake REGISTERED checkpoint");
    restart_joiner_pair(world, idx, false).expect("restart exact registered node and enclave");
}

/// The restart must preserve exactly the registered pre-state; only subsequent
/// stake/readiness may create one pending target and one activation.
#[then("registration survives and the join can activate once")]
fn registered_restart_then_join_activates(world: &mut World) {
    let idx = world.validators.joiner_index();
    let addr = world.state.joiner_addr.clone().expect("joiner identity");
    let checkpoint = restart_fresh_checkpoint(world, 40)
        .expect("registered restart makes all-five fresh finality");
    let state = restart_snapshot(world, checkpoint, &addr, "registered_restart_before_stake")
        .expect("pinned registered restart state");
    restart_require_membership(world, &state, 0)
        .expect("restart preserves registration and zero stake");
    restart_check_logs(world, idx, true, false)
        .expect("registered enclave recovered its sealed identity");
    let key = world.validators.joiner().evm_key().expect("joiner key");
    let stake = world.rpc.stake(&key, 1000).expect("stake after restart");
    let staked = restart_finalize_hash(world, &stake).expect("finalized stake on all five nodes");
    let state = restart_snapshot(world, staked, &addr, "registered_restart_staked")
        .expect("pinned pending stake");
    restart_require_membership(world, &state, 1).expect("stake alone creates pending membership");
    // Anchor the admission to the actual stake, not an epoch preceding a
    // legitimate rotation while the registered process was restarting.
    world.state.lifecycle_before = Some(staked);
    let ready = world
        .rpc
        .confirm_ready(&key)
        .expect("confirm after restart");
    restart_finalize_hash(world, &ready).expect("finalized readiness receipt");
    restart_activation(world, false).expect("one canonical post-stake admission");
    assert!(
        world.localnet.has_share_file(idx),
        "activated registered restart has no persisted share"
    );
    assert_eq!(
        world
            .rpc
            .has_threshold_shares(world.validators.http_port(idx)),
        Some(true),
        "activated registered restart has no private signing share"
    );
    restart_prove_signing(world, &addr, 60, 60)
        .expect("registered restart closes an eligible signing window");
    restart_check_logs(world, idx, true, false).expect("registered replacement remains healthy");
}
