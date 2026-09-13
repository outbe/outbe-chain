use super::restart_activation;
use super::restart_capture_incarnations;
use super::restart_check_logs;
use super::restart_inflight_round;
use super::restart_install_replacement;
use super::restart_pin_before;
use super::restart_prove_signing;
use super::restart_retain_frozen_target;
use super::restart_sealed_node_evidence;
use super::wait_for_dkg_retry_snapshot;

use crate::internal::launch_log::LaunchLog;

use crate::world::World;

use cucumber::then;
use cucumber::when;
use eyre::ensure;

use serde_json::json;

use std::thread::sleep;
use std::time::Duration;

/// Interrupt one existing committee member only after the scheduled 4->5
/// reshare has actually frozen and entered DKG. The restart must not create a
/// second target or permit a partial activation.
#[when("an active validator and enclave restart during a joining reshare")]
fn restart_active_validator_during_reshare(world: &mut World) {
    let idx = world.validators.joiner_index();
    world
        .localnet
        .provision_joiner(idx)
        .expect("provision joiner");
    world
        .localnet
        .launch_caught_up_joiner(idx, &[])
        .expect("launch joiner");
    let key = world.validators.joiner().evm_key().expect("joiner key");
    world.state.joiner_addr = Some(world.rpc.address_of(&key).expect("joiner identity"));
    restart_capture_incarnations(world, 3)
        .expect("arm incumbent and survivor identities before reshare");
    world.rpc.stake(&key, 1000).expect("stake joiner");
    world.rpc.confirm_ready(&key).expect("confirm joiner ready");
    let mut ceremony_started = false;
    for _ in 0..1_800 {
        let log = world
            .state
            .lifecycle_incarnations
            .get_mut(&0)
            .expect("primary owner")
            .node_log
            .read()
            .expect("current primary log");
        if log.contains("freezing validator set and starting DKG rotation") {
            ceremony_started = true;
            break;
        }
        sleep(Duration::from_millis(100));
    }
    assert!(ceremony_started, "joining reshare never entered DKG");
    assert!(
        !world
            .state
            .lifecycle_incarnations
            .get_mut(&0)
            .expect("primary owner")
            .node_log
            .read()
            .expect("current primary log")
            .contains("persisted completed DKG state before activation"),
        "reshare completed before the intended active-validator fault"
    );
    let keys = world.localnet.scenario_dir().join("validator-3/data/keys");
    wait_for_dkg_retry_snapshot(&keys, 3, "dkg_dealer_retry.hex");
    wait_for_dkg_retry_snapshot(&keys, 3, "dkg_player_retry.hex");
    restart_pin_before(world, 1, 3)
        .expect("canonical pending frozen target before incumbent fault");
    let mut old = world
        .state
        .lifecycle_incarnations
        .remove(&3)
        .expect("owned incumbent");
    let previous = (old.node_pid, old.enclave_pid);
    let expected_round = restart_inflight_round(
        &old.node_log.read().expect("current incumbent log"),
    )
    .expect("incumbent must still be in the incomplete frozen ceremony immediately before fault");
    world.state.restart_observations.push(json!({
        "phase": "restart_inflight_armed", "index": 3, "node_pid": old.node_pid, "round": expected_round,
    }));
    let dir = world.localnet.scenario_dir().join("validator-3");
    let mut logs = None;
    let mut round = None;
    let observations = &mut world.state.restart_observations;
    world
        .localnet
        .restart_validator_and_enclave_owned_observed(3, previous.0, previous.1, |_| {
            old.node_log.seal()?;
            old.enclave_log.seal()?;
            observations.push(restart_sealed_node_evidence(3, &mut old)?);
            let sealed_round = restart_inflight_round(&old.node_log.read()?)?;
            ensure!(
                sealed_round == expected_round,
                "incumbent frozen target changed during fault/reap"
            );
            round = Some(sealed_round);
            logs = Some((
                LaunchLog::arm(&dir.join("node.log"))?,
                LaunchLog::arm(&dir.join("enclave.log"))?,
            ));
            Ok(())
        })
        .expect("restart exactly the owned incumbent and enclave");
    restart_retain_frozen_target(
        world,
        3,
        round.expect("sealed interrupted incumbent target"),
    )
    .expect("historical pre-fault frozen commitment on all survivors");
    let (node_log, enclave_log) = logs.expect("replacement intervals armed after reaping");
    restart_install_replacement(world, 3, previous, node_log, enclave_log, true)
        .expect("same incumbent public identity; other four owners unchanged");
}

/// The interrupted scheduled target may retry, but activation remains atomic:
/// the joiner enters once, the restarted incumbent remains active, and every
/// node converges on the same epoch and committee.
#[then("the frozen reshare activates once with the restarted validator in lockstep")]
fn active_restart_reshare_converges(world: &mut World) {
    restart_activation(world, true)
        .expect("one canonical frozen-target admission after incumbent recovery");
    restart_check_logs(world, 3, true, true)
        .expect("current incumbent recovered enclave and dealer transcript");
    assert_eq!(
        world
            .rpc
            .has_threshold_shares(world.validators.http_port(3)),
        Some(true),
        "restarted ACTIVE incumbent has no current private threshold share"
    );
    let key = world.validators.get(3).evm_key().expect("incumbent key");
    let address = world.rpc.address_of(&key).expect("incumbent identity");
    restart_prove_signing(world, &address, 60, 60)
        .expect("incumbent closes eligible signing window with all five peers");
    restart_check_logs(world, 3, true, true).expect("same incumbent incarnation remains healthy");
}
