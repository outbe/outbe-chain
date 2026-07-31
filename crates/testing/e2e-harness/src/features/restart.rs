//! Steps for `features/s4_restart_active.feature`. An ACTIVE validator's DKG share lives on
//! disk (keys-dir), not the enclave. Killing and restarting ONLY the node (the
//! enclave container stays up) must resume signing from the persisted share
//! WITHOUT a fresh DKG ceremony.

use std::thread::sleep;
use std::time::Duration;

use cucumber::{then, when};

use crate::world::rpc::Rpc;
use crate::world::World;

/// Lockstep probe (s4:46-48): both nodes make progress and converge within 3 blocks.
///
/// Do not require a block in every fixed sampling interval. A real SGX
/// committee can pause as a whole for longer than one interval; that is not
/// evidence that the recovering node fell behind.
fn lockstep_ok(rpc: &Rpc, committee: u16, joiner: u16) -> bool {
    let Some(initial_committee) = rpc.finalized(committee) else {
        return false;
    };
    let Some(initial_joiner) = rpc.finalized(joiner) else {
        return false;
    };
    for _ in 0..30 {
        sleep(Duration::from_secs(2));
        let ch = rpc.head(committee).unwrap_or(0);
        let vh = rpc.head(joiner).unwrap_or(0);
        let cf = rpc.finalized(committee).unwrap_or(0);
        let vf = rpc.finalized(joiner).unwrap_or(0);
        if cf > initial_committee
            && vf > initial_joiner
            && ch.abs_diff(vh) <= 3
            && cf.abs_diff(vf) <= 3
        {
            return true;
        }
    }
    false
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

/// Catch the first observable freeze of a 4→5 target and immediately restart
/// the joining node plus enclave, before it can persist completed material.
#[when("a joining validator is restarted during its DKG ceremony")]
fn restart_joiner_during_dkg(world: &mut World) {
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
    assert!(world.rpc.wait_block(joiner_port, 20, 40).is_some());

    let key = world.validators.joiner().evm_key().expect("joiner key");
    let addr = world.rpc.address_of(&key).expect("joiner address");
    world.state.joiner_addr = Some(addr.clone());
    world.rpc.stake(&key, 1000).expect("stake joiner");
    world.rpc.confirm_ready(&key).expect("confirm joiner ready");

    let mut ceremony_started = false;
    for _ in 0..1_800 {
        if world
            .localnet
            .log_has(idx, "freezing validator set and starting DKG rotation")
        {
            ceremony_started = true;
            break;
        }
        sleep(Duration::from_millis(100));
    }
    assert!(ceremony_started, "joiner's DKG ceremony never started");
    assert!(
        !world
            .localnet
            .log_has(idx, "persisted completed DKG state before activation"),
        "DKG completed before the intended in-flight restart"
    );
    assert_eq!(world.rpc.validator_status(primary, &addr), Some(1));
    assert!(!world.rpc.is_participant(primary, &addr));
    world.state.marker_height = world.rpc.head(primary);
    world.state.marker_count = world
        .rpc
        .epoch_on(primary)
        .and_then(|epoch| usize::try_from(epoch).ok());

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

/// An interrupted ceremony may retry, but it must never partially activate;
/// the 4-node committee remains live until one finalized DKG outcome activates.
#[then("the old committee stays live and a later DKG activates the joiner once")]
fn interrupted_dkg_retries_without_partial_activation(world: &mut World) {
    let primary = world.validators.primary_port();
    let idx = world.validators.joiner_index();
    let joiner_port = world.validators.http_port(idx);
    let addr = world.state.joiner_addr.clone().expect("joiner address");
    let marker = world.state.marker_height.expect("restart height");
    let old_epoch = world.state.marker_count.expect("restart epoch");

    assert!(
        world.rpc.wait_block(primary, marker + 3, 40).is_some(),
        "old committee stopped while joiner DKG was interrupted"
    );
    if !world.rpc.is_participant(primary, &addr) {
        assert_eq!(world.rpc.validator_status(primary, &addr), Some(1));
        assert_eq!(world.rpc.active_count(primary), Some(4));
    }

    assert!(
        world.rpc.wait_participant(primary, &addr, 90),
        "joiner did not activate after interrupted DKG retry"
    );
    let expected_epoch = u64::try_from(old_epoch + 1).expect("epoch fits u64");
    assert_eq!(world.rpc.validator_status(primary, &addr), Some(2));
    assert_eq!(world.rpc.active_count(primary), Some(5));
    assert_eq!(world.rpc.epoch_on(primary), Some(expected_epoch));
    assert!(
        world.localnet.has_share_file(idx),
        "retried DKG did not persist the joiner's share"
    );
    assert!(
        world
            .localnet
            .enclave_log_has(idx, "unsealed offer key + group signature"),
        "joiner enclave did not recover its sealed state during DKG restart"
    );

    let target = world.rpc.head(primary).unwrap_or_default() + 3;
    let mut ports = world.validators.committee_ports();
    ports.push(joiner_port);
    for port in ports {
        assert!(world.rpc.wait_block(port, target, 60).is_some());
        assert_eq!(world.rpc.active_count(port), Some(5));
        assert_eq!(world.rpc.epoch_on(port), Some(expected_epoch));
    }
}

/// Restart at the earliest durable join checkpoint: registration, P2P identity
/// and enclave join are committed, but no stake/readiness or DKG side effect is.
#[when("a registered joining node and enclave restart before staking")]
fn restart_registered_joiner_before_staking(world: &mut World) {
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
    assert!(world.rpc.wait_block(joiner_port, 20, 40).is_some());

    let key = world.validators.joiner().evm_key().expect("joiner key");
    let addr = world.rpc.address_of(&key).expect("joiner address");
    world.state.joiner_addr = Some(addr.clone());
    assert_eq!(world.rpc.validator_status(primary, &addr), Some(0));
    assert_eq!(
        world.rpc.stake_on(primary, &addr),
        Some(alloy_primitives::U256::ZERO)
    );
    assert!(!world.rpc.is_participant(primary, &addr));
    assert_eq!(world.rpc.active_count(primary), Some(4));
    world.state.marker_height = world.rpc.head(primary);
    world.state.marker_count = world
        .rpc
        .epoch_on(primary)
        .and_then(|epoch| usize::try_from(epoch).ok());

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

/// The restart must preserve exactly the registered pre-state; only subsequent
/// stake/readiness may create one pending target and one activation.
#[then("registration survives and the join can activate once")]
fn registered_restart_then_join_activates(world: &mut World) {
    let primary = world.validators.primary_port();
    let idx = world.validators.joiner_index();
    let joiner_port = world.validators.http_port(idx);
    let addr = world.state.joiner_addr.clone().expect("joiner address");
    let old_epoch = world.state.marker_count.expect("pre-restart epoch");
    let marker = world.state.marker_height.expect("pre-restart height");

    assert!(world.rpc.wait_block(joiner_port, marker, 40).is_some());
    assert_eq!(world.rpc.validator_status(primary, &addr), Some(0));
    assert_eq!(
        world.rpc.stake_on(primary, &addr),
        Some(alloy_primitives::U256::ZERO)
    );
    assert!(!world.rpc.is_participant(primary, &addr));
    assert_eq!(world.rpc.active_count(primary), Some(4));
    assert_eq!(
        world.rpc.epoch_on(primary),
        Some(u64::try_from(old_epoch).expect("epoch fits u64"))
    );
    assert!(
        world
            .localnet
            .enclave_log_has(idx, "unsealed offer key + group signature"),
        "registered joiner's enclave did not recover sealed state"
    );

    let key = world.validators.joiner().evm_key().expect("joiner key");
    world.rpc.stake(&key, 1000).expect("stake after restart");
    assert_eq!(world.rpc.validator_status(primary, &addr), Some(1));
    world
        .rpc
        .confirm_ready(&key)
        .expect("confirm after restart");
    assert!(
        world.rpc.wait_participant(primary, &addr, 90),
        "registered joiner did not activate after restart"
    );
    let expected_epoch = u64::try_from(old_epoch + 1).expect("epoch fits u64");
    assert_eq!(world.rpc.validator_status(primary, &addr), Some(2));
    assert_eq!(world.rpc.active_count(primary), Some(5));
    assert_eq!(world.rpc.epoch_on(primary), Some(expected_epoch));

    let target = world.rpc.head(primary).unwrap_or_default() + 3;
    let mut ports = world.validators.committee_ports();
    ports.push(joiner_port);
    for port in ports {
        assert!(world.rpc.wait_block(port, target, 60).is_some());
        assert_eq!(world.rpc.active_count(port), Some(5));
        assert_eq!(world.rpc.epoch_on(port), Some(expected_epoch));
    }
}

/// Interrupt one existing committee member only after the scheduled 4→5
/// reshare has actually frozen and entered DKG. The restart must not create a
/// second target or permit a partial activation.
#[when("an active validator and enclave restart during a joining reshare")]
fn restart_active_validator_during_reshare(world: &mut World) {
    let primary = world.validators.primary_port();
    let idx = world.validators.joiner_index();
    let joiner_port = world.validators.http_port(idx);
    world
        .localnet
        .provision_joiner(idx)
        .expect("provision joiner");
    world
        .localnet
        .launch_joiner(idx, &[])
        .expect("launch joiner");
    assert!(world.rpc.wait_block(joiner_port, 20, 40).is_some());

    let key = world.validators.joiner().evm_key().expect("joiner key");
    let addr = world.rpc.address_of(&key).expect("joiner address");
    world.state.joiner_addr = Some(addr.clone());
    world.rpc.stake(&key, 1000).expect("stake joiner");
    world.rpc.confirm_ready(&key).expect("confirm joiner ready");

    let mut ceremony_started = false;
    for _ in 0..1_800 {
        if world
            .localnet
            .log_has(0, "freezing validator set and starting DKG rotation")
        {
            ceremony_started = true;
            break;
        }
        sleep(Duration::from_millis(100));
    }
    assert!(ceremony_started, "joining reshare never entered DKG");
    assert!(
        !world
            .localnet
            .log_has(0, "persisted completed DKG state before activation"),
        "reshare completed before the intended active-validator restart"
    );
    assert_eq!(world.rpc.validator_status(primary, &addr), Some(1));
    assert!(!world.rpc.is_participant(primary, &addr));
    assert_eq!(world.rpc.active_count(primary), Some(4));
    let retry_snapshot = world
        .localnet
        .scenario_dir()
        .join("validator-3/data/keys/dkg_dealer_retry.hex");
    let mut retry_snapshot_persisted = false;
    for _ in 0..100 {
        if retry_snapshot.exists() {
            retry_snapshot_persisted = true;
            break;
        }
        sleep(Duration::from_millis(50));
    }
    assert!(
        retry_snapshot_persisted,
        "active dealer did not persist its retry transcript before restart"
    );
    world.state.marker_height = world.rpc.head(primary);
    world.state.marker_count = world
        .rpc
        .epoch_on(primary)
        .and_then(|epoch| usize::try_from(epoch).ok());

    world
        .localnet
        .restart_validator_and_enclave(3)
        .expect("restart active validator and enclave during reshare");
}

/// The interrupted scheduled target may retry, but activation remains atomic:
/// the joiner enters once, the restarted incumbent remains active, and every
/// node converges on the same epoch and committee.
#[then("the frozen reshare activates once with the restarted validator in lockstep")]
fn active_restart_reshare_converges(world: &mut World) {
    let primary = world.validators.primary_port();
    let restarted = world.validators.http_port(3);
    let idx = world.validators.joiner_index();
    let joiner_port = world.validators.http_port(idx);
    let addr = world.state.joiner_addr.clone().expect("joiner address");
    let marker = world.state.marker_height.expect("restart height");
    let old_epoch = world.state.marker_count.expect("restart epoch");

    assert!(
        world.rpc.wait_block(primary, marker + 3, 40).is_some(),
        "old committee stopped finalizing during active-validator restart"
    );
    assert!(
        world.rpc.wait_block(restarted, marker + 3, 60).is_some(),
        "restarted active validator did not catch up"
    );
    if !world.rpc.is_participant(primary, &addr) {
        assert_eq!(world.rpc.validator_status(primary, &addr), Some(1));
        assert_eq!(world.rpc.active_count(primary), Some(4));
    }

    assert!(
        world.rpc.wait_participant(primary, &addr, 90),
        "frozen reshare did not activate after active-validator recovery"
    );
    let expected_epoch = u64::try_from(old_epoch + 1).expect("epoch fits u64");
    let target = world.rpc.head(primary).unwrap_or_default() + 3;
    let mut ports = world.validators.committee_ports();
    ports.push(joiner_port);
    for port in ports {
        assert!(world.rpc.wait_block(port, target, 60).is_some());
        let status = world.rpc.validator_status(port, &addr);
        let active_count = world.rpc.active_count(port);
        let epoch = world.rpc.epoch_on(port);
        let head = world.rpc.head(port);
        assert_eq!(
            status,
            Some(2),
            "joiner status differs on RPC {port}: active_count={active_count:?} epoch={epoch:?} head={head:?}"
        );
        assert_eq!(active_count, Some(5), "active count differs on RPC {port}");
        assert_eq!(epoch, Some(expected_epoch), "epoch differs on RPC {port}");
    }
    assert!(
        world
            .localnet
            .enclave_log_has(3, "unsealed offer key + group signature"),
        "restarted active validator did not recover sealed enclave state"
    );
    assert!(
        world
            .localnet
            .log_has(3, "restoring durable DKG dealer transcript"),
        "restarted active dealer did not restore the interrupted DKG transcript"
    );
    assert!(
        lockstep_ok(&world.rpc, primary, restarted),
        "restarted active validator did not return to finalized lockstep"
    );
}
