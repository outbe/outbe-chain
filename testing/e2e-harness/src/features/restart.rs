//! Restart steps used by `features/validator_lifecycle.feature`. An ACTIVE validator's DKG share lives on
//! disk (keys-dir), not the enclave. Killing and restarting ONLY the node (the
//! enclave container stays up) must resume signing from the persisted share
//! WITHOUT a fresh DKG ceremony.

use std::path::Path;
use std::thread::sleep;
use std::time::Duration;

use cucumber::{given, then, when};
use eyre::{ensure, eyre, Result};
use outbe_primitives::consensus::LATE_FINALIZE_WINDOW_K;
use outbe_primitives::reshare_artifact::{decode_outbe_block_artifacts, ConsensusHeaderArtifact};
use serde_json::json;

use crate::features::common::boot_localnet;
use crate::internal::{addresses, eth, launch_log::LaunchLog, pending_dkg::PendingDkgCheckpoint};
use crate::world::rpc::{FinalizedCheckpoint, Rpc, TxOutcome};
use crate::world::state::{PendingDkgRestartState, RestartIncarnation};
use crate::world::World;

/// Put the freeze boundary inside the bounded restart scenario while leaving
/// enough activation grace for a real-SGX ceremony to recover.
#[given("a fresh localnet with a restartable DKG window")]
fn restartable_dkg_setup(world: &mut World) {
    boot_localnet(
        world,
        6,
        &[
            ("TESTNET_EPOCH_LENGTH_BLOCKS", "120".to_string()),
            ("TESTNET_DKG_PREPARE_WINDOW_BLOCKS", "60".to_string()),
            ("TESTNET_DKG_ACTIVATION_GRACE_BLOCKS", "120".to_string()),
        ],
    );
}

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
        let (Some(ch), Some(vh), Some(cf), Some(vf)) = (
            rpc.head(committee),
            rpc.head(joiner),
            rpc.finalized(committee),
            rpc.finalized(joiner),
        ) else {
            continue;
        };
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

fn wait_for_dkg_retry_snapshot(keys_dir: impl AsRef<Path>, validator: usize, file: &str) {
    let snapshot = keys_dir.as_ref().join(file);
    for _ in 0..1_800 {
        if snapshot.exists() {
            return;
        }
        sleep(Duration::from_millis(100));
    }
    panic!("validator-{validator} did not persist {file} before the restart crash point");
}

/// Bring a joiner to ACTIVE with a persisted (keys-dir) share (s4:13-30).
#[when("a joiner reaches active with a persisted share")]
fn joiner_active_persisted_share(world: &mut World) {
    let primary = world.validators.primary_port();
    let idx = world.validators.joiner_index();
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
        .launch_caught_up_joiner(idx, &["--consensus.keys-dir", &keys])
        .expect("launch joiner (keys-dir)");

    let key = world.validators.joiner().evm_key().expect("joiner key");
    let addr = world.rpc.address_of(&key).expect("joiner addr");
    world.state.joiner_addr = Some(addr.clone());
    world.rpc.stake(&key, 1000).expect("stake");
    sleep(Duration::from_secs(6));
    world.rpc.confirm_ready(&key).expect("confirm ready");

    // A fresh LocalNet activates the admitted joiner only at the next certified
    // DKG boundary. Four co-located real-SGX validators may need more than the
    // old 400-second allowance to reach that boundary; use the same bounded
    // allowance as the lifecycle admission scenario.
    assert!(
        world
            .rpc
            .wait_participant(primary, &addr, 70)
            .expect("wait for observable consensus participation"),
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
    world.state.marker_count = Some(
        world
            .localnet
            .log_count(idx, "running DKG ceremony")
            .expect("read required owned process log"),
    );
    world.state.marker_height = world.rpc.head(primary);
    world.localnet.stop_joiner(idx).expect("stop joiner");
    let keys = world.localnet.keys_dir(idx);
    world
        .localnet
        .launch_joiner(idx, &["--consensus.keys-dir", &keys])
        .expect("relaunch joiner");
}

/// Stop every committee node and enclave, then relaunch them from the same
/// datadirs. Each identity must unseal its own permanent key; there is no peer
/// redelivery path.
#[when("the entire committee and its enclaves are stopped and restarted")]
fn committee_and_enclaves_restarted(world: &mut World) {
    let ports = world.validators.committee_ports();
    let height = ports
        .iter()
        .map(|&port| {
            world
                .rpc
                .finalized_result(port)
                .expect("pre-restart finality")
        })
        .min()
        .expect("nonempty committee");
    let before = world
        .rpc
        .wait_finalized_checkpoint(&ports, height, 1)
        .expect("common pre-restart finalized hash/root");
    let original_pids: Vec<_> = (0..world.validators.size())
        .map(|index| {
            world
                .localnet
                .live_validator_and_enclave_pids(index)
                .expect("both original committee processes must be owned and live")
        })
        .collect();
    assert!(
        world.state.committee_restart.is_empty(),
        "committee restart already armed"
    );
    world.state.marker_height = Some(before.height);
    world.state.restart_observations.push(serde_json::json!({
        "phase": "committee_before_restart",
        "checkpoint": committee_checkpoint_json(before),
        "original_pids": original_pids,
    }));
    let mut logs = Vec::new();
    world
        .localnet
        .restart_committee_and_enclaves_observed(|stopped| {
            // Old processes have been reaped; no replacement has started yet.
            for index in 0..original_pids.len() {
                let dir = stopped.scenario_dir().join(format!("validator-{index}"));
                logs.push((
                    crate::internal::launch_log::LaunchLog::arm(&dir.join("node.log"))?,
                    crate::internal::launch_log::LaunchLog::arm(&dir.join("enclave.log"))?,
                ));
            }
            Ok(())
        })
        .expect("restart committee and enclaves");
    for (index, (node_log, enclave_log)) in logs.into_iter().enumerate() {
        let (node_pid, enclave_pid) = world
            .localnet
            .live_validator_and_enclave_pids(index)
            .expect("both replacement committee processes must be owned and live");
        world.state.restart_observations.push(serde_json::json!({
            "phase": "committee_replacement",
            "validator": index,
            "node_pid": node_pid,
            "enclave_pid": enclave_pid,
            "node_log_start": node_log.start_offset(),
            "enclave_log_start": enclave_log.start_offset(),
        }));
        assert_ne!(node_pid, original_pids[index].0, "node was not replaced");
        assert_ne!(
            enclave_pid, original_pids[index].1,
            "enclave was not replaced"
        );
        world
            .state
            .committee_restart
            .push(crate::world::state::RestartIncarnation {
                node_pid,
                enclave_pid,
                node_log,
                enclave_log,
            });
    }
}

/// Every enclave must use its restart fast-path, every validator must advance,
/// and an enclave-backed Tribute offer must remain executable.
#[then("all validators recover sealed TEE state and resume finalization")]
fn committee_recovers_sealed_tee_state(world: &mut World) {
    let before = world.state.marker_height.expect("pre-restart height");
    let ports = world.validators.committee_ports();
    committee_assert_live(world);
    let target = world
        .rpc
        .fresh_finality_target(&ports)
        .expect("fresh target from every restarted validator")
        .max(before.checked_add(2).expect("restart height overflow"));
    let progressed = world
        .rpc
        .wait_finalized_checkpoint(&ports, target, 60)
        .expect("restarted committee must advance on one finalized hash/root");
    committee_assert_live(world);
    let before_checkpoint = world
        .state
        .restart_observations
        .iter()
        .rev()
        .find(|observation| observation["phase"] == "committee_before_restart")
        .expect("retained pre-restart checkpoint")["checkpoint"]
        .clone();
    for &port in &ports {
        assert_eq!(
            committee_checkpoint_json(
                world
                    .rpc
                    .checkpoint_at(port, before)
                    .expect("pre-restart finalized block must remain canonical")
            ),
            before_checkpoint,
            "validator RPC {port} changed the pre-restart finalized hash/root"
        );
    }
    world.state.restart_observations.push(serde_json::json!({
        "phase": "committee_restarted_finality",
        "target": target,
        "checkpoint": committee_checkpoint_json(progressed),
    }));
    for index in 0..world.validators.size() {
        let incarnation = &mut world.state.committee_restart[index];
        let enclave_log = incarnation
            .enclave_log
            .read()
            .expect("replacement enclave log identity");
        let unsealed: Vec<_> = enclave_log.lines().filter(|line| {
            *line == "outbe-tee-enclave: unsealed offer key + group signature <- /tee/sealed_root.bin (restart fast-path)"
        }).collect();
        world.state.restart_observations.push(serde_json::json!({
            "phase": "committee_unsealed",
            "validator": index,
            "enclave_pid": incarnation.enclave_pid,
            "records": unsealed,
        }));
        assert_eq!(
            unsealed.len(),
            1,
            "validator-{index} enclave did not recover its sealed offer key"
        );
    }
    let wwd = world.state.wwd.clone().expect("wwd");
    let key = world.validators.get(0).evm_key().expect("validator-0 key");
    let primary = world.validators.primary_port();
    let supply_before = committee_supply_at(world, primary, progressed);
    for &port in &ports {
        assert_eq!(
            committee_supply_at(world, port, progressed),
            supply_before,
            "pre-offer finalized supply parity"
        );
    }
    let expected_supply = supply_before
        .checked_add(alloy_primitives::U256::from(1))
        .expect("Tribute supply overflow");
    // Retain an exact prefix of the existing launch capture. The suffix must
    // come from these same processes after this checkpoint, not earlier replay.
    let offer_prefixes: Vec<_> = world
        .state
        .committee_restart
        .iter_mut()
        .map(|incarnation| {
            incarnation
                .enclave_log
                .read()
                .expect("checkpoint replacement enclave log before the new offer")
        })
        .collect();
    committee_assert_live(world);
    world.state.restart_observations.push(serde_json::json!({
        "phase": "committee_before_new_offer",
        "checkpoint": committee_checkpoint_json(progressed),
        "supply": supply_before.to_string(),
        "expected_supply": expected_supply.to_string(),
        "enclave_log_starts": world.state.committee_restart.iter().zip(&offer_prefixes)
            .map(|(incarnation, prefix)| incarnation.enclave_log.start_offset()
                .checked_add(u64::try_from(prefix.len()).expect("log length fits u64"))
                .expect("offer log offset overflow")).collect::<Vec<_>>(),
    }));
    // This helper submits a real offer before polling, even if supply is already visible.
    let transaction_hash = world
        .rpc
        .offer_until_supply_hash(&key, &wwd, primary, &expected_supply.to_string(), 5)
        .expect("new post-restart Tribute offer must be submitted and included");
    let receipt = crate::internal::eth::receipt_json(&world.rpc.url(primary), &transaction_hash)
        .expect("new Tribute transaction receipt must be observable");
    world.state.restart_observations.push(serde_json::json!({
        "phase": "committee_new_offer_receipt",
        "transaction_hash": transaction_hash,
        "receipt": receipt,
    }));
    let outcome = crate::world::rpc::TxOutcome {
        transaction_hash,
        success: receipt.get("status").and_then(serde_json::Value::as_str) == Some("0x1"),
        receipt,
    };
    assert!(outcome.success, "new Tribute offer reverted");
    assert!(
        outcome.block_number().expect("Tribute receipt height") > progressed.height,
        "Tribute receipt predates post-restart observation"
    );
    let finalized = world.rpc.finalize_outcome(&outcome, &ports, 60).expect(
        "new successful Tribute receipt must be canonical and finalized on every validator",
    );
    committee_assert_live(world);
    for &port in &ports {
        assert_eq!(
            committee_supply_at(world, port, finalized),
            expected_supply,
            "new finalized Tribute must increase supply by exactly one"
        );
    }
    world.state.restart_observations.push(serde_json::json!({
        "phase": "committee_new_offer_finalized",
        "transaction_hash": outcome.transaction_hash,
        "checkpoint": committee_checkpoint_json(finalized),
        "supply_before": supply_before.to_string(),
        "supply_after": expected_supply.to_string(),
    }));
    for (index, prefix) in offer_prefixes.iter().enumerate() {
        let incarnation = &mut world.state.committee_restart[index];
        incarnation
            .node_log
            .seal()
            .expect("seal replacement node observation");
        incarnation
            .enclave_log
            .seal()
            .expect("seal replacement enclave observation");
        let enclave_log = incarnation
            .enclave_log
            .read()
            .expect("replacement enclave log identity");
        let offer_log = enclave_log
            .strip_prefix(prefix.as_str())
            .expect("pre-offer launch log prefix changed");
        let decrypted: Vec<_> = offer_log
            .lines()
            .filter(|line| {
                line.starts_with("outbe-tee-enclave: req=process_tribute_offer_batch ")
                    && line
                        .split_ascii_whitespace()
                        .filter(|field| field.starts_with("outcome="))
                        .eq(std::iter::once("outcome=ok"))
            })
            .collect();
        let node_log = incarnation
            .node_log
            .read()
            .expect("replacement node log identity");
        let ceremonies: Vec<_> = node_log
            .lines()
            .filter(|line| line.contains("running DKG ceremony"))
            .collect();
        world.state.restart_observations.push(serde_json::json!({
            "phase": "committee_restart_served_new_offer",
            "validator": index,
            "node_pid": incarnation.node_pid,
            "enclave_pid": incarnation.enclave_pid,
            "decrypt_records": decrypted,
            "new_ceremony_records": ceremonies,
        }));
        assert!(
            !decrypted.is_empty(),
            "validator-{index} lacks a successful new-offer decrypt"
        );
        assert!(
            ceremonies.is_empty(),
            "validator-{index} restart triggered a fresh DKG ceremony"
        );
    }
    committee_assert_live(world);
}

fn committee_assert_live(world: &mut World) {
    assert_eq!(
        world.state.committee_restart.len(),
        world.validators.size(),
        "every committee replacement must be observed"
    );
    for index in 0..world.validators.size() {
        let observed = world
            .localnet
            .live_validator_and_enclave_pids(index)
            .expect("committee replacement processes must remain owned and live");
        let expected = &world.state.committee_restart[index];
        assert_eq!(
            observed,
            (expected.node_pid, expected.enclave_pid),
            "validator-{index} changed process incarnation during restart proof"
        );
    }
}

fn committee_checkpoint_json(
    checkpoint: crate::world::rpc::FinalizedCheckpoint,
) -> serde_json::Value {
    serde_json::json!({
        "height": checkpoint.height,
        "block_hash": format!("{:#x}", checkpoint.block_hash),
        "state_root": format!("{:#x}", checkpoint.state_root),
    })
}

fn committee_supply_at(
    world: &World,
    port: u16,
    checkpoint: crate::world::rpc::FinalizedCheckpoint,
) -> alloy_primitives::U256 {
    assert!(
        world
            .rpc
            .finalized_result(port)
            .expect("supply observation finality")
            >= checkpoint.height
    );
    assert_eq!(
        world
            .rpc
            .checkpoint_at(port, checkpoint.height)
            .expect("supply checkpoint"),
        checkpoint
    );
    let supply = crate::internal::eth::read_call_at_result(
        &world.rpc.url(port),
        crate::internal::addresses::TRIBUTE_ADDR,
        &crate::internal::eth::ITribute::totalSupplyCall {},
        checkpoint.height,
    )
    .expect("read Tribute supply at exact finalized checkpoint");
    assert_eq!(
        world
            .rpc
            .checkpoint_at(port, checkpoint.height)
            .expect("recheck supply checkpoint"),
        checkpoint
    );
    supply
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
        .expect("restarted node RPC must reach its pre-restart height");
    assert!(
        h >= restart_h,
        "restarted node did not catch up (head {h} < {restart_h})"
    );
    assert_eq!(
        world
            .localnet
            .log_count(idx, "running DKG ceremony")
            .expect("read required owned process log"),
        pre_ceremony,
        "a fresh DKG ceremony was triggered by the restart"
    );
    assert!(
        world
            .rpc
            .is_participant(primary, &addr)
            .expect("observe consensus participation"),
        "node is not an ACTIVE participant after restart"
    );
    assert!(
        lockstep_ok(&world.rpc, primary, joiner_port),
        "restarted validator does not resume signing in lockstep"
    );
    assert_eq!(
        world
            .localnet
            .log_count(0, "byzantine evidence observed")
            .expect("read required owned process log"),
        0,
        "byzantine/equivocation evidence around the restart"
    );

    // The DKG-share restart can occur after the genesis-defined Tribute offering
    // phase has closed. State parity proves the restarted validator re-executed
    // the canonical chain without coupling this recovery check to a new offer.
    assert_eq!(
        world.rpc.supply(primary),
        world.rpc.supply(joiner_port),
        "canonical supply parity post-restart"
    );
}

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

fn pending_boundary_at(
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
    let primary = world.validators.primary_port();
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
    world.rpc.stake(&key, 1000).expect("stake joiner");
    world.rpc.confirm_ready(&key).expect("confirm joiner ready");

    let mut ceremony_started = false;
    for _ in 0..1_800 {
        if world
            .localnet
            .log_has(idx, "freezing validator set and starting DKG rotation")
            .expect("read required owned process log")
        {
            ceremony_started = true;
            break;
        }
        sleep(Duration::from_millis(100));
    }
    assert!(ceremony_started, "joiner's DKG ceremony never started");
    wait_for_dkg_retry_snapshot(&keys, idx, "dkg_player_retry.hex");
    assert!(
        !world
            .localnet
            .log_has(idx, "persisted completed DKG state before activation")
            .expect("read required owned process log"),
        "DKG completed before the intended in-flight restart"
    );
    assert_eq!(world.rpc.validator_status(primary, &addr), Some(1));
    assert!(!world
        .rpc
        .is_participant(primary, &addr)
        .expect("observe consensus participation"));
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
    if !world
        .rpc
        .is_participant(primary, &addr)
        .expect("observe consensus participation")
    {
        assert_eq!(world.rpc.validator_status(primary, &addr), Some(1));
        assert_eq!(world.rpc.active_count(primary), Some(4));
    }

    assert!(
        world
            .rpc
            .wait_participant(primary, &addr, 90)
            .expect("wait for observable consensus participation"),
        "joiner did not activate after interrupted DKG retry"
    );
    let expected_epoch = u64::try_from(old_epoch + 1).expect("epoch fits u64");
    assert_eq!(world.rpc.validator_status(primary, &addr), Some(2));
    assert_eq!(world.rpc.active_count(primary), Some(5));
    assert_eq!(world.rpc.epoch_on(primary), Some(expected_epoch));
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
        "retried DKG did not promote the joiner's share after activation"
    );
    assert_eq!(
        world.rpc.has_threshold_shares(joiner_port),
        Some(true),
        "ACTIVE joiner has no current private threshold share"
    );
    assert!(
        world
            .localnet
            .enclave_log_has(idx, "unsealed offer key + group signature")
            .expect("read required owned process log"),
        "joiner enclave did not recover its sealed state during DKG restart"
    );
    let baseline = world
        .rpc
        .finalized_result(primary)
        .expect("read finalized baseline after interrupted DKG activation");
    let mut ports = world.validators.committee_ports();
    ports.push(joiner_port);
    world
        .rpc
        .wait_finalized_checkpoint(&ports, baseline.saturating_add(2), 90)
        .expect("all five nodes converge after interrupted DKG recovery");

    // ACTIVE is committed while executing the epoch-boundary block, after that
    // block was already finalized by the old committee. Its absentee window
    // therefore closes K blocks later and may record that one pre-eligibility
    // finalization. Close the window before taking the signer-liveness baseline;
    // otherwise this assertion races delayed accounting for a block the joiner
    // was canonically forbidden to sign.
    let eligibility_height = world
        .rpc
        .head(primary)
        .expect("primary head at joiner eligibility");
    assert!(
        world
            .rpc
            .wait_block(primary, eligibility_height + LATE_FINALIZE_WINDOW_K, 60)
            .is_some(),
        "committee did not close the pre-eligibility voter-accounting window"
    );
    let voter_misses_before = world
        .rpc
        .voter_miss_count(primary, &addr)
        .expect("joiner voter-miss count after activation");
    let target = world
        .rpc
        .head(primary)
        .expect("primary head after joiner eligibility")
        + 5;
    let mut ports = world.validators.committee_ports();
    ports.push(joiner_port);
    for port in ports {
        assert!(world.rpc.wait_block(port, target, 60).is_some());
        assert_eq!(world.rpc.active_count(port), Some(5));
        assert_eq!(world.rpc.epoch_on(port), Some(expected_epoch));
    }
    assert_eq!(
        world.rpc.voter_miss_count(primary, &addr),
        Some(voter_misses_before),
        "ACTIVE joiner accumulated voter misses after DKG recovery"
    );
}

/// Restart at the earliest durable join checkpoint: registration, P2P identity
/// and enclave join are committed, but no stake/readiness or DKG side effect is.
#[when("a registered joining node and enclave restart before staking")]
fn restart_registered_joiner_before_staking(world: &mut World) {
    let primary = world.validators.primary_port();
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
    assert_eq!(world.rpc.validator_status(primary, &addr), Some(0));
    assert_eq!(
        world.rpc.stake_on(primary, &addr),
        Some(alloy_primitives::U256::ZERO)
    );
    assert!(!world
        .rpc
        .is_participant(primary, &addr)
        .expect("observe consensus participation"));
    assert_eq!(world.rpc.active_count(primary), Some(4));
    world.state.joiner_offer_public_before_restart = Some(
        world
            .localnet
            .node_offer_public(idx)
            .expect("registered joiner offer key must match canonical chain state"),
    );
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
    assert!(!world
        .rpc
        .is_participant(primary, &addr)
        .expect("observe consensus participation"));
    assert_eq!(world.rpc.active_count(primary), Some(4));
    assert_eq!(
        world.rpc.epoch_on(primary),
        Some(u64::try_from(old_epoch).expect("epoch fits u64"))
    );
    let before = world
        .state
        .joiner_offer_public_before_restart
        .expect("pre-restart joiner offer key");
    let after = world
        .localnet
        .node_offer_public(idx)
        .expect("restarted joiner offer key must match canonical chain state");
    assert_eq!(
        after, before,
        "registered joiner restart changed the permanent resident offer key"
    );
    assert!(
        world
            .localnet
            .enclave_log_has(idx, "unsealed offer key + group signature")
            .expect("read required owned process log"),
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
        world
            .rpc
            .wait_participant(primary, &addr, 90)
            .expect("wait for observable consensus participation"),
        "registered joiner did not activate after restart"
    );
    let expected_epoch = u64::try_from(old_epoch + 1).expect("epoch fits u64");
    assert_eq!(world.rpc.validator_status(primary, &addr), Some(2));
    assert_eq!(world.rpc.active_count(primary), Some(5));
    assert_eq!(world.rpc.epoch_on(primary), Some(expected_epoch));

    let baseline = world
        .rpc
        .finalized_result(primary)
        .expect("read finalized baseline after registered joiner activation");
    let mut ports = world.validators.committee_ports();
    ports.push(joiner_port);
    world
        .rpc
        .wait_finalized_checkpoint(&ports, baseline.saturating_add(2), 90)
        .expect("all five nodes converge after registered joiner restart");
    for port in ports {
        assert_eq!(world.rpc.active_count(port), Some(5));
        assert_eq!(world.rpc.epoch_on(port), Some(expected_epoch));
    }
}

/// Interrupt one existing committee member only after the scheduled 4->5
/// reshare has actually frozen and entered DKG. The restart must not create a
/// second target or permit a partial activation.
#[when("an active validator and enclave restart during a joining reshare")]
fn restart_active_validator_during_reshare(world: &mut World) {
    let primary = world.validators.primary_port();
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
    let addr = world.rpc.address_of(&key).expect("joiner address");
    world.state.joiner_addr = Some(addr.clone());
    world.rpc.stake(&key, 1000).expect("stake joiner");
    world.rpc.confirm_ready(&key).expect("confirm joiner ready");

    let mut ceremony_started = false;
    for _ in 0..1_800 {
        if world
            .localnet
            .log_has(0, "freezing validator set and starting DKG rotation")
            .expect("read required owned process log")
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
            .log_has(0, "persisted completed DKG state before activation")
            .expect("read required owned process log"),
        "reshare completed before the intended active-validator restart"
    );
    assert_eq!(world.rpc.validator_status(primary, &addr), Some(1));
    assert!(!world
        .rpc
        .is_participant(primary, &addr)
        .expect("observe consensus participation"));
    assert_eq!(world.rpc.active_count(primary), Some(4));
    let incumbent_keys = world.localnet.scenario_dir().join("validator-3/data/keys");
    wait_for_dkg_retry_snapshot(&incumbent_keys, 3, "dkg_dealer_retry.hex");
    wait_for_dkg_retry_snapshot(&incumbent_keys, 3, "dkg_player_retry.hex");
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
    let restarted_key = world
        .validators
        .get(3)
        .evm_key()
        .expect("restarted validator key");
    let restarted_addr = world
        .rpc
        .address_of(&restarted_key)
        .expect("restarted validator address");
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
    if !world
        .rpc
        .is_participant(primary, &addr)
        .expect("observe consensus participation")
    {
        assert_eq!(world.rpc.validator_status(primary, &addr), Some(1));
        assert_eq!(world.rpc.active_count(primary), Some(4));
    }

    assert!(
        world
            .rpc
            .wait_participant(primary, &addr, 90)
            .expect("wait for observable consensus participation"),
        "frozen reshare did not activate after active-validator recovery"
    );
    let expected_epoch = u64::try_from(old_epoch + 1).expect("epoch fits u64");
    let target = world
        .rpc
        .head(primary)
        .expect("primary head after active-validator recovery")
        + 3;
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
            .enclave_log_has(3, "unsealed offer key + group signature")
            .expect("read required owned process log"),
        "restarted active validator did not recover sealed enclave state"
    );
    assert!(
        world
            .localnet
            .log_has(3, "restoring durable DKG dealer transcript")
            .expect("read required owned process log"),
        "restarted active dealer did not restore the interrupted DKG transcript"
    );
    assert_eq!(
        world.rpc.has_threshold_shares(restarted),
        Some(true),
        "restarted ACTIVE validator has no current private threshold share"
    );
    let voter_misses_before = world
        .rpc
        .voter_miss_count(primary, &restarted_addr)
        .expect("restarted validator voter-miss count after activation");
    let voting_target = world
        .rpc
        .head(primary)
        .expect("primary head before recovered-validator voting window")
        + 5;
    assert!(
        world.rpc.wait_block(primary, voting_target, 60).is_some(),
        "committee did not continue after restarted validator activation"
    );
    assert_eq!(
        world.rpc.voter_miss_count(primary, &restarted_addr),
        Some(voter_misses_before),
        "restarted ACTIVE validator accumulated voter misses after DKG recovery"
    );
    assert!(
        lockstep_ok(&world.rpc, primary, restarted),
        "restarted active validator did not return to finalized lockstep"
    );
}
