//! Steps for `features/follower_upstream.feature` — port of
//! The follower-upstream feature:
//!   S1  a cold `--upstream` follower syncs a reshared chain to lockstep
//!   S1b a follower-of-follower (`--upstream=follower1`) syncs off the first
//!   S3  a validator killed mid-epoch is restarted and re-locksteps
//!   S2  warm promotion: follower1's synced datadir restarts as a --validator
//!
//! Followers occupy high node slots (14/15), well clear of the committee (0..N)
//! and the joiner (N); all share validator-0's enclave (slot 0). Each slot owns
//! its own port block, allocated on first use.

use std::thread::sleep;
use std::time::Duration;

use cucumber::{given, then, when};

use crate::features::common::boot_localnet;
use crate::world::rpc::Rpc;
use crate::world::World;

const FOLLOWER1_SLOT: usize = 14;
const FOLLOWER2_SLOT: usize = 15;

/// Hold readiness until the current DKG target has already been frozen, while
/// leaving enough blocks for the readiness transaction to finalize before the
/// activation boundary. This makes warm promotion exercise the share-less
/// verifier's boundary transition instead of racing into the current ceremony.
fn wait_for_post_freeze_readiness_window(rpc: &Rpc, port: u16) -> bool {
    for _ in 0..120 {
        let activation = rpc
            .consensus_status_field(port, "nextPlannedActivationHeight")
            .and_then(|value| value.parse::<u64>().ok());
        if let (Some(head), Some(activation)) = (rpc.head(port), activation) {
            let readiness_floor = activation.saturating_sub(12);
            if head >= readiness_floor && head.saturating_add(2) < activation {
                return true;
            }
        }
        sleep(Duration::from_secs(2));
    }
    false
}

/// Follower within 4 blocks of the committee (script's `lockstep`).
fn lockstep(rpc: &Rpc, committee: u16, node: u16) -> bool {
    matches!(
        (rpc.head(committee), rpc.head(node)),
        (Some(c), Some(h)) if c.saturating_sub(h) <= 4
    )
}

fn wait_lockstep(rpc: &Rpc, committee: u16, node: u16, tries: u32) -> bool {
    for _ in 0..tries {
        sleep(Duration::from_secs(6));
        if lockstep(rpc, committee, node) {
            return true;
        }
    }
    false
}

fn wait_finalized_checkpoint_match(rpc: &Rpc, committee: u16, node: u16, tries: u32) -> bool {
    let Some(target) = rpc.finalized(committee) else {
        return false;
    };
    let Some(expected_hash) = rpc.block_hash(committee, target) else {
        return false;
    };
    let Some(expected_root) = rpc.state_root(committee, target) else {
        return false;
    };
    for _ in 0..tries {
        if rpc.finalized(node).is_some_and(|height| height >= target)
            && rpc.block_hash(node, target).as_ref() == Some(&expected_hash)
            && rpc.state_root(node, target).as_ref() == Some(&expected_root)
        {
            return true;
        }
        sleep(Duration::from_secs(2));
    }
    false
}

/// Setup with a SHORT epoch (60) + prepare window (15) so reshares come quickly
/// (follower_upstream.sh:11).
#[given("a fresh localnet with a short epoch")]
fn tuned_setup(world: &mut World) {
    boot_localnet(
        world,
        6,
        &[
            ("TESTNET_EPOCH_LENGTH_BLOCKS", "60".to_string()),
            ("TESTNET_DKG_PREPARE_WINDOW_BLOCKS", "15".to_string()),
        ],
    );
}

/// Drive the committee past a reshare (`vrfMaterialVersion` becomes non-zero).
#[when("the committee drives past a reshare")]
fn drive_past_reshare(world: &mut World) {
    let primary = world.validators.primary_port();
    let mut reshared = false;
    for _ in 0..70 {
        sleep(Duration::from_secs(5));
        if let Some(v) = world
            .rpc
            .consensus_status_field(primary, "vrfMaterialVersion")
        {
            if v != "0" && v != "null" && !v.is_empty() {
                reshared = true;
                break;
            }
        }
    }
    assert!(reshared, "no reshare observed within the window");
}

/// S1 — launch a cold follower with `--upstream` = committee.
#[when("a cold follower syncs from the committee")]
fn cold_follower(world: &mut World) {
    world
        .localnet
        .launch_follower("follower", FOLLOWER1_SLOT, 0, 0)
        .expect("launch follower1");
}

/// S1 — follower1 reaches lockstep with the committee.
#[then("the follower reaches lockstep with the committee")]
fn follower_lockstep(world: &mut World) {
    let primary = world.validators.primary_port();
    let f1 = world.validators.http_port(FOLLOWER1_SLOT);
    assert!(
        wait_lockstep(&world.rpc, primary, f1, 30),
        "follower1 did not reach lockstep"
    );
}

#[then("the follower reaches the committee finalized checkpoint with matching hash and state root")]
fn follower_exact_finality(world: &mut World) {
    let primary = world.validators.primary_port();
    let follower = world.validators.http_port(FOLLOWER1_SLOT);
    assert!(
        wait_finalized_checkpoint_match(&world.rpc, primary, follower, 45),
        "follower never reached the committee finalized checkpoint with matching hash/state root"
    );
}

#[when("the follower loses its only upstream while the committee advances")]
fn lose_upstream(world: &mut World) {
    let follower = world.validators.http_port(FOLLOWER1_SLOT);
    let surviving_validator = world.validators.http_port(1);
    let follower_before = world
        .rpc
        .finalized(follower)
        .expect("follower finalized height");
    let committee_before = world
        .rpc
        .finalized(surviving_validator)
        .expect("committee finalized height");
    world.state.marker_height = Some(follower_before);
    world
        .localnet
        .kill_validator(0)
        .expect("kill follower upstream");
    assert!(
        world
            .rpc
            .wait_block(surviving_validator, committee_before.saturating_add(5), 60)
            .is_some(),
        "remaining 3-of-4 committee did not advance after upstream loss"
    );
}

#[then("the disconnected follower makes no unverified finalized progress")]
fn follower_stalls_safely(world: &mut World) {
    let follower = world.validators.http_port(FOLLOWER1_SLOT);
    let before = world.state.marker_height.expect("captured follower height");
    let after = world
        .rpc
        .finalized(follower)
        .expect("follower RPC remains readable");
    assert!(
        after <= before.saturating_add(1),
        "disconnected follower advanced from {before} to {after} without its upstream"
    );
}

#[when("the follower switches to a healthy upstream and restarts from its durable datadir")]
fn switch_upstream_and_restart_follower(world: &mut World) {
    world
        .localnet
        .restart()
        .expect("restore validator-0 upstream");
    let primary = world.validators.primary_port();
    let target = world
        .rpc
        .finalized(world.validators.http_port(1))
        .expect("surviving committee finalized height");
    assert!(
        world.rpc.wait_block(primary, target, 60).is_some(),
        "restored upstream did not catch up"
    );
    world
        .localnet
        .stop_follower("follower")
        .expect("stop follower during catch-up");
    world
        .localnet
        .launch_follower("follower", FOLLOWER1_SLOT, 1, 0)
        .expect("restart follower from durable datadir against healthy upstream");
}

/// S1b — follower1 publishes its tip; launch follower2 chained off it.
#[when("a second follower chains off the first")]
fn chained_follower(world: &mut World) {
    let f1 = world.validators.http_port(FOLLOWER1_SLOT);
    let tip = world
        .rpc
        .consensus_status_field(f1, "lastFinalizedBlock")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    assert!(
        tip > 0,
        "follower1 did not publish lastFinalizedBlock ({tip})"
    );
    world
        .localnet
        .launch_follower("follower2", FOLLOWER2_SLOT, FOLLOWER1_SLOT, 0)
        .expect("launch follower2");
}

/// S1b — the chained follower reaches lockstep too.
#[then("the chained follower reaches lockstep with the committee")]
fn chained_lockstep(world: &mut World) {
    let primary = world.validators.primary_port();
    let f2 = world.validators.http_port(FOLLOWER2_SLOT);
    assert!(
        wait_lockstep(&world.rpc, primary, f2, 30),
        "follower2 (chained off follower1) did not reach lockstep"
    );
}

/// S3 — kill validator-3 mid-epoch and restart it.
#[when("a validator is killed and restarted mid-epoch")]
fn validator_catchup(world: &mut World) {
    world.localnet.kill_validator(3).expect("kill validator-3");
    sleep(Duration::from_secs(25));
    world.localnet.restart().expect("restart committee");
}

/// S3 — the restarted validator catches up to lockstep.
#[then("the restarted validator catches up to lockstep")]
fn validator_relockstep(world: &mut World) {
    let primary = world.validators.primary_port();
    let v3 = world.validators.http_port(3);
    assert!(
        wait_lockstep(&world.rpc, primary, v3, 30),
        "validator-3 did not catch up after restart"
    );
}

/// S2 — warm promotion: stop followers, reuse follower1's synced datadir as the
/// joiner's, stake, and launch it as a validator.
#[when("the first follower is promoted to a validator with its warm datadir")]
fn warm_promotion(world: &mut World) {
    let primary = world.validators.primary_port();
    let idx = world.validators.joiner_index();
    let joiner_port = world.validators.http_port(idx);

    world.localnet.stop_followers().expect("stop followers");
    sleep(Duration::from_secs(3));
    world
        .localnet
        .provision_joiner(idx)
        .expect("provision joiner");
    world
        .localnet
        .move_datadir("follower/data", &format!("validator-{idx}/data"))
        .expect("move warm datadir");

    let key = world.validators.joiner().evm_key().expect("joiner key");
    let addr = world.rpc.address_of(&key).expect("joiner addr");
    world.state.joiner_addr = Some(addr);
    world.rpc.stake(&key, 1000).expect("stake");
    world
        .localnet
        .launch_joiner(idx, &[])
        .expect("launch warm joiner");
    assert!(
        wait_lockstep(&world.rpc, primary, joiner_port, 30),
        "warm-restarted joiner did not sync"
    );
    assert!(
        wait_for_post_freeze_readiness_window(&world.rpc, primary),
        "no post-freeze readiness window observed for late warm promotion"
    );
    world.rpc.confirm_ready(&key).expect("confirm ready");
}

/// S2 — the promoted validator activates and stays in lockstep.
#[then("the promoted validator activates and stays in lockstep")]
fn promoted_activates(world: &mut World) {
    let primary = world.validators.primary_port();
    let idx = world.validators.joiner_index();
    let joiner_port = world.validators.http_port(idx);
    let addr = world.state.joiner_addr.clone().expect("joiner addr");

    assert!(
        world.rpc.wait_participant(primary, &addr, 60),
        "warm-promoted node never became a consensus participant"
    );
    sleep(Duration::from_secs(20));
    assert!(
        lockstep(&world.rpc, primary, joiner_port),
        "warm-promoted validator stalled after activation"
    );
}
