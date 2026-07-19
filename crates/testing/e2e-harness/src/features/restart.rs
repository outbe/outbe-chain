//! Steps for `features/s4_restart_active.feature`. An ACTIVE validator's DKG share lives on
//! disk (keys-dir), not the enclave. Killing and restarting ONLY the node (the
//! enclave container stays up) must resume signing from the persisted share
//! WITHOUT a fresh DKG ceremony.

use std::thread::sleep;
use std::time::Duration;

use cucumber::{then, when};

use crate::world::rpc::Rpc;
use crate::world::World;

/// Lockstep probe (s4:46-48): 5×10s, joiner within 3 of committee and advancing.
fn lockstep_ok(rpc: &Rpc, committee: u16, joiner: u16) -> bool {
    let mut prev = 0u64;
    for _ in 0..5 {
        sleep(Duration::from_secs(10));
        let ch = rpc.head(committee).unwrap_or(0);
        let vh = rpc.head(joiner).unwrap_or(0);
        if ch.saturating_sub(vh) > 3 || vh <= prev {
            return false;
        }
        prev = vh;
    }
    true
}

/// Bring a joiner to ACTIVE with a persisted (keys-dir) share (s4:13-30).
#[when("a joiner reaches active with a persisted share")]
fn joiner_active_persisted_share(world: &mut World) {
    let primary = world.validators.primary_port();
    let idx = world.validators.joiner_index();
    let joiner_port = world.validators.http_port(idx);
    let wwd = world.state.wwd.clone().expect("wwd");

    let v0 = world.validators.get(0).evm_key().expect("v0 key");
    assert!(
        world.rpc.offer_until_supply(&v0, &wwd, primary, "1", 5),
        "pre-restart offer did not land (supply != 1)"
    );

    world
        .localnet
        .provision_joiner(idx)
        .expect("provision joiner");
    let keys = world.localnet.keys_dir(idx);
    world
        .localnet
        .launch_joiner(idx, &["--consensus.keys-dir", &keys])
        .expect("launch joiner (keys-dir)");
    world.rpc.wait_block(joiner_port, 20, 40);

    let key = world.validators.joiner().evm_key().expect("joiner key");
    let addr = world.rpc.address_of(&key).expect("joiner addr");
    world.state.joiner_addr = Some(addr.clone());
    world.rpc.stake(&key, 1000).expect("stake");
    sleep(Duration::from_secs(6));
    world.rpc.confirm_ready(&key).expect("confirm ready");

    assert!(
        world.rpc.wait_participant(primary, &addr, 40),
        "joiner did not reach ACTIVE before the restart"
    );
    assert!(
        world.localnet.has_share_file(idx),
        "DKG share was not persisted to the keys dir"
    );
    sleep(Duration::from_secs(20)); // sign a few blocks as ACTIVE
}

/// Kill only the node (enclave container stays up) and restart it with the same
/// keys-dir/datadir (s4:32-37).
#[when("the node is killed and restarted with the same keys")]
fn node_killed_and_restarted(world: &mut World) {
    let primary = world.validators.primary_port();
    let idx = world.validators.joiner_index();
    world.state.marker_count = Some(world.localnet.log_count(idx, "running DKG ceremony"));
    world.state.marker_height = world.rpc.head(primary);
    world.localnet.stop_joiner(idx).expect("stop joiner");
    let keys = world.localnet.keys_dir(idx);
    world
        .localnet
        .launch_joiner(idx, &["--consensus.keys-dir", &keys])
        .expect("relaunch joiner");
}

/// Stop every committee node and enclave, then relaunch them from the same
/// datadirs. Unlike the single-node restart above, no live enclave remains to
/// answer a key-handoff request: recovery therefore depends on sealed state.
#[when("the entire committee and its enclaves are stopped and restarted")]
fn committee_and_enclaves_restarted(world: &mut World) {
    let primary = world.validators.primary_port();
    world.state.marker_height = world.rpc.head(primary);
    world.state.marker_count = Some(world.localnet.log_count(0, "running DKG ceremony"));
    world
        .localnet
        .restart_committee_and_enclaves()
        .expect("restart committee and enclaves");
}

/// Every enclave must use its restart fast-path, every validator must advance,
/// and an enclave-backed Tribute offer must remain executable.
#[then("all validators recover sealed TEE state and resume finalization")]
fn committee_recovers_sealed_tee_state(world: &mut World) {
    let before = world.state.marker_height.expect("pre-restart height");
    let target = before + 2;
    let mut ports = vec![world.validators.primary_port()];
    ports.extend(world.validators.peer_ports());
    for port in ports {
        let height = world.rpc.wait_block(port, target, 60).unwrap_or(0);
        assert!(
            height >= target,
            "validator RPC {port} did not advance after full restart ({height} < {target})"
        );
    }

    for index in 0..world.validators.size() {
        assert!(
            world.localnet.enclave_log_has(
                index,
                "unsealed offer key + group signature <- /tee/sealed_root.bin (restart fast-path)"
            ),
            "validator-{index} enclave did not recover its sealed offer key"
        );
        assert!(
            !world
                .localnet
                .log_has(index, "TEE key-handoff did not complete"),
            "validator-{index} fell back to a timed-out TEE handoff"
        );
    }
    assert_eq!(
        world.localnet.log_count(0, "running DKG ceremony"),
        world.state.marker_count.expect("pre-restart DKG count"),
        "full restart unexpectedly triggered a new DKG ceremony"
    );

    let wwd = world.state.wwd.clone().expect("wwd");
    let key = world.validators.get(0).evm_key().expect("validator-0 key");
    let primary = world.validators.primary_port();
    assert!(
        world.rpc.offer_until_supply(&key, &wwd, primary, "1", 5),
        "post-committee-restart offer did not land (supply != 1)"
    );
}

/// The restarted node catches up and resumes signing WITHOUT a fresh ceremony
/// (s4:38-55).
#[then("it resumes signing from the persisted share without a new ceremony")]
fn resumes_without_new_ceremony(world: &mut World) {
    let primary = world.validators.primary_port();
    let idx = world.validators.joiner_index();
    let joiner_port = world.validators.http_port(idx);
    let addr = world.state.joiner_addr.clone().expect("joiner addr");
    let restart_h = world.state.marker_height.expect("restart height");
    let pre_ceremony = world.state.marker_count.expect("pre ceremony count");

    let h = world
        .rpc
        .wait_block(joiner_port, restart_h, 30)
        .unwrap_or(0);
    assert!(
        h >= restart_h,
        "restarted node did not catch up (head {h} < {restart_h})"
    );
    assert_eq!(
        world.localnet.log_count(idx, "running DKG ceremony"),
        pre_ceremony,
        "a fresh DKG ceremony was triggered by the restart"
    );
    assert!(
        world.rpc.is_participant(primary, &addr),
        "node is not an ACTIVE participant after restart"
    );
    assert!(
        lockstep_ok(&world.rpc, primary, joiner_port),
        "restarted validator does not resume signing in lockstep"
    );
    assert_eq!(
        world.localnet.log_count(0, "byzantine evidence observed"),
        0,
        "byzantine/equivocation evidence around the restart"
    );

    // Enclave still works: an offer is executed by the reconnected node.
    let wwd = world.state.wwd.clone().expect("wwd");
    let v1 = world
        .validators
        .by_name("validator-1")
        .expect("v1")
        .evm_key()
        .expect("v1 key");
    assert!(
        world.rpc.offer_until_supply(&v1, &wwd, primary, "2", 5),
        "post-node-restart offer did not land (supply != 2)"
    );
    sleep(Duration::from_secs(6));
    assert_eq!(
        world.rpc.supply(primary),
        world.rpc.supply(joiner_port),
        "enclave offer parity post-restart"
    );
}

/// Complete a 4→5 DKG while the joiner is still PENDING, leaving a durable
/// recovery checkpoint and a real block interval before activation.
#[when("a joiner completes DKG and waits below the activation boundary")]
fn joiner_completes_dkg_before_activation(world: &mut World) {
    let primary = world.validators.primary_port();
    let idx = world.validators.joiner_index();
    let joiner_port = world.validators.http_port(idx);
    world
        .localnet
        .provision_joiner(idx)
        .expect("provision joiner");
    let keys = world.localnet.keys_dir(idx);
    world
        .localnet
        .launch_joiner(idx, &["--consensus.keys-dir", &keys])
        .expect("launch joiner");
    assert!(
        world.rpc.wait_block(joiner_port, 20, 40).is_some(),
        "joiner never cold-synced"
    );

    let key = world.validators.joiner().evm_key().expect("joiner key");
    let addr = world.rpc.address_of(&key).expect("joiner address");
    world.state.joiner_addr = Some(addr.clone());
    world.rpc.stake(&key, 1000).expect("stake joiner");
    world.rpc.confirm_ready(&key).expect("confirm joiner ready");

    let mut observed = false;
    for _ in 0..90 {
        if world
            .localnet
            .log_has(idx, "persisted completed DKG state before activation")
        {
            observed = true;
            break;
        }
        sleep(Duration::from_secs(2));
    }
    assert!(observed, "joiner never reached durable pending DKG state");
    assert_eq!(
        world.rpc.validator_status(primary, &addr),
        Some(1),
        "joiner must remain PENDING before activation"
    );
    assert!(
        !world.rpc.is_participant(primary, &addr),
        "joiner participated before the activation boundary"
    );
    assert!(
        world.localnet.has_share_file(idx),
        "completed DKG material was not persisted before restart"
    );
    world.state.marker_height = world.rpc.head(primary);
    world.state.marker_count = world
        .rpc
        .epoch_on(primary)
        .and_then(|epoch| usize::try_from(epoch).ok());
}

/// Restart both halves of the joining validator while the finalized DKG result
/// is durable but has not yet become the active committee.
#[when("the joining node and enclave restart before activation")]
fn restart_joiner_before_activation(world: &mut World) {
    let primary = world.validators.primary_port();
    let idx = world.validators.joiner_index();
    let addr = world.state.joiner_addr.clone().expect("joiner address");
    assert_eq!(world.rpc.validator_status(primary, &addr), Some(1));
    assert!(!world.rpc.is_participant(primary, &addr));

    world.localnet.stop_joiner(idx).expect("stop joiner");
    world
        .localnet
        .restart_joiner_enclave(idx)
        .expect("restart joiner enclave");
    let keys = world.localnet.keys_dir(idx);
    world
        .localnet
        .launch_joiner(idx, &["--consensus.keys-dir", &keys])
        .expect("restart joiner node");
}

/// Startup must restore the pending boundary/material, activate at the planned
/// epoch exactly once, and leave every validator on one live committee state.
#[then("the recovered pending DKG activates once and consensus continues")]
fn pending_dkg_recovers_and_activates(world: &mut World) {
    let primary = world.validators.primary_port();
    let idx = world.validators.joiner_index();
    let joiner_port = world.validators.http_port(idx);
    let addr = world.state.joiner_addr.clone().expect("joiner address");
    let old_epoch = world.state.marker_count.expect("pre-restart epoch");

    assert!(
        world.rpc.wait_participant(primary, &addr, 60),
        "restarted joiner never activated from pending DKG"
    );
    assert_eq!(world.rpc.validator_status(primary, &addr), Some(2));
    assert_eq!(world.rpc.active_count(primary), Some(5));
    let expected_epoch = u64::try_from(old_epoch + 1).expect("epoch fits u64");
    assert_eq!(world.rpc.epoch_on(primary), Some(expected_epoch));
    assert!(
        world.localnet.log_has(
            idx,
            "threshold material ready from durable pending DKG state and boundary snapshot",
        ) || world.localnet.log_has(
            idx,
            "threshold material ready from promoted pending DKG state",
        ),
        "restart did not use durable pending DKG recovery"
    );
    assert!(
        world
            .localnet
            .enclave_log_has(idx, "unsealed offer key + group signature"),
        "joiner enclave did not recover sealed state"
    );

    let target = world.rpc.head(primary).unwrap_or_default() + 3;
    let mut ports = world.validators.committee_ports();
    ports.push(joiner_port);
    for port in ports {
        assert!(
            world.rpc.wait_block(port, target, 60).is_some(),
            "RPC {port} did not continue after pending-DKG recovery"
        );
        assert_eq!(world.rpc.active_count(port), Some(5));
        assert_eq!(world.rpc.epoch_on(port), Some(expected_epoch));
    }
    assert!(
        lockstep_ok(&world.rpc, primary, joiner_port),
        "recovered joiner did not sign in lockstep"
    );
}
