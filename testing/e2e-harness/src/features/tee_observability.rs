//! Steps for `features/tee_observability.feature` - enclave canary health,
//! per-request telemetry, and the session reconnect/revocation contract.

use std::thread::sleep;
use std::time::Duration;

use cucumber::{then, when};
use serde_json::Value;

use crate::world::World;

/// The parsed `outbe_consensusStatus.enclave` object of validator `index`.
fn enclave_status(world: &World, index: usize) -> Option<Value> {
    let port = world.validators.http_port(index);
    let raw = world.rpc.consensus_status_field(port, "enclave")?;
    serde_json::from_str(&raw).ok()
}

/// Poll until validator `index`'s canary state is one of `want`; returns the
/// last observed state either way. The harness runs the canary at a 5s
/// cadence, so `tries` x 2s bounds the wait.
fn wait_enclave_state(world: &World, index: usize, want: &[&str], tries: usize) -> String {
    let mut last = String::from("<no enclave status>");
    for _ in 0..tries {
        if let Some(enclave) = enclave_status(world, index) {
            if let Some(state) = enclave.get("state").and_then(Value::as_str) {
                last = state.to_string();
                if want.contains(&state) {
                    return last;
                }
            }
        }
        sleep(Duration::from_secs(2));
    }
    last
}

#[then("every validator reports a ready enclave canary")]
fn every_validator_reports_ready_canary(world: &mut World) {
    for index in 0..world.validators.size() {
        let state = wait_enclave_state(world, index, &["ready"], 45);
        assert_eq!(
            state, "ready",
            "validator-{index} canary never became ready (last state: {state})"
        );
        let enclave = enclave_status(world, index).expect("enclave status");
        assert_eq!(
            enclave.get("offerKeyReady").and_then(Value::as_bool),
            Some(true),
            "validator-{index} reports no resident offer key"
        );
    }
}

#[then("every enclave log shows per-request telemetry")]
fn every_enclave_log_shows_telemetry(world: &mut World) {
    for index in 0..world.validators.size() {
        assert!(
            world
                .localnet
                .enclave_log_has(index, "req=process_tribute_offer_batch")
                .expect("read required owned process log"),
            "validator-{index} enclave log has no canary-decrypt telemetry line"
        );
        assert!(
            world
                .localnet
                .enclave_log_has(index, "req=get_public_keys")
                .expect("read required owned process log"),
            "validator-{index} enclave log has no get_public_keys telemetry line"
        );
    }
}

#[when("validator-1's enclave sidecar restarts with its sealed identity")]
fn enclave_sidecar_restarts_sealed(world: &mut World) {
    world
        .localnet
        .restart_enclave_only(1)
        .expect("restart validator-1 enclave sidecar");
}

#[then("validator-1's enclave session reconnects without a node restart")]
fn enclave_session_reconnects(world: &mut World) {
    let state = wait_enclave_state(world, 1, &["ready"], 45);
    assert_eq!(
        state, "ready",
        "validator-1 canary did not recover after a sealed enclave restart (last: {state})"
    );
    assert!(
        world.localnet.validator_running(1),
        "validator-1's node must have kept running across the enclave restart"
    );
    assert!(
        world
            .localnet
            .enclave_log_has(
                1,
                "unsealed offer key + group signature <- /tee/sealed_root.bin (restart fast-path)"
            )
            .expect("read restarted enclave log"),
        "restarted enclave did not restore its sealed offer key"
    );
}

#[when("validator-1's enclave restarts with a fresh identity")]
fn enclave_restarts_fresh_identity(world: &mut World) {
    world
        .localnet
        .restart_enclave_with_fresh_identity(1)
        .expect("restart validator-1 enclave with a fresh identity");
}

#[then("validator-1 reports a refused enclave session while the rest stay ready")]
fn revoked_session_reported(world: &mut World) {
    // The pinned session must refuse the impostor: the canary can only report
    // unavailable (session revoked / probe refused) or degraded - never ready.
    let state = wait_enclave_state(world, 1, &["unavailable", "degraded"], 45);
    assert!(
        state == "unavailable" || state == "degraded",
        "validator-1 canary should be refused against a re-keyed enclave, got: {state}"
    );
    assert!(
        world.localnet.validator_running(1),
        "fail-closed revocation must not take the node down"
    );
    // Give the healthy validators one more canary cycle, then require ready.
    for index in [0usize, 2, 3] {
        let state = wait_enclave_state(world, index, &["ready"], 15);
        assert_eq!(
            state, "ready",
            "validator-{index} canary must stay ready (its enclave was untouched)"
        );
    }
}
