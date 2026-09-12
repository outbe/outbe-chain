use super::restart_capture_incarnations;
use super::restart_check_logs;
use super::restart_finalize_hash;
use super::restart_install_replacement;
use super::restart_pin_before;
use super::restart_prove_signing;
use super::restart_require_membership;
use super::restart_sealed_node_evidence;
use super::restart_snapshot;

use crate::internal::launch_log::LaunchLog;

use crate::world::World;

use cucumber::then;
use cucumber::when;

use serde_json::json;

use std::thread::sleep;
use std::time::Duration;

/// Bring a joiner to ACTIVE with a persisted (keys-dir) share (s4:13-30).
#[when("a joiner reaches active with a persisted share")]
fn joiner_active_persisted_share(world: &mut World) {
    let primary = world.validators.primary_port();
    let idx = world.validators.joiner_index();
    let wwd = world.state.wwd.clone().expect("wwd");
    let v0 = world.validators.get(0).evm_key().expect("v0 key");
    world.state.tribute_tx_hash = Some(
        world
            .rpc
            .offer_until_supply_hash(&v0, &wwd, primary, "1", 5)
            .expect("pre-restart offer did not land"),
    );
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
    let addr = world.rpc.address_of(&key).expect("joiner identity");
    world.state.joiner_addr = Some(addr.clone());
    world.rpc.stake(&key, 1000).expect("stake");
    sleep(Duration::from_secs(6));
    world.rpc.confirm_ready(&key).expect("confirm ready");
    assert!(
        world
            .rpc
            .wait_participant(primary, &addr, 70)
            .expect("observable participation"),
        "joiner did not activate before restart"
    );
    assert!(
        world.localnet.has_share_file(idx),
        "share was not persisted before restart"
    );
    sleep(Duration::from_secs(20));
}

/// Kill only the node (enclave container stays up) and restart it with the same
/// keys-dir/datadir (s4:32-37).
#[when("the node is killed and restarted with the same keys")]
fn node_killed_and_restarted(world: &mut World) {
    let idx = world.validators.joiner_index();
    restart_capture_incarnations(world, idx).expect("capture exact pre-fault owners");
    // The primary has not restarted in this scenario. Preserve the original
    // whole-incarnation negative, then cover subsequent output with its interval.
    assert_eq!(
        world
            .localnet
            .log_count(0, "byzantine evidence observed")
            .expect("read owned primary log"),
        0,
        "pre-fault byzantine/equivocation evidence"
    );
    restart_pin_before(world, 2, idx).expect("canonical ACTIVE pre-fault checkpoint");
    let offer = world
        .state
        .tribute_tx_hash
        .clone()
        .expect("pre-restart offer");
    restart_finalize_hash(world, &offer).expect("pre-restart offer finalized on all five nodes");
    let mut old = world
        .state
        .lifecycle_incarnations
        .remove(&idx)
        .expect("owned restart target");
    let previous = (old.node_pid, old.enclave_pid);
    let status = world
        .localnet
        .kill_validator_owned(idx, old.node_pid)
        .expect("fault exact owned node");
    assert_eq!(
        std::os::unix::process::ExitStatusExt::signal(&status),
        Some(9),
        "node-only restart did not observe the requested SIGKILL"
    );
    world.state.restart_observations.push(json!({
        "phase": "restart_node_fault", "index": idx, "node_pid": old.node_pid,
        "enclave_pid": old.enclave_pid, "exit_signal": 9,
    }));
    old.node_log.seal().expect("seal old node incarnation");
    world.state.restart_observations.push(
        restart_sealed_node_evidence(idx, &mut old).expect("retain sealed old node observation"),
    );
    let dir = world
        .localnet
        .scenario_dir()
        .join(format!("validator-{idx}"));
    let node_log = LaunchLog::arm(&dir.join("node.log")).expect("arm replacement node");
    let keys = world.localnet.keys_dir(idx);
    world
        .localnet
        .launch_joiner(idx, &["--consensus.keys-dir", &keys])
        .expect("restart same node slot");
    restart_install_replacement(world, idx, previous, node_log, old.enclave_log, false)
        .expect("only node replaced; enclave and public identity preserved");
}

/// The restarted node catches up and resumes signing WITHOUT a fresh ceremony
/// (s4:38-55).
#[then("it resumes signing from the persisted share without a new ceremony")]
fn resumes_without_new_ceremony(world: &mut World) {
    let idx = world.validators.joiner_index();
    let addr = world.state.joiner_addr.clone().expect("joiner identity");
    // Retain the original 30x3s catch-up and 30x2s lockstep allowances.
    let after = restart_prove_signing(world, &addr, 30, 20)
        .expect("all-five finalized eligible signing proof");
    assert_eq!(
        world
            .rpc
            .has_threshold_shares(world.validators.http_port(idx)),
        Some(true),
        "restarted validator has no current private threshold share"
    );
    assert!(
        world.localnet.has_share_file(idx),
        "persisted signing share disappeared"
    );
    restart_check_logs(world, idx, false, false)
        .expect("persisted-share startup without genesis DKG");
    let state = restart_snapshot(world, after, &addr, "persisted_share_restart_complete")
        .expect("typed pinned supply and membership parity");
    restart_require_membership(world, &state, 2).expect("same five ACTIVE identities");
    assert_eq!(
        state.supply,
        alloy_primitives::U256::from(1),
        "pre-restart Tribute supply changed"
    );
}
