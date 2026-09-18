//! Steps for `features/tee_observability.feature` - enclave canary health,
//! per-request telemetry, and production session identity/recovery.

use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::internal::launch_log::LaunchLog;
use eyre::{ensure, Result};

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

/// One owned replacement and the pre-fault public identity. Secret seals stay
/// on disk under the scenario directory and never enter the evidence JSON.
#[derive(Debug)]
pub(crate) struct TeeObservation {
    node_pid: u32,
    enclave_pid: u32,
    armed: Instant,
    previous_last_ok_age: u64,
    baseline_failures: u64,
    original_manifest: Vec<u8>,
    original_offer: [u8; 32],
    log: LaunchLog,
}

#[derive(Clone, Copy)]
enum RestartKind {
    Preserve,
    Substitute,
    Restore,
}

fn restart_observed(world: &mut World, kind: RestartKind) {
    let before = world
        .localnet
        .live_validator_and_enclave_pids(1)
        .expect("live fault target");
    let directory = world.localnet.scenario_dir().join("validator-1");
    let manifest = outbe_tee::load_committed_enclave_manifest_v1(&directory.join("data"))
        .expect("original node manifest")
        .encode_canonical()
        .expect("canonical manifest");
    let original_offer = if let Some(mut previous) = world.state.tee_observability.take() {
        assert_eq!(
            before,
            (previous.node_pid, previous.enclave_pid),
            "owned incarnation changed"
        );
        assert_eq!(
            manifest, previous.original_manifest,
            "running node manifest changed"
        );
        previous.log.seal().expect("seal previous observation");
        previous.original_offer
    } else {
        world
            .localnet
            .node_offer_public(1)
            .expect("original permanent offer key")
    };
    let log = match kind {
        RestartKind::Preserve => world.localnet.restart_enclave_only(1),
        RestartKind::Substitute => world.localnet.restart_enclave_with_fresh_identity(1),
        RestartKind::Restore => world.localnet.restore_enclave_identity(1),
    }
    .expect("replace the owned enclave");
    let after = world
        .localnet
        .live_validator_and_enclave_pids(1)
        .expect("live replacement");
    assert_eq!(
        before.0, after.0,
        "node restarted during enclave-only fault"
    );
    assert_ne!(before.1, after.1, "enclave incarnation did not change");
    // Require probes newer than the completed launch/initialization, including
    // when stopping the old process overlapped an in-flight successful canary.
    // The launch-scoped log also covers the entire replacement startup interval.
    let status = enclave_status(world, 1).expect("post-launch canary baseline");
    let previous_last_ok_age = status["lastOkAgoMillis"]
        .as_u64()
        .expect("canary has succeeded");
    let baseline_failures = status["consecutiveFailures"]
        .as_u64()
        .expect("failure counter");
    let armed = Instant::now();
    let replacement_manifest = matches!(kind, RestartKind::Substitute).then(|| {
        outbe_tee::load_committed_enclave_manifest_v1(
            &directory.join("tee-observability-replacement-host"),
        )
        .expect("replacement public identity")
        .encode_canonical()
        .expect("canonical replacement manifest")
    });
    world.state.restart_observations.push(serde_json::json!({
        "phase": "tee_enclave_replaced", "node_pid": after.0,
        "old_enclave_pid": before.1, "enclave_pid": after.1,
        "enclave_log_start": log.start_offset(),
        "original_public_manifest": hex::encode(&manifest),
        "replacement_public_manifest": replacement_manifest.map(hex::encode),
        "baseline_canary": status,
    }));
    world.state.tee_observability = Some(TeeObservation {
        node_pid: after.0,
        enclave_pid: after.1,
        armed,
        previous_last_ok_age,
        baseline_failures,
        original_manifest: manifest,
        original_offer,
        log,
    });
}

fn assert_incarnation(world: &mut World, observation: &TeeObservation) {
    assert_eq!(
        world
            .localnet
            .live_validator_and_enclave_pids(1)
            .expect("owned live node and enclave"),
        (observation.node_pid, observation.enclave_pid),
        "observation lost its exact process incarnation"
    );
}

// Leave a full second of margin for the separate RPC and monotonic-clock reads.
// A cached success is older than the observation and cannot pass this predicate.
fn fresh_success(status: &Value, elapsed: Duration) -> bool {
    status["state"] == "ready"
        && status["offerKeyReady"] == true
        && status["consecutiveFailures"] == 0
        && status["lastFailureClass"].is_null()
        && status["lastOkAgoMillis"]
            .as_u64()
            .is_some_and(|age| u128::from(age).saturating_add(1_000) < elapsed.as_millis())
}

fn successful_request(log: &str, request: &str) -> bool {
    let request_field = format!("req={request}");
    log.lines().any(|line| {
        line.split_whitespace().any(|field| field == request_field)
            && line.split_whitespace().any(|field| field == "outcome=ok")
    })
}

fn authentication_rejections(log: &str) -> usize {
    log.lines().filter(|line| {
        line.contains("tee enclave: connection error: handshake error: decrypt error")
            || line.contains("tee enclave: connection error: handshake error: Noise IK initiator is not the authorized NodeHost")
    }).count()
}

/// Require both a fresh failed node probe and enclave-side authentication
/// evidence. A dead socket or an uninitialized replacement cannot satisfy it.
fn refused_probe(status: &Value, log: &str, previous_failures: u64) -> bool {
    matches!(status["state"].as_str(), Some("unavailable" | "degraded"))
        && status["consecutiveFailures"]
            .as_u64()
            .is_some_and(|n| n > previous_failures)
        && matches!(
            status["lastFailureClass"].as_str(),
            Some(
                "GetPublicKeys failed: io"
                    | "GetPublicKeys failed: handshake"
                    | "GetPublicKeys failed: noise"
            )
        )
        && authentication_rejections(log) > 0
}

fn no_new_success(status: &Value, elapsed: Duration, previous_age: u64, log: &str) -> Result<()> {
    let age = status["lastOkAgoMillis"]
        .as_u64()
        .ok_or_else(|| eyre::eyre!("missing last success age"))?;
    ensure!(
        u128::from(age).saturating_add(1_000) >= u128::from(previous_age) + elapsed.as_millis(),
        "node accepted a new successful canary from the substituted identity"
    );
    ensure!(
        !log.contains("req=process_tribute_offer_batch"),
        "substituted enclave received an authenticated Tribute decrypt request"
    );
    Ok(())
}

#[when("validator-1's enclave sidecar restarts with its sealed identity")]
fn enclave_sidecar_restarts_sealed(world: &mut World) {
    restart_observed(world, RestartKind::Preserve);
}

#[then("validator-1's enclave session reconnects without a node restart")]
fn enclave_session_reconnects(world: &mut World) {
    let mut observation = world.state.tee_observability.take().expect("armed restart");
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        assert_incarnation(world, &observation);
        let status = enclave_status(world, 1).expect("reconnecting canary RPC");
        let log = observation
            .log
            .read()
            .expect("replacement-only enclave log");
        if fresh_success(&status, observation.armed.elapsed())
            && successful_request(&log, "get_public_keys")
            && successful_request(&log, "process_tribute_offer_batch")
            && log.contains(
                "unsealed offer key + group signature <- /tee/sealed_root.bin (restart fast-path)",
            )
        {
            let directory = world.localnet.scenario_dir().join("validator-1");
            let manifest = outbe_tee::load_committed_enclave_manifest_v1(&directory.join("data"))
                .expect("unchanged node manifest")
                .encode_canonical()
                .expect("canonical manifest");
            assert_eq!(manifest, observation.original_manifest);
            assert_eq!(
                world
                    .localnet
                    .node_offer_public(1)
                    .expect("restored offer key"),
                observation.original_offer
            );
            world.state.restart_observations.push(serde_json::json!({
                "phase": "tee_fresh_reconnect", "node_pid": observation.node_pid,
                "enclave_pid": observation.enclave_pid, "canary": status,
                "enclave_log_start": observation.log.start_offset(), "enclave_log": log,
            }));
            world.state.tee_observability = Some(observation);
            return;
        }
        assert!(
            Instant::now() < deadline,
            "no fresh successful reconnect: {status}; {log}"
        );
        sleep(Duration::from_secs(1));
    }
}

#[when("validator-1's enclave restarts with a fresh identity")]
fn enclave_restarts_fresh_identity(world: &mut World) {
    restart_observed(world, RestartKind::Substitute);
}

#[then("validator-1 reports a refused enclave session while the rest stay ready")]
fn refused_session_reported(world: &mut World) {
    let mut observation = world
        .state
        .tee_observability
        .take()
        .expect("armed identity substitution");
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut failures = observation.baseline_failures;
    let mut rejections = 0;
    let mut fresh_probes = 0;
    loop {
        assert_incarnation(world, &observation);
        let status = enclave_status(world, 1).expect("refused session canary RPC");
        let log = observation.log.read().expect("substituted enclave log");
        no_new_success(
            &status,
            observation.armed.elapsed(),
            observation.previous_last_ok_age,
            &log,
        )
        .expect("new identity must never be adopted");
        if fresh_probes > 0 {
            assert!(
                matches!(status["state"].as_str(), Some("unavailable" | "degraded")),
                "refused session became healthy: {status}"
            );
        }
        let current_rejections = authentication_rejections(&log);
        if refused_probe(&status, &log, failures) && current_rejections > rejections {
            failures = status["consecutiveFailures"]
                .as_u64()
                .expect("fresh failure count");
            rejections = current_rejections;
            fresh_probes += 1;
            world.state.restart_observations.push(serde_json::json!({
                "phase": "tee_fresh_authentication_refusal", "probe": fresh_probes,
                "node_pid": observation.node_pid, "enclave_pid": observation.enclave_pid,
                "canary": status, "authentication_rejections": rejections,
                "enclave_log_start": observation.log.start_offset(), "enclave_log": log,
            }));
            if fresh_probes == 3 {
                break;
            }
        }
        assert!(
            Instant::now() < deadline,
            "no sustained authenticated refusal: {status}; {log}"
        );
        sleep(Duration::from_secs(1));
    }
    for index in [0usize, 2, 3] {
        assert_eq!(
            wait_enclave_state(world, index, &["ready"], 15),
            "ready",
            "healthy peer {index}"
        );
    }
    // Revalidate after observing peers, so a recovery during that wait cannot pass.
    let status = enclave_status(world, 1).expect("final refused canary");
    no_new_success(
        &status,
        observation.armed.elapsed(),
        observation.previous_last_ok_age,
        &observation.log.read().expect("refusal log"),
    )
    .expect("identity remained refused");
    assert_incarnation(world, &observation);
    world.state.tee_observability = Some(observation);
}

#[when("validator-1's original sealed enclave identity is restored")]
fn original_identity_restored(world: &mut World) {
    restart_observed(world, RestartKind::Restore);
}

#[then("the committee finalizes fresh blocks after enclave identity recovery")]
fn fresh_finality_after_recovery(world: &mut World) {
    let ports = world.validators.committee_ports();
    let target = world
        .rpc
        .fresh_finality_target(&ports)
        .expect("post-recovery finality baseline");
    world
        .rpc
        .wait_finalized_checkpoint(&ports, target, 60)
        .expect("two fresh exact committee checkpoints");
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ready(age: u64) -> Value {
        json!({"state":"ready", "offerKeyReady":true, "consecutiveFailures":0,
            "lastFailureClass":null, "lastOkAgoMillis":age})
    }

    #[test]
    fn cached_ready_cannot_prove_a_reconnect() {
        assert!(!fresh_success(&ready(12_000), Duration::from_secs(10)));
        assert!(!fresh_success(&ready(9_500), Duration::from_secs(10)));
        assert!(fresh_success(&ready(500), Duration::from_secs(10)));
        assert!(!successful_request(
            "req=process_tribute_offer_batch peer=local outcome=err",
            "process_tribute_offer_batch"
        ));
        assert!(successful_request(
            "req=process_tribute_offer_batch peer=local outcome=ok",
            "process_tribute_offer_batch"
        ));
    }

    #[test]
    fn unavailable_requires_fresh_probes_and_authentication_evidence() {
        let failure = json!({"state":"unavailable", "consecutiveFailures":3,
            "lastFailureClass":"GetPublicKeys failed: io", "lastOkAgoMillis":30_000});
        let auth = "tee enclave: connection error: handshake error: decrypt error";
        assert!(!refused_probe(&failure, "connection refused", 2));
        assert!(!refused_probe(
            &failure,
            "tee enclave: connection error: handshake error: enclave is not initialized",
            2
        ));
        assert!(!refused_probe(&failure, auth, 3));
        assert!(refused_probe(&failure, auth, 2));
    }

    #[test]
    fn refusal_rejects_a_hidden_success_or_accepted_decrypt() {
        assert!(no_new_success(&ready(30_000), Duration::from_secs(20), 10_000, "").is_ok());
        assert!(no_new_success(&ready(2_000), Duration::from_secs(20), 10_000, "").is_err());
        assert!(no_new_success(
            &ready(30_000),
            Duration::from_secs(20),
            10_000,
            "req=process_tribute_offer_batch outcome=err"
        )
        .is_err());
    }
}
