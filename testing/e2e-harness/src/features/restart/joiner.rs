use super::restart_activation;
use super::restart_capture_incarnations;
use super::restart_check_logs;
use super::restart_joiner_pair;
use super::restart_pin_before;
use super::restart_prove_signing;
use super::wait_for_dkg_retry_snapshot;

use crate::internal::addresses;
use crate::internal::eth;
use crate::internal::launch_log::LaunchLog;
use crate::internal::pending_dkg::PendingDkgCheckpoint;
use crate::world::rpc::FinalizedCheckpoint;
use crate::world::rpc::Rpc;
use crate::world::rpc::TxOutcome;
use crate::world::state::PendingDkgRestartState;
use crate::world::state::RestartIncarnation;
use crate::world::World;

use cucumber::then;
use cucumber::when;
use eyre::ensure;
use eyre::eyre;
use eyre::Result;

use outbe_primitives::reshare_artifact::decode_outbe_block_artifacts;
use outbe_primitives::reshare_artifact::ConsensusHeaderArtifact;
use serde_json::json;
use std::path::Path;
use std::thread::sleep;
use std::time::Duration;

/// Complete a 4->5 DKG while the joiner is still PENDING, leaving a durable
/// recovery checkpoint and a real block interval before activation.
#[when("a joiner completes DKG and waits below the activation boundary")]
fn joiner_completes_dkg_before_activation(world: &mut World) {
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
    let addr = world.rpc.address_of(&key).expect("joiner address");
    world.state.joiner_addr = Some(addr.clone());
    let mut ports = world.validators.committee_ports();
    ports.push(world.validators.http_port(idx));
    let stake = world.rpc.stake(&key, 1000).expect("stake joiner");
    pending_finalize_transaction(&world.rpc, &stake, &ports)
        .expect("joiner stake must be canonically finalized");
    let ready = world.rpc.confirm_ready(&key).expect("confirm joiner ready");
    let admission = pending_finalize_transaction(&world.rpc, &ready, &ports)
        .expect("joiner readiness must be canonically finalized");

    let mut observed = false;
    for _ in 0..90 {
        world
            .localnet
            .live_validator_and_enclave_pids(idx)
            .expect("owned joiner and enclave must remain alive before the crash point");
        if Path::new(&keys)
            .join("dkg_pending_boundary.bin")
            .try_exists()
            .expect("observe installed pending DKG snapshot")
        {
            observed = true;
            break;
        }
        sleep(Duration::from_secs(2));
    }
    assert!(observed, "joiner never reached durable pending DKG state");
    let public_key = world
        .localnet
        .consensus_public_key(idx)
        .expect("read provisioned consensus public key");
    let public_key = hex::decode(public_key.trim().trim_start_matches("0x"))
        .expect("decode consensus public key");
    let checkpoint = PendingDkgCheckpoint::observe(Path::new(&keys), &public_key)
        .expect("pending triplet must match the installed boundary and this joiner");
    let before = world
        .rpc
        .wait_finalized_checkpoint(
            &ports,
            admission.height.max(checkpoint.completed_at_height),
            60,
        )
        .expect("all nodes must share a finalized pre-restart checkpoint");
    assert!(
        before.height < checkpoint.artifact.planned_activation_height,
        "missed completed-but-pending crash point: activation boundary already reached"
    );
    let old_epoch = checkpoint
        .artifact
        .epoch
        .checked_sub(1)
        .expect("incoming DKG epoch");
    for &port in &ports {
        pending_assert_membership(&world.rpc, port, &addr, before.height, 1, false, old_epoch)
            .expect("joiner must remain PENDING at the common finalized checkpoint");
    }
    let original_pids = world
        .localnet
        .live_validator_and_enclave_pids(idx)
        .expect("capture owned live node and enclave before restart");
    world.state.restart_observations.push(json!({
        "phase": "completed_pending", "slot": idx, "height": before.height,
        "block_hash": before.block_hash, "state_root": before.state_root,
        "stake_tx": stake, "ready_tx": ready, "node_pid": original_pids.0,
        "enclave_pid": original_pids.1, "epoch": checkpoint.artifact.epoch,
        "dkg_cycle": checkpoint.artifact.dkg_cycle,
        "completed_at_height": checkpoint.completed_at_height,
        "planned_activation_height": checkpoint.artifact.planned_activation_height,
        "target_set_hash": checkpoint.artifact.target_set_hash,
        "outcome_hash": alloy_primitives::keccak256(&checkpoint.artifact.outcome)
    }));
    world.state.pending_dkg_restart = Some(PendingDkgRestartState {
        checkpoint,
        keys_dir: keys.into(),
        consensus_public_key: public_key,
        before,
        original_pids,
        replacement: None,
    });
}

fn pending_finalize_transaction(
    rpc: &Rpc,
    hash: &str,
    ports: &[u16],
) -> Result<FinalizedCheckpoint> {
    let port = *ports.first().ok_or_else(|| eyre!("missing receipt RPC"))?;
    let receipt = eth::raw_json_result(&rpc.url(port), "eth_getTransactionReceipt", json!([hash]))?;
    rpc.finalize_outcome(
        &TxOutcome {
            transaction_hash: hash.to_owned(),
            success: true,
            receipt,
        },
        ports,
        60,
    )
}

fn pending_assert_membership(
    rpc: &Rpc,
    port: u16,
    addr: &str,
    height: u64,
    status: u8,
    participant: bool,
    epoch: u64,
) -> Result<()> {
    let record = rpc.validator_record_at(port, addr, height).ok_or_else(|| {
        eyre!("cannot observe validator record at finalized h{height} on RPC {port}")
    })?;
    ensure!(
        record.status == status,
        "unexpected validator status at finalized h{height}"
    );
    let actual = eth::read_call_at_result(
        &rpc.url(port),
        addresses::VS_ADDR,
        &eth::IValidatorSet::isConsensusParticipantCall {
            addr: addr.parse()?,
        },
        height,
    )
    .map_err(|error| eyre!(error))?;
    ensure!(
        actual == participant,
        "unexpected participation at finalized h{height}"
    );
    let actual = eth::read_call_at_result(
        &rpc.url(port),
        addresses::VS_ADDR,
        &eth::IValidatorSet::getEpochNumberCall {},
        height,
    )
    .map_err(|error| eyre!(error))?;
    ensure!(
        u64::try_from(actual)? == epoch,
        "unexpected epoch at finalized h{height}"
    );
    Ok(())
}

/// Restart both halves of the joining validator while the finalized DKG result
/// is durable but has not yet become the active committee.
#[when("the joining node and enclave restart before activation")]
fn restart_joiner_before_activation(world: &mut World) {
    let primary = world.validators.primary_port();
    let idx = world.validators.joiner_index();
    let addr = world.state.joiner_addr.clone().expect("joiner address");
    let mut state = world
        .state
        .pending_dkg_restart
        .take()
        .expect("pending DKG checkpoint");
    let height = world
        .rpc
        .finalized_result(primary)
        .expect("pre-stop finalized height");
    assert!(
        height >= state.before.height
            && height < state.checkpoint.artifact.planned_activation_height,
        "completed-but-pending restart window was missed"
    );
    pending_assert_membership(
        &world.rpc,
        primary,
        &addr,
        height,
        1,
        false,
        state
            .checkpoint
            .artifact
            .epoch
            .checked_sub(1)
            .expect("incoming epoch"),
    )
    .expect("finalized state must still be pending immediately before stop");
    assert_eq!(
        world
            .localnet
            .live_validator_and_enclave_pids(idx)
            .expect("owned original processes"),
        state.original_pids,
        "joiner incarnation changed before the intended fault"
    );

    world.localnet.stop_joiner(idx).expect("stop joiner");
    assert_eq!(
        PendingDkgCheckpoint::observe(&state.keys_dir, &state.consensus_public_key)
            .expect("pending checkpoint survives node stop"),
        state.checkpoint,
        "a different ceremony was persisted before restart"
    );
    let dir = world
        .localnet
        .scenario_dir()
        .join(format!("validator-{idx}"));
    let node_log = LaunchLog::arm(&dir.join("node.log")).expect("arm replacement node log");
    let mut enclave_log = None;
    let mut stopped_height = None;
    let rpc = &world.rpc;
    world
        .localnet
        .restart_joiner_enclave_observed(idx, |_| {
            let observed = rpc.finalized_result(primary)?;
            ensure!(
                observed < state.checkpoint.artifact.planned_activation_height,
                "completed-but-pending fault window was crossed before both launchers stopped"
            );
            stopped_height = Some(observed);
            enclave_log = Some(LaunchLog::arm(&dir.join("enclave.log"))?);
            Ok(())
        })
        .expect("restart joiner enclave");
    let enclave_log = enclave_log.expect("replacement enclave interval armed after teardown");
    let keys = world.localnet.keys_dir(idx);
    assert_eq!(
        Path::new(&keys),
        state.keys_dir,
        "restart must preserve keys directory"
    );
    world
        .localnet
        .launch_joiner(idx, &["--consensus.keys-dir", &keys])
        .expect("restart joiner node");
    let (node_pid, enclave_pid) = world
        .localnet
        .live_validator_and_enclave_pids(idx)
        .expect("replacement node and enclave must be alive");
    assert_ne!(node_pid, state.original_pids.0, "node was not restarted");
    assert_ne!(
        enclave_pid, state.original_pids.1,
        "enclave was not restarted"
    );
    world
        .state
        .restart_observations
        .push(json!({"phase": "pending_restart",
        "slot": idx, "node_pid": node_pid, "enclave_pid": enclave_pid,
        "pre_stop_finalized_height": height, "node_log_start": node_log.start_offset(),
        "stopped_finalized_height": stopped_height.expect("observed stop checkpoint"),
        "enclave_log_start": enclave_log.start_offset()}));
    state.replacement = Some(RestartIncarnation {
        node_pid,
        enclave_pid,
        node_log,
        enclave_log,
    });
    world.state.pending_dkg_restart = Some(state);
}

/// Startup must restore the pending boundary/material, activate at the planned
/// epoch exactly once, and leave every validator on one live committee state.
#[then("the recovered pending DKG activates once and consensus continues")]
fn pending_dkg_recovers_and_activates(world: &mut World) {
    let primary = world.validators.primary_port();
    let idx = world.validators.joiner_index();
    let joiner_port = world.validators.http_port(idx);
    let addr = world.state.joiner_addr.clone().expect("joiner address");
    let mut state = world
        .state
        .pending_dkg_restart
        .take()
        .expect("pending DKG checkpoint");
    let replacement = state.replacement.as_mut().expect("replacement processes");
    let expected_pids = (replacement.node_pid, replacement.enclave_pid);
    let expected_epoch = state.checkpoint.artifact.epoch;
    let mut ports = world.validators.committee_ports();
    ports.push(joiner_port);
    let mut next_height = state
        .before
        .height
        .checked_add(1)
        .expect("boundary scan start");
    let mut activation = None;
    let mut promoted_at = None;
    let mut promotion_error = None;
    for _ in 0..60 {
        assert_eq!(
            world
                .localnet
                .live_validator_and_enclave_pids(idx)
                .expect("owned replacement liveness"),
            expected_pids,
            "replacement identity changed during recovery"
        );
        let finalized = world
            .rpc
            .finalized_result(primary)
            .expect("observe finalized boundary head");
        while activation.is_none() && next_height <= finalized {
            if let Some(artifact) = pending_boundary_at(&world.rpc, primary, next_height)
                .expect("decode canonical finalized boundary")
            {
                assert_eq!(
                    artifact, state.checkpoint.artifact,
                    "finalized a different boundary instead of the recovered pending DKG"
                );
                activation = Some(next_height);
                break;
            }
            next_height = next_height.checked_add(1).expect("boundary scan height");
        }
        if let Some(height) = activation {
            let local_height = world
                .rpc
                .finalized_result(joiner_port)
                .expect("replacement finalized height during promotion");
            if local_height >= height {
                assert_eq!(
                    world
                        .rpc
                        .checkpoint_at(joiner_port, height)
                        .expect("replacement boundary checkpoint"),
                    world
                        .rpc
                        .checkpoint_at(primary, height)
                        .expect("canonical boundary checkpoint")
                );
                match state
                    .checkpoint
                    .verify_active(&state.keys_dir, &state.consensus_public_key)
                {
                    Ok(()) => {
                        promoted_at = Some(local_height);
                        break;
                    }
                    Err(error) => promotion_error = Some(error.to_string()),
                }
            }
        }
        sleep(Duration::from_secs(10));
    }
    let activation =
        activation.expect("restarted joiner never finalized its exact pending DKG boundary");
    let promoted_at = promoted_at.unwrap_or_else(|| panic!(
        "matching active material/pending retirement not observed within recovery allowance: {promotion_error:?}"));
    world
        .state
        .restart_observations
        .push(json!({"phase": "pending_promoted",
        "slot": idx, "activation_height": activation, "observed_finalized_height": promoted_at,
        "epoch": expected_epoch, "dkg_cycle": state.checkpoint.artifact.dkg_cycle,
        "outcome_hash": alloy_primitives::keccak256(&state.checkpoint.artifact.outcome)}));
    assert!(
        activation >= state.checkpoint.artifact.planned_activation_height,
        "boundary activated before its planned height"
    );
    world
        .rpc
        .wait_finalized_checkpoint(&ports, activation, 60)
        .expect("all five nodes must finalize the recovered boundary");
    let boundary = world
        .rpc
        .checkpoint_at(primary, activation)
        .expect("activation checkpoint");
    for &port in &ports {
        assert_eq!(
            world
                .rpc
                .checkpoint_at(port, activation)
                .expect("peer activation checkpoint"),
            boundary
        );
        pending_assert_membership(&world.rpc, port, &addr, activation, 2, true, expected_epoch)
            .expect("finalized activation must have exactly the incoming epoch and ACTIVE joiner");
        let active = eth::read_call_at_result(
            &world.rpc.url(port),
            addresses::VS_ADDR,
            &eth::IValidatorSet::activeValidatorCountCall {},
            activation,
        )
        .expect("read active count at exact finalized activation");
        assert_eq!(
            u64::from(active),
            5,
            "finalized activation must have all five validators"
        );
    }
    let node_log = replacement
        .node_log
        .read()
        .expect("read only replacement node log");
    assert!(
        node_log.contains("recovered durable pending DKG boundary snapshot"),
        "replacement did not recover the installed pending snapshot"
    );
    assert!(
        node_log
            .contains("restored future DKG handoff; current-epoch channels will be acquired first"),
        "replacement missed pre-activation recovery and only restored an already active boundary"
    );
    assert!(
        !node_log.contains("running DKG ceremony"),
        "replacement ran fresh genesis bootstrap instead of recovering completed pending DKG"
    );
    let enclave_log = replacement
        .enclave_log
        .read()
        .expect("read only replacement enclave log");
    assert!(
        enclave_log.contains("unsealed offer key + group signature"),
        "replacement enclave did not unseal the same sealed state"
    );
    // Promotion/retirement was observed before waiting for slow peers. A later
    // legitimate DKG cycle may now write its own pending files.
    let target = world
        .rpc
        .fresh_finality_target(&ports)
        .expect("fresh post-activation finality anchor");
    let after = world
        .rpc
        .wait_finalized_checkpoint(&ports, target, 60)
        .expect("five nodes must make new exact finalized hash/root progress after activation");
    // Later legitimate rotations do not invalidate this exact pinned activation.
    // What must never recur is this SAME completed ceremony's boundary.
    for height in activation.checked_add(1).expect("post-activation height")..=after.height {
        if let Some(artifact) = pending_boundary_at(&world.rpc, primary, height)
            .expect("observe post-recovery finalized boundaries")
        {
            assert!(
                artifact.epoch != expected_epoch
                    || artifact.dkg_cycle != state.checkpoint.artifact.dkg_cycle,
                "the recovered completed ceremony activated more than once"
            );
        }
    }
    assert_eq!(
        world
            .localnet
            .live_validator_and_enclave_pids(idx)
            .expect("replacement final liveness"),
        expected_pids
    );
    world
        .state
        .restart_observations
        .push(json!({"phase": "pending_activated",
        "slot": idx, "epoch": expected_epoch, "activation_height": activation,
        "activation_hash": boundary.block_hash, "activation_state_root": boundary.state_root,
        "progress_height": after.height, "progress_hash": after.block_hash,
        "progress_state_root": after.state_root, "node_pid": replacement.node_pid,
        "enclave_pid": replacement.enclave_pid, "pending_material_retired": true}));
    world.state.pending_dkg_restart = Some(state);
}

pub(super) fn pending_boundary_at(
    rpc: &Rpc,
    port: u16,
    height: u64,
) -> Result<Option<outbe_primitives::consensus::DkgBoundaryArtifact>> {
    let checkpoint = rpc.checkpoint_at(port, height)?;
    let block = eth::raw_json_result(
        &rpc.url(port),
        "eth_getBlockByNumber",
        json!([format!("0x{height:x}"), false]),
    )?;
    let hash: alloy_primitives::B256 = serde_json::from_value(
        block
            .get("hash")
            .cloned()
            .ok_or_else(|| eyre!("finalized block omitted hash"))?,
    )?;
    ensure!(
        hash == checkpoint.block_hash,
        "canonical block changed during boundary observation"
    );
    let encoded = block
        .get("extraData")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| eyre!("finalized block omitted extraData"))?;
    let bytes = hex::decode(encoded.trim_start_matches("0x"))?;
    let artifacts = decode_outbe_block_artifacts(&bytes)
        .map_err(|error| eyre!("invalid finalized block artifacts: {error}"))?;
    Ok(match artifacts.consensus_header_artifact {
        Some(ConsensusHeaderArtifact::BoundaryOutcome(artifact)) => Some(artifact),
        _ => None,
    })
}

/// Catch the first observable freeze of a 4->5 target and immediately restart
/// the joining node plus enclave, before it can persist completed material.
#[when("a joining validator is restarted during its DKG ceremony")]
fn restart_joiner_during_dkg(world: &mut World) {
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
    restart_capture_incarnations(world, idx).expect("arm owned pre-ceremony intervals");
    world.rpc.stake(&key, 1000).expect("stake joiner");
    world.rpc.confirm_ready(&key).expect("confirm joiner ready");
    let mut ceremony_started = false;
    for _ in 0..1_800 {
        let log = world
            .state
            .lifecycle_incarnations
            .get_mut(&idx)
            .expect("joiner owner")
            .node_log
            .read()
            .expect("current joiner log");
        if log.contains("freezing validator set and starting DKG rotation") {
            ceremony_started = true;
            break;
        }
        sleep(Duration::from_millis(100));
    }
    assert!(ceremony_started, "joiner never entered DKG");
    wait_for_dkg_retry_snapshot(&keys, idx, "dkg_player_retry.hex");
    assert!(
        !world
            .state
            .lifecycle_incarnations
            .get_mut(&idx)
            .expect("joiner owner")
            .node_log
            .read()
            .expect("current joiner log")
            .contains("persisted completed DKG state before activation"),
        "DKG completed before the intended in-flight fault"
    );
    restart_pin_before(world, 1, idx).expect("canonical pending pre-fault state");
    restart_joiner_pair(world, idx, true)
        .expect("restart exact in-flight joiner and enclave with preserved keys");
}

/// An interrupted ceremony may retry, but it must never partially activate;
/// the 4-node committee remains live until one finalized DKG outcome activates.
#[then("the old committee stays live and a later DKG activates the joiner once")]
fn interrupted_dkg_retries_without_partial_activation(world: &mut World) {
    let idx = world.validators.joiner_index();
    let addr = world.state.joiner_addr.clone().expect("joiner identity");
    let activation = restart_activation(world, true)
        .expect("one canonical retry admission; old committee until boundary");
    let mut share_promoted = false;
    for _ in 0..30 {
        if world.localnet.has_share_file(idx) {
            share_promoted = true;
            break;
        }
        sleep(Duration::from_secs(1));
    }
    assert!(
        share_promoted,
        "retried DKG did not persist its active share"
    );
    assert_eq!(
        world
            .rpc
            .has_threshold_shares(world.validators.http_port(idx)),
        Some(true),
        "ACTIVE joiner has no current private threshold share"
    );
    restart_check_logs(world, idx, true, false).expect("current enclave recovered sealed state");
    // Drain pre-eligibility accounting (including its permitted boundary miss),
    // then close the ENTIRE five-block eligible signing window.
    let after =
        restart_prove_signing(world, &addr, 60, 60).expect("recovered joiner signing window");
    assert!(
        after.height > activation.height,
        "no post-activation finality"
    );
    restart_check_logs(world, idx, true, false)
        .expect("same recovered incarnation remains healthy");
}
